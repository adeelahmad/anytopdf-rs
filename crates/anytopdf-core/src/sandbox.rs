//! Opt-in OS sandbox for runtime plugins. See `docs/design/plugin-sandbox.md`.
use crate::{CommandExt, contain_process_tree};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::{Path, PathBuf},
    process::{Command, Output},
    str::FromStr,
    time::Duration,
};

/// How strongly runtime plugin processes are confined.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxMode {
    /// Plugins run as ordinary child processes.
    #[default]
    Off,
    /// Every process a plugin starts ends with its call.
    Contain,
    /// `Contain`, writes only inside the job workspace, and no sockets.
    Strict,
}

impl SandboxMode {
    pub const ALL: [SandboxMode; 3] = [Self::Off, Self::Contain, Self::Strict];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Contain => "contain",
            Self::Strict => "strict",
        }
    }

    /// Fails when this platform cannot enforce the mode.
    pub fn check_supported(self) -> Result<()> {
        match self {
            Self::Off | Self::Contain => Ok(()),
            Self::Strict => platform::check_strict(),
        }
    }
}

impl fmt::Display for SandboxMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SandboxMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.as_str() == value)
            .with_context(|| format!("unknown sandbox mode {value:?}"))
    }
}

/// Sandbox settings shared by every plugin invocation of a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SandboxPolicy {
    pub mode: SandboxMode,
    /// Extra paths a `Strict` plugin may read (interpreters, virtualenvs, caches).
    pub allow_read: Vec<PathBuf>,
}

/// What one sandboxed execution may touch besides system locations.
#[derive(Debug, Clone, Default)]
pub struct SandboxAccess<'a> {
    /// Read-write directory; `None` allows no writes.
    pub workspace: Option<&'a Path>,
    /// Files or directories readable for this call only.
    pub read: Vec<&'a Path>,
}

impl SandboxPolicy {
    /// Builds a command that runs `program` under this policy.
    pub fn command(&self, program: &Path, access: &SandboxAccess<'_>) -> Result<Command> {
        match self.mode {
            SandboxMode::Off => Ok(Command::new(program)),
            SandboxMode::Contain => {
                let mut command = Command::new(program);
                contain_process_tree(&mut command);
                Ok(command)
            }
            SandboxMode::Strict => {
                platform::check_strict()?;
                let rules = self.rules(program, access)?;
                let mut command = platform::strict_command(program, &rules)?;
                if let Some(workspace) = rules.write.first() {
                    for name in ["TMPDIR", "TEMP", "TMP"] {
                        command.env(name, workspace);
                    }
                }
                Ok(command)
            }
        }
    }

    /// Runs a command from [`SandboxPolicy::command`] with the bounded-output guard.
    pub fn output(&self, command: &mut Command, timeout: Duration) -> Result<Output> {
        match self.mode {
            SandboxMode::Off => command.bounded_output(timeout),
            SandboxMode::Contain | SandboxMode::Strict => command.bounded_output_contained(timeout),
        }
    }

    fn rules(&self, program: &Path, access: &SandboxAccess<'_>) -> Result<Rules> {
        let mut read = Vec::new();
        for path in [
            program,
            &program.canonicalize().context("resolve plugin path")?,
        ] {
            if let Some(parent) = path.parent() {
                read.push(canonical(parent)?);
            }
        }
        for path in access
            .read
            .iter()
            .copied()
            .chain(self.allow_read.iter().map(PathBuf::as_path))
        {
            read.push(canonical(path)?);
        }
        let write = access
            .workspace
            .map(canonical)
            .transpose()?
            .into_iter()
            .collect();
        read.sort();
        read.dedup();
        Ok(Rules { read, write })
    }
}

/// Errors early when a run asks for a mode the platform cannot enforce.
pub fn validate_sandbox_policy(policy: &SandboxPolicy) -> Result<()> {
    policy.mode.check_supported()?;
    if policy.mode != SandboxMode::Strict && !policy.allow_read.is_empty() {
        bail!("--plugin-sandbox-allow-read requires --plugin-sandbox strict");
    }
    Ok(())
}

fn canonical(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("sandbox path {} is not accessible", path.display()))
}

/// Canonical paths for one strict execution.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
struct Rules {
    read: Vec<PathBuf>,
    write: Vec<PathBuf>,
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod platform {
    //! Landlock for the filesystem, seccomp for sockets and session escapes.
    use super::Rules;
    use crate::contain_process_tree;
    use anyhow::{Context, Result, bail};
    use std::{
        ffi::CString,
        io,
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::{ffi::OsStrExt, process::CommandExt},
        },
        path::Path,
        process::Command,
        sync::Arc,
    };

