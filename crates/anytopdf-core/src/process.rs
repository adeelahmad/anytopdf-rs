//! Bounded subprocess execution: a timeout/output guard with optional process-tree
//! containment. The OS sandbox lives in `sandbox`.
use anyhow::{Context, Result, bail};
use std::{
    io::{Read, Seek, SeekFrom},
    process::{Child, Command, ExitStatus, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

pub const MAX_PROCESS_OUTPUT: u64 = 16 * 1024 * 1024;

/// Kills a running child, and with containment every process it started.
struct ChildGuard {
    child: Child,
    tree: ProcessTree,
}

impl ChildGuard {
    fn kill(&mut self) {
        self.tree.kill(&mut self.child);
        let _ = self.child.wait();
    }

    /// Reports the exit status. A contained child is reaped only after its
    /// group is killed, so the process-group id stays reserved until then.
    fn exited(&mut self) -> Result<Option<ExitStatus>> {
        if !self.tree.is_contained() {
            return Ok(self.child.try_wait()?);
        }
        if !self.tree.leader_exited(&mut self.child)? {
            return Ok(None);
        }
        self.tree.kill(&mut self.child);
        Ok(Some(self.child.wait()?))
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.kill();
        }
    }
}

pub trait CommandExt {
    fn bounded_output(&mut self, timeout: Duration) -> Result<Output>;
    /// Like [`CommandExt::bounded_output`], but the call ends every process the
    /// child started. On Unix the command must first pass through [`contain_process_tree`].
    fn bounded_output_contained(&mut self, timeout: Duration) -> Result<Output>;
}

impl CommandExt for Command {
    fn bounded_output(&mut self, timeout: Duration) -> Result<Output> {
        run_bounded(self, timeout, false)
    }

    fn bounded_output_contained(&mut self, timeout: Duration) -> Result<Output> {
        run_bounded(self, timeout, true)
    }
}

fn run_bounded(command: &mut Command, timeout: Duration, contain: bool) -> Result<Output> {
    // Files avoid pipe deadlocks and unbounded allocations from noisy providers.
    let mut stdout = tempfile::tempfile().context("create subprocess stdout")?;
    let mut stderr = tempfile::tempfile().context("create subprocess stderr")?;
    command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?);
    let child = command
        .spawn()
        .with_context(|| format!("start {:?}", command.get_program()))?;
    let mut child = ChildGuard {
        tree: ProcessTree::Single,
        child,
    };
    if contain {
        child.tree = ProcessTree::attach(&child.child)?;
    }
    let started = Instant::now();
    let status = loop {
        let oversized = stdout.metadata()?.len() > MAX_PROCESS_OUTPUT
            || stderr.metadata()?.len() > MAX_PROCESS_OUTPUT;
        if oversized || started.elapsed() >= timeout {
            child.kill();
            if oversized {
                bail!("subprocess output exceeded {MAX_PROCESS_OUTPUT} bytes");
            }
            bail!("subprocess timed out after {:.1}s", timeout.as_secs_f64());
        }
        if let Some(status) = child.exited()? {
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

/// Starts the command as the leader of a new process group, so the whole group
/// can be killed. Register this before any other `pre_exec` hook.
#[cfg(unix)]
pub fn contain_process_tree(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: setpgid is async-signal-safe and touches no parent state.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Windows contains the tree with a job object attached after spawn.
#[cfg(not(unix))]
pub fn contain_process_tree(_command: &mut Command) {}

enum ProcessTree {
    Single,
    #[cfg(unix)]
    Group(libc::pid_t),
    #[cfg(windows)]
    Job(windows::Job),
}

impl ProcessTree {
    #[cfg(unix)]
    fn attach(child: &Child) -> Result<Self> {
        Ok(Self::Group(child.id() as libc::pid_t))
    }

    #[cfg(windows)]
    fn attach(child: &Child) -> Result<Self> {
        Ok(Self::Job(windows::Job::attach(child)?))
    }

    #[cfg(not(any(unix, windows)))]
    fn attach(_child: &Child) -> Result<Self> {
        bail!("process-tree containment is not supported on this platform")
    }

    fn is_contained(&self) -> bool {
        !matches!(self, Self::Single)
    }

    #[cfg(unix)]
    fn leader_exited(&self, child: &mut Child) -> Result<bool> {
        // SAFETY: waitid writes only into the zeroed siginfo it is given.
        unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
            if libc::waitid(libc::P_PID, child.id() as libc::id_t, &mut info, flags) != 0 {
                return Err(std::io::Error::last_os_error()).context("wait for subprocess");
            }
            Ok(info.si_pid() != 0)
        }
    }

    #[cfg(not(unix))]
    fn leader_exited(&self, child: &mut Child) -> Result<bool> {
        // The open process handle keeps the job membership; reaping is harmless here.
        Ok(child.try_wait()?.is_some())
    }

    fn kill(&self, child: &mut Child) {
        match self {
            Self::Single => {
                let _ = child.kill();
            }
            #[cfg(unix)]
            Self::Group(group) => {
                // SAFETY: plain signal delivery; the unreaped leader keeps the id reserved.
                unsafe { libc::killpg(*group, libc::SIGKILL) };
            }
            #[cfg(windows)]
            Self::Job(job) => {
                job.terminate();
                let _ = child.kill();
            }
        }
    }
}

#[cfg(windows)]
mod windows {
    use anyhow::{Result, bail};
    use std::{os::windows::io::AsRawHandle, process::Child, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
    };

    pub(super) struct Job(HANDLE);

    impl Job {
        pub(super) fn attach(child: &Child) -> Result<Self> {
            // SAFETY: the handles are owned here or borrowed from a live Child.
            unsafe {
                let handle = CreateJobObjectW(ptr::null(), ptr::null());
                if handle.is_null() {
                    bail!("create job object: {}", std::io::Error::last_os_error());
                }
                let job = Self(handle);
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) == 0
                {
                    bail!("configure job object: {}", std::io::Error::last_os_error());
                }
                if AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE) == 0 {
                    bail!("assign job object: {}", std::io::Error::last_os_error());
                }
                Ok(job)
            }
        }

        pub(super) fn terminate(&self) {
            // SAFETY: the job handle is owned and open.
            unsafe { TerminateJobObject(self.0, 1) };
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: closing the owned handle kills remaining members (KILL_ON_JOB_CLOSE).
            unsafe { CloseHandle(self.0) };
        }
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
