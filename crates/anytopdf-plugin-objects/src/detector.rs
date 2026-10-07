//! Configuration and the YOLO detector built on the shared ONNX helpers.

use crate::yolo;
use anyhow::{Context, Result, bail};
use anytopdf_onnx::{Model, letterbox, nms};
use image::DynamicImage;
use std::{fs, path::PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub model: Option<PathBuf>,
    pub labels: Option<PathBuf>,
    pub confidence: f32,
    pub iou: f32,
    pub classes: Vec<String>,
    pub input_size: u32,
    pub max_detections: usize,
    pub device: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let get = |name: &str| get(name).filter(|v| !v.trim().is_empty());
        fn parse<T: std::str::FromStr>(name: &str, value: Option<String>, default: T) -> Result<T> {
            match value {
                None => Ok(default),
                Some(v) => v
                    .trim()
                    .parse()
                    .map_err(|_| anyhow::anyhow!("{name}={v:?} is not a valid number")),
            }
        }
        let confidence = parse(
            "ANYTOPDF_OBJECTS_CONFIDENCE",
            get("ANYTOPDF_OBJECTS_CONFIDENCE"),
            0.25f32,
        )?;
        let iou = parse("ANYTOPDF_OBJECTS_IOU", get("ANYTOPDF_OBJECTS_IOU"), 0.45f32)?;
        for (name, value) in [
            ("ANYTOPDF_OBJECTS_CONFIDENCE", confidence),
            ("ANYTOPDF_OBJECTS_IOU", iou),
        ] {
            if !(0.0..=1.0).contains(&value) {
                bail!("{name} must be between 0 and 1");
            }
        }
        let input_size = parse(
            "ANYTOPDF_OBJECTS_INPUT_SIZE",
            get("ANYTOPDF_OBJECTS_INPUT_SIZE"),
            640u32,
        )?;
        if !(32..=4096).contains(&input_size) {
            bail!("ANYTOPDF_OBJECTS_INPUT_SIZE must be between 32 and 4096");
        }
        let max_detections = parse(
            "ANYTOPDF_OBJECTS_MAX",
            get("ANYTOPDF_OBJECTS_MAX"),
            100usize,
        )?;
        Ok(Self {
            model: get("ANYTOPDF_OBJECTS_MODEL").map(PathBuf::from),
            labels: get("ANYTOPDF_OBJECTS_LABELS").map(PathBuf::from),
            confidence,
            iou,
            classes: get("ANYTOPDF_OBJECTS_CLASSES")
                .map(|v| {
                    v.split(',')
                        .map(|c| c.trim().to_lowercase())
                        .filter(|c| !c.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            input_size,
            max_detections,
            device: get("ANYTOPDF_OBJECTS_DEVICE"),
        })
    }
}

/// One detected object with a region normalized to the source image.
#[derive(Debug, Clone, PartialEq)]
pub struct Object {
    pub label: String,
    pub class_id: usize,
    pub confidence: f32,
    /// `[x, y, width, height]`, top-left origin, 0..1.
    pub region: [f32; 4],
}

pub struct Detector {
    model: Model,
    labels: Vec<String>,
    layout: yolo::Layout,
    config: Config,
    /// File name of the model, recorded as provenance.
    pub model_name: String,
}

impl Detector {
    pub fn load(config: Config) -> Result<Self> {
        let Some(path) = config.model.clone() else {
            bail!("set ANYTOPDF_OBJECTS_MODEL to a YOLO .onnx model");
        };
        let model = Model::load(&path, config.input_size)?;
        let model_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "model.onnx".into());
        Self::with_model(model, model_name, config)
    }

    pub fn with_model(model: Model, model_name: String, config: Config) -> Result<Self> {
        let labels = match &config.labels {
            Some(path) => yolo::parse_labels_file(
                &fs::read_to_string(path)
                    .with_context(|| format!("read labels {}", path.display()))?,
            ),
            None => model
                .metadata("names")
                .and_then(yolo::parse_ultralytics_names)
                .unwrap_or_else(|| yolo::COCO.iter().map(|s| s.to_string()).collect()),
        };
        if let Some(unknown) = config
            .classes
            .iter()
            .find(|c| !labels.iter().any(|l| l.to_lowercase() == **c))
        {
            bail!(
                "ANYTOPDF_OBJECTS_CLASSES names {unknown:?}, which the model's labels do not include"
            );
        }
        let input = model.input_size();
        let shape = match model.output_shape(0) {
            Some(shape) => shape,
            // Dynamic output shapes are only known after one inference.
            None => {
                let blank = anytopdf_onnx::ndarray::Array4::<f32>::zeros((
                    1,
                    3,
                    input.1 as usize,
                    input.0 as usize,
                ));
                let outputs = model.run(blank)?;
                outputs
                    .first()
                    .context("model produced no output")?
                    .shape()
                    .to_vec()
            }
        };
        let layout = yolo::layout(&shape, labels.len(), input)
            .with_context(|| format!("{model_name} is not a recognised YOLO detector"))?;
        Ok(Self {
            model,
            labels,
            layout,
            config,
            model_name,
        })
    }

    fn label(&self, class: usize) -> String {
        self.labels
            .get(class)
            .filter(|l| !l.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("class-{class}"))
    }

    pub fn detect(&self, image: &DynamicImage) -> Result<Vec<Object>> {
        let (width, height) = self.model.input_size();
        let (mut input, fit) = letterbox(image, width, height);
        let scale = self.layout.head.pixel_scale();
        if scale != 1.0 {
            input.mapv_inplace(|v| v * scale);
        }
        let outputs = self.model.run(input)?;
        let output = outputs.first().context("model produced no output")?;
        let allowed = |class: usize| {
            self.config.classes.is_empty()
                || self
                    .labels
                    .get(class)
                    .is_some_and(|l| self.config.classes.contains(&l.to_lowercase()))
        };
        let candidates = yolo::decode(output, &self.layout, self.config.confidence, &allowed)?;
        Ok(nms(candidates, self.config.iou, self.config.max_detections)
            .into_iter()
            .filter_map(|d| {
                Some(Object {
                    label: self.label(d.class),
                    class_id: d.class,
                    confidence: d.score,
                    region: fit.to_normalized(d.x1, d.y1, d.x2, d.y2)?,
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_reads_thresholds_and_class_allow_list() {
        let config = Config::from_lookup(|name| {
            match name {
                "ANYTOPDF_OBJECTS_MODEL" => Some("/m/yolo11n.onnx"),
                "ANYTOPDF_OBJECTS_CONFIDENCE" => Some("0.5"),
                "ANYTOPDF_OBJECTS_CLASSES" => Some(" Person, dog ,"),
                "ANYTOPDF_OBJECTS_LABELS" => Some(""),
                _ => None,
            }
            .map(String::from)
        })
        .unwrap();
        assert_eq!(config.model, Some(PathBuf::from("/m/yolo11n.onnx")));
        assert_eq!(config.labels, None);
        assert_eq!((config.confidence, config.iou), (0.5, 0.45));
        assert_eq!(config.classes, ["person", "dog"]);
        assert_eq!((config.input_size, config.max_detections), (640, 100));
    }

    #[test]
    fn config_rejects_out_of_range_values() {
        let with = |key: &'static str, value: &'static str| {
            Config::from_lookup(move |name| (name == key).then(|| value.to_string()))
        };
        assert!(with("ANYTOPDF_OBJECTS_CONFIDENCE", "1.5").is_err());
        assert!(with("ANYTOPDF_OBJECTS_IOU", "abc").is_err());
        assert!(with("ANYTOPDF_OBJECTS_INPUT_SIZE", "8").is_err());
        assert!(with("ANYTOPDF_OBJECTS_MAX", "-1").is_err());
    }

    #[test]
    fn missing_model_is_reported_by_name() {
        let error = Detector::load(Config::from_lookup(|_| None).unwrap())
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("ANYTOPDF_OBJECTS_MODEL"));
    }
}
