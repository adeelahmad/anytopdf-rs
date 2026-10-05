use crate::state::WatchState;
use anyhow::{Context, Result};
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

/// What the server reports when the mailbox is selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MailboxStatus {
    pub uid_validity: u32,
    pub uid_next: Option<u32>,
}

/// The mailbox operations the watcher needs. [`crate::ImapMailbox`] implements it over
/// IMAP; tests use an in-memory mailbox.
pub trait Mailbox {
    /// Select (or re-select) the watched mailbox.
    fn status(&mut self) -> Result<MailboxStatus>;
    /// UIDs strictly greater than `last_uid` that match the configured search, ascending.
    fn uids_after(&mut self, last_uid: u32) -> Result<Vec<u32>>;
    /// RFC 822 size of a message, or `None` when it no longer exists.
    fn size(&mut self, uid: u32) -> Result<Option<u64>>;
    /// Raw message bytes without setting `\Seen`, or `None` when it no longer exists.
    fn fetch(&mut self, uid: u32) -> Result<Option<Vec<u8>>>;
    fn mark_seen(&mut self, uid: u32) -> Result<()>;
    fn move_to(&mut self, uid: u32, folder: &str) -> Result<()>;
    /// Block until the mailbox may have changed or `timeout` elapses.
    fn wait(&mut self, timeout: Duration) -> Result<()>;
}

/// A fetched message written to the spool as a raw `.eml` file.
#[derive(Clone, Debug)]
pub struct SpooledMessage {
    pub mailbox: String,
    pub uid_validity: u32,
    pub uid: u32,
    /// File-name stem shared by the spooled message and its outputs.
    pub stem: String,
    pub path: PathBuf,
    pub size: u64,
}

/// A successful hand-off. `output` is set when the sink produced a file directly.
#[derive(Clone, Debug, Default)]
pub struct Delivery {
    pub output: Option<PathBuf>,
}

/// Receives each spooled message. Returning `Ok` means the watcher may forget the
/// message: it was converted, queued, or otherwise taken over by the sink.
pub trait MessageSink {
    fn deliver(&mut self, message: &SpooledMessage) -> Result<Delivery>;
}

#[derive(Clone, Debug)]
pub struct WatchOptions {
    pub state_dir: PathBuf,
    /// On first run (or after a UIDVALIDITY change), process existing messages too.
    pub backfill: bool,
    pub max_message_bytes: u64,
    /// Delivery attempts before a message is parked in `<state-dir>/failed`.
    pub max_attempts: u32,
    pub mark_seen: bool,
    pub move_to: Option<String>,
    /// Keep delivered `.eml` files in `<state-dir>/spool` instead of deleting them.
    pub keep_eml: bool,
    pub quiet: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PassReport {
    pub delivered: Vec<u32>,
    /// Failed this pass and scheduled for another attempt.
    pub failed: Vec<u32>,
    /// Given up on: over the size limit or out of attempts.
    pub parked: Vec<u32>,
    /// Gone from the server before they could be fetched.
    pub vanished: Vec<u32>,
}

pub struct Watcher {
    host: String,
    user: String,
    mailbox: String,
    opts: WatchOptions,
}

impl Watcher {
    pub fn new(host: &str, user: &str, mailbox: &str, opts: WatchOptions) -> Self {
        Watcher {
            host: host.into(),
            user: user.into(),
            mailbox: mailbox.into(),
            opts,
        }
    }

    pub fn state_path(&self) -> PathBuf {
        self.opts.state_dir.join("state.json")
    }

    fn log(&self, message: &str) {
        if !self.opts.quiet {
            eprintln!("imap: {message}");
        }
    }

