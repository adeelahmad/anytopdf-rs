use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::Path,
};

pub const STATE_SCHEMA: &str = "anytopdf.imap-state/1";

/// Durable progress for one mailbox. UIDs are only meaningful for one UIDVALIDITY.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchState {
    pub schema_version: String,
    pub host: String,
    pub user: String,
    pub mailbox: String,
    pub uid_validity: u32,
    /// Highest UID that has been delivered, parked or scheduled for retry.
    pub last_uid: u32,
    /// UIDs whose delivery failed, with the number of attempts so far.
    #[serde(default)]
    pub retry: BTreeMap<u32, u32>,
    /// UIDs that are no longer attempted (too large or out of attempts).
    #[serde(default)]
    pub parked: BTreeSet<u32>,
}

impl WatchState {
    pub fn new(host: &str, user: &str, mailbox: &str, uid_validity: u32, last_uid: u32) -> Self {
        WatchState {
            schema_version: STATE_SCHEMA.into(),
            host: host.into(),
            user: user.into(),
            mailbox: mailbox.into(),
            uid_validity,
            last_uid,
            retry: BTreeMap::new(),
            parked: BTreeSet::new(),
        }
    }

    /// Load state, or `None` when the file does not exist yet.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        let state: WatchState = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse watcher state {}", path.display()))?;
        if state.schema_version != STATE_SCHEMA {
            bail!(
                "{} has schema_version {:?}; expected {STATE_SCHEMA:?}",
                path.display(),
                state.schema_version
            );
        }
        Ok(Some(state))
    }

    /// Refuse to reuse a state file written for a different account or mailbox.
    pub fn check_identity(&self, host: &str, user: &str, mailbox: &str) -> Result<()> {
        if self.host != host || self.user != user || self.mailbox != mailbox {
            bail!(
                "state belongs to {}@{} mailbox {:?}, not {user}@{host} mailbox {mailbox:?}; use a separate --state-dir per mailbox",
                self.user,
                self.host,
                self.mailbox
            );
        }
        Ok(())
    }

    /// Write atomically: a crash leaves either the old or the new state.
    pub fn save(&self, path: &Path) -> Result<()> {
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir)
            .with_context(|| format!("create temporary state in {}", dir.display()))?;
        serde_json::to_writer_pretty(&mut tmp, self)?;
        tmp.write_all(b"\n")?;
        tmp.as_file().sync_all()?;
        tmp.persist(path)
            .with_context(|| format!("replace {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_and_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/state.json");
        assert!(WatchState::load(&path).unwrap().is_none());
        let mut state = WatchState::new("h", "u", "INBOX", 7, 42);
        state.retry.insert(40, 2);
        state.parked.insert(41);
        state.save(&path).unwrap();
        assert_eq!(WatchState::load(&path).unwrap(), Some(state));
    }

    #[test]
    fn state_for_another_mailbox_is_rejected() {
        let state = WatchState::new("h", "u", "INBOX", 1, 0);
        assert!(state.check_identity("h", "u", "INBOX").is_ok());
        assert!(state.check_identity("h", "u", "Scans").is_err());
        assert!(state.check_identity("other", "u", "INBOX").is_err());
    }

    #[test]
    fn unknown_state_schema_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut state = WatchState::new("h", "u", "INBOX", 1, 0);
        state.schema_version = "anytopdf.imap-state/9".into();
        state.save(&path).unwrap();
        assert!(WatchState::load(&path).is_err());
    }
}
