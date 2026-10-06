//! CLIP image and text encoders on ONNX Runtime-free CPU inference.

use crate::tokenizer::{CONTEXT_LENGTH, Tokenizer};
use anyhow::{Context, Result, bail};
use anytopdf_onnx::{
    encoder::{Fit, ImageSpec, InputData, OnnxModel, image_to_nchw, l2_normalize},
    ndarray::{Array2, ArrayD, Axis},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    cell::OnceCell,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

/// OpenAI CLIP's pixel statistics, used when the model has no preprocessor config.
const CLIP_MEAN: [f32; 3] = [0.481_454_66, 0.457_827_5, 0.408_210_73];
const CLIP_STD: [f32; 3] = [0.268_629_54, 0.261_302_6, 0.275_777_1];

/// Embeds images and text into one shared space.
pub trait Encoder {
    /// Stable identifier of the model, so vectors from different models never mix.
    fn model_id(&self) -> &str;
    fn encode_image(&self, path: &Path) -> Result<Vec<f32>>;
    fn encode_text(&self, text: &str) -> Result<Vec<f32>>;
}

/// The files of a CLIP model directory.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelFiles {
    pub vision: PathBuf,
    pub text: PathBuf,
    pub tokenizer: PathBuf,
    pub preprocessor: Option<PathBuf>,
}

