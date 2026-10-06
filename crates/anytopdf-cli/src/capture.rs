//! `anytopdf capture screen`: record the screen with FFmpeg, then convert the recording.
//! The recording goes through the video importer, so interval sampling, scene-change
//! selection and perceptual dedupe pick the pages exactly as for any video file.

use crate::cli::{Cli, Commands, ScreenArgs};
use crate::config::Resolved;
use crate::exit::{CliError, ExitClass, fail, tag};
use crate::{convert::convert, naming, publish::checked_destination};
use anyhow::Context;
use anytopdf_builtin::capture::{
    CapturePlatform, Encoder, PERMISSION_HINT, ScreenGrabber, request_screen_recording,
    screen_recording_permitted,
};
use anytopdf_core::{PluginOptions, RuntimePluginPolicy};
use clap::FromArgMatches;
use std::{
    io::Write,
    path::Path,
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

/// How long FFmpeg gets to finish the file after being asked to stop.
const STOP_GRACE: Duration = Duration::from_secs(15);
/// Extra time past `--duration` before FFmpeg is asked to stop.
const DURATION_SLACK: Duration = Duration::from_secs(30);

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

pub(crate) fn screen(
    args: ScreenArgs,
    policy: &RuntimePluginPolicy,
    resolved: &Resolved,
) -> Result<(), CliError> {
    validate(&args)?;
    let passthrough_has = |flag: &str| {
        args.convert
            .iter()
            .any(|a| a == flag || a.starts_with(&format!("{flag}=")))
    };
    for flag in ["--output", "-o", "--output-dir"] {
        if passthrough_has(flag) {
            return Err(fail(
                ExitClass::Usage,
                format!("pass the PDF path as `capture screen -o`, not `{flag}` after `--`"),
            ));
        }
    }
    let scratch = tempfile::Builder::new()
        .prefix("anytopdf-capture-")
        .tempdir()?;
    let stamp = utc_stamp(SystemTime::now());
    let recording = match &args.keep_recording {
        Some(path) => {
            if path.exists() {
                return Err(fail(
                    ExitClass::Usage,
                    format!("recording exists: {}", path.display()),
                ));
            }
            path.clone()
        }
        None => scratch.path().join(format!("screen-{stamp}.mkv")),
    };
    let overwrite = passthrough_has("--overwrite");
    let output = match &args.output {
        Some(path) => path.clone(),
        None => naming::next_free_path(
            &std::env::current_dir()?.join(format!("screen-{stamp}.pdf")),
            &[],
        )?,
    };
    // Refuse a taken output before recording rather than after.
    tag(
        ExitClass::Usage,
        checked_destination(&output, &[], overwrite),
    )?;
    let (convert_args, options) = convert_args(
        &args,
        &recording,
        &output,
        passthrough_has("--video-interval"),
        resolved,
    )?;
    let policy = &RuntimePluginPolicy {
        options: Arc::new(options),
        ..policy.clone()
    };

    let ffmpeg = tag(
        ExitClass::Provider,
        which::which("ffmpeg").context("screen capture requires ffmpeg on PATH"),
    )?;
    let grabber = match (&args.input_format, &args.input) {
        (Some(format), Some(input)) => ScreenGrabber::custom(format, input, args.framerate),
        _ => ScreenGrabber::for_platform(
            CapturePlatform::current(),
            args.display,
            args.framerate,
            std::env::var("DISPLAY").ok().as_deref(),
        ),
    };
    if grabber.reads_screen() && !screen_recording_permitted() {
        request_screen_recording();
        return Err(fail(ExitClass::Provider, PERMISSION_HINT));
    }

    if !convert_args.quiet && !convert_args.events && !convert_args.json {
        let until = match args.duration {
            Some(seconds) => format!("for {seconds} s"),
            None => "until Ctrl-C".to_string(),
        };
        eprintln!(
            "recording screen ({} {}) {until}",
            grabber.format, grabber.input
        );
    }
    let ffmpeg_args = grabber.ffmpeg_args(Encoder::detect(&ffmpeg), args.duration, &recording);
    let log = scratch.path().join("ffmpeg.log");
    let status = {
        let _guard = InterruptGuard::install();
        record(&ffmpeg, &ffmpeg_args, &log, args.duration)?
    };
    let stopped_by_user = INTERRUPTED.swap(false, Ordering::SeqCst);
    let recorded = std::fs::metadata(&recording).is_ok_and(|m| m.len() > 0);
    if !recorded || (!status.success() && !stopped_by_user) {
        let detail = std::fs::read_to_string(&log).unwrap_or_default();
        let last = detail.lines().rev().find(|l| !l.trim().is_empty());
        return Err(fail(
            ExitClass::Provider,
            format!(
                "ffmpeg {} recorded no screen video: {}",
                grabber.format,
                last.unwrap_or("no output")
            ),
        ));
    }
    convert(convert_args, policy)
}

fn validate(args: &ScreenArgs) -> Result<(), CliError> {
    let positive = |v: f64| v.is_finite() && v > 0.0;
    if !positive(args.interval) {
        return Err(fail(
            ExitClass::Usage,
            "--interval must be finite and greater than zero",
        ));
    }
    if !positive(args.framerate) || args.framerate > 60.0 {
        return Err(fail(
            ExitClass::Usage,
            "--framerate must be greater than zero and at most 60",
        ));
    }
    if args.duration.is_some_and(|d| !positive(d)) {
        return Err(fail(
            ExitClass::Usage,
            "--duration must be finite and greater than zero",
        ));
    }
    Ok(())
}

/// Parses the `convert` run for the recording up front, so bad options fail before
/// anything is recorded. Per-type options given after `--` are layered over the
/// configuration this run already resolved.
fn convert_args(
    args: &ScreenArgs,
    recording: &Path,
    output: &Path,
    interval_given: bool,
    resolved: &Resolved,
) -> Result<(crate::cli::ConvertArgs, PluginOptions), CliError> {
    let mut argv: Vec<std::ffi::OsString> = vec!["anytopdf".into(), "convert".into()];
    argv.push(recording.into());
    argv.push("--output".into());
    argv.push(output.into());
    if !interval_given {
        argv.push("--video-interval".into());
        argv.push(args.interval.to_string().into());
    }
    argv.extend(args.convert.iter().map(Into::into));
    let invalid = |e: &dyn std::fmt::Display| {
        fail(
            ExitClass::Usage,
            format!("invalid convert options after `--`: {e}"),
        )
    };
    let matches = crate::cli::command()
        .try_get_matches_from(argv)
        .map_err(|e| invalid(&e))?;
    let mut cli = Cli::from_arg_matches(&matches).map_err(|e| invalid(&e))?;
    let options = resolved
        .with_convert(&mut cli, &matches)
        .map_err(|e| invalid(&format!("{e:#}")))?
        .tables();
    match cli.command {
        Commands::Convert(convert) => Ok((*convert, options)),
        _ => Err(fail(
            ExitClass::Internal,
            "capture did not build a convert run",
        )),
    }
}

/// Runs FFmpeg until it exits, asking it to stop (`q` on stdin) on Ctrl-C or once the
/// duration has clearly passed, and killing it if it ignores that.
fn record(
    ffmpeg: &Path,
    args: &[String],
    log: &Path,
    duration: Option<f64>,
) -> Result<ExitStatus, CliError> {
    let mut child = tag(
        ExitClass::Provider,
        Command::new(ffmpeg)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(log)?)
            .spawn()
            .context("start ffmpeg"),
    )?;
    let deadline = duration.map(|d| Instant::now() + Duration::from_secs_f64(d) + DURATION_SLACK);
    let mut asked: Option<Instant> = None;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        let overdue = deadline.is_some_and(|d| Instant::now() >= d);
        if asked.is_none() && (INTERRUPTED.load(Ordering::SeqCst) || overdue) {
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(b"q\n").and_then(|()| stdin.flush());
            }
            asked = Some(Instant::now());
        }
        if asked.is_some_and(|t| t.elapsed() > STOP_GRACE) {
            let _ = child.kill();
            return Ok(child.wait()?);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Turns Ctrl-C into a request to stop recording while it is installed; the default
/// handling (end the process) returns when it is dropped.
struct InterruptGuard;

impl InterruptGuard {
    fn install() -> Self {
        INTERRUPTED.store(false, Ordering::SeqCst);
        platform::install();
        InterruptGuard
    }
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        platform::restore();
    }
}

