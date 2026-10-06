//! Shared ONNX inference helpers for anytopdf model plugins (CLIP, face and
//! object detection).
//!
//! Inference runs on the CPU with [tract](https://github.com/sonos/tract), a
//! pure-Rust ONNX runtime, so plugins build on every release target without
//! downloading a native runtime and ship as one self-contained executable.

use anyhow::{Context, Result, bail, ensure};
use image::{DynamicImage, imageops::FilterType};
use std::{fmt, path::Path, sync::Arc};
use tract_onnx::prelude::*;
use tract_onnx::tract_hir::infer::Factoid;

use ndarray::{Array4, ArrayD, IxDyn};
pub use tract_onnx::prelude::tract_ndarray as ndarray;

/// Element type of a model input, as declared by the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementType {
    F32,
    I32,
    I64,
}

/// A model input: its name and declared element type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputSpec {
    pub name: String,
    pub element: ElementType,
}

/// Data for one model input. Integer data is converted to the input's declared
/// integer width, so callers need not know whether a model wants i32 or i64.
#[derive(Debug, Clone)]
pub enum InputData {
    F32(ArrayD<f32>),
    I64(ArrayD<i64>),
}

/// An ONNX model optimized for fixed input shapes.
pub struct OnnxModel {
    plan: Arc<TypedRunnableModel>,
    inputs: Vec<InputSpec>,
    outputs: Vec<String>,
}

impl fmt::Debug for OnnxModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OnnxModel")
            .field("inputs", &self.inputs)
            .field("outputs", &self.outputs)
            .finish()
    }
}

impl OnnxModel {
    /// Loads `path` and fixes every input to the matching shape in `shapes`
    /// (one per model input, in model order). Dynamic dimensions such as the
    /// batch size must be made concrete here.
    pub fn load(path: &Path, shapes: &[&[usize]]) -> Result<Self> {
        Self::load_with(path, |ix, _| shapes.get(ix).map(|s| s.to_vec()))
    }

    /// Like [`OnnxModel::load`], but asks `shape(index, name)` for each input's
    /// shape, for models whose input count or names vary between exports.
    pub fn load_with(
        path: &Path,
        shape: impl Fn(usize, &str) -> Option<Vec<usize>>,
    ) -> Result<Self> {
        // Exports often declare symbolic output shapes (a `batch` dim) that
        // conflict with the concrete input shapes pinned below; tract re-derives them.
        let mut model = tract_onnx::onnx()
            .with_ignore_output_shapes(true)
            .with_ignore_value_info(true)
            .model_for_path(path)
            .map_err(anyhow_from)
            .with_context(|| format!("load ONNX model {}", path.display()))?;
        let count = model.inputs.len();
        let mut inputs = Vec::with_capacity(count);
        for ix in 0..count {
            let name = model.node(model.inputs[ix].node).name.clone();
            let shape = shape(ix, &name).with_context(|| {
                format!("no shape given for input {name} of {}", path.display())
            })?;
            let fact = model.input_fact(ix).map_err(anyhow_from)?;
            let datum = fact.datum_type.concretize().unwrap_or(DatumType::F32);
            let element = match datum {
                DatumType::F32 => ElementType::F32,
                DatumType::I32 => ElementType::I32,
                DatumType::I64 => ElementType::I64,
                other => bail!("unsupported input element type {other:?}"),
            };
            model
                .set_input_fact(ix, InferenceFact::dt_shape(datum, shape))
                .map_err(anyhow_from)?;
            inputs.push(InputSpec { name, element });
        }
        let outputs = model
            .outputs
            .iter()
            .map(|outlet| model.node(outlet.node).name.clone())
            .collect();
        let plan = model
            .into_optimized()
            .and_then(|m| m.into_runnable())
            .map_err(anyhow_from)
            .with_context(|| format!("optimize ONNX model {}", path.display()))?;
        Ok(Self {
            plan,
            inputs,
            outputs,
        })
    }

    pub fn inputs(&self) -> &[InputSpec] {
        &self.inputs
    }

    pub fn outputs(&self) -> &[String] {
        &self.outputs
    }

    /// Runs the model on one value per input and returns every output as f32.
    pub fn run(&self, data: Vec<InputData>) -> Result<Vec<ArrayD<f32>>> {
        ensure!(
            data.len() == self.inputs.len(),
            "model expects {} input(s), got {}",
            self.inputs.len(),
            data.len()
        );
        let mut values = TVec::new();
        for (spec, data) in self.inputs.iter().zip(data) {
            let tensor: Tensor = match (spec.element, data) {
                (ElementType::F32, InputData::F32(a)) => a.into(),
                (ElementType::I64, InputData::I64(a)) => a.into(),
                (ElementType::I32, InputData::I64(a)) => {
                    let narrowed = a
                        .iter()
                        .map(|&v| i32::try_from(v))
                        .collect::<Result<Vec<_>, _>>()
                        .context("integer input does not fit in i32")?;
                    ArrayD::from_shape_vec(a.raw_dim(), narrowed)?.into()
                }
                (ElementType::F32, InputData::I64(a)) => a.mapv(|v| v as f32).into(),
                (_, InputData::F32(_)) => bail!("input {} expects integers", spec.name),
            };
            values.push(tensor.into());
        }
        let outputs = self.plan.run(values).map_err(anyhow_from)?;
        outputs
            .iter()
            .map(|value| {
                let tensor = value
                    .cast_to::<f32>()
                    .map_err(anyhow_from)
                    .context("output is not numeric")?;
                let view = tensor.to_plain_array_view::<f32>().map_err(anyhow_from)?;
                Ok(view.to_owned().into_dimensionality::<IxDyn>()?)
            })
            .collect()
    }
}