fn first_existing(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    names.iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

impl ModelFiles {
    /// Finds the model files in `dir`. Accepts the layout of the
    /// `anytopdf-plugin-clip --fetch-model` download (OpenAI ViT-B/32 export:
    /// `visual.onnx`, `textual.onnx`, `bpe_simple_vocab_16e6.txt.gz`) and Hugging
    /// Face ONNX exports (`onnx/vision_model.onnx`, `onnx/text_model.onnx`,
    /// `tokenizer.json`, `preprocessor_config.json`).
    pub fn locate(dir: &Path) -> Result<Self> {
        if !dir.is_dir() {
            bail!("model directory {} does not exist", dir.display());
        }
        let find = |names: &[&str], what: &str| {
            first_existing(dir, names).with_context(|| {
                format!(
                    "{} has no {what} (looked for {})",
                    dir.display(),
                    names.join(", ")
                )
            })
        };
        Ok(Self {
            vision: find(
                &["visual.onnx", "vision_model.onnx", "onnx/vision_model.onnx"],
                "image encoder",
            )?,
            text: find(
                &["textual.onnx", "text_model.onnx", "onnx/text_model.onnx"],
                "text encoder",
            )?,
            tokenizer: find(
                &[
                    "tokenizer.json",
                    "bpe_simple_vocab_16e6.txt.gz",
                    "bpe_simple_vocab_16e6.txt",
                ],
                "tokenizer",
            )?,
            preprocessor: first_existing(dir, &["preprocessor_config.json"]),
        })
    }

    /// `clip-` plus a digest of both encoders' sizes and first MiB, which is
    /// cheap to compute and changes whenever either model file is replaced.
    pub fn model_id(&self) -> Result<String> {
        let mut hasher = Sha256::new();
        for path in [&self.vision, &self.text] {
            let len = fs::metadata(path)?.len();
            hasher.update(len.to_le_bytes());
            let mut head = Vec::with_capacity(1 << 20);
            File::open(path)?.take(1 << 20).read_to_end(&mut head)?;
            hasher.update(&head);
        }
        let digest = hasher.finalize();
        let hex: String = digest[..6].iter().map(|b| format!("{b:02x}")).collect();
        Ok(format!("clip-{hex}"))
    }

    /// Pixel preprocessing from `preprocessor_config.json`, else CLIP's defaults.
    pub fn image_spec(&self) -> Result<ImageSpec> {
        let mut spec = ImageSpec {
            size: 224,
            fit: Fit::CenterCrop,
            mean: CLIP_MEAN,
            std: CLIP_STD,
        };
        let Some(path) = &self.preprocessor else {
            return Ok(spec);
        };
        let config: Value = serde_json::from_slice(&fs::read(path)?)
            .with_context(|| format!("parse {}", path.display()))?;
        let size = &config["crop_size"];
        let size = size["height"].as_u64().or_else(|| size.as_u64());
        if let Some(size) = size.and_then(|s| u32::try_from(s).ok()) {
            spec.size = size;
        }
        let triple = |v: &Value| -> Option<[f32; 3]> {
            let a = v.as_array()?;
            Some([
                a.first()?.as_f64()? as f32,
                a.get(1)?.as_f64()? as f32,
                a.get(2)?.as_f64()? as f32,
            ])
        };
        if let Some(mean) = triple(&config["image_mean"]) {
            spec.mean = mean;
        }
        if let Some(std) = triple(&config["image_std"]) {
            spec.std = std;
        }
        Ok(spec)
    }
}

/// CLIP on the CPU. The text encoder loads on first use, so runs that only
/// embed images and skip scene tags never pay for it.
pub struct Clip {
    id: String,
    files: ModelFiles,
    spec: ImageSpec,
    vision: OnceCell<OnnxModel>,
    text: OnceCell<(OnnxModel, Tokenizer)>,
}

impl Clip {
    pub fn open(dir: &Path) -> Result<Self> {
        let files = ModelFiles::locate(dir)?;
        Ok(Self {
            id: files.model_id()?,
            spec: files.image_spec()?,
            files,
            vision: OnceCell::new(),
            text: OnceCell::new(),
        })
    }

    fn vision(&self) -> Result<&OnnxModel> {
        if let Some(model) = self.vision.get() {
            return Ok(model);
        }
        let side = self.spec.size as usize;
        let model = OnnxModel::load(&self.files.vision, &[&[1, 3, side, side]])?;
        Ok(self.vision.get_or_init(|| model))
    }

    fn text(&self) -> Result<&(OnnxModel, Tokenizer)> {
        if let Some(pair) = self.text.get() {
            return Ok(pair);
        }
        let tokenizer = Tokenizer::load(&self.files.tokenizer)?;
        // Token ids and, when the export has one, the attention mask.
        let model = OnnxModel::load_with(&self.files.text, |_, _| Some(vec![1, CONTEXT_LENGTH]))?;
        Ok(self.text.get_or_init(|| (model, tokenizer)))
    }
}

/// The embedding among a model's outputs: the one whose name mentions
/// `embed`, else the first two-dimensional output.
fn pick_embedding(model: &OnnxModel, outputs: Vec<ArrayD<f32>>) -> Result<Vec<f32>> {
    let named = model
        .outputs()
        .iter()
        .position(|name| name.contains("embed"));
    let index = named
        .or_else(|| outputs.iter().position(|o| o.ndim() == 2))
        .context("model has no embedding output")?;
    let output = outputs.into_iter().nth(index).context("missing output")?;
    let mut vector: Vec<f32> = match output.ndim() {
        2 => output.index_axis(Axis(0), 0).iter().copied().collect(),
        1 => output.iter().copied().collect(),
        n => bail!("embedding output has {n} dimensions"),
    };
    if vector.is_empty() || vector.iter().any(|v| !v.is_finite()) {
        bail!("model produced an empty or non-finite embedding");
    }
    l2_normalize(&mut vector);
    Ok(vector)
}

impl Encoder for Clip {
    fn model_id(&self) -> &str {
        &self.id
    }

    fn encode_image(&self, path: &Path) -> Result<Vec<f32>> {
        let image = image::open(path).with_context(|| format!("decode {}", path.display()))?;
        let model = self.vision()?;
        let pixels = image_to_nchw(&image, &self.spec).into_dyn();
        pick_embedding(model, model.run(vec![InputData::F32(pixels)])?)
    }

    fn encode_text(&self, text: &str) -> Result<Vec<f32>> {
        let (model, tokenizer) = self.text()?;
        let (ids, mask) = tokenizer.encode_padded(text);
        let row = |v: Vec<i64>| -> Result<InputData> {
            Ok(InputData::I64(
                Array2::from_shape_vec((1, CONTEXT_LENGTH), v)?.into_dyn(),
            ))
        };
        let inputs = model
            .inputs()
            .iter()
            .map(|spec| {
                if spec.name.contains("mask") {
                    row(mask.clone())
                } else {
                    row(ids.clone())
                }
            })
            .collect::<Result<Vec<_>>>()?;
        pick_embedding(model, model.run(inputs)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_accepts_hugging_face_layout_and_reads_preprocessing() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("onnx")).unwrap();
        for name in [
            "onnx/vision_model.onnx",
            "onnx/text_model.onnx",
            "tokenizer.json",
        ] {
            fs::write(dir.path().join(name), b"x").unwrap();
        }
        fs::write(
            dir.path().join("preprocessor_config.json"),
            r#"{"crop_size": {"height": 256, "width": 256},
                "image_mean": [0.5, 0.5, 0.5], "image_std": [0.5, 0.5, 0.5]}"#,
        )
        .unwrap();
        let files = ModelFiles::locate(dir.path()).unwrap();
        assert!(files.vision.ends_with("onnx/vision_model.onnx"));
        let spec = files.image_spec().unwrap();
        assert_eq!(spec.size, 256);
        assert_eq!(spec.mean, [0.5; 3]);
        assert!(files.model_id().unwrap().starts_with("clip-"));
    }

    #[test]
    fn locate_names_what_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("visual.onnx"), b"x").unwrap();
        let err = ModelFiles::locate(dir.path()).unwrap_err();
        assert!(format!("{err:#}").contains("text encoder"));
    }

    #[test]
    fn model_id_changes_when_a_model_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["visual.onnx", "textual.onnx", "tokenizer.json"] {
            fs::write(dir.path().join(name), b"one").unwrap();
        }
        let files = ModelFiles::locate(dir.path()).unwrap();
        let before = files.model_id().unwrap();
        fs::write(dir.path().join("visual.onnx"), b"two").unwrap();
        assert_ne!(before, files.model_id().unwrap());
    }
}
