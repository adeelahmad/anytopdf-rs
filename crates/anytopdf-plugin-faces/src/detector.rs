//! YuNet face detection: model loading, input sizing and output decoding.

use anyhow::{Context, Result, ensure};
use anytopdf_onnx::{Channels, Detection, Letterbox, OnnxModel, letterbox, nms};
use image::RgbImage;
use std::path::Path;

/// YuNet 2026may from the OpenCV model zoo (MIT); see `models/README.md`.
pub const EMBEDDED_MODEL: &[u8] = include_bytes!("../models/face_detection_yunet_2026may.onnx");
pub const EMBEDDED_MODEL_NAME: &str = "yunet-2026may";

const STRIDES: [usize; 3] = [8, 16, 32];
const NMS_IOU: f32 = 0.3;

/// A detected face in source-image pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub score: f32,
    /// Right eye, left eye, nose tip, right and left mouth corner, in that
    /// order. "Right" is the subject's right, which appears on the image's left.
    pub landmarks: [(f32, f32); 5],
}

pub struct Detector {
    model_bytes: Option<Vec<u8>>,
    model_path: Option<std::path::PathBuf>,
    pub score_threshold: f32,
    pub min_size: f32,
    pub input_size: u32,
}

impl Detector {
    pub fn embedded() -> Self {
        Self {
            model_bytes: Some(EMBEDDED_MODEL.to_vec()),
            model_path: None,
            score_threshold: 0.8,
            min_size: 20.0,
            input_size: 640,
        }
    }

    pub fn from_path(path: &Path) -> Self {
        Self {
            model_bytes: None,
            model_path: Some(path.to_path_buf()),
            ..Self::embedded()
        }
    }

    fn load(&self, input: [usize; 4]) -> Result<OnnxModel> {
        match (&self.model_bytes, &self.model_path) {
            (Some(bytes), _) => OnnxModel::from_bytes(bytes, input),
            (None, Some(path)) => OnnxModel::from_path(path, input),
            (None, None) => unreachable!("detector has a model"),
        }
    }

    /// Detects faces, largest-score first, in source-image pixel coordinates.
    pub fn detect(&self, image: &RgbImage) -> Result<Vec<Face>> {
        let (width, height) = input_dimensions(image.width(), image.height(), self.input_size);
        let boxed = letterbox(image, width, height, Channels::Bgr);
        let model = self.load([1, 3, height as usize, width as usize])?;
        let outputs = model.run(boxed.tensor.clone())?;
        let data: Vec<&[f32]> = outputs.iter().map(|o| o.data.as_slice()).collect();
        let detections = decode(&data, width as usize, height as usize, self.score_threshold)?;
        Ok(nms(detections, NMS_IOU)
            .into_iter()
            .map(|d| to_source(&boxed, &d))
            .filter(|f| f.width.min(f.height) >= self.min_size)
            .collect())
    }
}

/// The detector input for an image: never upscaled, the longest side at most
/// `limit`, both sides rounded up to a multiple of the largest stride.
pub fn input_dimensions(width: u32, height: u32, limit: u32) -> (u32, u32) {
    let longest = width.max(height).max(1) as f32;
    let scale = (limit.max(32) as f32 / longest).min(1.0);
    let round = |side: u32| {
        let scaled = (side as f32 * scale).round().max(1.0) as u32;
        scaled.div_ceil(32) * 32
    };
    (round(width), round(height))
}

