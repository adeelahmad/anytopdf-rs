//! Shared ONNX inference helpers for anytopdf vision plugins.
//!
//! Models run on [tract](https://github.com/sonos/tract), a pure-Rust ONNX
//! runtime, so plugins that use this crate build and ship on every release
//! target without downloading native libraries. The crate covers the parts
//! every detector needs: loading a model with a fixed image input, letterbox
//! preprocessing into an NCHW tensor, mapping boxes back to normalized source
//! coordinates, and non-maximum suppression. [`detector`] serves fixed-input
//! detectors with named outputs and keypoints (faces), and [`encoder`] serves
//! multi-input encoders and embedding maths (CLIP).

use anyhow::{Context, Result, bail};
use image::{DynamicImage, GenericImageView, imageops::FilterType};
use std::{collections::BTreeMap, path::Path};
use tract_onnx::prelude::*;
use tract_onnx::tract_hir::{infer::Factoid, internal::DimLike};

pub use tract_onnx::prelude::tract_ndarray as ndarray;

pub mod detector;
pub mod encoder;
#[cfg(feature = "testing")]
pub mod testing;

type Plan = Arc<TypedRunnableModel>;

/// Exporters annotate intermediate values with symbolic shapes (`batch`) that
/// conflict with the fixed input this crate sets, so only the input is trusted.
fn onnx() -> tract_onnx::Onnx {
    tract_onnx::onnx()
        .with_ignore_output_shapes(true)
        .with_ignore_value_info(true)
}

/// A loaded image model with one `[1, 3, height, width]` float input.
pub struct Model {
    plan: Plan,
    width: u32,
    height: u32,
    metadata: BTreeMap<String, String>,
}

impl Model {
    /// Loads an ONNX model. When the model's input height and width are not
    /// fixed, `fallback_size` is used for both.
    pub fn load(path: &Path, fallback_size: u32) -> Result<Self> {
        let model = onnx()
            .model_for_path(path)
            .map_err(|e| anyhow::anyhow!("{e:#}"))
            .with_context(|| format!("load ONNX model {}", path.display()))?;
        Self::from_inference(model, fallback_size)
    }

    /// Builds a model from an in-memory ONNX graph.
    pub fn from_proto(proto: &tract_onnx::pb::ModelProto, fallback_size: u32) -> Result<Self> {
        let model = onnx()
            .model_for_proto_model(proto)
            .map_err(|e| anyhow::anyhow!("{e:#}"))
            .context("load ONNX model")?;
        Self::from_inference(model, fallback_size)
    }

    fn from_inference(model: InferenceModel, fallback_size: u32) -> Result<Self> {
        if model.inputs.len() != 1 {
            bail!("expected one image input, found {}", model.inputs.len());
        }
        let metadata = model
            .properties
            .iter()
            .filter_map(|(key, value)| {
                let key = key.strip_prefix("onnx.metadata_props.")?;
                let value = value.try_as_plain_ram().ok()?.to_scalar::<String>().ok()?;
                Some((key.to_string(), value.clone()))
            })
            .collect();
        let fact = model
            .input_fact(0)
            .map_err(|e| anyhow::anyhow!("{e:#}"))?
            .clone();
        let dims: Vec<Option<u32>> = fact
            .shape
            .dims()
            .map(|d| {
                d.concretize()
                    .and_then(|d: TDim| d.to_usize().ok())
                    .and_then(|d| u32::try_from(d).ok())
                    .filter(|d| *d > 0)
            })
            .collect();
        if !dims.is_empty() && dims.len() != 4 {
            bail!("expected a [batch, 3, height, width] image input");
        }
        if let Some(Some(channels)) = dims.get(1)
            && *channels != 3
        {
            bail!("expected 3 input channels, found {channels}");
        }
        let height = dims.get(2).copied().flatten().unwrap_or(fallback_size);
        let width = dims.get(3).copied().flatten().unwrap_or(fallback_size);
        if width == 0 || height == 0 {
            bail!("model input size must be positive");
        }
        let plan = model
            .with_input_fact(0, f32::fact([1, 3, height as usize, width as usize]).into())
            .and_then(|m| m.into_optimized())
            .and_then(|m| m.into_runnable())
            .map_err(|e| anyhow::anyhow!("{e:#}"))
            .context("prepare ONNX model")?;
        Ok(Self {
            plan,
            width,
            height,
            metadata,
        })
    }

    /// The model's input width and height in pixels.
    pub fn input_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// A string from the model's `metadata_props`, such as Ultralytics' `names`.
    pub fn metadata(&self, key: &str) -> Option<&str> {
        self.metadata.get(key).map(String::as_str)
    }

