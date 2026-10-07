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

/// The detection head a model's output was recognised as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Head {
    /// YOLOv8 / YOLO11: box then class scores.
    Anchorless,
    /// YOLOv5: box, objectness, then class scores.
    Objectness,
    /// YOLOX: box, objectness, then class scores, one row per grid cell of
    /// strides 8, 16 and 32. Boxes may still be grid offsets and log sizes.
    Yolox,
}

impl Head {
    /// YOLOX expects raw 0..=255 pixels; Ultralytics models expect 0..1.
    pub fn pixel_scale(self) -> f32 {
        match self {
            Head::Yolox => 255.0,
            Head::Anchorless | Head::Objectness => 1.0,
        }
    }
}

/// YOLOX output strides, finest first, in the order its rows are laid out.
const YOLOX_STRIDES: [u32; 3] = [8, 16, 32];

/// Rows a YOLOX head produces for a `width` x `height` input.
fn yolox_rows(width: u32, height: u32) -> usize {
    YOLOX_STRIDES
        .iter()
        .map(|s| (width / s) as usize * (height / s) as usize)
        .sum()
}

/// The decoded layout of one output tensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub head: Head,
    pub classes: usize,
    boxes: usize,
    /// Features run along axis 1 (`[1, features, boxes]`).
    features_first: bool,
    /// Model input width and height, for YOLOX grid decoding.
    input: (u32, u32),
}

/// Recognises a detection output of `shape` for a model with `labels` class
/// names and a `width` x `height` input. Anything that is not a YOLOv8/11,
/// YOLOv5 or YOLOX head for exactly that many classes is an error, because
/// guessing turns an unknown model into one that silently finds nothing.
pub fn layout(shape: &[usize], labels: usize, input: (u32, u32)) -> Result<Layout> {
    let (a, b) = match shape {
        [1, a, b] | [a, b] => (*a, *b),
        _ => bail!(
            "unsupported detection output shape {shape:?}; expected a YOLOv8/YOLO11, \
             YOLOv5 or YOLOX head of shape [1, features, boxes] or [1, boxes, features]"
        ),
    };
    let pick = |features: usize, boxes: usize, features_first: bool| {
        let head = if labels > 0 && features == labels + 4 {
            Head::Anchorless
        } else if labels > 0 && features == labels + 5 {
            if boxes == yolox_rows(input.0, input.1) {
                Head::Yolox
            } else {
                Head::Objectness
            }
        } else {
            return None;
        };
        Some(Layout {
            head,
            classes: labels,
            boxes,
            features_first,
            input,
        })
    };
    // Prefer the orientation whose feature count matches the labels.
    if let Some(l) = pick(b, a, false).or_else(|| pick(a, b, true)) {
        return Ok(l);
    }
    let features = a.min(b);
    bail!(
        "detection output shape {shape:?} does not match the {labels} class labels: \
         expected {} features per box (YOLOv8/YOLO11) or {} (YOLOv5/YOLOX), found {features}; \
         set ANYTOPDF_OBJECTS_LABELS to the model's class names, one per line",
        labels + 4,
        labels + 5
    )
}

/// Whether a YOLOX output still holds grid offsets and log sizes (exported
/// without `decode_in_inference`). Decoded centres span the input in pixels;
/// raw ones are offsets of about one cell.
fn yolox_is_raw(at: &dyn Fn(usize, usize) -> f32, boxes: usize) -> bool {
    (0..boxes).all(|item| {
        let (x, y) = (at(0, item), at(1, item));
        !(x.is_finite() && y.is_finite()) || (x.abs() < 4.0 && y.abs() < 4.0)
    })
}

