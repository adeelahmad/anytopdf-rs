//! Model-free signal analysis on 16 kHz mono audio, one window per second:
//! silence by level, speech/music/noise by the classic low-energy and
//! zero-crossing discriminators, and ITU-R BS.1770 (K-weighted) loudness for
//! raised-voice detection.

pub const SAMPLE_RATE: usize = 16_000;
/// One analysis window: one second.
pub const WINDOW: usize = SAMPLE_RATE;
/// 20 ms sub-frames inside a window.
const SUBFRAME: usize = SAMPLE_RATE / 50;
/// Windows quieter than this (RMS, dBFS) are silence.
pub const SILENCE_DBFS: f32 = -50.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowStats {
    pub start: f64,
    pub end: f64,
    pub rms_dbfs: f32,
    /// K-weighted loudness of the window in LUFS.
    pub loudness: f32,
    /// Share of sub-frames with less than half the window's mean energy.
    /// High for speech, which pauses between syllables.
    pub low_energy_ratio: f32,
    /// Share of sub-frames whose zero-crossing rate is over 1.5x the
    /// window's mean. High for speech, which alternates voiced and unvoiced.
    pub high_zcr_ratio: f32,
    /// Mean zero crossings per sample. Near 0.5 for broadband noise.
    pub zcr: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Class {
    Speech,
    Music,
    Noise,
    Silence,
}

impl Class {
    pub fn label(self) -> &'static str {
        match self {
            Class::Speech => "speech",
            Class::Music => "music",
            Class::Noise => "noise",
            Class::Silence => "silence",
        }
    }
}

/// Scores from a sound-event model for the speech/music decision, when one
/// is configured. Without them the signal heuristics decide.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelScores {
    pub speech: f32,
    pub music: f32,
}

pub fn classify(stats: &WindowStats, scores: Option<ModelScores>, threshold: f32) -> Class {
    if stats.rms_dbfs < SILENCE_DBFS {
        return Class::Silence;
    }
    if let Some(scores) = scores {
        let best = scores.speech.max(scores.music);
        return if best < threshold {
            Class::Noise
        } else if scores.speech >= scores.music {
            Class::Speech
        } else {
            Class::Music
        };
    }
    if stats.low_energy_ratio >= 0.15 || stats.high_zcr_ratio >= 0.15 {
        Class::Speech
    } else if stats.zcr >= 0.3 {
        Class::Noise
    } else {
        Class::Music
    }
}

/// Analyzes consecutive windows of one stream. The K-weighting filter keeps
/// its state across windows, so feed every sample exactly once, in order.
pub struct Analyzer {
    filter: KWeighting,
    position: usize,
}

impl Default for Analyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer {
    pub fn new() -> Self {
        Self {
            filter: KWeighting::new(SAMPLE_RATE as f64),
            position: 0,
        }
    }

    /// Statistics for the next window. `samples` is at most [`WINDOW`] long;
    /// only the stream's last window is shorter.
    pub fn window(&mut self, samples: &[f32]) -> WindowStats {
        let start = self.position as f64 / SAMPLE_RATE as f64;
        self.position += samples.len();
        let end = self.position as f64 / SAMPLE_RATE as f64;
        let n = samples.len().max(1) as f64;

        let energy: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        let weighted: f64 = samples
            .iter()
            .map(|&s| {
                let y = self.filter.process(f64::from(s));
                y * y
            })
            .sum();

        let frames: Vec<(f64, f64)> = samples
            .chunks(SUBFRAME)
            .filter(|c| c.len() * 2 >= SUBFRAME)
            .map(|c| {
                let e =
                    c.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>() / c.len() as f64;
                (e, crossings(c) as f64 / c.len() as f64)
            })
            .collect();
        let count = frames.len().max(1) as f64;
        let mean_energy = frames.iter().map(|f| f.0).sum::<f64>() / count;
        let mean_zcr = frames.iter().map(|f| f.1).sum::<f64>() / count;
        let low = frames.iter().filter(|f| f.0 < 0.5 * mean_energy).count();
        let high = frames.iter().filter(|f| f.1 > 1.5 * mean_zcr).count();

        WindowStats {
            start,
            end,
            rms_dbfs: decibels(energy / n) as f32,
            loudness: (-0.691 + decibels(weighted / n)) as f32,
            low_energy_ratio: (low as f64 / count) as f32,
            high_zcr_ratio: (high as f64 / count) as f32,
            zcr: (crossings(samples) as f64 / n) as f32,
        }
    }
}

