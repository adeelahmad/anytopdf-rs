//! File-backed conversion queue: a watched inbox, job records moved between state
//! directories by atomic renames, and a durable outbox of signed webhooks.

mod serve;
mod time;
mod webhook;
mod worker;

use crate::cli::{Cli, Commands, QueueCommand, QueueServeArgs};
use crate::exit::{CliError, ExitClass, fail, tag};
use anyhow::{Context, Result};
use anytopdf_core::atomic_write;
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(crate) use time::{now_secs, rfc3339};
pub(crate) use webhook::{Webhooks, new_secret};
pub(crate) use worker::{WorkerConfig, work};

pub(crate) const JOB_SCHEMA: &str = "anytopdf.job/1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl JobState {
    pub(crate) const ALL: [JobState; 4] = [
        JobState::Queued,
        JobState::Running,
        JobState::Succeeded,
        JobState::Failed,
    ];

    fn dir(self) -> &'static str {
        match self {
            JobState::Queued => "pending",
            JobState::Running => "running",
            JobState::Succeeded => "done",
            JobState::Failed => "failed",
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Succeeded => "succeeded",
            JobState::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Origin {
    Inbox,
    Cli,
    Http,
    Imap,
}

/// One conversion job (`anytopdf.job/1`). Optional fields are omitted, never null.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Job {
    pub(crate) schema_version: String,
    pub(crate) id: String,
    pub(crate) state: JobState,
    pub(crate) origin: Origin,
    pub(crate) inputs: Vec<PathBuf>,
    pub(crate) convert_args: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) lease_expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) output: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exit_code: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pages: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

impl Job {
    pub(crate) fn new(
        origin: Origin,
        inputs: Vec<PathBuf>,
        convert_args: Vec<String>,
        cwd: PathBuf,
    ) -> Job {
        Job {
            schema_version: JOB_SCHEMA.into(),
            // UUIDv7 ids sort by creation time, so the queue is FIFO by file name.
            id: new_job_id(),
            state: JobState::Queued,
            origin,
            inputs,
            convert_args,
            cwd,
            created_at: rfc3339(now_secs()),
            started_at: None,
            lease_expires_at: None,
            finished_at: None,
            output: None,
            status: None,
            exit_code: None,
            pages: None,
            error: None,
        }
    }
}

pub(crate) fn new_job_id() -> String {
    format!("job_{}", uuid::Uuid::now_v7().simple())
}

/// A queue directory. Every state change is a rename or an atomic write inside it.
#[derive(Clone, Debug)]
pub(crate) struct Queue {
    root: PathBuf,
}

impl Queue {
    pub(crate) fn open(root: &Path) -> Result<Queue> {
        let root = std::path::absolute(root)
            .with_context(|| format!("invalid queue path: {}", root.display()))?;
        let queue = Queue { root };
        let mut dirs = vec![
            queue.inbox(),
            queue.root.join("work"),
            queue.outbox(),
            queue.root.join("webhooks/pending"),
            queue.root.join("webhooks/failed"),
        ];
        dirs.extend(JobState::ALL.map(|s| queue.state_dir(s)));
        for dir in dirs {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("create queue directory {}", dir.display()))?;
        }
        Ok(queue)
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn inbox(&self) -> PathBuf {
        self.root.join("inbox")
    }

    pub(crate) fn outbox(&self) -> PathBuf {
        self.root.join("outbox")
    }

    pub(crate) fn work_dir(&self, id: &str) -> PathBuf {
        self.root.join("work").join(id)
    }

    pub(crate) fn webhooks_dir(&self, name: &str) -> PathBuf {
        self.root.join("webhooks").join(name)
    }

    fn state_dir(&self, state: JobState) -> PathBuf {
        self.root.join("jobs").join(state.dir())
    }

    fn job_path(&self, state: JobState, id: &str) -> PathBuf {
        self.state_dir(state).join(format!("{id}.json"))
    }

    fn write(&self, job: &Job) -> Result<()> {
        atomic_write(
            &self.job_path(job.state, &job.id),
            &serde_json::to_vec_pretty(job)?,
        )
    }

    pub(crate) fn enqueue(&self, job: &Job) -> Result<()> {
        anyhow::ensure!(
            job.state == JobState::Queued,
            "only queued jobs are enqueued"
        );
        self.write(job)
    }

