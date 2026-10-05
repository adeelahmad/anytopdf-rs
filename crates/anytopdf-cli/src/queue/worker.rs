use super::{Job, JobState, Origin, Queue, Webhooks, now_secs};
use crate::naming;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

/// Extra lease time beyond `--job-timeout` before another worker may requeue a job.
const LEASE_GRACE_SECS: u64 = 60;
/// How often a running conversion is checked while webhooks are delivered.
const CHILD_POLL: Duration = Duration::from_millis(100);
/// Partial-download names that are never claimed from the inbox.
const IGNORED_SUFFIXES: [&str; 5] = [".part", ".tmp", ".crdownload", ".download", ".partial"];

pub(crate) struct WorkerConfig {
    pub(crate) queue: Queue,
    pub(crate) exe: PathBuf,
    pub(crate) global_args: Vec<String>,
    pub(crate) inbox_args: Vec<String>,
    pub(crate) poll: Duration,
    pub(crate) job_timeout: u64,
    pub(crate) once: bool,
    pub(crate) quiet: bool,
    pub(crate) hooks: Option<Webhooks>,
}

impl WorkerConfig {
    fn log(&self, line: &str) {
        if !self.quiet {
            eprintln!("{line}");
        }
    }

    fn deliver(&self) {
        if let Some(hooks) = &self.hooks {
            match hooks.deliver_due(&self.queue) {
                Ok(lines) => lines.iter().for_each(|l| self.log(l)),
                Err(e) => self.log(&format!("webhook delivery error: {e:#}")),
            }
        }
    }

    fn notify(&self, event: &str, job: &Job) {
        if let Some(hooks) = &self.hooks
            && let Err(e) = hooks.enqueue(&self.queue, event, job)
        {
            self.log(&format!("could not queue webhook {event}: {e:#}"));
        }
    }
}

/// Inbox candidates waiting to prove they are no longer being written.
type Seen = HashMap<PathBuf, (u64, Option<SystemTime>)>;

pub(crate) fn work(cfg: WorkerConfig) -> Result<()> {
    if let Some(hooks) = &cfg.hooks {
        for url in hooks.plain_http() {
            cfg.log(&format!("warning: webhook {url} is not HTTPS"));
        }
    }
    cfg.log(&format!(
        "watching {} (inbox {})",
        cfg.queue.root().display(),
        cfg.queue.inbox().display()
    ));
    let mut seen = Seen::new();
    loop {
        for job in cfg.queue.requeue_expired(now_secs())? {
            cfg.log(&format!("{} requeued after an expired lease", job.id));
        }
        let unsettled = scan_inbox(&cfg, &mut seen)?;
        cfg.deliver();
        if let Some(job) = cfg.queue.list(JobState::Queued)?.into_iter().next() {
            run_next(&cfg, &job.id)?;
            continue;
        }
        if cfg.once && unsettled == 0 {
            break;
        }
        std::thread::sleep(cfg.poll);
    }
    cfg.deliver();
    Ok(())
}

fn ignored(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    name.starts_with('.') || IGNORED_SUFFIXES.iter().any(|s| lower.ends_with(s))
}

/// Claim inbox files whose size and mtime held still since the previous scan.
/// Returns how many candidates are still settling.
fn scan_inbox(cfg: &WorkerConfig, seen: &mut Seen) -> Result<usize> {
    let mut current = Seen::new();
    for entry in std::fs::read_dir(cfg.queue.inbox())? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || ignored(&name) {
            continue;
        }
        current.insert(entry.path(), (meta.len(), meta.modified().ok()));
    }
    let mut unsettled = 0;
    let mut stable: Vec<PathBuf> = current
        .iter()
        .filter(|(path, stamp)| seen.get(*path) == Some(stamp))
        .map(|(path, _)| path.clone())
        .collect();
    stable.sort();
    for path in &stable {
        current.remove(path);
        if let Err(e) = claim_inbox_file(cfg, path) {
            cfg.log(&format!("could not claim {}: {e:#}", path.display()));
        }
    }
    unsettled += current.len();
    *seen = current;
    Ok(unsettled)
}

