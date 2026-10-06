//! Fixed-input detectors with named outputs (YuNet faces): a model whose
//! first input is pinned to one `[1, channels, height, width]` shape, an RGB or
//! BGR letterbox of raw 0..=255 pixels padded at the right and bottom, and
//! keypoint-carrying detections with per-class non-maximum suppression.

use anyhow::{Context, Result, bail, ensure};
use image::RgbImage;
use std::path::Path;
use tract_onnx::prelude::*;

/// An ONNX model optimized for one fixed `[1, channels, height, width]` input.
pub struct OnnxModel {
    plan: Arc<TypedRunnableModel>,
    input: [usize; 4],
    outputs: Vec<String>,
}

/// One named output tensor, flattened in row-major order.
#[derive(Debug, Clone)]
pub struct Output {
    pub name: String,
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

impl OnnxModel {
    /// Loads a model from memory and fixes its first input to `input`.
    pub fn from_bytes(bytes: &[u8], input: [usize; 4]) -> Result<Self> {
        let model = tract_onnx::onnx()
            .model_for_read(&mut std::io::Cursor::new(bytes))
            .context("parse ONNX model")?;
        Self::prepare(model, input)
    }

    /// Loads a model file and fixes its first input to `input`.
    pub fn from_path(path: &Path, input: [usize; 4]) -> Result<Self> {
        let model = tract_onnx::onnx()
            .model_for_path(path)
            .with_context(|| format!("load ONNX model {}", path.display()))?;
        Self::prepare(model, input)
    }

    fn prepare(model: InferenceModel, input: [usize; 4]) -> Result<Self> {
        ensure!(
            input.iter().all(|&d| d > 0),
            "model input dimensions must be positive"
        );
        let outputs = model
            .outputs
            .iter()
            .map(|outlet| {
                model
                    .outlet_label(*outlet)
                    .map(str::to_owned)
                    .unwrap_or_else(|| model.node(outlet.node).name.clone())
            })
            .collect();
        let plan = model
            .with_input_fact(0, f32::fact(input).into())
            .context("set model input shape")?
            .into_optimized()
            .context("optimize ONNX model")?
            .into_runnable()
            .context("prepare ONNX model")?;
        Ok(Self {
            plan,
            input,
            outputs,
        })
    }

    /// The `[batch, channels, height, width]` input this model expects.
    pub fn input_shape(&self) -> [usize; 4] {
        self.input
    }

    /// Runs the model on a flattened NCHW tensor of `input_shape()`.
    pub fn run(&self, input: Vec<f32>) -> Result<Vec<Output>> {
        let tensor = Tensor::from_shape(&self.input, &input).context("build input tensor")?;
        let results = self
            .plan
            .run(tvec!(tensor.into()))
            .context("run ONNX model")?;
        results
            .iter()
            .zip(&self.outputs)
            .map(|(value, name)| {
                let view = value
                    .to_plain_array_view::<f32>()
                    .with_context(|| format!("output {name} is not f32"))?;
                Ok(Output {
                    name: name.clone(),
                    shape: view.shape().to_vec(),
                    data: view.iter().copied().collect(),
                })
            })
            .collect()
    }
}

/// Finds an output by name.
pub fn output<'a>(outputs: &'a [Output], name: &str) -> Result<&'a Output> {
    match outputs.iter().find(|o| o.name == name) {
        Some(found) => Ok(found),
        None => bail!("model has no output named {name}"),
    }
}

/// Channel order of the tensor `letterbox` builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channels {
    Rgb,
    Bgr,
}

/// An image scaled to fit a model input, keeping its aspect ratio, and padded
/// at the right and bottom with zeros.
#[derive(Debug, Clone)]
pub struct Letterbox {
    /// Flattened `[1, 3, height, width]` tensor of raw 0..=255 pixel values.
    pub tensor: Vec<f32>,
    /// Model pixels per source pixel.
    pub scale: f32,
    pub source_width: u32,
    pub source_height: u32,
}

impl Letterbox {
    /// Maps a point in model-input pixels back to normalized source coordinates.
    pub fn normalize(&self, x: f32, y: f32) -> (f32, f32) {
        (
            x / self.scale / self.source_width as f32,
            y / self.scale / self.source_height as f32,
        )
    }
}

