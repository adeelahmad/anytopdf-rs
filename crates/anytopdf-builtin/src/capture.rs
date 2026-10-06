//! Screen recording through FFmpeg's platform grabbers. The recording is an ordinary
//! video file, so its frames reach the PDF through the video importer's interval,
//! scene-change and perceptual-dedupe sampling like any other video.

use anytopdf_core::CommandExt;
use std::{path::Path, process::Command, time::Duration};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Operating system whose FFmpeg grabber records the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapturePlatform {
    MacOs,
    Windows,
    Linux,
}

impl CapturePlatform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// The FFmpeg input that records one screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenGrabber {
    /// FFmpeg input format (`-f`), e.g. `x11grab`.
    pub format: String,
    /// Options placed before `-i`.
    pub input_options: Vec<String>,
    /// The `-i` argument.
    pub input: String,
    /// Filters needed before encoding, e.g. `hwdownload` for `ddagrab`.
    pub filters: Vec<String>,
}

impl ScreenGrabber {
    /// The platform grabber for `display`, recording `framerate` frames per second.
    ///
    /// `display` is the macOS screen index (`Capture screen N`), the Windows output
    /// index (`ddagrab`; the whole desktop through `gdigrab` when omitted) or the X11
    /// display number on Linux (`$DISPLAY`, else `:0`, when omitted).
    pub fn for_platform(
        platform: CapturePlatform,
        display: Option<u32>,
        framerate: f64,
        x_display: Option<&str>,
    ) -> Self {
        let rate = format_rate(framerate);
        match platform {
            CapturePlatform::MacOs => Self {
                format: "avfoundation".into(),
                input_options: strings(&["-framerate", &rate, "-capture_cursor", "1"]),
                input: format!("Capture screen {}:none", display.unwrap_or(0)),
                filters: vec![],
            },
            CapturePlatform::Windows => match display {
                None => Self {
                    format: "gdigrab".into(),
                    input_options: strings(&["-framerate", &rate, "-draw_mouse", "1"]),
                    input: "desktop".into(),
                    filters: vec![],
                },
                Some(index) => Self {
                    format: "lavfi".into(),
                    input_options: vec![],
                    input: format!("ddagrab=output_idx={index}:framerate={rate}"),
                    filters: strings(&["hwdownload", "format=bgra"]),
                },
            },
            CapturePlatform::Linux => Self {
                format: "x11grab".into(),
                input_options: strings(&["-framerate", &rate, "-draw_mouse", "1"]),
                input: match (display, x_display) {
                    (Some(n), _) => format!(":{n}"),
                    (None, Some(name)) if !name.is_empty() => name.to_string(),
                    _ => ":0".into(),
                },
                filters: vec![],
            },
        }
    }

    /// A caller-chosen FFmpeg input, e.g. `kmsgrab`, or `lavfi` with a test source.
    pub fn custom(format: &str, input: &str, framerate: f64) -> Self {
        // A lavfi source is generated, not grabbed: `-re` paces it like a live screen.
        let input_options = match format {
            "lavfi" => strings(&["-re"]),
            _ => strings(&["-framerate", &format_rate(framerate)]),
        };
        Self {
            format: format.into(),
            input_options,
            input: input.into(),
            filters: vec![],
        }
    }

    /// True for inputs that read the screen and so need OS permission to record.
    pub fn reads_screen(&self) -> bool {
        self.format != "lavfi" || self.input.starts_with("ddagrab")
    }

    /// FFmpeg arguments that record into `output` (Matroska, which stays readable when
    /// the recording is cut short) for `duration` seconds, or until `q` on stdin.
    pub fn ffmpeg_args(
        &self,
        encoder: Encoder,
        duration: Option<f64>,
        output: &Path,
    ) -> Vec<String> {
        let mut args = strings(&["-hide_banner", "-nostats", "-loglevel", "error", "-y", "-f"]);
        args.push(self.format.clone());
        args.extend(self.input_options.iter().cloned());
        args.push("-i".into());
        args.push(self.input.clone());
        if let Some(seconds) = duration {
            args.push("-t".into());
            args.push(format_rate(seconds));
        }
        let mut filters = self.filters.clone();
        // 4:2:0 encoders need even dimensions; screens and windows are not always even.
        filters.push("scale=trunc(iw/2)*2:trunc(ih/2)*2".into());
        filters.push(format!("format={}", encoder.pixel_format()));
        args.push("-vf".into());
        args.push(filters.join(","));
        args.push("-an".into());
        args.extend(encoder.args().iter().map(|s| s.to_string()));
        args.push(output.to_string_lossy().into_owned());
        args
    }
}

