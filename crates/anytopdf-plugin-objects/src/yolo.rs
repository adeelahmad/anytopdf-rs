//! Decoding YOLO detection heads and resolving their class labels.
//!
//! Supported output layouts (batch 1):
//! - YOLOv8 / YOLO11: `[1, 4 + classes, boxes]` or its transpose, each box
//!   `cx, cy, w, h` followed by one score per class.
//! - YOLOv5: `[1, boxes, 5 + classes]` or its transpose, with an objectness
//!   score after the box that multiplies every class score.

use anyhow::{Result, bail};
use anytopdf_onnx::{Detection, ndarray::ArrayD};
use regex::Regex;

pub const COCO: [&str; 80] = [
    "person",
    "bicycle",
    "car",
    "motorcycle",
    "airplane",
    "bus",
    "train",
    "truck",
    "boat",
    "traffic light",
    "fire hydrant",
    "stop sign",
    "parking meter",
    "bench",
    "bird",
    "cat",
    "dog",
    "horse",
    "sheep",
    "cow",
    "elephant",
    "bear",
    "zebra",
    "giraffe",
    "backpack",
    "umbrella",
    "handbag",
    "tie",
    "suitcase",
    "frisbee",
    "skis",
    "snowboard",
    "sports ball",
    "kite",
    "baseball bat",
    "baseball glove",
    "skateboard",
    "surfboard",
    "tennis racket",
    "bottle",
    "wine glass",
    "cup",
    "fork",
    "knife",
    "spoon",
    "bowl",
    "banana",
    "apple",
    "sandwich",
    "orange",
    "broccoli",
    "carrot",
    "hot dog",
    "pizza",
    "donut",
    "cake",
    "chair",
    "couch",
    "potted plant",
    "bed",
    "dining table",
    "toilet",
    "tv",
    "laptop",
    "mouse",
    "remote",
    "keyboard",
    "cell phone",
    "microwave",
    "oven",
    "toaster",
    "sink",
    "refrigerator",
    "book",
    "clock",
    "vase",
    "scissors",
    "teddy bear",
    "hair drier",
    "toothbrush",
];

/// Parses a labels file: one class name per line, in class-index order.
/// Trailing blank lines are ignored.
pub fn parse_labels_file(text: &str) -> Vec<String> {
    let mut labels: Vec<String> = text.lines().map(|l| l.trim().to_string()).collect();
    while labels.last().is_some_and(String::is_empty) {
        labels.pop();
    }
    labels
}

