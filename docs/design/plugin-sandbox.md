# Design: opt-in runtime plugin sandbox

Status: implemented behind `--plugin-sandbox` (default `off`).

## Problem

Runtime plugins (`anytopdf-plugin-*`) are native processes that run with the
user's permissions. The host bounds them only by wall-clock timeouts and 16 MiB
output caps. The threat model records three resulting gaps:

- D1 `os-sandbox`: no filesystem, network or process isolation.
- D3 `descendant-containment`: on timeout only the direct child is killed, so a
  plugin's own children keep running.
- D13 `untrusted-plugin-safety`: installing a plugin grants it the user's
  permissions.

## Goals

- Confine a plugin's writes to the job workspace and deny it the network.
- Stop every process a plugin started when its call ends or times out.
- Keep the current behaviour unless the user opts in.
- Fail closed: if a requested level cannot be enforced, the plugin does not run.

Non-goals for this step: CPU, memory and disk quotas; sandboxing the built-in
providers (FFmpeg, ExifTool, Tesseract, docTR); a Windows filesystem or network
sandbox. Each is listed under "Follow-ups".

## Interface

```
--plugin-sandbox off|contain|strict      (global, default off)
--plugin-sandbox-allow-read PATH         (global, repeatable)
```

| Level | What it adds | Linux | macOS | Windows |
| --- | --- | --- | --- | --- |
| `off` | today's behaviour | yes | yes | yes |
| `contain` | process-tree containment | process group | process group | job object |
| `strict` | `contain` + filesystem + no network | Landlock + seccomp | `sandbox-exec` | refused |

`strict` checks support before any plugin runs. When it cannot be enforced
(Windows, a Linux kernel without Landlock, a CPU architecture other than x86-64
or AArch64, or a macOS without `/usr/bin/sandbox-exec`) the command exits with a
usage error instead of silently degrading.

The levels apply to every plugin execution, including the `--anytopdf-manifest`
call made during discovery, so a sandboxed discovery run no longer gives a
plugin unconfined execution (narrows D2 when `strict` is on).

## `contain`

Unix: the plugin is started as the leader of a new process group
(`setpgid(0, 0)` before `exec`). The host waits for it with
`waitid(WNOWAIT)`, so the leader stays a zombie and its process-group id
cannot be reused while the host signals it. When the plugin exits, times out or
exceeds the output cap, the host sends `SIGKILL` to the whole group and then
reaps the leader. Background processes a plugin leaves behind therefore do not
outlive its call.

Windows: the plugin is assigned to a job object with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Children it creates join the job. The host
terminates the job when the call ends.

Limits:

- On macOS a descendant that calls `setsid` or `setpgid` leaves the group. On
  Linux `strict` blocks both calls; `contain` alone does not.
- On Windows there is a short window between process creation and job
  assignment in which a child could start outside the job (std `Command` does
  not expose `CREATE_SUSPENDED` resumption).
- The plugin no longer shares the terminal's foreground process group, so a
  terminal Ctrl-C reaches the host only. If the host is killed by the signal it
  cannot clean up the group; the plugin still ends at its own pace.

## `strict`

Writes: only inside the job workspace, plus `/dev/null`. `TMPDIR` (and on
Windows `TEMP`/`TMP`) point at the workspace. During discovery there is no
workspace, so a manifest call may not write anywhere.

Reads: system locations, the plugin's own directory, the source file being
processed, the workspace, and every `--plugin-sandbox-allow-read` path. Use that
flag for interpreters, virtual environments or model caches that live in the
home directory.

Network: no network access, including connecting to Unix-domain sockets (they
reach local daemons such as a container runtime). Linux denies creating sockets
at all; macOS lets a socket be created but denies binding and connecting it.
`socketpair` stays available.

Renderer plugins write their PDF inside the workspace; the host moves it to the
requested output path afterwards.

### Linux

All set-up that allocates happens in the parent. The child, between `fork` and
`exec`, only makes system calls:

1. `setpgid(0, 0)` (containment).
2. `prctl(PR_SET_NO_NEW_PRIVS)`.
3. `landlock_restrict_self` with a ruleset built in the parent. Handled rights
   are every filesystem right the running kernel's Landlock ABI knows (v1 to
   v5+). System directories (`/usr`, `/bin`, `/sbin`, `/lib*`, `/etc`, `/opt`,
   `/nix`, `/proc`, `/sys`, `/dev`) get read and execute; the workspace gets
   everything except execute.
4. A seccomp filter that returns `EPERM` for `socket`, `setsid`, `setpgid` and
   `io_uring_setup`, and for any system call made with a foreign architecture or
   the x32 ABI.

`/proc` stays readable because common runtimes need `/proc/self`. Landlock's
ptrace scoping stops a sandboxed process from reading the sensitive entries
(`environ`, `mem`, `fd`) of processes outside its domain; world-readable entries
such as `cmdline` stay visible.

### macOS

The plugin runs under `/usr/bin/sandbox-exec -p <profile>`. The profile allows
by default, then denies `network*`, denies `file-write*` except the workspace
and `/dev/null`, and denies `file-read-data` under `/Users`, `/Volumes` and the
user's home directory except the allowed read paths. Metadata (names and sizes)
stays readable, and Mach IPC is not restricted. `sandbox-exec` is deprecated by
Apple but present on every supported macOS.

### Windows

Not available. AppContainer or a restricted token would be the route; see
follow-ups.

## Threat model effect

With `--plugin-sandbox strict` on Linux, D1 and D3 become claimed properties for
runtime plugins (filesystem writes, network, descendant containment), with the
caveats above. On macOS D1 is claimed for writes and network and D3 is
best-effort. `contain` turns D3 into a claim on Linux and Windows for processes
that do not call `setsid`. Default runs are unchanged, so the default-run model
stays as it is. Proposed wording for the threat model is sent to the owner of
`SECURITY.md` and the threat model rather than edited here.

## Follow-ups

1. Quotas: `RLIMIT_AS`/`RLIMIT_FSIZE` or a cgroup v2 leaf on Linux, job object
   memory limits on Windows.
2. Providers: run FFmpeg, ExifTool, Tesseract and docTR under the same levels.
3. Windows `strict` through AppContainer.
4. Report sandbox support in `doctor --json`.
5. Consider making `contain` the default once it has shipped a release cycle.