/// Scales `image` to fit inside `width` x `height` and builds an NCHW tensor.
pub fn letterbox(image: &RgbImage, width: u32, height: u32, channels: Channels) -> Letterbox {
    let (source_width, source_height) = image.dimensions();
    let scale = (width as f32 / source_width.max(1) as f32)
        .min(height as f32 / source_height.max(1) as f32);
    let fitted_width = ((source_width as f32 * scale).round() as u32).clamp(1, width);
    let fitted_height = ((source_height as f32 * scale).round() as u32).clamp(1, height);
    let resized = image::imageops::resize(
        image,
        fitted_width,
        fitted_height,
        image::imageops::FilterType::Triangle,
    );
    let plane = (width * height) as usize;
    let mut tensor = vec![0.0f32; 3 * plane];
    for (x, y, pixel) in resized.enumerate_pixels() {
        let offset = (y * width + x) as usize;
        for c in 0..3 {
            let source = match channels {
                Channels::Rgb => c,
                Channels::Bgr => 2 - c,
            };
            tensor[c * plane + offset] = f32::from(pixel[source]);
        }
    }
    Letterbox {
        tensor,
        scale,
        source_width,
        source_height,
    }
}

/// An axis-aligned detection in any consistent coordinate space.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub score: f32,
    pub class: usize,
    /// Optional keypoints (for example face landmarks) in the same space.
    pub keypoints: Vec<(f32, f32)>,
}

impl Detection {
    pub fn area(&self) -> f32 {
        self.width.max(0.0) * self.height.max(0.0)
    }

    /// Intersection over union with another detection.
    pub fn iou(&self, other: &Detection) -> f32 {
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        let intersection = (right - left).max(0.0) * (bottom - top).max(0.0);
        let union = self.area() + other.area() - intersection;
        if union <= 0.0 {
            0.0
        } else {
            intersection / union
        }
    }
}

/// Greedy per-class non-maximum suppression, highest score first.
pub fn nms(mut detections: Vec<Detection>, iou_threshold: f32) -> Vec<Detection> {
    detections.retain(|d| d.score.is_finite());
    detections.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Detection> = Vec::new();
    for candidate in detections {
        if kept
            .iter()
            .all(|k| k.class != candidate.class || k.iou(&candidate) <= iou_threshold)
        {
            kept.push(candidate);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(x: f32, score: f32, class: usize) -> Detection {
        Detection {
            x,
            y: 0.0,
            width: 10.0,
            height: 10.0,
            score,
            class,
            keypoints: Vec::new(),
        }
    }

    #[test]
    fn nms_keeps_the_best_of_overlapping_boxes() {
        let kept = nms(
            vec![
                detection(0.0, 0.6, 0),
                detection(1.0, 0.9, 0),
                detection(30.0, 0.5, 0),
            ],
            0.3,
        );
        let scores: Vec<f32> = kept.iter().map(|d| d.score).collect();
        assert_eq!(scores, vec![0.9, 0.5]);
    }

    #[test]
    fn nms_never_suppresses_across_classes() {
        let kept = nms(vec![detection(0.0, 0.9, 0), detection(0.0, 0.8, 1)], 0.3);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn nms_drops_non_finite_scores() {
        let kept = nms(vec![detection(0.0, f32::NAN, 0)], 0.3);
        assert!(kept.is_empty());
    }

    #[test]
    fn iou_of_disjoint_and_identical_boxes() {
        let a = detection(0.0, 1.0, 0);
        assert_eq!(a.iou(&detection(50.0, 1.0, 0)), 0.0);
        assert!((a.iou(&a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn letterbox_keeps_aspect_and_maps_back_to_normalized_coordinates() {
        let image = RgbImage::from_pixel(200, 100, image::Rgb([10, 20, 30]));
        let boxed = letterbox(&image, 64, 64, Channels::Bgr);
        assert_eq!(boxed.tensor.len(), 3 * 64 * 64);
        assert!((boxed.scale - 0.32).abs() < 1e-6);
        // BGR: blue plane first, and the padded bottom half stays zero.
        assert_eq!(boxed.tensor[0], 30.0);
        assert_eq!(boxed.tensor[2 * 64 * 64], 10.0);
        assert_eq!(boxed.tensor[63 * 64], 0.0);
        let (x, y) = boxed.normalize(64.0, 32.0);
        assert!((x - 1.0).abs() < 1e-6 && (y - 1.0).abs() < 1e-6);
    }
}
