//! Print receipts: one JSON line per submitted job with when it arrived, the
//! peer address, the signed-in user and the job name. The print helper never
//! sees the peer address, so the front is the only place to record it.

use crate::ipp::IppSummary;
use anyhow::{Context, Result};
use serde_json::json;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const RECEIPT_SCHEMA: &str = "anytopdf.print-receipt/1";

pub struct Receipts(Mutex<File>);

impl Receipts {
    /// Opens `path` for appending, creating it (mode 0600 on Unix) if needed.
    pub fn open(path: &Path) -> Result<Receipts> {
        let mut options = OpenOptions::new();
        options.append(true).create(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options
            .open(path)
            .with_context(|| format!("cannot open receipts file {}", path.display()))?;
        Ok(Receipts(Mutex::new(file)))
    }

    pub(crate) fn record(&self, peer: SocketAddr, user: Option<&str>, request: &IppSummary) {
        let unix_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = json!({
            "schema_version": RECEIPT_SCHEMA,
            "unix_time": unix_time,
            "peer": peer.ip().to_string(),
            "user": user,
            "operation": request.operation_name(),
            "request_id": request.request_id,
            "job_name": request.job_name,
        });
        let mut file = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = writeln!(file, "{line}") {
            eprintln!("print remote: cannot write receipt: {e}");
        }
    }
}