    const SYSTEM_READ: &[&str] = &[
        "/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/opt", "/nix",
        "/proc", "/sys", "/dev",
    ];

    const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
    const LANDLOCK_RULE_PATH_BENEATH: libc::c_int = 1;
    const EXECUTE: u64 = 1 << 0;
    const WRITE_FILE: u64 = 1 << 1;
    const READ_FILE: u64 = 1 << 2;
    const READ_DIR: u64 = 1 << 3;
    const TRUNCATE: u64 = 1 << 14;
    const IOCTL_DEV: u64 = 1 << 15;
    /// Rights that may appear in a rule for a regular file.
    const FILE_RIGHTS: u64 = EXECUTE | WRITE_FILE | READ_FILE | TRUNCATE | IOCTL_DEV;

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
    }

    #[repr(C, packed)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
    }

    fn abi_version() -> i64 {
        // SAFETY: the version query reads no memory.
        unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<RulesetAttr>(),
                0usize,
                LANDLOCK_CREATE_RULESET_VERSION,
            )
        }
    }

    /// Filesystem rights the running kernel's Landlock ABI can restrict.
    fn handled_rights(abi: i64) -> u64 {
        match abi {
            1 => (1 << 13) - 1,
            2 => (1 << 14) - 1,
            3 | 4 => (1 << 15) - 1,
            _ => (1 << 16) - 1,
        }
    }

    pub(super) fn check_strict() -> Result<()> {
        let abi = abi_version();
        if abi < 1 {
            bail!(
                "strict plugin sandbox needs Linux Landlock, which this kernel does not provide ({})",
                io::Error::last_os_error()
            );
        }
        Ok(())
    }

    struct Ruleset {
        fd: OwnedFd,
        handled: u64,
    }

    impl Ruleset {
        fn new() -> Result<Self> {
            let handled = handled_rights(abi_version());
            let attr = RulesetAttr {
                handled_access_fs: handled,
            };
            // SAFETY: attr outlives the call and its size is passed exactly.
            let fd = unsafe {
                libc::syscall(
                    libc::SYS_landlock_create_ruleset,
                    &attr as *const RulesetAttr,
                    size_of::<RulesetAttr>(),
                    0u32,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error()).context("create Landlock ruleset");
            }
            // SAFETY: the kernel returned a new, owned close-on-exec descriptor.
            let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
            Ok(Self { fd, handled })
        }

        /// Grants `rights` beneath `path`; a missing path is an error only when `required`.
        fn allow(&self, path: &Path, rights: u64, required: bool) -> Result<()> {
            let name = CString::new(path.as_os_str().as_bytes())?;
            // SAFETY: name is a valid C string for the duration of the call.
            let raw = unsafe { libc::open(name.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
            if raw < 0 {
                if required {
                    return Err(io::Error::last_os_error())
                        .with_context(|| format!("open sandbox path {}", path.display()));
                }
                return Ok(());
            }
            // SAFETY: open returned a new owned descriptor.
            let fd = unsafe { OwnedFd::from_raw_fd(raw) };
            let rights = if path.is_dir() {
                rights
            } else {
                rights & FILE_RIGHTS
            };
            let attr = PathBeneathAttr {
                allowed_access: rights & self.handled,
                parent_fd: fd.as_raw_fd(),
            };
            // SAFETY: attr and both descriptors outlive the call.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_landlock_add_rule,
                    self.fd.as_raw_fd(),
                    LANDLOCK_RULE_PATH_BENEATH,
                    &attr as *const PathBeneathAttr,
                    0u32,
                )
            };
            if result != 0 {
                return Err(io::Error::last_os_error())
                    .with_context(|| format!("add sandbox rule for {}", path.display()));
            }
            Ok(())
        }
    }

    fn ruleset(rules: &Rules) -> Result<Ruleset> {
        let ruleset = Ruleset::new()?;
        let read = EXECUTE | READ_FILE | READ_DIR;
        for path in SYSTEM_READ {
            ruleset.allow(Path::new(path), read | IOCTL_DEV, false)?;
        }
        ruleset.allow(
            Path::new("/dev/null"),
            READ_FILE | WRITE_FILE | TRUNCATE,
            false,
        )?;
        for path in &rules.read {
            ruleset.allow(path, read, true)?;
        }
        for path in &rules.write {
            ruleset.allow(path, ruleset.handled & !EXECUTE, true)?;
        }
        Ok(ruleset)
    }

    // Classic BPF, as used by seccomp.
    const BPF_LD_W_ABS: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
    const BPF_JEQ_K: u16 = 0x05 | 0x10;
    const BPF_JGE_K: u16 = 0x05 | 0x30;
    const BPF_RET_K: u16 = 0x06;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const SECCOMP_DATA_NR: u32 = 0;
    const SECCOMP_DATA_ARCH: u32 = 4;
    #[cfg(target_arch = "x86_64")]
    const AUDIT_ARCH: u32 = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    const AUDIT_ARCH: u32 = 0xc000_00b7;
    const X32_SYSCALL_BIT: u32 = 0x4000_0000;

    const DENIED_SYSCALLS: &[libc::c_long] = &[
        libc::SYS_socket,
        libc::SYS_setsid,
        libc::SYS_setpgid,
        libc::SYS_io_uring_setup,
    ];

    fn stmt(code: u16, k: u32) -> libc::sock_filter {
        libc::sock_filter {
            code,
            jt: 0,
            jf: 0,
            k,
        }
    }

    /// Returns EPERM for denied calls, foreign architectures and the x32 ABI.
    fn seccomp_filter() -> Vec<libc::sock_filter> {
        let mut program = vec![stmt(BPF_LD_W_ABS, SECCOMP_DATA_ARCH)];
        let mut deny_jumps = Vec::new();
        // Mismatched architecture: skip nothing on match, jump to deny otherwise.
        deny_jumps.push((program.len(), false));
        program.push(stmt(BPF_JEQ_K, AUDIT_ARCH));
        program.push(stmt(BPF_LD_W_ABS, SECCOMP_DATA_NR));
        if cfg!(target_arch = "x86_64") {
            deny_jumps.push((program.len(), true));
            program.push(stmt(BPF_JGE_K, X32_SYSCALL_BIT));
        }
        for nr in DENIED_SYSCALLS {
            deny_jumps.push((program.len(), true));
            program.push(stmt(BPF_JEQ_K, *nr as u32));
        }
        program.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
        let deny = program.len();
        program.push(stmt(BPF_RET_K, SECCOMP_RET_ERRNO | libc::EPERM as u32));
        for (index, on_true) in deny_jumps {
            let offset = (deny - index - 1) as u8;
            if on_true {
                program[index].jt = offset;
            } else {
                program[index].jf = offset;
            }
        }
        program
    }

    pub(super) fn strict_command(program: &Path, rules: &Rules) -> Result<Command> {
        let ruleset = Arc::new(ruleset(rules)?);
        let filter = Arc::new(seccomp_filter());
        let mut command = Command::new(program);
        contain_process_tree(&mut command);
        // SAFETY: the hook only makes system calls on state prepared in the parent.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::syscall(
                    libc::SYS_landlock_restrict_self,
                    ruleset.fd.as_raw_fd(),
                    0u32,
                ) != 0
                {
                    return Err(io::Error::last_os_error());
                }
                let program = libc::sock_fprog {
                    len: filter.len() as u16,
                    filter: filter.as_ptr() as *mut libc::sock_filter,
                };
                if libc::prctl(
                    libc::PR_SET_SECCOMP,
                    libc::SECCOMP_MODE_FILTER,
                    &program as *const libc::sock_fprog,
                ) != 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(command)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn seccomp_jumps_land_on_the_deny_return() {
            let program = seccomp_filter();
            let deny = program.len() - 1;
            for (index, instruction) in program.iter().enumerate() {
                if instruction.code == BPF_JEQ_K || instruction.code == BPF_JGE_K {
                    let target = index + 1 + instruction.jt.max(instruction.jf) as usize;
                    assert_eq!(target, deny, "instruction {index}");
                }
            }
            assert_eq!(program[deny].k, SECCOMP_RET_ERRNO | libc::EPERM as u32);
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    //! Seatbelt through `sandbox-exec`.
    use super::Rules;
    use crate::contain_process_tree;
    use anyhow::{Result, bail};
    use std::{
        fmt::Write as _,
        path::{Path, PathBuf},
        process::Command,
    };

    const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

    pub(super) fn check_strict() -> Result<()> {
        if !Path::new(SANDBOX_EXEC).is_file() {
            bail!("strict plugin sandbox needs {SANDBOX_EXEC}, which is missing");
        }
        Ok(())
    }

    fn quote(path: &Path) -> String {
        let text = path.to_string_lossy();
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    }

    pub(super) fn profile(rules: &Rules, home: Option<PathBuf>) -> String {
        let mut profile = String::from(
            "(version 1)\n(allow default)\n(deny network*)\n(deny file-write*)\n\
             (allow file-write* (literal \"/dev/null\"))\n",
        );
        for path in &rules.write {
            let _ = writeln!(profile, "(allow file-write* (subpath {}))", quote(path));
        }
        profile.push_str("(deny file-read-data (subpath \"/Users\") (subpath \"/Volumes\"))\n");
        if let Some(home) = home {
            let _ = writeln!(profile, "(deny file-read-data (subpath {}))", quote(&home));
        }
        for path in rules.read.iter().chain(&rules.write) {
            let _ = writeln!(profile, "(allow file-read-data (subpath {}))", quote(path));
        }
        profile
    }

    pub(super) fn strict_command(program: &Path, rules: &Rules) -> Result<Command> {
        let home =
            std::env::var_os("HOME").and_then(|home| PathBuf::from(home).canonicalize().ok());
        let mut command = Command::new(SANDBOX_EXEC);
        command.arg("-p").arg(profile(rules, home)).arg(program);
        contain_process_tree(&mut command);
        Ok(command)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn profile_quotes_paths_and_scopes_writes_to_workspace() {
            let rules = Rules {
                read: vec![PathBuf::from("/opt/plug\"in")],
                write: vec![PathBuf::from("/private/var/ws")],
            };
            let text = profile(&rules, Some(PathBuf::from("/Users/me")));
            assert!(text.contains("(allow file-write* (subpath \"/private/var/ws\"))"));
            assert!(text.contains("(subpath \"/opt/plug\\\"in\")"));
            assert!(text.contains("(deny network*)"));
        }
    }
}

