//! Optional sound-event model: an AudioSet tagger (YAMNet, PANNs CNN14 or any
//! export that takes a 16 kHz waveform and returns per-class scores) run
//! through ONNX Runtime, which is loaded at run time.

use anyhow::{Context, Result, bail};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

/// Scores one window of 16 kHz mono samples; returns one score per class.
pub trait Tagger {
    fn scores(&mut self, window: &[f32]) -> Result<Vec<f32>>;
}

/// AudioSet classes this plugin reports, grouped under one event label each.
/// Speech and music feed the speech/music decision instead. Classes that
/// describe how a person feels (crying, sobbing, screaming, whimpering) are
/// left out on purpose.
const EVENTS: &[(&str, &[&str])] = &[
    (
        "laughter",
        &[
            "Laughter",
            "Baby laughter",
            "Giggle",
            "Snicker",
            "Belly laugh",
            "Chuckle, chortle",
        ],
    ),
    ("applause", &["Applause", "Clapping"]),
    ("cheering", &["Cheering"]),
    ("singing", &["Singing", "Choir"]),
    ("crying-baby", &["Baby cry, infant cry"]),
    ("dog", &["Dog", "Bark", "Yip", "Bow-wow"]),
    (
        "siren",
        &[
            "Siren",
            "Police car (siren)",
            "Ambulance (siren)",
            "Fire engine, fire truck (siren)",
            "Civil defense siren",
        ],
    ),
    (
        "alarm",
        &[
            "Alarm",
            "Smoke detector, smoke alarm",
            "Fire alarm",
            "Alarm clock",
        ],
    ),
    ("gunshot", &["Gunshot, gunfire", "Machine gun", "Fusillade"]),
    ("explosion", &["Explosion"]),
    (
        "vehicle",
        &[
            "Vehicle",
            "Car",
            "Motor vehicle (road)",
            "Truck",
            "Bus",
            "Motorcycle",
            "Traffic noise, roadway noise",
        ],
    ),
    ("car-horn", &["Vehicle horn, car horn, honking"]),
    ("door", &["Door", "Slam", "Doorbell", "Sliding door"]),
    ("knock", &["Knock"]),
    (
        "keyboard-typing",
        &["Typing", "Computer keyboard", "Typewriter"],
    ),
    (
        "telephone",
        &["Telephone", "Telephone bell ringing", "Ringtone"],
    ),
    ("glass-breaking", &["Shatter"]),
];

/// Which model classes make up each reported label.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelMap {
    pub classes: usize,
    pub speech: Vec<usize>,
    pub music: Vec<usize>,
    pub events: Vec<(&'static str, Vec<usize>)>,
}

impl LabelMap {
    pub fn new(names: &[String]) -> Self {
        let find = |wanted: &[&str]| -> Vec<usize> {
            names
                .iter()
                .enumerate()
                .filter(|(_, n)| wanted.iter().any(|w| w.eq_ignore_ascii_case(n)))
                .map(|(i, _)| i)
                .collect()
        };
        Self {
            classes: names.len(),
            speech: find(&["Speech"]),
            music: find(&["Music"]),
            events: EVENTS
                .iter()
                .map(|(label, classes)| (*label, find(classes)))
                .filter(|(_, idx)| !idx.is_empty())
                .collect(),
        }
    }

    fn best(scores: &[f32], idx: &[usize]) -> f32 {
        idx.iter()
            .filter_map(|&i| scores.get(i))
            .fold(0.0, |a, &b| a.max(b))
    }

    pub fn speech_music(&self, scores: &[f32]) -> (f32, f32) {
        (
            Self::best(scores, &self.speech),
            Self::best(scores, &self.music),
        )
    }

    /// Event labels scoring at least `threshold`, with their score.
    pub fn events(&self, scores: &[f32], threshold: f32) -> Vec<(&'static str, f32)> {
        self.events
            .iter()
            .map(|(label, idx)| (*label, Self::best(scores, idx)))
            .filter(|(_, score)| *score >= threshold)
            .collect()
    }
}