    /// Process everything new (and everything due for retry) once.
    pub fn poll_once(
        &self,
        mailbox: &mut dyn Mailbox,
        sink: &mut dyn MessageSink,
    ) -> Result<PassReport> {
        let status = mailbox.status()?;
        let path = self.state_path();
        let previous = local(WatchState::load(&path))?;
        if let Some(state) = &previous {
            local(state.check_identity(&self.host, &self.user, &self.mailbox))?;
        }
        let mut state = match previous {
            Some(state) if state.uid_validity == status.uid_validity => state,
            previous => {
                if let Some(old) = previous {
                    self.log(&format!(
                        "UIDVALIDITY changed from {} to {}; earlier progress no longer applies",
                        old.uid_validity, status.uid_validity
                    ));
                }
                let start = if self.opts.backfill {
                    0
                } else {
                    match status.uid_next {
                        Some(next) => next.saturating_sub(1),
                        None => mailbox.uids_after(0)?.into_iter().max().unwrap_or(0),
                    }
                };
                let state = WatchState::new(
                    &self.host,
                    &self.user,
                    &self.mailbox,
                    status.uid_validity,
                    start,
                );
                local(state.save(&path))?;
                state
            }
        };

        let mut queue: Vec<u32> = state.retry.keys().copied().collect();
        queue.extend(
            mailbox
                .uids_after(state.last_uid)?
                .into_iter()
                .filter(|uid| *uid > state.last_uid),
        );
        queue.sort_unstable();
        queue.dedup();

        let mut report = PassReport::default();
        for uid in queue {
            self.process(uid, &mut state, mailbox, sink, &mut report)?;
            state.last_uid = state.last_uid.max(uid);
            local(state.save(&path))?;
        }
        Ok(report)
    }

    fn process(
        &self,
        uid: u32,
        state: &mut WatchState,
        mailbox: &mut dyn Mailbox,
        sink: &mut dyn MessageSink,
        report: &mut PassReport,
    ) -> Result<()> {
        let max = self.opts.max_message_bytes;
        let size = match mailbox.size(uid)? {
            None => {
                self.vanished(uid, state, report);
                return Ok(());
            }
            Some(size) => size,
        };
        if size > max {
            self.log(&format!(
                "uid {uid}: {size} bytes exceeds --max-message-bytes {max}; skipped"
            ));
            Self::park(uid, state, report);
            return Ok(());
        }
        let bytes = match mailbox.fetch(uid)? {
            None => {
                self.vanished(uid, state, report);
                return Ok(());
            }
            Some(bytes) => bytes,
        };
        if bytes.len() as u64 > max {
            self.log(&format!(
                "uid {uid}: {} bytes exceeds --max-message-bytes {max}; skipped",
                bytes.len()
            ));
            Self::park(uid, state, report);
            return Ok(());
        }
        let stem = message_stem(&self.mailbox, state.uid_validity, uid);
        let spool = self.opts.state_dir.join("spool");
        let path = spool.join(format!("{stem}.eml"));
        write_atomic(&path, &bytes)?;
        let message = SpooledMessage {
            mailbox: self.mailbox.clone(),
            uid_validity: state.uid_validity,
            uid,
            stem: stem.clone(),
            path: path.clone(),
            size: bytes.len() as u64,
        };
        match sink.deliver(&message) {
            Ok(delivery) => {
                state.retry.remove(&uid);
                report.delivered.push(uid);
                match &delivery.output {
                    Some(out) => self.log(&format!("uid {uid}: wrote {}", out.display())),
                    None => self.log(&format!("uid {uid}: delivered")),
                }
                if self.opts.mark_seen
                    && let Err(e) = mailbox.mark_seen(uid)
                {
                    self.log(&format!("uid {uid}: could not mark as seen: {e:#}"));
                }
                if let Some(folder) = &self.opts.move_to
                    && let Err(e) = mailbox.move_to(uid, folder)
                {
                    self.log(&format!("uid {uid}: could not move to {folder:?}: {e:#}"));
                }
                if !self.opts.keep_eml {
                    let _ = std::fs::remove_file(&path);
                }
            }
            Err(e) => {
                let attempts = state.retry.get(&uid).copied().unwrap_or(0) + 1;
                if attempts >= self.opts.max_attempts {
                    let failed = self.opts.state_dir.join("failed");
                    let parked = failed.join(format!("{stem}.eml"));
                    local(
                        std::fs::create_dir_all(&failed)
                            .and_then(|()| std::fs::rename(&path, &parked))
                            .with_context(|| {
                                format!("move {} to {}", path.display(), parked.display())
                            }),
                    )?;
                    self.log(&format!(
                        "uid {uid}: attempt {attempts} failed, giving up; kept {}: {e:#}",
                        parked.display()
                    ));
                    Self::park(uid, state, report);
                } else {
                    state.retry.insert(uid, attempts);
                    report.failed.push(uid);
                    self.log(&format!(
                        "uid {uid}: attempt {attempts} of {} failed: {e:#}",
                        self.opts.max_attempts
                    ));
                }
            }
        }
        Ok(())
    }