/// Video encoder for the recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoder {
    /// H.264 tuned for still screen content; small files for long recordings.
    X264,
    /// Built into every FFmpeg; larger files, used when libx264 is missing.
    Mjpeg,
}

impl Encoder {
    /// libx264 when this FFmpeg has it, otherwise MJPEG.
    pub fn detect(ffmpeg: &Path) -> Self {
        let listed = Command::new(ffmpeg)
            .args(["-hide_banner", "-encoders"])
            .bounded_output(PROBE_TIMEOUT)
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        if listed.split_whitespace().any(|word| word == "libx264") {
            Self::X264
        } else {
            Self::Mjpeg
        }
    }

    fn args(self) -> &'static [&'static str] {
        match self {
            Self::X264 => &[
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-tune",
                "stillimage",
                "-crf",
                "18",
            ],
            Self::Mjpeg => &["-c:v", "mjpeg", "-q:v", "3"],
        }
    }

    fn pixel_format(self) -> &'static str {
        match self {
            Self::X264 => "yuv420p",
            Self::Mjpeg => "yuvj420p",
        }
    }
}

/// One line of `anytopdf doctor` screen-capture output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureStatus {
    pub name: &'static str,
    pub available: bool,
    pub detail: String,
}

/// Whether `anytopdf capture screen` can record here: the FFmpeg grabber and, on
/// macOS, the Screen Recording permission of the app running anytopdf.
pub fn capture_status() -> Vec<CaptureStatus> {
    let platform = CapturePlatform::current();
    let grabber = ScreenGrabber::for_platform(
        platform,
        None,
        1.0,
        std::env::var("DISPLAY").ok().as_deref(),
    );
    let mut out = Vec::new();
    let Ok(ffmpeg) = which::which("ffmpeg") else {
        out.push(CaptureStatus {
            name: "screen",
            available: false,
            detail: "install FFmpeg to record the screen".into(),
        });
        return out;
    };
    let devices = Command::new(&ffmpeg)
        .args(["-hide_banner", "-devices"])
        .bounded_output(PROBE_TIMEOUT)
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let found = has_input_device(&devices, &grabber.format);
    let mut detail = if found {
        format!("FFmpeg {}", grabber.format)
    } else {
        format!("this FFmpeg has no {} input device", grabber.format)
    };
    let mut available = found;
    if platform == CapturePlatform::Linux && found {
        match std::env::var("DISPLAY") {
            Ok(display) if !display.is_empty() => detail.push_str(&format!(" on {display}")),
            _ if std::env::var_os("WAYLAND_DISPLAY").is_some() => {
                available = false;
                detail.push_str(
                    "; Wayland session without XWayland $DISPLAY: use --input-format kmsgrab",
                );
            }
            _ => {
                available = false;
                detail.push_str("; no $DISPLAY set");
            }
        }
    }
    out.push(CaptureStatus {
        name: "screen",
        available,
        detail,
    });
    if platform == CapturePlatform::MacOs {
        let granted = screen_recording_permitted();
        out.push(CaptureStatus {
            name: "permission",
            available: granted,
            detail: if granted {
                "Screen Recording allowed".into()
            } else {
                PERMISSION_HINT.into()
            },
        });
    }
    out
}

/// Where to grant the macOS Screen Recording permission.
pub const PERMISSION_HINT: &str = "Screen Recording not allowed: System Settings > Privacy & Security > Screen Recording, enable the terminal app running anytopdf, then restart it";

/// True when an `ffmpeg -devices` listing has a demuxing device named `format`.
pub fn has_input_device(listing: &str, format: &str) -> bool {
    listing.lines().any(|line| {
        let mut parts = line.split_whitespace();
        matches!(parts.next(), Some(flags) if flags.starts_with('D') && flags.len() <= 2)
            && parts
                .next()
                .is_some_and(|names| names.split(',').any(|n| n == format))
    })
}

/// macOS Screen Recording permission for the app that launched this process. Always
/// true elsewhere.
#[cfg(target_os = "macos")]
pub fn screen_recording_permitted() -> bool {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
    }
    // SAFETY: takes no arguments and only reads the TCC permission state (macOS 10.15+).
    unsafe { CGPreflightScreenCaptureAccess() }
}