/// Reads class names in model order: an AudioSet class map CSV
/// (`index,mid,display_name`, as shipped with YAMNet and PANNs) or plain
/// text with one name per line.
pub fn read_labels(path: &Path) -> Result<Vec<String>> {
    let text =
        fs::read_to_string(path).with_context(|| format!("read labels {}", path.display()))?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty()).peekable();
    let csv = lines
        .peek()
        .is_some_and(|l| l.to_ascii_lowercase().contains("display_name"));
    if csv {
        lines.next();
    }
    let names: Vec<String> = lines
        .map(|line| {
            if csv {
                let name = line.splitn(3, ',').nth(2).unwrap_or("").trim();
                name.trim_matches('"').to_string()
            } else {
                line.trim().to_string()
            }
        })
        .collect();
    if names.is_empty() {
        bail!("no class names in {}", path.display());
    }
    Ok(names)
}

/// Labels next to the model when none are named: `<stem>_class_map.csv`
/// (YAMNet), `class_labels_indices.csv` (PANNs) or `labels.csv`/`labels.txt`.
pub fn default_labels(model: &Path) -> Option<PathBuf> {
    let dir = model.parent()?;
    let stem = model.file_stem()?.to_string_lossy();
    [
        format!("{stem}_class_map.csv"),
        "class_labels_indices.csv".into(),
        "labels.csv".into(),
        "labels.txt".into(),
    ]
    .into_iter()
    .map(|name| dir.join(name))
    .find(|p| p.is_file())
}

pub struct OnnxTagger {
    session: ort::session::Session,
    input: String,
    rank: usize,
    classes: usize,
}

impl OnnxTagger {
    /// Loads ONNX Runtime (from `ORT_DYLIB_PATH`, else the platform's
    /// default library name on the loader path) and the model.
    pub fn load(model: &Path, classes: usize, device: &str) -> Result<Self> {
        let library = env::var_os("ORT_DYLIB_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(default_runtime_library()));
        let providers = match device {
            "" | "cpu" => Vec::new(),
            "cuda" => vec![ort::ep::CUDA::default().build().error_on_failure()],
            "coreml" => vec![ort::ep::CoreML::default().build().error_on_failure()],
            other => bail!("unknown device {other:?}; use cpu, cuda or coreml"),
        };
        // ONNX Runtime can only be initialized once per process; the plugin
        // loads at most one model per request.
        let committed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ort::init_from(&library)
                .map(|builder| builder.with_execution_providers(providers).commit())
        }));
        match committed {
            Ok(Ok(_)) => {}
            Ok(Err(e)) if cfg!(target_env = "musl") => bail!(
                "load ONNX Runtime {}: {e}; this static Linux build cannot load shared libraries, \
                 build the plugin from source to use a model",
                library.display()
            ),
            Ok(Err(e)) => bail!("load ONNX Runtime {}: {e}", library.display()),
            Err(_) => bail!(
                "load ONNX Runtime {}: the library could not be initialized",
                library.display()
            ),
        }
        let session = ort::session::Session::builder()
            .and_then(|mut b| b.commit_from_file(model))
            .map_err(|e| anyhow::anyhow!("load model {}: {e}", model.display()))?;
        let Some(input) = session.inputs().first() else {
            bail!("model {} has no inputs", model.display());
        };
        let rank = input.dtype().tensor_shape().map_or(1, |s| s.len());
        if !(1..=2).contains(&rank) {
            bail!(
                "model {} takes a rank-{rank} input; expected a 16 kHz waveform",
                model.display()
            );
        }
        let input = input.name().to_string();
        Ok(Self {
            session,
            input,
            rank,
            classes,
        })
    }
}

fn default_runtime_library() -> &'static str {
    if cfg!(windows) {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    }
}