#[cfg(unix)]
mod platform {
    use super::{INTERRUPTED, Ordering};

    extern "C" fn on_interrupt(_: libc::c_int) {
        INTERRUPTED.store(true, Ordering::SeqCst);
    }

    pub(super) fn install() {
        let handler: extern "C" fn(libc::c_int) = on_interrupt;
        // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
        unsafe { libc::signal(libc::SIGINT, handler as libc::sighandler_t) };
    }

    pub(super) fn restore() {
        // SAFETY: restores the default disposition.
        unsafe { libc::signal(libc::SIGINT, libc::SIG_DFL) };
    }
}

#[cfg(windows)]
mod platform {
    use super::{INTERRUPTED, Ordering};
    use windows_sys::Win32::Foundation::{FALSE, TRUE};
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler,
    };

    unsafe extern "system" fn on_interrupt(kind: u32) -> windows_sys::core::BOOL {
        if kind == CTRL_C_EVENT || kind == CTRL_BREAK_EVENT {
            INTERRUPTED.store(true, Ordering::SeqCst);
            TRUE
        } else {
            FALSE
        }
    }

    pub(super) fn install() {
        // SAFETY: registers a handler that only stores to an atomic.
        unsafe { SetConsoleCtrlHandler(Some(on_interrupt), TRUE) };
    }

    pub(super) fn restore() {
        // SAFETY: removes the handler registered by `install`.
        unsafe { SetConsoleCtrlHandler(Some(on_interrupt), FALSE) };
    }
}