fn claim_inbox_file(cfg: &WorkerConfig, path: &Path) -> Result<()> {
    let name = path.file_name().context("inbox entry has no name")?;
    let cwd = std::env::current_dir()?;
    let mut job = Job::new(Origin::Inbox, Vec::new(), cfg.inbox_args.clone(), cwd);
    let input_dir = cfg.queue.work_dir(&job.id).join("input");
    std::fs::create_dir_all(&input_dir)?;
    let claimed = input_dir.join(name);
    if std::fs::rename(path, &claimed).is_err() {
        // Another worker took it first.
        let _ = std::fs::remove_dir_all(cfg.queue.work_dir(&job.id));
        return Ok(());
    }
    job.inputs = vec![claimed];
    cfg.queue.enqueue(&job)?;
    cfg.log(&format!(
        "{} queued from inbox: {}",
        job.id,
        name.to_string_lossy()
    ));
    Ok(())
}

fn run_next(cfg: &WorkerConfig, id: &str) -> Result<()> {
    let lease = cfg.job_timeout + LEASE_GRACE_SECS;
    let Some(mut job) = cfg.queue.claim(id, lease)? else {
        return Ok(());
    };
    cfg.notify("job.received", &job);
    cfg.log(&format!("{} started", job.id));
    let state = match execute(cfg, &mut job) {
        Ok(()) if job.exit_code == Some(0) => JobState::Succeeded,
        Ok(()) => JobState::Failed,
        Err(e) => {
            job.error = Some(format!("{e:#}"));
            JobState::Failed
        }
    };
    cfg.queue.finish(&mut job, state)?;
    match state {
        JobState::Succeeded => {
            cfg.notify("job.completed", &job);
            let output = job.output.as_deref().and_then(|p| cfg.queue.relative(p));
            cfg.log(&format!(
                "{} completed: {} ({} pages)",
                job.id,
                output.unwrap_or_default(),
                job.pages.unwrap_or(0)
            ));
        }
        _ => {
            cfg.notify("job.failed", &job);
            cfg.log(&format!(
                "{} failed: {}",
                job.id,
                job.error.as_deref().unwrap_or("conversion failed")
            ));
        }
    }
    cfg.deliver();
    Ok(())
}

/// Run `anytopdf convert --events --json` for one job and record its outcome.
fn execute(cfg: &WorkerConfig, job: &mut Job) -> Result<()> {
    let work = cfg.queue.work_dir(&job.id);
    std::fs::create_dir_all(&work)?;
    let base = naming::default_output(&job.inputs, &cfg.queue.outbox());
    let output = naming::next_free_path(&base, &[])?;
    job.output = Some(output.clone());

    let events = File::create(work.join("events.ndjson"))?;
    let result = File::create(work.join("convert.json"))?;
    let mut command = Command::new(&cfg.exe);
    command
        .args(&cfg.global_args)
        .arg("convert")
        .args(&job.convert_args)
        .args(["--events", "--json", "-o"])
        .arg(&output)
        .arg("--")
        .args(&job.inputs)
        .current_dir(&job.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(result))
        .stderr(Stdio::from(events));
    let mut child = command
        .spawn()
        .with_context(|| format!("start {}", cfg.exe.display()))?;
    let deadline = Instant::now() + Duration::from_secs(cfg.job_timeout);
    let exit = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            job.output = None;
            anyhow::bail!("conversion timed out after {}s", cfg.job_timeout);
        }
        cfg.deliver();
        std::thread::sleep(CHILD_POLL);
    };

    let report: serde_json::Value = std::fs::read(work.join("convert.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    job.exit_code = exit.code().and_then(|c| u8::try_from(c).ok()).or_else(|| {
        report["exit_code"]
            .as_u64()
            .and_then(|c| u8::try_from(c).ok())
    });
    job.status = report["status"].as_str().map(str::to_owned);
    job.pages = report["outputs"][0]["pages"].as_u64().map(|p| p as usize);
    if let Some(error) = report["error"].as_str() {
        job.error = Some(error.to_owned());
    }
    if job.exit_code != Some(0) {
        job.output = None;
        if job.error.is_none() {
            job.error = Some(format!("convert exited with {exit}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_downloads_and_dotfiles_are_ignored() {
        for name in [
            ".DS_Store",
            "scan.pdf.part",
            "a.TMP",
            "b.crdownload",
            ".~lock",
        ] {
            assert!(ignored(name), "{name}");
        }
        for name in ["scan.jpg", "notes.txt", "partial.txt"] {
            assert!(!ignored(name), "{name}");
        }
    }
}
