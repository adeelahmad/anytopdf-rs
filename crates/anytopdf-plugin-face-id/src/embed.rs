//! ArcFace-style face embeddings through tract (pure-Rust ONNX inference).

use anyhow::{Context, Result, bail};
use image::RgbImage;
use sha2::{Digest, Sha256};
use std::path::Path;
use tract_onnx::pb::GraphProto;
use tract_onnx::prelude::*;
use tract_onnx::tract_hir::infer::Factoid;

type Runnable = std::sync::Arc<TypedRunnableModel>;

/// Overrides how crops are scaled before inference: `raw` (0 to 255) or
/// `normalized` (-1 to 1). Unset, the model graph decides.
pub const INPUT_ENV: &str = "ANYTOPDF_FACE_EMBED_INPUT";

/// How pixel values are fed to the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputScale {
    /// 0 to 255, for models that scale their own input (OpenCV SFace,
    /// ONNX-zoo ArcFace R100).
    Raw,
    /// `(p - 127.5) / 127.5`, for InsightFace exports such as `w600k_mbf`.
    Normalized,
}

pub struct Embedder {
    model: Runnable,
    /// Square crop size the model expects.
    pub size: u32,
    nhwc: bool,
    pub scale: InputScale,
    /// First 16 hex digits of the model file's SHA-256 (with a raw-input
    /// marker, so vectors from the earlier normalized feed never match).
    pub id: String,
}

impl Embedder {
    /// Loads an ONNX model whose single input is a 3-channel square crop, in
    /// NCHW (InsightFace ArcFace) or NHWC layout, with a static or dynamic
    /// batch dimension. The crop size comes from the model, else 112.
    pub fn load(path: &Path, scale: Option<InputScale>) -> Result<Self> {
        let bytes =
            std::fs::read(path).with_context(|| format!("read model {}", path.display()))?;
        let onnx = tract_onnx::onnx();
        let proto = onnx
            .proto_model_for_read(&mut &bytes[..])
            .context("parse ONNX model")?;
        let scale = scale.unwrap_or_else(|| {
            if proto.graph.as_ref().is_some_and(scales_own_input) {
                InputScale::Raw
            } else {
                InputScale::Normalized
            }
        });
        let mut digest = Sha256::new();
        digest.update(&bytes);
        if scale == InputScale::Raw {
            digest.update(b"\0input=raw");
        }
        let id = digest
            .finalize()
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect();
        let model = onnx
            .model_for_proto_model(&proto)
            .context("parse ONNX model")?;
        let dims: Vec<Option<usize>> = model
            .input_fact(0)
            .context("model has no input")?
            .shape
            .dims()
            .map(|d| {
                d.concretize()
                    .and_then(|d: TDim| d.as_i64())
                    .and_then(|d| usize::try_from(d).ok())
            })
            .collect();
        if dims.len() != 4 {
            bail!("model input must have 4 dimensions, found {}", dims.len());
        }
        let nhwc = dims[3] == Some(3) && dims[1] != Some(3);
        let size = if nhwc { dims[1] } else { dims[2] }.unwrap_or(112) as u32;
        if !(16..=1024).contains(&size) {
            bail!("unsupported model input size {size}");
        }
        let shape: [usize; 4] = if nhwc {
            [1, size as usize, size as usize, 3]
        } else {
            [1, 3, size as usize, size as usize]
        };
        let model = model
            .with_input_fact(0, f32::fact(shape).into())?
            .into_optimized()
            .context("optimize ONNX model")?
            .into_runnable()?;
        Ok(Self {
            model,
            size,
            nhwc,
            scale,
            id,
        })
    }