    /// Runs the model and returns every output as a float array.
    pub fn run(&self, input: ndarray::Array4<f32>) -> Result<Vec<ndarray::ArrayD<f32>>> {
        let outputs = self
            .plan
            .run(tvec!(Tensor::from(input).into()))
            .map_err(|e| anyhow::anyhow!("{e:#}"))
            .context("run ONNX model")?;
        outputs
            .iter()
            .map(|t: &TValue| {
                let t = t.cast_to::<f32>().map_err(|e| anyhow::anyhow!("{e:#}"))?;
                Ok(t.try_as_plain_ram()
                    .and_then(|v| v.to_array_view::<f32>())
                    .map_err(|e| anyhow::anyhow!("{e:#}"))?
                    .to_owned())
            })
            .collect()
    }
}

/// How a source image was fitted into the model input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Letterbox {
    pub scale: f32,
    pub pad_x: f32,
    pub pad_y: f32,
    pub source_width: u32,
    pub source_height: u32,
}

impl Letterbox {
    /// Maps a box in model-input pixels (corners) to a normalized
    /// `[x, y, width, height]` on the source image, clipped to the image.
    /// Returns `None` when nothing of the box is left on the image.
    pub fn to_normalized(&self, x1: f32, y1: f32, x2: f32, y2: f32) -> Option<[f32; 4]> {
        let w = self.source_width as f32;
        let h = self.source_height as f32;
        let unmap_x = |x: f32| ((x - self.pad_x) / self.scale).clamp(0.0, w) / w;
        let unmap_y = |y: f32| ((y - self.pad_y) / self.scale).clamp(0.0, h) / h;
        let (left, right) = (unmap_x(x1.min(x2)), unmap_x(x1.max(x2)));
        let (top, bottom) = (unmap_y(y1.min(y2)), unmap_y(y1.max(y2)));
        let region = [left, top, right - left, bottom - top];
        (region.iter().all(|v| v.is_finite()) && region[2] > 0.0 && region[3] > 0.0)
            .then_some(region)
    }
}

/// Grey used by Ultralytics for letterbox padding.
const PAD: f32 = 114.0 / 255.0;

/// Scales `image` to fit `width` x `height` without distortion, centres it on
/// a grey canvas and returns the RGB `[1, 3, height, width]` tensor in 0..1.
pub fn letterbox(
    image: &DynamicImage,
    width: u32,
    height: u32,
) -> (ndarray::Array4<f32>, Letterbox) {
    let (source_width, source_height) = image.dimensions();
    let scale = (width as f32 / source_width.max(1) as f32)
        .min(height as f32 / source_height.max(1) as f32);
    let scaled_w = ((source_width as f32 * scale).round() as u32).clamp(1, width);
    let scaled_h = ((source_height as f32 * scale).round() as u32).clamp(1, height);
    let pad_x = (width - scaled_w) / 2;
    let pad_y = (height - scaled_h) / 2;
    let resized = image
        .resize_exact(scaled_w, scaled_h, FilterType::Triangle)
        .to_rgb8();
    let mut tensor =
        ndarray::Array4::<f32>::from_elem((1, 3, height as usize, width as usize), PAD);
    for (x, y, pixel) in resized.enumerate_pixels() {
        let (tx, ty) = ((x + pad_x) as usize, (y + pad_y) as usize);
        for c in 0..3 {
            tensor[[0, c, ty, tx]] = f32::from(pixel[c]) / 255.0;
        }
    }
    (
        tensor,
        Letterbox {
            scale,
            pad_x: pad_x as f32,
            pad_y: pad_y as f32,
            source_width,
            source_height,
        },
    )
}

/// One candidate box in model-input pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    pub class: usize,
    pub score: f32,
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl Detection {
    fn area(&self) -> f32 {
        (self.x2 - self.x1).max(0.0) * (self.y2 - self.y1).max(0.0)
    }

    pub fn iou(&self, other: &Detection) -> f32 {
        let w = (self.x2.min(other.x2) - self.x1.max(other.x1)).max(0.0);
        let h = (self.y2.min(other.y2) - self.y1.max(other.y1)).max(0.0);
        let intersection = w * h;
        let union = self.area() + other.area() - intersection;
        if union > 0.0 {
            intersection / union
        } else {
            0.0
        }
    }
}

/// Greedy per-class non-maximum suppression: keeps the highest-scoring box
/// and drops same-class boxes overlapping it by more than `iou`. The result
/// is sorted by descending score and holds at most `limit` boxes.
pub fn nms(mut detections: Vec<Detection>, iou: f32, limit: usize) -> Vec<Detection> {
    detections.retain(|d| d.score.is_finite());
    detections.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Detection> = Vec::new();
    for candidate in detections {
        if kept.len() >= limit {
            break;
        }
        if kept
            .iter()
            .all(|k| k.class != candidate.class || k.iou(&candidate) <= iou)
        {
            kept.push(candidate);
        }
    }
    kept
}