    /// Move a queued job to running. `None` means another worker claimed it first.
    pub(crate) fn claim(&self, id: &str, lease_secs: u64) -> Result<Option<Job>> {
        let from = self.job_path(JobState::Queued, id);
        let to = self.job_path(JobState::Running, id);
        if std::fs::rename(&from, &to).is_err() {
            return Ok(None);
        }
        let mut job = read_job(&to)?;
        let now = now_secs();
        job.state = JobState::Running;
        job.started_at = Some(rfc3339(now));
        job.lease_expires_at = Some(now + lease_secs);
        self.write(&job)?;
        Ok(Some(job))
    }

    /// Record a terminal state and drop the running record.
    pub(crate) fn finish(&self, job: &mut Job, state: JobState) -> Result<()> {
        job.state = state;
        job.lease_expires_at = None;
        job.finished_at = Some(rfc3339(now_secs()));
        self.write(job)?;
        let running = self.job_path(JobState::Running, &job.id);
        std::fs::remove_file(&running).with_context(|| format!("remove {}", running.display()))
    }

    /// Jobs in one state, oldest first. Unreadable records are skipped.
    pub(crate) fn list(&self, state: JobState) -> Result<Vec<Job>> {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(self.state_dir(state))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        Ok(paths.iter().filter_map(|p| read_job(p).ok()).collect())
    }

    /// The record for `id` in whichever state it is in.
    pub(crate) fn find(&self, id: &str) -> Result<Option<Job>> {
        // A job can move between state directories while we look; retry once.
        for _ in 0..2 {
            for state in JobState::ALL {
                let path = self.job_path(state, id);
                if path.is_file()
                    && let Ok(job) = read_job(&path)
                {
                    return Ok(Some(job));
                }
            }
        }
        Ok(None)
    }

    /// Return running jobs whose lease expired (their worker died) to the queue.
    pub(crate) fn requeue_expired(&self, now: u64) -> Result<Vec<Job>> {
        let mut requeued = Vec::new();
        for job in self.list(JobState::Running)? {
            if job.lease_expires_at.is_some_and(|lease| lease >= now) {
                continue;
            }
            let running = self.job_path(JobState::Running, &job.id);
            let mut job = job;
            job.state = JobState::Queued;
            job.started_at = None;
            job.lease_expires_at = None;
            self.write(&job)?;
            std::fs::remove_file(&running)?;
            requeued.push(job);
        }
        Ok(requeued)
    }

    /// Path relative to the queue root with forward slashes, for path-free payloads.
    pub(crate) fn relative(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.root).ok()?;
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        Some(parts.join("/"))
    }
}

fn read_job(path: &Path) -> Result<Job> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse job {}", path.display()))
}

/// Options the worker sets itself; jobs may not override them.
const RESERVED: &str =
    "-o/--output, --output-dir, --events, --json and --dump-graph are set by the queue worker";

/// Validate convert options with the real `convert` parser.
pub(crate) fn check_convert_args(args: &[String]) -> Result<(), CliError> {
    let mut argv = vec!["anytopdf".to_string(), "convert".to_string()];
    argv.extend(args.iter().cloned());
    argv.push("--".into());
    argv.push("placeholder-input".into());
    let cli = tag(
        ExitClass::Usage,
        Cli::try_parse_from(&argv).map_err(|e| anyhow::anyhow!("invalid convert options: {e}")),
    )?;
    let Commands::Convert(convert) = cli.command else {
        return Err(fail(ExitClass::Usage, "invalid convert options"));
    };
    if convert.output.is_some()
        || convert.output_dir.is_some()
        || convert.events
        || convert.json
        || convert.dump_graph.is_some()
    {
        return Err(fail(ExitClass::Usage, RESERVED));
    }
    if !convert
        .inputs
        .iter()
        .all(|i| i == Path::new("placeholder-input"))
    {
        return Err(fail(
            ExitClass::Usage,
            "convert options after `--` may not name inputs; list inputs before `--`",
        ));
    }
    Ok(())
}