fn crossings(samples: &[f32]) -> usize {
    samples
        .windows(2)
        .filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0))
        .count()
}

fn decibels(power: f64) -> f64 {
    10.0 * power.max(1e-12).log10()
}

/// Windows whose loudness is at least `margin_lu` above the median loudness
/// of the file's speech windows. A measurable level jump, not a guess about
/// how anyone feels. Needs a few speech windows to set the baseline.
pub fn raised_voices(windows: &[(WindowStats, Class)], margin_lu: f32) -> Vec<(usize, f32)> {
    const MIN_SPEECH_WINDOWS: usize = 5;
    let mut speech: Vec<f32> = windows
        .iter()
        .filter(|(_, c)| *c == Class::Speech)
        .map(|(s, _)| s.loudness)
        .collect();
    if speech.len() < MIN_SPEECH_WINDOWS {
        return Vec::new();
    }
    speech.sort_by(f32::total_cmp);
    let baseline = speech[speech.len() / 2];
    windows
        .iter()
        .enumerate()
        .filter(|(_, (s, c))| *c == Class::Speech && s.loudness >= baseline + margin_lu)
        .map(|(i, _)| (i, baseline))
        .collect()
}

/// The two-stage BS.1770 pre-filter (high shelf, then high pass), with
/// coefficients derived for any sample rate as in libebur128.
struct KWeighting {
    stages: [Biquad; 2],
}