/// Checks a requested execution device. The tract runtime runs on the CPU
/// only, so any other request yields a warning and CPU execution.
pub fn device_warning(requested: Option<&str>) -> Option<String> {
    match requested.map(str::trim).filter(|d| !d.is_empty()) {
        None => None,
        Some(d) if d.eq_ignore_ascii_case("cpu") || d.eq_ignore_ascii_case("auto") => None,
        Some(d) => Some(format!(
            "device {d:?} is not available in the built-in ONNX runtime; running on the CPU"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    #[test]
    fn letterbox_centres_wide_images_and_pads_with_grey() {
        let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(200, 100, Rgb([255, 0, 0])));
        let (tensor, fit) = letterbox(&image, 64, 64);
        assert_eq!(tensor.shape(), &[1, 3, 64, 64]);
        assert_eq!(fit.scale, 0.32);
        assert_eq!((fit.pad_x, fit.pad_y), (0.0, 16.0));
        assert!((tensor[[0, 0, 0, 0]] - PAD).abs() < 1e-6);
        assert_eq!(tensor[[0, 0, 32, 32]], 1.0);
        assert_eq!(tensor[[0, 1, 32, 32]], 0.0);
    }

    #[test]
    fn letterbox_boxes_map_back_to_normalized_source_coordinates() {
        let fit = Letterbox {
            scale: 0.32,
            pad_x: 0.0,
            pad_y: 16.0,
            source_width: 200,
            source_height: 100,
        };
        let [x, y, w, h] = fit.to_normalized(16.0, 24.0, 48.0, 40.0).unwrap();
        assert!((x - 0.25).abs() < 1e-5 && (w - 0.5).abs() < 1e-5);
        assert!((y - 0.25).abs() < 1e-5 && (h - 0.5).abs() < 1e-5);
        // Boxes inside the padding vanish; boxes crossing the edge are clipped.
        assert_eq!(fit.to_normalized(0.0, 0.0, 64.0, 10.0), None);
        let [_, y, _, h] = fit.to_normalized(0.0, 0.0, 64.0, 32.0).unwrap();
        assert_eq!(y, 0.0);
        assert!((h - 0.5).abs() < 1e-5);
    }

    fn det(class: usize, score: f32, x1: f32) -> Detection {
        Detection {
            class,
            score,
            x1,
            y1: 0.0,
            x2: x1 + 10.0,
            y2: 10.0,
        }
    }

    #[test]
    fn nms_drops_overlapping_boxes_of_the_same_class_only() {
        let kept = nms(
            vec![
                det(0, 0.5, 1.0),
                det(0, 0.9, 0.0),
                det(1, 0.8, 0.0),
                det(0, 0.7, 50.0),
            ],
            0.45,
            10,
        );
        let scores: Vec<f32> = kept.iter().map(|d| d.score).collect();
        assert_eq!(scores, [0.9, 0.8, 0.7]);
        assert_eq!(
            nms(vec![det(0, 0.9, 0.0), det(0, 0.8, 50.0)], 0.45, 1).len(),
            1
        );
    }

    #[test]
    fn non_cpu_devices_warn_and_fall_back() {
        assert_eq!(device_warning(None), None);
        assert_eq!(device_warning(Some("CPU")), None);
        assert!(device_warning(Some("cuda")).unwrap().contains("CPU"));
    }
}

#[cfg(all(test, feature = "testing"))]
mod model_tests {
    use super::*;
    use image::{Rgb, RgbImage};

    #[test]
    fn loads_models_reads_metadata_and_runs_inference() {
        let proto = testing::constant_model(
            Some(32),
            &[1, 6, 2],
            (0..12).map(|v| v as f32).collect(),
            &[("names", "{0: 'cat', 1: 'dog'}")],
        );
        let dir = std::env::temp_dir().join(format!("anytopdf-onnx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fixture.onnx");
        std::fs::write(&path, testing::encode(&proto)).unwrap();
        let model = Model::load(&path, 640).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(model.input_size(), (32, 32));
        assert_eq!(model.metadata("names"), Some("{0: 'cat', 1: 'dog'}"));
        let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(10, 20, Rgb([9, 9, 9])));
        let (input, _) = letterbox(&image, 32, 32);
        let outputs = model.run(input).unwrap();
        assert_eq!(outputs[0].shape(), &[1, 6, 2]);
        assert_eq!(outputs[0][[0, 5, 1]], 11.0);
    }

    #[test]
    fn dynamic_inputs_use_the_fallback_size() {
        let proto = testing::constant_model(None, &[1, 5, 1], vec![0.0; 5], &[]);
        let model = Model::from_proto(&proto, 64).unwrap();
        assert_eq!(model.input_size(), (64, 64));
    }
}