fn anyhow_from(e: TractError) -> anyhow::Error {
    anyhow::anyhow!("{e:#}")
}

/// How an image is fitted to a model's square input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Resize the shortest side to the input size, then crop the centre (CLIP).
    CenterCrop,
    /// Resize both sides to the input size, ignoring the aspect ratio.
    Stretch,
}

/// Pixel preprocessing for a square RGB model input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageSpec {
    pub size: u32,
    pub fit: Fit,
    /// Per-channel mean and standard deviation applied after scaling to 0..1.
    pub mean: [f32; 3],
    pub std: [f32; 3],
}

/// Converts an image to a normalized `1 x 3 x size x size` RGB tensor.
pub fn image_to_nchw(image: &DynamicImage, spec: &ImageSpec) -> Array4<f32> {
    let size = spec.size.max(1);
    let fitted = match spec.fit {
        Fit::Stretch => image.resize_exact(size, size, FilterType::CatmullRom),
        Fit::CenterCrop => {
            let (w, h) = (image.width().max(1), image.height().max(1));
            let scale = size as f64 / w.min(h) as f64;
            let nw = ((w as f64 * scale).round() as u32).max(size);
            let nh = ((h as f64 * scale).round() as u32).max(size);
            image.resize_exact(nw, nh, FilterType::CatmullRom).crop_imm(
                (nw - size) / 2,
                (nh - size) / 2,
                size,
                size,
            )
        }
    };
    let rgb = fitted.to_rgb8();
    let side = size as usize;
    Array4::from_shape_fn((1, 3, side, side), |(_, c, y, x)| {
        let value = rgb.get_pixel(x as u32, y as u32)[c] as f32 / 255.0;
        (value - spec.mean[c]) / spec.std[c]
    })
}

/// Scales `vector` to unit length in place; a zero vector stays zero.
pub fn l2_normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 && norm.is_finite() {
        vector.iter_mut().for_each(|v| *v /= norm);
    }
}

/// Cosine similarity of two vectors of equal length (0 for a zero vector).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|v| v * v).sum::<f32>().sqrt();
    let nb = b.iter().map(|v| v * v).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    const IDENTITY: ImageSpec = ImageSpec {
        size: 4,
        fit: Fit::CenterCrop,
        mean: [0.0; 3],
        std: [1.0; 3],
    };

    #[test]
    fn center_crop_keeps_the_middle_of_a_wide_image() {
        // Left third red, middle third green, right third blue.
        let image = RgbImage::from_fn(12, 4, |x, _| match x / 4 {
            0 => Rgb([255, 0, 0]),
            1 => Rgb([0, 255, 0]),
            _ => Rgb([0, 0, 255]),
        });
        let tensor = image_to_nchw(&DynamicImage::ImageRgb8(image), &IDENTITY);
        assert_eq!(tensor.shape(), &[1, 3, 4, 4]);
        assert!(tensor[[0, 1, 2, 2]] > 0.9, "centre should be green");
        assert!(tensor[[0, 0, 2, 2]] < 0.1);
    }

    #[test]
    fn normalization_applies_mean_and_std_per_channel() {
        let image = RgbImage::from_pixel(2, 2, Rgb([255, 0, 51]));
        let spec = ImageSpec {
            size: 2,
            fit: Fit::Stretch,
            mean: [0.5, 0.5, 0.5],
            std: [0.5, 0.25, 1.0],
        };
        let tensor = image_to_nchw(&DynamicImage::ImageRgb8(image), &spec);
        assert!((tensor[[0, 0, 0, 0]] - 1.0).abs() < 1e-5);
        assert!((tensor[[0, 1, 0, 0]] + 2.0).abs() < 1e-5);
        assert!((tensor[[0, 2, 1, 1]] + 0.3).abs() < 1e-5);
    }

    #[test]
    fn cosine_and_normalize_handle_zero_vectors() {
        let mut v = [3.0, 4.0];
        l2_normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        let mut zero = [0.0, 0.0];
        l2_normalize(&mut zero);
        assert_eq!(zero, [0.0, 0.0]);
        assert_eq!(cosine(&zero, &v), 0.0);
        assert!((cosine(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn loading_a_missing_model_names_the_path() {
        let err = OnnxModel::load(Path::new("/nonexistent/model.onnx"), &[&[1]]).unwrap_err();
        assert!(format!("{err:#}").contains("/nonexistent/model.onnx"));
    }
}