/// Decodes YuNet's twelve outputs (`cls`, `obj`, `bbox`, `kps` for strides
/// 8, 16 and 32, in that order) into detections in model-input pixels.
pub fn decode(
    outputs: &[&[f32]],
    width: usize,
    height: usize,
    score_threshold: f32,
) -> Result<Vec<Detection>> {
    ensure!(
        outputs.len() == 12,
        "YuNet model must have 12 outputs, found {}",
        outputs.len()
    );
    let mut detections = Vec::new();
    for (level, stride) in STRIDES.into_iter().enumerate() {
        let cols = width / stride;
        let rows = height / stride;
        let cells = cols * rows;
        let (cls, obj, bbox, kps) = (
            outputs[level],
            outputs[3 + level],
            outputs[6 + level],
            outputs[9 + level],
        );
        ensure!(
            cls.len() == cells && obj.len() == cells && bbox.len() == cells * 4,
            "YuNet stride {stride} output has an unexpected size"
        );
        ensure!(
            kps.len() == cells * 10,
            "YuNet stride {stride} landmarks have an unexpected size"
        );
        let s = stride as f32;
        for index in 0..cells {
            let score = (cls[index].clamp(0.0, 1.0) * obj[index].clamp(0.0, 1.0)).sqrt();
            if score.is_nan() || score < score_threshold {
                continue;
            }
            let col = (index % cols) as f32;
            let row = (index / cols) as f32;
            let b = &bbox[index * 4..index * 4 + 4];
            let center_x = (col + b[0]) * s;
            let center_y = (row + b[1]) * s;
            let box_width = b[2].exp() * s;
            let box_height = b[3].exp() * s;
            let k = &kps[index * 10..index * 10 + 10];
            detections.push(Detection {
                x: center_x - box_width / 2.0,
                y: center_y - box_height / 2.0,
                width: box_width,
                height: box_height,
                score,
                class: 0,
                keypoints: (0..5)
                    .map(|n| ((k[2 * n] + col) * s, (k[2 * n + 1] + row) * s))
                    .collect(),
            });
        }
    }
    Ok(detections)
}

fn to_source(boxed: &Letterbox, detection: &Detection) -> Face {
    let scale = boxed.scale;
    let mut landmarks = [(0.0, 0.0); 5];
    for (slot, (x, y)) in landmarks.iter_mut().zip(&detection.keypoints) {
        *slot = (x / scale, y / scale);
    }
    Face {
        x: detection.x / scale,
        y: detection.y / scale,
        width: detection.width / scale,
        height: detection.height / scale,
        score: detection.score,
        landmarks,
    }
}

/// Loads any image the `image` crate decodes as RGB.
pub fn load_rgb(path: &Path) -> Result<RgbImage> {
    Ok(image::open(path)
        .with_context(|| format!("decode {}", path.display()))?
        .to_rgb8())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_dimensions_cap_the_longest_side_and_round_to_strides() {
        assert_eq!(input_dimensions(1920, 1080, 640), (640, 384));
        assert_eq!(input_dimensions(100, 50, 640), (128, 64));
        assert_eq!(input_dimensions(0, 0, 640), (32, 32));
    }

    #[test]
    fn decode_places_one_confident_cell_with_its_landmarks() {
        // A 64x32 input: stride 8 has 8x4 cells, 16 has 4x2, 32 has 2x1.
        let (w, h) = (64, 32);
        let cells = [32, 8, 2];
        let mut data: Vec<Vec<f32>> = Vec::new();
        for per in [1, 1, 4, 10] {
            for c in cells {
                data.push(vec![0.0; c * per]);
            }
        }
        // Stride 16, cell (row 1, col 2) = index 6: score sqrt(0.9 * 1.0).
        data[1][6] = 0.9;
        data[4][6] = 1.0;
        data[7][6 * 4..6 * 4 + 4].copy_from_slice(&[0.5, 0.5, 0.0, 0.0]);
        data[10][60..70].copy_from_slice(&[0.0, 0.0, 1.0, 0.0, 0.5, 0.5, 0.0, 1.0, 1.0, 1.0]);
        let refs: Vec<&[f32]> = data.iter().map(Vec::as_slice).collect();
        let found = decode(&refs, w, h, 0.5).unwrap();
        assert_eq!(found.len(), 1);
        let d = &found[0];
        assert!((d.score - 0.9f32.sqrt()).abs() < 1e-6);
        // Center (2.5, 1.5) * 16 = (40, 24); size exp(0) * 16 = 16.
        assert_eq!((d.x, d.y, d.width, d.height), (32.0, 16.0, 16.0, 16.0));
        assert_eq!(d.keypoints[0], (32.0, 16.0));
        assert_eq!(d.keypoints[2], (40.0, 24.0));
        assert!(decode(&refs, w, h, 0.99).unwrap().is_empty());
    }

    #[test]
    fn decode_rejects_a_model_with_the_wrong_outputs() {
        assert!(decode(&[&[0.0]], 32, 32, 0.5).is_err());
    }

    #[test]
    fn blank_image_has_no_faces() {
        let image = RgbImage::from_pixel(96, 64, image::Rgb([128, 128, 128]));
        assert!(Detector::embedded().detect(&image).unwrap().is_empty());
    }
}