/// Parses the `names` metadata Ultralytics writes into ONNX exports, a
/// Python dict literal such as `{0: 'person', 1: 'bicycle'}`.
pub fn parse_ultralytics_names(text: &str) -> Option<Vec<String>> {
    let entry = Regex::new(r#"(\d+)\s*:\s*(?:'((?:[^'\\]|\\.)*)'|"((?:[^"\\]|\\.)*)")"#).ok()?;
    let mut pairs: Vec<(usize, String)> = entry
        .captures_iter(text)
        .filter_map(|c| {
            let index = c[1].parse().ok()?;
            let name = c.get(2).or_else(|| c.get(3))?.as_str().replace("\\'", "'");
            Some((index, name))
        })
        .collect();
    if pairs.is_empty() {
        return None;
    }
    pairs.sort_by_key(|(i, _)| *i);
    let len = pairs.last()?.0 + 1;
    let mut names: Vec<String> = (0..len).map(|i| format!("class-{i}")).collect();
    for (i, name) in pairs {
        names[i] = name;
    }
    Some(names)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Head {
    /// Box then class scores.
    Anchorless,
    /// Box, objectness, then class scores.
    Objectness,
}

/// The decoded layout of one output tensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    head: Head,
    classes: usize,
    boxes: usize,
    /// Features run along axis 1 (`[1, features, boxes]`).
    features_first: bool,
}

fn layout(shape: &[usize], labels: usize) -> Result<Layout> {
    let (a, b) = match shape {
        [1, a, b] | [a, b] => (*a, *b),
        _ => bail!("unsupported detection output shape {shape:?}; expected [1, features, boxes]"),
    };
    let pick = |features: usize, boxes: usize, features_first: bool| {
        let (head, classes) = if labels > 0 && features == labels + 4 {
            (Head::Anchorless, labels)
        } else if labels > 0 && features == labels + 5 {
            (Head::Objectness, labels)
        } else {
            return None;
        };
        Some(Layout {
            head,
            classes,
            boxes,
            features_first,
        })
    };
    // Prefer the orientation whose feature count matches the labels; else
    // assume the shorter axis holds the features, as in every YOLO export.
    if let Some(l) = pick(a, b, true).or_else(|| pick(b, a, false)) {
        return Ok(l);
    }
    let (features, boxes, features_first) = if a <= b { (a, b, true) } else { (b, a, false) };
    if features <= 4 {
        bail!("detection output shape {shape:?} has no class scores");
    }
    Ok(Layout {
        head: Head::Anchorless,
        classes: features - 4,
        boxes,
        features_first,
    })
}

/// Decodes candidate boxes scoring at least `threshold` from a YOLO output.
/// Returns the candidates and the number of classes the head scores.
pub fn decode(
    output: &ArrayD<f32>,
    labels: usize,
    threshold: f32,
    allowed: &dyn Fn(usize) -> bool,
) -> Result<(Vec<Detection>, usize)> {
    let shape = output.shape().to_vec();
    let layout = layout(&shape, labels)?;
    let flat = output
        .as_slice()
        .map(<[f32]>::to_vec)
        .unwrap_or_else(|| output.iter().copied().collect());
    let features = layout.classes
        + match layout.head {
            Head::Anchorless => 4,
            Head::Objectness => 5,
        };
    let at = |feature: usize, item: usize| {
        if layout.features_first {
            flat[feature * layout.boxes + item]
        } else {
            flat[item * features + feature]
        }
    };
    let first_class = features - layout.classes;
    let mut detections = Vec::new();
    for item in 0..layout.boxes {
        let objectness = match layout.head {
            Head::Anchorless => 1.0,
            Head::Objectness => at(4, item),
        };
        let mut best: Option<(usize, f32)> = None;
        for class in 0..layout.classes {
            if !allowed(class) {
                continue;
            }
            let score = at(first_class + class, item) * objectness;
            if score.is_finite() && best.is_none_or(|(_, s)| score > s) {
                best = Some((class, score));
            }
        }
        let Some((class, score)) = best else { continue };
        if score < threshold {
            continue;
        }
        let (cx, cy, w, h) = (at(0, item), at(1, item), at(2, item), at(3, item));
        if ![cx, cy, w, h].iter().all(|v| v.is_finite()) || w <= 0.0 || h <= 0.0 {
            continue;
        }
        detections.push(Detection {
            class,
            score: score.min(1.0),
            x1: cx - w / 2.0,
            y1: cy - h / 2.0,
            x2: cx + w / 2.0,
            y2: cy + h / 2.0,
        });
    }
    Ok((detections, layout.classes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anytopdf_onnx::ndarray::{Array, IxDyn};

    fn tensor(shape: &[usize], values: Vec<f32>) -> ArrayD<f32> {
        Array::from_shape_vec(IxDyn(shape), values).unwrap()
    }

    #[test]
    fn decodes_yolov8_heads_with_features_first() {
        // Two boxes, two classes: [1, 6, 2].
        let output = tensor(
            &[1, 6, 2],
            vec![
                10.0, 30.0, // cx
                10.0, 30.0, // cy
                4.0, 8.0, // w
                4.0, 8.0, // h
                0.9, 0.1, // class 0
                0.2, 0.6, // class 1
            ],
        );
        let (dets, classes) = decode(&output, 2, 0.5, &|_| true).unwrap();
        assert_eq!(classes, 2);
        assert_eq!(dets.len(), 2);
        assert_eq!((dets[0].class, dets[0].score), (0, 0.9));
        assert_eq!(
            (dets[0].x1, dets[0].y1, dets[0].x2, dets[0].y2),
            (8.0, 8.0, 12.0, 12.0)
        );
        assert_eq!((dets[1].class, dets[1].score), (1, 0.6));
    }

    #[test]
    fn decodes_transposed_and_objectness_heads() {
        // YOLOv5: [1, boxes, 5 + classes] with objectness.
        let output = tensor(
            &[1, 2, 7],
            vec![
                5.0, 5.0, 2.0, 2.0, 0.5, 0.9, 0.1, //
                5.0, 5.0, 2.0, 2.0, 0.1, 0.9, 0.1,
            ],
        );
        let (dets, _) = decode(&output, 2, 0.3, &|_| true).unwrap();
        assert_eq!(dets.len(), 1);
        assert!((dets[0].score - 0.45).abs() < 1e-6);
    }

    #[test]
    fn class_allow_list_and_threshold_filter_candidates() {
        let output = tensor(&[1, 6, 1], vec![10.0, 10.0, 4.0, 4.0, 0.9, 0.4]);
        let (dets, _) = decode(&output, 2, 0.3, &|class| class == 1).unwrap();
        assert_eq!((dets[0].class, dets[0].score), (1, 0.4));
        let (dets, _) = decode(&output, 2, 0.5, &|class| class == 1).unwrap();
        assert!(dets.is_empty());
    }

    #[test]
    fn unknown_class_counts_fall_back_to_the_short_axis() {
        let output = tensor(&[1, 7, 10], vec![0.0; 70]);
        let (_, classes) = decode(&output, 80, 0.5, &|_| true).unwrap();
        assert_eq!(classes, 3);
        assert!(decode(&tensor(&[1, 2, 2, 2], vec![0.0; 8]), 80, 0.5, &|_| true).is_err());
    }

    #[test]
    fn reads_ultralytics_names_and_label_files() {
        let names = parse_ultralytics_names("{0: 'person', 2: \"traffic light\", 1: 'it\\'s'}");
        assert_eq!(names.unwrap(), ["person", "it's", "traffic light"]);
        assert_eq!(parse_ultralytics_names("nothing"), None);
        assert_eq!(parse_labels_file("cat\n dog \n\n"), ["cat", "dog"]);
    }
}