#[cfg(not(any(
    target_os = "macos",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
)))]
mod platform {
    use super::Rules;
    use anyhow::{Result, bail};
    use std::{path::Path, process::Command};

    pub(super) fn check_strict() -> Result<()> {
        bail!(
            "strict plugin sandbox is not available on this platform; use --plugin-sandbox contain"
        )
    }

    pub(super) fn strict_command(_program: &Path, _rules: &Rules) -> Result<Command> {
        check_strict().map(|()| unreachable!())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_modes_round_trip_through_their_names() {
        for mode in SandboxMode::ALL {
            assert_eq!(mode.as_str().parse::<SandboxMode>().unwrap(), mode);
        }
        assert!("jail".parse::<SandboxMode>().is_err());
        assert_eq!(SandboxMode::default(), SandboxMode::Off);
    }

    #[test]
    fn missing_allow_read_path_is_rejected() {
        let policy = SandboxPolicy {
            mode: SandboxMode::Strict,
            allow_read: vec![PathBuf::from("/definitely/not/here")],
        };
        let exe = std::env::current_exe().unwrap();
        assert!(policy.rules(&exe, &SandboxAccess::default()).is_err());
    }

    #[test]
    fn contained_command_runs_normally() {
        let policy = SandboxPolicy {
            mode: SandboxMode::Contain,
            ..Default::default()
        };
        let exe = std::env::current_exe().unwrap();
        let mut command = policy.command(&exe, &SandboxAccess::default()).unwrap();
        command.arg("--list");
        let out = policy
            .output(&mut command, Duration::from_secs(30))
            .unwrap();
        assert!(out.status.success());
    }

    #[test]
    fn unsupported_strict_mode_fails_closed() {
        if SandboxMode::Strict.check_supported().is_ok() {
            return;
        }
        let policy = SandboxPolicy {
            mode: SandboxMode::Strict,
            ..Default::default()
        };
        let exe = std::env::current_exe().unwrap();
        assert!(policy.command(&exe, &SandboxAccess::default()).is_err());
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;
    use std::{fs, thread, time::Instant};

    fn strict(allow_read: Vec<PathBuf>) -> Option<SandboxPolicy> {
        if let Err(e) = SandboxMode::Strict.check_supported() {
            eprintln!("skipping strict sandbox test: {e:#}");
            return None;
        }
        Some(SandboxPolicy {
            mode: SandboxMode::Strict,
            allow_read,
        })
    }

    fn shell(
        policy: &SandboxPolicy,
        access: &SandboxAccess<'_>,
        script: &str,
        args: &[&Path],
    ) -> Output {
        let mut command = policy.command(Path::new("/bin/sh"), access).unwrap();
        command.arg("-c").arg(script).arg("sh").args(args);
        policy
            .output(&mut command, Duration::from_secs(20))
            .unwrap()
    }

    fn alive(pid: &str) -> bool {
        let out = Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .unwrap();
        let stat = String::from_utf8_lossy(&out.stdout);
        let stat = stat.trim();
        !stat.is_empty() && !stat.starts_with('Z')
    }

    fn eventually_dead(pid: &str) -> bool {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            if !alive(pid) {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn contain_kills_background_children_when_the_plugin_exits() {
        let workspace = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy {
            mode: SandboxMode::Contain,
            ..Default::default()
        };
        let access = SandboxAccess {
            workspace: Some(workspace.path()),
            ..Default::default()
        };
        let out = shell(
            &policy,
            &access,
            r#"sleep 30 & echo $! > "$1/pid""#,
            &[workspace.path()],
        );
        assert!(out.status.success());
        let pid = fs::read_to_string(workspace.path().join("pid")).unwrap();
        assert!(
            eventually_dead(pid.trim()),
            "background child {pid} survived"
        );
    }

    #[test]
    fn contain_kills_background_children_on_timeout() {
        let workspace = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy {
            mode: SandboxMode::Contain,
            ..Default::default()
        };
        let mut command = policy
            .command(Path::new("/bin/sh"), &SandboxAccess::default())
            .unwrap();
        command
            .arg("-c")
            .arg(r#"sleep 30 & echo $! > "$1/pid"; wait"#)
            .arg("sh")
            .arg(workspace.path());
        let error = policy
            .output(&mut command, Duration::from_millis(500))
            .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        let pid = fs::read_to_string(workspace.path().join("pid")).unwrap();
        assert!(
            eventually_dead(pid.trim()),
            "background child {pid} survived"
        );
    }

    #[test]
    fn strict_plugin_writes_only_inside_workspace() {
        let Some(policy) = strict(Vec::new()) else {
            return;
        };
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let access = SandboxAccess {
            workspace: Some(workspace.path()),
            ..Default::default()
        };
        let out = shell(
            &policy,
            &access,
            r#"echo ok > "$1/inside"; echo ok > "$TMPDIR/temp"; echo bad > "$2/outside" || exit 9"#,
            &[workspace.path(), outside.path()],
        );
        assert_eq!(
            out.status.code(),
            Some(9),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(workspace.path().join("inside").exists());
        assert!(workspace.path().join("temp").exists());
        assert!(!outside.path().join("outside").exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn strict_plugin_reads_only_allowed_paths() {
        let shared = tempfile::tempdir().unwrap();
        let secret = tempfile::tempdir().unwrap();
        fs::write(shared.path().join("file"), "shared").unwrap();
        fs::write(secret.path().join("file"), "secret").unwrap();
        let Some(policy) = strict(vec![shared.path().into()]) else {
            return;
        };
        let out = shell(
            &policy,
            &SandboxAccess::default(),
            r#"cat "$1/file" && ! cat "$2/file""#,
            &[shared.path(), secret.path()],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.stdout, b"shared");
    }

    /// Run in a sandboxed child by `strict_plugin_cannot_use_the_network`.
    #[test]
    #[ignore = "helper for strict_plugin_cannot_use_the_network"]
    fn network_probe() {
        let Some(listener) = std::env::var_os("ANYTOPDF_SANDBOX_PROBE") else {
            return;
        };
        let tcp = std::net::TcpListener::bind("127.0.0.1:0").is_ok();
        let unix = std::os::unix::net::UnixStream::connect(listener).is_ok();
        // 20 when both are denied; +1 if TCP bind worked, +2 if the Unix connect did.
        std::process::exit(20 + i32::from(tcp) + 2 * i32::from(unix));
    }

    #[test]
    fn strict_plugin_cannot_use_the_network() {
        let Some(policy) = strict(Vec::new()) else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let exe = std::env::current_exe().unwrap();
        let mut command = policy.command(&exe, &SandboxAccess::default()).unwrap();
        command
            .args(["--exact", "sandbox::unix_tests::network_probe", "--ignored"])
            .args(["--nocapture", "--test-threads=1"])
            .env("ANYTOPDF_SANDBOX_PROBE", &socket);
        let out = policy
            .output(&mut command, Duration::from_secs(30))
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(20),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