pub(crate) fn run(command: QueueCommand, global_args: Vec<String>) -> Result<(), CliError> {
    match command {
        QueueCommand::Add {
            queue,
            inputs,
            convert,
        } => add(&queue, &inputs, convert),
        QueueCommand::Work(args) => {
            check_convert_args(&args.convert)?;
            if !args.poll_interval.is_finite() || args.poll_interval < 0.05 {
                return Err(fail(
                    ExitClass::Usage,
                    "--poll-interval must be at least 0.05 seconds",
                ));
            }
            let hooks = if args.webhook.is_empty() {
                None
            } else {
                let secret = std::env::var("ANYTOPDF_WEBHOOK_SECRET").map_err(|_| {
                    fail(
                        ExitClass::Usage,
                        "--webhook requires ANYTOPDF_WEBHOOK_SECRET (create one with `anytopdf queue secret`)",
                    )
                })?;
                Some(tag(
                    ExitClass::Usage,
                    Webhooks::new(&args.webhook, &secret),
                )?)
            };
            let queue = tag(ExitClass::Usage, Queue::open(&args.queue))?;
            Ok(work(WorkerConfig {
                queue,
                exe: std::env::current_exe().context("locate the anytopdf executable")?,
                global_args,
                inbox_args: args.convert,
                poll: std::time::Duration::from_secs_f64(args.poll_interval),
                job_timeout: args.job_timeout,
                once: args.once,
                quiet: args.quiet,
                hooks,
            })?)
        }
        QueueCommand::Serve(args) => serve_command(args),
        QueueCommand::Status { queue } => Ok(status(&queue)?),
        QueueCommand::Secret => {
            println!("{}", new_secret());
            Ok(())
        }
    }
}

fn serve_command(args: QueueServeArgs) -> Result<(), CliError> {
    check_convert_args(&args.convert)?;
    let token = std::env::var("ANYTOPDF_QUEUE_TOKEN").unwrap_or_default();
    if token.trim().len() < serve::MIN_TOKEN_LEN {
        return Err(fail(
            ExitClass::Usage,
            format!(
                "queue serve requires ANYTOPDF_QUEUE_TOKEN of at least {} characters (create one with `anytopdf queue secret`)",
                serve::MIN_TOKEN_LEN
            ),
        ));
    }
    if let Err(problem) =
        serve::check_exposure(args.listen, args.tls_cert.is_some(), args.allow_public_bind)
    {
        return Err(fail(
            ExitClass::Usage,
            format!("refusing to start the upload server: {problem}"),
        ));
    }
    let tls = match (&args.tls_cert, &args.tls_key) {
        (Some(cert), Some(key)) => {
            Some(tag(ExitClass::Input, anytopdf_print::load_tls(cert, key))?)
        }
        _ => None,
    };
    let queue = tag(ExitClass::Usage, Queue::open(&args.queue))?;
    let listener = tag(ExitClass::Provider, serve::bind(args.listen))?;
    let scheme = if tls.is_some() { "https" } else { "http" };
    eprintln!(
        "queue serve: listening on {scheme}://{}/v1/jobs",
        listener.local_addr()?
    );
    Ok(serve::serve(
        listener,
        serve::ServeConfig {
            queue,
            token: token.trim().as_bytes().to_vec(),
            max_upload: args.max_upload_mb.saturating_mul(1024 * 1024),
            convert_args: args.convert,
            tls,
            max_connections: usize::from(args.max_connections),
            quiet: args.quiet,
        },
    )?)
}

fn add(queue: &Path, inputs: &[PathBuf], convert: Vec<String>) -> Result<(), CliError> {
    check_convert_args(&convert)?;
    let mut absolute = Vec::new();
    for input in inputs {
        let path = tag(
            ExitClass::Input,
            input
                .canonicalize()
                .with_context(|| format!("input not found: {}", input.display())),
        )?;
        absolute.push(path);
    }
    let queue = tag(ExitClass::Usage, Queue::open(queue))?;
    let job = Job::new(Origin::Cli, absolute, convert, std::env::current_dir()?);
    queue.enqueue(&job)?;
    println!("{}", job.id);
    Ok(())
}

