use anyhow::{Context, Result, bail};
use anytopdf_core::*;
use image::DynamicImage;
use regex::Regex;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub struct VideoImporter {
    interval: f64,
    scene_threshold: f64,
    dedupe_distance: u32,
    max_frames: usize,
}

impl VideoImporter {
    pub fn new(
        interval: f64,
        scene_threshold: f64,
        dedupe_distance: u32,
        max_frames: usize,
    ) -> Self {
        Self {
            interval,
            scene_threshold,
            dedupe_distance,
            max_frames,
        }
    }
}

impl Plugin for VideoImporter {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "ffmpeg-video".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "importer".into(),
            extensions: [
                "mp4", "mov", "m4v", "mkv", "avi", "webm", "mpg", "mpeg", "mts", "m2ts", "ts",
                "wmv", "flv", "3gp",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            mime_types: vec!["video/*".into()],
            priority: 80,
        }
    }
}

impl Importer for VideoImporter {
    fn probe(&self, source: &SourceRecord) -> ProbeScore {
        if source
            .detected_type
            .as_deref()
            .is_some_and(|m| m.starts_with("video/"))
        {
            return ProbeScore::MAGIC;
        }
        let ext = source
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if self.descriptor().extensions.iter().any(|x| x == &ext) {
            ProbeScore::EXTENSION
        } else {
            ProbeScore::NONE
        }
    }

    fn import(&self, ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
        if !self.interval.is_finite()
            || self.interval <= 0.0
            || !self.scene_threshold.is_finite()
            || !(0.0..=1.0).contains(&self.scene_threshold)
            || self.dedupe_distance > 64
        {
            bail!("invalid video sampling options");
        }
        let ffmpeg = which::which("ffmpeg").context("video input requires ffmpeg")?;
        let root = ctx.workspace.join(format!("video-{}", source.id));
        fs::create_dir_all(&root)?;

        let mut frames = Vec::new();
        frames.extend(extract(
            &ffmpeg,
            &source.path,
            &root.join("interval"),
            &format!(
                "select='isnan(prev_selected_t)+gte(t-prev_selected_t\\,{})'",
                self.interval.max(0.1)
            ),
            "interval",
            self.max_frames,
        )?);
        frames.extend(extract(
            &ffmpeg,
            &source.path,
            &root.join("scene"),
            &format!(
                "select='gt(scene\\,{})'",
                self.scene_threshold.clamp(0.0, 1.0)
            ),
            "scene",
            self.max_frames,
        )?);

        frames.sort_by(|a, b| a.0.total_cmp(&b.0));
        frames.dedup_by(|a, b| (a.0 - b.0).abs() < 0.075);

        let mut units = Vec::new();
        let mut hashes = Vec::new();

        for (ts, frame, reason) in frames {
            let img = match image::open(&frame) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let hash = dhash(&img);
            if hashes
                .iter()
                .rev()
                .take(12)
                .any(|old| hamming(hash, *old) <= self.dedupe_distance)
            {
                continue;
            }
            hashes.push(hash);

            let mut unit = Unit::visual(source.id, frame);
            unit.time_range = Some(TimeRange::point(ts));
            unit.metadata
                .insert("video.frame-selection".into(), reason.clone());
            unit.annotations.push(Annotation {
                kind: AnnotationKind::Timestamp,
                text: format!("video timestamp: {ts:.3} seconds"),
                provider: "ffmpeg-video".into(),
                confidence: None,
                region: None,
                time_range: unit.time_range,
                attributes: Metadata::new(),
            });
            unit.annotations.push(Annotation::text(
                AnnotationKind::Scene,
                "ffmpeg-video",
                format!("selected keyframe ({reason})"),
            ));
            units.push(unit);

            if self.max_frames > 0 && units.len() >= self.max_frames {
                break;
            }
        }

        if units.is_empty() {
            bail!("ffmpeg produced no usable video frames");
        }

        Ok(ImportOutcome {
            source,
            units,
            warnings: vec![],
        })
    }
}

fn extract(
    ffmpeg: &Path,
    input: &Path,
    dir: &Path,
    select: &str,
    reason: &str,
    max_frames: usize,
) -> Result<Vec<(f64, PathBuf, String)>> {
    fs::create_dir_all(dir)?;
    let pattern = dir.join("frame-%08d.png");

    let mut command = Command::new(ffmpeg);
    command
        .args(["-hide_banner", "-loglevel", "info", "-i"])
        .arg(input)
        .args(["-map", "0:v:0", "-vf"])
        .arg(format!("{select},showinfo"))
        .args(["-fps_mode", "vfr", "-an", "-sn", "-dn", "-y"]);
    if max_frames > 0 {
        command.args(["-frames:v", &max_frames.to_string()]);
    }
    let output = command
        .arg(&pattern)
        .bounded_output(std::time::Duration::from_secs(300))
        .context("run ffmpeg frame extraction")?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let re = Regex::new(r"\bpts_time:([0-9eE.+-]+)").unwrap();
    let times: Vec<f64> = re
        .captures_iter(&stderr)
        .filter_map(|c| c.get(1)?.as_str().parse().ok())
        .collect();

    let mut paths: Vec<_> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("png"))
        .collect();
    paths.sort();

    if !output.status.success() {
        bail!(
            "ffmpeg failed: {}",
            stderr.lines().last().unwrap_or("unknown ffmpeg error")
        );
    }

    if times.len() < paths.len() {
        bail!("ffmpeg did not report timestamps for every frame");
    }
    Ok(paths
        .into_iter()
        .zip(times)
        .map(|(p, time)| (time, p, reason.to_string()))
        .collect())
}

fn dhash(img: &DynamicImage) -> u64 {
    let gray = img.thumbnail_exact(9, 8).to_luma8();
    let mut out = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            let a = gray.get_pixel(x, y)[0];
            let b = gray.get_pixel(x + 1, y)[0];
            if a > b {
                out |= 1 << (y * 8 + x);
            }
        }
    }
    out
}

fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}