impl KWeighting {
    fn new(rate: f64) -> Self {
        use std::f64::consts::PI;
        let (f0, gain_db, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
        let k = (PI * f0 / rate).tan();
        let vh = 10f64.powf(gain_db / 20.0);
        let vb = vh.powf(0.4996667741545416);
        let a0 = 1.0 + k / q + k * k;
        let shelf = Biquad::new(
            [
                (vh + vb * k / q + k * k) / a0,
                2.0 * (k * k - vh) / a0,
                (vh - vb * k / q + k * k) / a0,
            ],
            [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        );
        let (f0, q) = (38.13547087602444, 0.5003270373238773);
        let k = (PI * f0 / rate).tan();
        let a0 = 1.0 + k / q + k * k;
        let high_pass = Biquad::new(
            [1.0, -2.0, 1.0],
            [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        );
        Self {
            stages: [shelf, high_pass],
        }
    }

    fn process(&mut self, x: f64) -> f64 {
        self.stages.iter_mut().fold(x, |x, stage| stage.process(x))
    }
}

struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn new(b: [f64; 3], a: [f64; 2]) -> Self {
        Self { b, a, z: [0.0; 2] }
    }

    // Transposed direct form II.
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

#[cfg(test)]
pub(crate) mod signals {
    //! Deterministic synthetic signals for tests.
    use super::SAMPLE_RATE;
    use std::f32::consts::TAU;

    pub fn tone(seconds: f32, hz: f32, amplitude: f32) -> Vec<f32> {
        (0..(seconds * SAMPLE_RATE as f32) as usize)
            .map(|i| amplitude * (TAU * hz * i as f32 / SAMPLE_RATE as f32).sin())
            .collect()
    }

    pub fn noise(seconds: f32, amplitude: f32, seed: u32) -> Vec<f32> {
        let mut state = seed.max(1);
        (0..(seconds * SAMPLE_RATE as f32) as usize)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                amplitude * (state as f32 / u32::MAX as f32 * 2.0 - 1.0)
            })
            .collect()
    }

    pub fn silence(seconds: f32) -> Vec<f32> {
        vec![0.0; (seconds * SAMPLE_RATE as f32) as usize]
    }

    /// Four "syllables" a second: a voiced tone, a short fricative burst and
    /// a pause, the energy and zero-crossing pattern of speech.
    pub fn speech(seconds: f32, amplitude: f32) -> Vec<f32> {
        let mut out = Vec::new();
        let mut seed = 7;
        while out.len() < (seconds * SAMPLE_RATE as f32) as usize {
            out.extend(tone(0.125, 180.0, amplitude));
            out.extend(noise(0.05, amplitude * 0.3, seed));
            out.extend(silence(0.075));
            seed += 1;
        }
        out.truncate((seconds * SAMPLE_RATE as f32) as usize);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::signals::*;
    use super::*;

    fn classes(samples: &[f32]) -> Vec<Class> {
        let mut analyzer = Analyzer::new();
        samples
            .chunks(WINDOW)
            .map(|w| classify(&analyzer.window(w), None, 0.3))
            .collect()
    }

    #[test]
    fn full_scale_1khz_sine_measures_minus_3_lufs() {
        let mut analyzer = Analyzer::new();
        let samples = tone(3.0, 997.0, 1.0);
        let stats: Vec<_> = samples.chunks(WINDOW).map(|w| analyzer.window(w)).collect();
        let last = stats.last().unwrap();
        assert!((last.loudness + 3.01).abs() < 0.2, "{last:?}");
        assert_eq!((last.start, last.end), (2.0, 3.0));
    }

    #[test]
    fn steady_tone_is_music_and_noise_is_noise_and_zeros_are_silence() {
        assert!(
            classes(&tone(3.0, 440.0, 0.3))
                .iter()
                .all(|c| *c == Class::Music)
        );
        assert!(
            classes(&noise(3.0, 0.3, 1))
                .iter()
                .all(|c| *c == Class::Noise)
        );
        assert!(classes(&silence(2.0)).iter().all(|c| *c == Class::Silence));
    }

    #[test]
    fn syllabic_signal_is_speech() {
        assert!(
            classes(&speech(4.0, 0.2))
                .iter()
                .all(|c| *c == Class::Speech)
        );
    }

    #[test]
    fn model_scores_override_the_heuristics_but_not_silence() {
        let mut analyzer = Analyzer::new();
        let tone = analyzer.window(&tone(1.0, 440.0, 0.3));
        let speech = ModelScores {
            speech: 0.9,
            music: 0.1,
        };
        let neither = ModelScores {
            speech: 0.1,
            music: 0.2,
        };
        assert_eq!(classify(&tone, Some(speech), 0.3), Class::Speech);
        assert_eq!(classify(&tone, Some(neither), 0.3), Class::Noise);
        let quiet = analyzer.window(&silence(1.0));
        assert_eq!(classify(&quiet, Some(speech), 0.3), Class::Silence);
    }

    #[test]
    fn only_speech_well_above_the_speech_baseline_is_raised() {
        let mut samples = speech(6.0, 0.05);
        samples.extend(speech(2.0, 0.4));
        samples.extend(tone(2.0, 440.0, 0.9));
        samples.extend(speech(2.0, 0.05));
        let mut analyzer = Analyzer::new();
        let windows: Vec<_> = samples
            .chunks(WINDOW)
            .map(|w| {
                let stats = analyzer.window(w);
                (stats, classify(&stats, None, 0.3))
            })
            .collect();
        let raised: Vec<usize> = raised_voices(&windows, 8.0).iter().map(|r| r.0).collect();
        assert_eq!(raised, vec![6, 7]);
    }

    #[test]
    fn too_little_speech_sets_no_baseline() {
        let windows: Vec<_> = speech(3.0, 0.3)
            .chunks(WINDOW)
            .map(|w| (Analyzer::new().window(w), Class::Speech))
            .collect();
        assert!(raised_voices(&windows, 1.0).is_empty());
    }
}