fn status(queue: &Path) -> Result<()> {
    anyhow::ensure!(
        queue.join("jobs").is_dir(),
        "not a queue directory: {}",
        queue.display()
    );
    let queue = Queue::open(queue)?;
    for state in JobState::ALL {
        for job in queue.list(state)? {
            let names: Vec<String> = job
                .inputs
                .iter()
                .map(|p| anytopdf_core::basename(p))
                .collect();
            let mut line = format!("{}  {:<9}  {}", job.id, state.as_str(), names.join(", "));
            if let Some(output) = job.output.as_deref().and_then(|p| queue.relative(p)) {
                line.push_str(&format!("  -> {output}"));
            }
            if let Some(code) = job.exit_code {
                line.push_str(&format!("  exit {code}"));
            }
            println!("{line}");
        }
    }
    let count = |name: &str| {
        std::fs::read_dir(queue.webhooks_dir(name)).map_or(0, |entries| entries.count())
    };
    println!(
        "webhooks: {} pending, {} failed",
        count("pending"),
        count("failed")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(dir: &Path) -> Job {
        Job::new(
            Origin::Cli,
            vec![dir.join("a.txt")],
            vec!["--ocr".into(), "off".into()],
            dir.to_path_buf(),
        )
    }

    #[test]
    fn claim_moves_a_job_once_and_finish_records_the_terminal_state() {
        let dir = tempfile::tempdir().unwrap();
        let queue = Queue::open(&dir.path().join("q")).unwrap();
        let mut queued = job(dir.path());
        queue.enqueue(&queued).unwrap();
        assert_eq!(queue.list(JobState::Queued).unwrap(), vec![queued.clone()]);

        let mut running = queue.claim(&queued.id, 60).unwrap().expect("claimed");
        assert_eq!(running.state, JobState::Running);
        assert!(running.lease_expires_at.is_some());
        assert!(queue.claim(&queued.id, 60).unwrap().is_none());
        assert!(queue.list(JobState::Queued).unwrap().is_empty());

        queue.finish(&mut running, JobState::Succeeded).unwrap();
        assert!(queue.list(JobState::Running).unwrap().is_empty());
        let done = queue.list(JobState::Succeeded).unwrap();
        assert_eq!(done.len(), 1);
        assert!(done[0].finished_at.is_some());
        assert!(done[0].lease_expires_at.is_none());
        queued.state = JobState::Succeeded;
        assert_eq!(done[0].id, queued.id);
    }

    #[test]
    fn expired_leases_return_jobs_to_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let queue = Queue::open(&dir.path().join("q")).unwrap();
        let fresh = job(dir.path());
        let stale = job(dir.path());
        queue.enqueue(&fresh).unwrap();
        queue.enqueue(&stale).unwrap();
        queue.claim(&fresh.id, 3600).unwrap().unwrap();
        queue.claim(&stale.id, 0).unwrap().unwrap();

        let requeued = queue.requeue_expired(now_secs() + 1).unwrap();
        assert_eq!(requeued.len(), 1);
        assert_eq!(requeued[0].id, stale.id);
        assert_eq!(requeued[0].state, JobState::Queued);
        assert!(requeued[0].started_at.is_none());
        let running = queue.list(JobState::Running).unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, fresh.id);
    }

    #[test]
    fn job_ids_sort_in_creation_order() {
        let ids: Vec<String> = (0..50).map(|_| new_job_id()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn convert_options_reject_worker_owned_flags_and_inputs() {
        assert!(check_convert_args(&[]).is_ok());
        assert!(
            check_convert_args(&[
                "--ocr".into(),
                "off".into(),
                "--profile".into(),
                "share".into()
            ])
            .is_ok()
        );
        for bad in [
            &["-o", "x.pdf"][..],
            &["--output-dir", "out"],
            &["--events"],
            &["--json"],
            &["--dump-graph", "g.json"],
            &["--no-such-flag"],
            &["extra-input.txt"],
        ] {
            let args: Vec<String> = bad.iter().map(|s| s.to_string()).collect();
            let err = check_convert_args(&args).unwrap_err();
            assert_eq!(err.class, ExitClass::Usage, "{bad:?}");
        }
    }

    #[test]
    fn relative_paths_use_forward_slashes_and_stay_inside_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let queue = Queue::open(&dir.path().join("q")).unwrap();
        assert_eq!(
            queue.relative(&queue.outbox().join("a.pdf")).as_deref(),
            Some("outbox/a.pdf")
        );
        assert_eq!(queue.relative(dir.path()), None);
    }
}