/// `YYYYMMDD-HHMMSS` in UTC, for default recording and PDF names.
fn utc_stamp(time: SystemTime) -> String {
    let secs = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use anytopdf_builtin::{BuiltinOptions, OcrMode};
    use std::path::PathBuf;

    fn screen_args(extra: &[&str]) -> ScreenArgs {
        let argv = ["anytopdf", "capture", "screen"].iter().chain(extra);
        match crate::cli::try_parse_from(argv).unwrap().command {
            Commands::Capture(crate::cli::CaptureCommand::Screen(args)) => *args,
            other => panic!("parsed {other:?}"),
        }
    }

    #[test]
    fn utc_stamp_formats_civil_time() {
        let at = |s: u64| utc_stamp(SystemTime::UNIX_EPOCH + Duration::from_secs(s));
        assert_eq!(at(0), "19700101-000000");
        assert_eq!(at(1_700_000_000), "20231114-221320");
        assert_eq!(at(951_782_400), "20000229-000000");
    }

    fn parse(extra: &[&str], interval_given: bool) -> (crate::cli::ConvertArgs, BuiltinOptions) {
        let args = screen_args(extra);
        let (parsed, options) = convert_args(
            &args,
            Path::new("rec.mkv"),
            Path::new("o.pdf"),
            interval_given,
            &Resolved::defaults(),
        )
        .unwrap();
        (parsed, BuiltinOptions::from_tables(&options).unwrap())
    }

    #[test]
    fn capture_interval_becomes_the_video_interval_unless_overridden() {
        let (parsed, options) = parse(&["--interval", "2.5", "--", "--ocr", "off"], false);
        assert_eq!(options.video.interval, 2.5);
        assert_eq!(options.ocr.mode, OcrMode::Off);
        assert_eq!(parsed.inputs, [PathBuf::from("rec.mkv")]);
        assert_eq!(parsed.output, Some(PathBuf::from("o.pdf")));
        let (_, options) = parse(&["--", "--video-interval", "9"], true);
        assert_eq!(options.video.interval, 9.0);
    }

    #[test]
    fn bad_convert_options_fail_before_recording() {
        for extra in [
            &["--", "--no-such-flag"][..],
            &["--", "--video-interval=-1"],
        ] {
            let args = screen_args(extra);
            let err = convert_args(
                &args,
                Path::new("rec.mkv"),
                Path::new("o.pdf"),
                true,
                &Resolved::defaults(),
            )
            .unwrap_err();
            assert_eq!(err.class, ExitClass::Usage, "{extra:?}");
        }
    }

    #[test]
    fn invalid_timing_is_a_usage_error() {
        for extra in [
            &["--interval", "0"][..],
            &["--framerate", "0"],
            &["--framerate", "120"],
            &["--duration=-1"],
            &["--duration", "NaN"],
        ] {
            let err = validate(&screen_args(extra)).unwrap_err();
            assert_eq!(err.class, ExitClass::Usage, "{extra:?}");
        }
        assert!(validate(&screen_args(&["--duration", "0.5"])).is_ok());
    }
}