    fn park(uid: u32, state: &mut WatchState, report: &mut PassReport) {
        state.retry.remove(&uid);
        state.parked.insert(uid);
        report.parked.push(uid);
    }

    fn vanished(&self, uid: u32, state: &mut WatchState, report: &mut PassReport) {
        self.log(&format!("uid {uid}: no longer on the server"));
        state.retry.remove(&uid);
        report.vanished.push(uid);
    }

    /// Watch until an unrecoverable error. Connection failures are retried with
    /// exponential backoff; state, spool and sink setup errors end the loop.
    pub fn run(
        &self,
        connect: &mut dyn FnMut() -> Result<Box<dyn Mailbox>>,
        sink: &mut dyn MessageSink,
        interval: Duration,
    ) -> Result<()> {
        const MAX_BACKOFF: Duration = Duration::from_secs(300);
        let mut backoff = Duration::from_secs(5);
        loop {
            let result: Result<()> = connect().and_then(|mut mailbox| {
                self.log(&format!("watching {:?} on {}", self.mailbox, self.host));
                loop {
                    self.poll_once(mailbox.as_mut(), sink)?;
                    backoff = Duration::from_secs(5);
                    mailbox.wait(interval)?;
                }
            });
            if let Err(e) = result {
                if is_local_error(&e) {
                    return Err(e);
                }
                self.log(&format!("{e:#}; reconnecting in {}s", backoff.as_secs()));
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

/// Errors from the state directory will not heal by reconnecting.
fn is_local_error(e: &anyhow::Error) -> bool {
    e.downcast_ref::<LocalError>().is_some()
}

fn local<T>(result: Result<T>) -> Result<T> {
    result.context(LocalError)
}

#[derive(Debug)]
struct LocalError;

impl std::fmt::Display for LocalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("watcher state directory")
    }
}

impl std::error::Error for LocalError {}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let write = || -> Result<()> {
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        tmp.write_all(bytes)?;
        tmp.persist(path)
            .with_context(|| format!("write {}", path.display()))?;
        Ok(())
    };
    local(write())
}

/// `<mailbox>-<uidvalidity>-<uid>` with the mailbox reduced to portable file-name
/// characters, e.g. `INBOX/Scans` becomes `INBOX_Scans`.
pub fn message_stem(mailbox: &str, uid_validity: u32, uid: u32) -> String {
    let mut slug: String = mailbox
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if slug.starts_with('.') || slug.is_empty() {
        slug.insert(0, '_');
    }
    format!("{slug}-{uid_validity}-{uid}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct FakeMailbox {
        uid_validity: u32,
        messages: BTreeMap<u32, Vec<u8>>,
        seen: Vec<u32>,
        moved: Vec<(u32, String)>,
    }

    impl FakeMailbox {
        fn with(uids: &[u32]) -> Self {
            FakeMailbox {
                uid_validity: 1,
                messages: uids
                    .iter()
                    .map(|u| (*u, format!("Subject: {u}\r\n\r\nbody").into_bytes()))
                    .collect(),
                ..Default::default()
            }
        }
    }

    impl Mailbox for FakeMailbox {
        fn status(&mut self) -> Result<MailboxStatus> {
            Ok(MailboxStatus {
                uid_validity: self.uid_validity,
                uid_next: Some(self.messages.keys().max().map_or(1, |m| m + 1)),
            })
        }
        fn uids_after(&mut self, last_uid: u32) -> Result<Vec<u32>> {
            Ok(self
                .messages
                .range(last_uid + 1..)
                .map(|(u, _)| *u)
                .collect())
        }
        fn size(&mut self, uid: u32) -> Result<Option<u64>> {
            Ok(self.messages.get(&uid).map(|m| m.len() as u64))
        }
        fn fetch(&mut self, uid: u32) -> Result<Option<Vec<u8>>> {
            Ok(self.messages.get(&uid).cloned())
        }
        fn mark_seen(&mut self, uid: u32) -> Result<()> {
            self.seen.push(uid);
            Ok(())
        }
        fn move_to(&mut self, uid: u32, folder: &str) -> Result<()> {
            self.moved.push((uid, folder.into()));
            self.messages.remove(&uid);
            Ok(())
        }
        fn wait(&mut self, _timeout: Duration) -> Result<()> {
            Ok(())
        }
    }

    /// Records deliveries; fails for UIDs listed in `fail`.
    #[derive(Default)]
    struct FakeSink {
        delivered: Vec<(u32, Vec<u8>)>,
        fail: Vec<u32>,
    }

    impl MessageSink for FakeSink {
        fn deliver(&mut self, message: &SpooledMessage) -> Result<Delivery> {
            if self.fail.contains(&message.uid) {
                bail!("no importer for message/rfc822");
            }
            let bytes = std::fs::read(&message.path)?;
            self.delivered.push((message.uid, bytes));
            Ok(Delivery::default())
        }
    }

    fn watcher(dir: &Path, backfill: bool) -> Watcher {
        Watcher::new(
            "imap.example.com",
            "user",
            "INBOX",
            WatchOptions {
                state_dir: dir.to_path_buf(),
                backfill,
                max_message_bytes: 1024,
                max_attempts: 2,
                mark_seen: false,
                move_to: None,
                keep_eml: false,
                quiet: true,
            },
        )
    }

    #[test]
    fn first_run_skips_existing_mail_unless_backfilling() {
        let dir = tempfile::tempdir().unwrap();
        let mut mailbox = FakeMailbox::with(&[1, 2]);
        let mut sink = FakeSink::default();
        let w = watcher(dir.path(), false);
        assert_eq!(
            w.poll_once(&mut mailbox, &mut sink).unwrap(),
            PassReport::default()
        );
        mailbox
            .messages
            .insert(3, b"Subject: 3\r\n\r\nnew".to_vec());
        let report = w.poll_once(&mut mailbox, &mut sink).unwrap();
        assert_eq!(report.delivered, vec![3]);
        assert_eq!(sink.delivered[0].1, b"Subject: 3\r\n\r\nnew");

        let dir = tempfile::tempdir().unwrap();
        let mut sink = FakeSink::default();
        let report = watcher(dir.path(), true)
            .poll_once(&mut mailbox, &mut sink)
            .unwrap();
        assert_eq!(report.delivered, vec![1, 2, 3]);
    }

    #[test]
    fn delivered_messages_are_not_delivered_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut mailbox = FakeMailbox::with(&[5, 9]);
        let mut sink = FakeSink::default();
        let w = watcher(dir.path(), true);
        w.poll_once(&mut mailbox, &mut sink).unwrap();
        let again = w.poll_once(&mut mailbox, &mut sink).unwrap();
        assert_eq!(again, PassReport::default());
        assert_eq!(sink.delivered.len(), 2);
        let state = WatchState::load(&w.state_path()).unwrap().unwrap();
        assert_eq!(state.last_uid, 9);
        assert!(!dir.path().join("spool/INBOX-1-5.eml").exists());
    }