    /// Embeds an aligned `size` x `size` RGB crop.
    pub fn embed(&self, crop: &RgbImage) -> Result<Vec<f32>> {
        if crop.dimensions() != (self.size, self.size) {
            bail!("crop must be {0}x{0}", self.size);
        }
        let n = self.size as usize;
        let value = |x: usize, y: usize, c: usize| {
            let p = f32::from(crop.get_pixel(x as u32, y as u32)[c]);
            match self.scale {
                InputScale::Raw => p,
                InputScale::Normalized => (p - 127.5) / 127.5,
            }
        };
        let input: Tensor = if self.nhwc {
            tract_ndarray::Array4::from_shape_fn((1, n, n, 3), |(_, y, x, c)| value(x, y, c)).into()
        } else {
            tract_ndarray::Array4::from_shape_fn((1, 3, n, n), |(_, c, y, x)| value(x, y, c)).into()
        };
        let outputs = self.model.run(tvec!(input.into()))?;
        let output = outputs[0].to_plain_array_view::<f32>()?;
        let vector: Vec<f32> = output.iter().copied().collect();
        if vector.is_empty() || vector.iter().any(|v: &f32| !v.is_finite()) {
            bail!("model returned an empty or non-finite embedding");
        }
        Ok(anytopdf_faces::vector::normalize(vector))
    }
}

/// Parses [`INPUT_ENV`].
pub fn input_scale_override(value: Option<&str>) -> Result<Option<InputScale>> {
    match value.map(str::trim) {
        None | Some("" | "auto") => Ok(None),
        Some("raw") => Ok(Some(InputScale::Raw)),
        Some("normalized") => Ok(Some(InputScale::Normalized)),
        Some(other) => bail!("{INPUT_ENV} must be raw, normalized or auto, not {other:?}"),
    }
}

/// True when the graph rescales its image input itself: the input (through
/// any `Identity` nodes) feeds a `Sub`, `Add`, `Mul` or `Div` by a constant.
/// MXNet-converted models (SFace, ONNX-zoo ArcFace) start this way and expect
/// 0-255 pixels; feeding them -1 to 1 makes every face embed alike.
fn scales_own_input(graph: &GraphProto) -> bool {
    let constants: std::collections::HashSet<&str> = graph
        .initializer
        .iter()
        .map(|t| t.name.as_str())
        .chain(
            graph
                .node
                .iter()
                .filter(|n| n.op_type == "Constant")
                .flat_map(|n| n.output.iter().map(String::as_str)),
        )
        .collect();
    let Some(input) = graph
        .input
        .iter()
        .map(|i| i.name.as_str())
        .find(|name| !constants.contains(name))
    else {
        return false;
    };
    let mut names = vec![input];
    while let Some(name) = names.pop() {
        for node in graph
            .node
            .iter()
            .filter(|n| n.input.iter().any(|i| i == name))
        {
            match node.op_type.as_str() {
                "Identity" => names.extend(node.output.iter().map(String::as_str)),
                "Sub" | "Add" | "Mul" | "Div"
                    if node
                        .input
                        .iter()
                        .any(|i| i != name && constants.contains(i.as_str())) =>
                {
                    return true;
                }
                _ => {}
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_scale_override_accepts_raw_normalized_and_auto() {
        assert_eq!(input_scale_override(None).unwrap(), None);
        assert_eq!(input_scale_override(Some("auto")).unwrap(), None);
        assert_eq!(
            input_scale_override(Some("raw")).unwrap(),
            Some(InputScale::Raw)
        );
        assert_eq!(
            input_scale_override(Some(" normalized ")).unwrap(),
            Some(InputScale::Normalized)
        );
        assert!(input_scale_override(Some("0-255")).is_err());
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn models_that_scale_their_own_input_get_raw_pixels() {
        let raw = Embedder::load(&fixture("tiny-embed-raw.onnx"), None).unwrap();
        assert_eq!(raw.scale, InputScale::Raw);
        let plain = Embedder::load(&fixture("tiny-embed.onnx"), None).unwrap();
        assert_eq!(plain.scale, InputScale::Normalized);
        let forced = Embedder::load(
            &fixture("tiny-embed-raw.onnx"),
            Some(InputScale::Normalized),
        )
        .unwrap();
        assert_eq!(forced.scale, InputScale::Normalized);
        assert_ne!(
            raw.id, forced.id,
            "raw-input vectors get their own model id"
        );
    }
}