impl Tagger for OnnxTagger {
    fn scores(&mut self, window: &[f32]) -> Result<Vec<f32>> {
        let shape: Vec<i64> = if self.rank == 2 {
            vec![1, window.len() as i64]
        } else {
            vec![window.len() as i64]
        };
        let tensor = ort::value::Tensor::from_array((shape, window.to_vec()))
            .map_err(|e| anyhow::anyhow!("build input: {e}"))?;
        let outputs = self
            .session
            .run(ort::inputs![self.input.as_str() => tensor])
            .map_err(|e| anyhow::anyhow!("run model: {e}"))?;
        let (_, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("read scores: {e}"))?;
        pool(data, self.classes)
    }
}

/// Max-pools a flat `[frames, classes]` output (YAMNet emits a frame every
/// 0.48 s) into one score per class.
pub fn pool(data: &[f32], classes: usize) -> Result<Vec<f32>> {
    if classes == 0 || data.is_empty() || !data.len().is_multiple_of(classes) {
        bail!(
            "model returned {} scores, which is not a multiple of the {classes} class names",
            data.len()
        );
    }
    let mut best = vec![f32::MIN; classes];
    for frame in data.chunks(classes) {
        for (b, &s) in best.iter_mut().zip(frame) {
            *b = b.max(s);
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        [
            "Speech",
            "Music",
            "Laughter",
            "Giggle",
            "Applause",
            "Crying, sobbing",
        ]
        .map(String::from)
        .to_vec()
    }

    #[test]
    fn label_map_groups_classes_and_skips_feelings() {
        let map = LabelMap::new(&names());
        assert_eq!(map.speech, vec![0]);
        assert_eq!(map.music, vec![1]);
        assert_eq!(
            map.events,
            vec![("laughter", vec![2, 3]), ("applause", vec![4])]
        );
        let scores = [0.9, 0.2, 0.1, 0.6, 0.31, 0.99];
        assert_eq!(map.speech_music(&scores), (0.9, 0.2));
        assert_eq!(
            map.events(&scores, 0.3),
            vec![("laughter", 0.6), ("applause", 0.31)]
        );
        assert!(map.events(&scores, 0.7).is_empty());
    }

    #[test]
    fn class_map_csv_and_plain_lists_both_parse() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("yamnet_class_map.csv");
        fs::write(
            &csv,
            "index,mid,display_name\n0,/m/09x0r,Speech\n1,/m/01j3sz,\"Chuckle, chortle\"\n",
        )
        .unwrap();
        assert_eq!(read_labels(&csv).unwrap(), ["Speech", "Chuckle, chortle"]);
        let txt = dir.path().join("labels.txt");
        fs::write(&txt, "Speech\n\nMusic\n").unwrap();
        assert_eq!(read_labels(&txt).unwrap(), ["Speech", "Music"]);
        fs::write(&txt, "\n").unwrap();
        assert!(read_labels(&txt).is_err());
    }

    #[test]
    fn labels_are_found_beside_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("yamnet.onnx");
        assert_eq!(default_labels(&model), None);
        fs::write(dir.path().join("yamnet_class_map.csv"), "x").unwrap();
        assert_eq!(
            default_labels(&model),
            Some(dir.path().join("yamnet_class_map.csv"))
        );
    }

    #[test]
    fn frames_are_max_pooled_and_shape_mismatches_fail() {
        assert_eq!(pool(&[0.1, 0.8, 0.5, 0.2], 2).unwrap(), vec![0.5, 0.8]);
        assert!(pool(&[0.1, 0.2, 0.3], 2).is_err());
        assert!(pool(&[], 2).is_err());
    }

    #[test]
    fn missing_runtime_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("missing.onnx");
        // SAFETY: tests in this module do not read the variable concurrently.
        unsafe { env::set_var("ORT_DYLIB_PATH", dir.path().join("no-such-onnxruntime")) };
        let error = OnnxTagger::load(&model, 2, "cpu").err().unwrap();
        assert!(format!("{error:#}").contains("ONNX Runtime"), "{error:#}");
        assert!(OnnxTagger::load(&model, 2, "tpu").is_err());
    }
}
