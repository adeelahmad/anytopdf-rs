//! Bounded subprocess execution. This is a timeout/output guard, not an OS sandbox.
use anyhow::{Context, Result, bail};
use std::{
    io::{Read, Seek, SeekFrom},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

pub const MAX_PROCESS_OUTPUT: u64 = 16 * 1024 * 1024;

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

pub trait CommandExt {
    fn bounded_output(&mut self, timeout: Duration) -> Result<Output>;
}

impl CommandExt for Command {
    fn bounded_output(&mut self, timeout: Duration) -> Result<Output> {
        // Files avoid pipe deadlocks and unbounded allocations from noisy providers.
        let mut stdout = tempfile::tempfile().context("create subprocess stdout")?;
        let mut stderr = tempfile::tempfile().context("create subprocess stderr")?;
        self.stdin(Stdio::null())
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?);
        let mut child = ChildGuard(
            self.spawn()
                .with_context(|| format!("start {:?}", self.get_program()))?,
        );
        let started = Instant::now();
        let status = loop {
            let oversized = stdout.metadata()?.len() > MAX_PROCESS_OUTPUT
                || stderr.metadata()?.len() > MAX_PROCESS_OUTPUT;
            if oversized || started.elapsed() >= timeout {
                let _ = child.0.kill();
                let _ = child.0.wait();
                if oversized {
                    bail!("subprocess output exceeded {MAX_PROCESS_OUTPUT} bytes");
                }
                bail!("subprocess timed out after {:.1}s", timeout.as_secs_f64());
            }
            if let Some(status) = child.0.try_wait()? {
                break status;
            }
            thread::sleep(Duration::from_millis(10));
        };
        stdout.seek(SeekFrom::Start(0))?;
        stderr.seek(SeekFrom::Start(0))?;
        let mut out = Vec::new();
        let mut err = Vec::new();
        stdout.take(MAX_PROCESS_OUTPUT + 1).read_to_end(&mut out)?;
        stderr.take(MAX_PROCESS_OUTPUT + 1).read_to_end(&mut err)?;
        if out.len() as u64 > MAX_PROCESS_OUTPUT || err.len() as u64 > MAX_PROCESS_OUTPUT {
            bail!("subprocess output exceeded {MAX_PROCESS_OUTPUT} bytes");
        }
        Ok(Output {
            status,
            stdout: out,
            stderr: err,
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn captures_stdout_stderr_and_failure_status() {
        let result = Command::new("sh")
            .args(["-c", "printf hello; printf problem >&2; exit 7"])
            .bounded_output(Duration::from_secs(2))
            .unwrap();
        assert_eq!(result.stdout, b"hello");
        assert_eq!(result.stderr, b"problem");
        assert_eq!(result.status.code(), Some(7));
    }

    #[test]
    fn terminates_hung_process() {
        let started = Instant::now();
        let error = Command::new("sh")
            .args(["-c", "exec sleep 10"])
            .bounded_output(Duration::from_millis(50))
            .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