    #[test]
    fn failed_delivery_is_retried_then_parked_with_its_message_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut mailbox = FakeMailbox::with(&[1, 2]);
        let mut sink = FakeSink {
            fail: vec![1],
            ..Default::default()
        };
        let w = watcher(dir.path(), true);
        let first = w.poll_once(&mut mailbox, &mut sink).unwrap();
        assert_eq!((first.delivered, first.failed), (vec![2], vec![1]));
        let second = w.poll_once(&mut mailbox, &mut sink).unwrap();
        assert_eq!(second.parked, vec![1]);
        assert!(dir.path().join("failed/INBOX-1-1.eml").is_file());
        let third = w.poll_once(&mut mailbox, &mut sink).unwrap();
        assert_eq!(third, PassReport::default());
        let state = WatchState::load(&w.state_path()).unwrap().unwrap();
        assert!(state.retry.is_empty());
        assert!(state.parked.contains(&1));
    }

    #[test]
    fn retry_succeeds_once_the_sink_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let mut mailbox = FakeMailbox::with(&[4]);
        let mut sink = FakeSink {
            fail: vec![4],
            ..Default::default()
        };
        let w = watcher(dir.path(), true);
        assert_eq!(
            w.poll_once(&mut mailbox, &mut sink).unwrap().failed,
            vec![4]
        );
        sink.fail.clear();
        assert_eq!(
            w.poll_once(&mut mailbox, &mut sink).unwrap().delivered,
            vec![4]
        );
    }

    #[test]
    fn oversized_messages_are_parked_without_fetching() {
        let dir = tempfile::tempdir().unwrap();
        let mut mailbox = FakeMailbox::with(&[1]);
        mailbox.messages.insert(2, vec![b'x'; 2048]);
        let mut sink = FakeSink::default();
        let report = watcher(dir.path(), true)
            .poll_once(&mut mailbox, &mut sink)
            .unwrap();
        assert_eq!((report.delivered, report.parked), (vec![1], vec![2]));
    }

    #[test]
    fn uidvalidity_change_restarts_from_the_current_mailbox() {
        let dir = tempfile::tempdir().unwrap();
        let mut mailbox = FakeMailbox::with(&[1, 2]);
        let mut sink = FakeSink::default();
        let w = watcher(dir.path(), false);
        w.poll_once(&mut mailbox, &mut sink).unwrap();
        mailbox.uid_validity = 2;
        mailbox.messages = FakeMailbox::with(&[1, 2, 3]).messages;
        assert_eq!(
            w.poll_once(&mut mailbox, &mut sink).unwrap(),
            PassReport::default()
        );
        let state = WatchState::load(&w.state_path()).unwrap().unwrap();
        assert_eq!((state.uid_validity, state.last_uid), (2, 3));
    }

    #[test]
    fn post_delivery_actions_mark_seen_and_move() {
        let dir = tempfile::tempdir().unwrap();
        let mut mailbox = FakeMailbox::with(&[7]);
        let mut sink = FakeSink::default();
        let mut w = watcher(dir.path(), true);
        w.opts.mark_seen = true;
        w.opts.move_to = Some("Archive/PDF".into());
        w.poll_once(&mut mailbox, &mut sink).unwrap();
        assert_eq!(mailbox.seen, vec![7]);
        assert_eq!(mailbox.moved, vec![(7, "Archive/PDF".to_string())]);
    }

    #[test]
    fn message_stem_is_a_portable_file_name() {
        assert_eq!(message_stem("INBOX", 3, 10), "INBOX-3-10");
        assert_eq!(
            message_stem("Archive/Scans 2026", 1, 2),
            "Archive_Scans_2026-1-2"
        );
        assert_eq!(message_stem(".hidden", 1, 2), "_.hidden-1-2");
        assert_eq!(message_stem("Ünïcode", 1, 2), "_n_code-1-2");
    }
}