/// Decodes candidate boxes scoring at least `threshold` from a YOLO output.
pub fn decode(
    output: &ArrayD<f32>,
    layout: &Layout,
    threshold: f32,
    allowed: &dyn Fn(usize) -> bool,
) -> Result<Vec<Detection>> {
    let shape = output.shape().to_vec();
    let actual = match shape.as_slice() {
        [1, a, b] | [a, b] => (*a, *b),
        _ => (0, 0),
    };
    let features = layout.classes
        + match layout.head {
            Head::Anchorless => 4,
            Head::Objectness | Head::Yolox => 5,
        };
    let expected = if layout.features_first {
        (features, layout.boxes)
    } else {
        (layout.boxes, features)
    };
    if actual != expected {
        bail!("model output shape {shape:?} changed from the one it was loaded with");
    }
    let flat = output
        .as_slice()
        .map(<[f32]>::to_vec)
        .unwrap_or_else(|| output.iter().copied().collect());
    let at = |feature: usize, item: usize| {
        if layout.features_first {
            flat[feature * layout.boxes + item]
        } else {
            flat[item * features + feature]
        }
    };
    // Each YOLOX row's grid cell and stride, when boxes still need decoding.
    let grid: Vec<(f32, f32, f32)> = if layout.head == Head::Yolox
        && yolox_is_raw(&at, layout.boxes)
    {
        let (width, height) = layout.input;
        YOLOX_STRIDES
            .iter()
            .flat_map(|&s| {
                let (columns, rows) = (width / s, height / s);
                (0..rows)
                    .flat_map(move |y| (0..columns).map(move |x| (x as f32, y as f32, s as f32)))
            })
            .collect()
    } else {
        Vec::new()
    };
    let first_class = features - layout.classes;
    let mut detections = Vec::new();
    for item in 0..layout.boxes {
        let objectness = match layout.head {
            Head::Anchorless => 1.0,
            Head::Objectness | Head::Yolox => at(4, item),
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
        let (mut cx, mut cy, mut w, mut h) = (at(0, item), at(1, item), at(2, item), at(3, item));
        if let Some(&(gx, gy, stride)) = grid.get(item) {
            cx = (cx + gx) * stride;
            cy = (cy + gy) * stride;
            w = w.exp() * stride;
            h = h.exp() * stride;
        }
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
    Ok(detections)
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
        let layout = layout(output.shape(), 2, (640, 640)).unwrap();
        assert_eq!((layout.head, layout.classes), (Head::Anchorless, 2));
        let dets = decode(&output, &layout, 0.5, &|_| true).unwrap();
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
        let layout = layout(output.shape(), 2, (640, 640)).unwrap();
        assert_eq!(layout.head, Head::Objectness);
        let dets = decode(&output, &layout, 0.3, &|_| true).unwrap();
        assert_eq!(dets.len(), 1);
        assert!((dets[0].score - 0.45).abs() < 1e-6);
    }

    #[test]
    fn class_allow_list_and_threshold_filter_candidates() {
        let output = tensor(&[1, 6, 1], vec![10.0, 10.0, 4.0, 4.0, 0.9, 0.4]);
        let layout = layout(output.shape(), 2, (640, 640)).unwrap();
        let dets = decode(&output, &layout, 0.3, &|class| class == 1).unwrap();
        assert_eq!((dets[0].class, dets[0].score), (1, 0.4));
        let dets = decode(&output, &layout, 0.5, &|class| class == 1).unwrap();
        assert!(dets.is_empty());
    }

    #[test]
    fn heads_that_do_not_match_the_labels_are_rejected_not_guessed() {
        let error = layout(&[1, 7, 10], 80, (640, 640)).unwrap_err().to_string();
        assert!(error.contains("expected 84 features"), "{error}");
        assert!(error.contains("ANYTOPDF_OBJECTS_LABELS"), "{error}");
        assert!(layout(&[1, 2, 2, 2], 80, (640, 640)).is_err());
        assert!(layout(&[1, 84, 8400], 0, (640, 640)).is_err());
        let wrong = layout(&[1, 6, 1], 2, (640, 640)).unwrap();
        assert!(decode(&tensor(&[1, 7, 1], vec![0.0; 7]), &wrong, 0.5, &|_| true).is_err());
    }

    /// A 32x32 YOLOX head: 4x4 + 2x2 + 1x1 grid rows of 2 classes.
    fn yolox(rows: &[(usize, [f32; 7])]) -> ArrayD<f32> {
        let mut values = vec![0.0; 21 * 7];
        for (row, features) in rows {
            values[row * 7..row * 7 + 7].copy_from_slice(features);
        }
        tensor(&[1, 21, 7], values)
    }

    #[test]
    fn yolox_heads_are_recognised_by_their_grid_rows() {
        let output = yolox(&[]);
        let layout = layout(output.shape(), 2, (32, 32)).unwrap();
        assert_eq!(layout.head, Head::Yolox);
        assert_eq!(layout.head.pixel_scale(), 255.0);
        // The same shape at another input size is YOLOv5-style.
        assert_eq!(
            super::layout(output.shape(), 2, (64, 64)).unwrap().head,
            Head::Objectness
        );
    }

    #[test]
    fn raw_yolox_offsets_are_decoded_on_their_grid() {
        // Row 5 is stride 8, cell (1, 1); row 20 is the stride-32 cell.
        let output = yolox(&[
            (5, [0.5, 0.5, 0.0, 1f32.ln(), 0.9, 0.1, 0.8]),
            (20, [0.5, 0.5, 2f32.ln(), 0.0, 0.5, 0.9, 0.2]),
        ]);
        let layout = layout(output.shape(), 2, (32, 32)).unwrap();
        let dets = decode(&output, &layout, 0.3, &|_| true).unwrap();
        assert_eq!(dets.len(), 2);
        assert_eq!(dets[0].class, 1);
        assert!((dets[0].score - 0.72).abs() < 1e-6);
        let corners = |d: &Detection| [d.x1, d.y1, d.x2, d.y2].map(|v| (v * 100.0).round() / 100.0);
        assert_eq!(corners(&dets[0]), [8.0, 8.0, 16.0, 16.0]);
        assert_eq!(dets[1].class, 0);
        assert_eq!(corners(&dets[1]), [-16.0, 0.0, 48.0, 32.0]);
    }

    #[test]
    fn decoded_yolox_boxes_are_used_as_they_are() {
        let output = yolox(&[(0, [20.0, 12.0, 8.0, 4.0, 1.0, 0.9, 0.0])]);
        let layout = layout(output.shape(), 2, (32, 32)).unwrap();
        let dets = decode(&output, &layout, 0.5, &|_| true).unwrap();
        assert_eq!(
            (dets[0].x1, dets[0].y1, dets[0].x2, dets[0].y2),
            (16.0, 10.0, 24.0, 14.0)
        );
    }

    #[test]
    fn reads_ultralytics_names_and_label_files() {
        let names = parse_ultralytics_names("{0: 'person', 2: \"traffic light\", 1: 'it\\'s'}");
        assert_eq!(names.unwrap(), ["person", "it's", "traffic light"]);
        assert_eq!(parse_ultralytics_names("nothing"), None);
        assert_eq!(parse_labels_file("cat\n dog \n\n"), ["cat", "dog"]);
    }
}