#[cfg(not(target_os = "macos"))]
pub fn screen_recording_permitted() -> bool {
    true
}

/// Asks macOS to show the Screen Recording prompt (once per app); no-op elsewhere.
#[cfg(target_os = "macos")]
pub fn request_screen_recording() {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGRequestScreenCaptureAccess() -> bool;
    }
    // SAFETY: takes no arguments; shows the system prompt at most once per app.
    unsafe {
        CGRequestScreenCaptureAccess();
    }
}

#[cfg(not(target_os = "macos"))]
pub fn request_screen_recording() {}

fn format_rate(value: f64) -> String {
    let text = format!("{value:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(platform: CapturePlatform, display: Option<u32>, x: Option<&str>) -> (String, String) {
        let g = ScreenGrabber::for_platform(platform, display, 2.0, x);
        (g.format, g.input)
    }

    #[test]
    fn each_platform_uses_its_ffmpeg_grabber() {
        use CapturePlatform::*;
        assert_eq!(
            input(MacOs, None, None),
            ("avfoundation".into(), "Capture screen 0:none".into())
        );
        assert_eq!(
            input(MacOs, Some(1), None),
            ("avfoundation".into(), "Capture screen 1:none".into())
        );
        assert_eq!(
            input(Windows, None, None),
            ("gdigrab".into(), "desktop".into())
        );
        assert_eq!(
            input(Windows, Some(1), None),
            ("lavfi".into(), "ddagrab=output_idx=1:framerate=2".into())
        );
        assert_eq!(
            input(Linux, None, Some(":1")),
            ("x11grab".into(), ":1".into())
        );
        assert_eq!(input(Linux, None, None), ("x11grab".into(), ":0".into()));
        assert_eq!(
            input(Linux, Some(2), Some(":1")),
            ("x11grab".into(), ":2".into())
        );
    }

    #[test]
    fn ddagrab_downloads_frames_before_encoding() {
        let g = ScreenGrabber::for_platform(CapturePlatform::Windows, Some(0), 1.0, None);
        let args = g.ffmpeg_args(Encoder::Mjpeg, None, Path::new("out.mkv"));
        let vf = &args[args.iter().position(|a| a == "-vf").unwrap() + 1];
        assert!(vf.starts_with("hwdownload,format=bgra,scale="), "{vf}");
        assert!(g.reads_screen());
    }

    #[test]
    fn ffmpeg_args_bound_duration_and_even_out_dimensions() {
        let g = ScreenGrabber::for_platform(CapturePlatform::Linux, None, 0.5, Some(":0"));
        let args = g.ffmpeg_args(Encoder::X264, Some(90.0), Path::new("rec.mkv"));
        let joined = args.join(" ");
        assert!(
            joined.contains("-f x11grab -framerate 0.5 -draw_mouse 1 -i :0 -t 90 "),
            "{joined}"
        );
        assert!(
            joined
                .contains("-vf scale=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p -an -c:v libx264"),
            "{joined}"
        );
        assert_eq!(args.last().unwrap(), "rec.mkv");
        let open = g.ffmpeg_args(Encoder::Mjpeg, None, Path::new("rec.mkv"));
        assert!(!open.iter().any(|a| a == "-t"));
        assert!(open.join(" ").contains("format=yuvj420p -an -c:v mjpeg"));
    }

    #[test]
    fn custom_lavfi_inputs_run_in_real_time_and_need_no_permission() {
        let g = ScreenGrabber::custom("lavfi", "testsrc2=size=64x48:rate=2", 2.0);
        assert_eq!(g.input_options, ["-re"]);
        assert!(!g.reads_screen());
        let k = ScreenGrabber::custom("kmsgrab", "-", 1.5);
        assert_eq!(k.input_options, ["-framerate", "1.5"]);
        assert!(k.reads_screen());
    }

    #[test]
    fn device_listing_matches_demuxers_only() {
        let listing = "Devices:\n D. = Demuxing supported\n .E = Muxing supported\n --\n DE alsa            ALSA\n  E caca            caca output\n D  x11grab         X11 screen capture\n D  avfoundation    AVFoundation input device\n DE video4linux2,v4l2 Video4Linux2\n";
        assert!(has_input_device(listing, "x11grab"));
        assert!(has_input_device(listing, "avfoundation"));
        assert!(has_input_device(listing, "v4l2"));
        assert!(!has_input_device(listing, "caca"));
        assert!(!has_input_device(listing, "gdigrab"));
    }
}
