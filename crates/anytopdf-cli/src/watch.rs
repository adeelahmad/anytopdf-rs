use crate::cli::ImapArgs;
use crate::exit::{CliError, ExitClass, fail};
use anytopdf_core::RuntimePluginPolicy;

#[cfg(not(feature = "imap"))]
pub(crate) fn watch_imap(_args: ImapArgs, _policy: &RuntimePluginPolicy) -> Result<(), CliError> {
    Err(fail(
        ExitClass::Usage,
        "this anytopdf build has no IMAP support; rebuild with `cargo build --features imap`",
    ))
}

#[cfg(feature = "imap")]
pub(crate) use imap::watch_imap;

#[cfg(feature = "imap")]
mod imap {
    use super::*;
    use crate::cli::Cli;
    use crate::exit::tag;
    use crate::queue::{Job, Origin, Queue, check_convert_args};
    use anyhow::{Context, Result, bail};
    use anytopdf_core::CommandExt;
    use anytopdf_imap::{
        Credential, Delivery, ImapConfig, Mailbox, MessageSink, SenderPolicy, SpooledMessage,
        TlsMode, WatchOptions, Watcher, connect, read_password,
    };
    use clap::Parser;
    use std::{ffi::OsString, path::PathBuf, process::Command, time::Duration};

    const PASSWORD_ENV: &str = "ANYTOPDF_IMAP_PASSWORD";
    const TOKEN_ENV: &str = "ANYTOPDF_IMAP_OAUTH_TOKEN";
    const IO_TIMEOUT: Duration = Duration::from_secs(120);

    /// Converts each spooled message with a child `anytopdf convert`, so a crash or
    /// hang on one hostile message cannot take the watcher down.
    struct ConvertSink {
        exe: PathBuf,
        global: Vec<OsString>,
        output_dir: PathBuf,
        extra: Vec<OsString>,
        timeout: Duration,
    }

    impl ConvertSink {
        fn command(&self, input: &std::path::Path, output: &std::path::Path) -> Command {
            let mut command = Command::new(&self.exe);
            command
                .args(&self.global)
                .args(convert_args(input, output, &self.extra))
                // Plugins run by the converter must not inherit mailbox credentials.
                .env_remove(PASSWORD_ENV)
                .env_remove(TOKEN_ENV);
            command
        }
    }

    impl MessageSink for ConvertSink {
        fn deliver(&mut self, message: &SpooledMessage) -> Result<Delivery> {
            let output = self.output_dir.join(format!("{}.pdf", message.stem));
            let result = self
                .command(&message.path, &output)
                .bounded_output(self.timeout)?;
            if !result.status.success() {
                let stderr = String::from_utf8_lossy(&result.stderr);
                let last = stderr
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("no error output");
                match result.status.code() {
                    Some(code) => bail!("convert exited with code {code}: {last}"),
                    None => bail!("convert was terminated: {last}"),
                }
            }
            Ok(Delivery {
                output: Some(output),
                note: None,
            })
        }
    }

    /// Hands each message to an `anytopdf queue` directory as a job. The queue owns a
    /// copy of the message, so the watcher may delete its spool file afterwards.
    struct QueueSink {
        queue: Queue,
        convert: Vec<String>,
    }

    impl MessageSink for QueueSink {
        fn deliver(&mut self, message: &SpooledMessage) -> Result<Delivery> {
            let mut job = Job::new(
                Origin::Imap,
                Vec::new(),
                self.convert.clone(),
                std::env::current_dir()?,
            );
            let input_dir = self.queue.work_dir(&job.id).join("input");
            std::fs::create_dir_all(&input_dir)
                .with_context(|| format!("create {}", input_dir.display()))?;
            let input = input_dir.join(format!("{}.eml", message.stem));
            std::fs::copy(&message.path, &input)
                .with_context(|| format!("copy message into {}", input.display()))?;
            job.inputs = vec![input];
            self.queue.enqueue(&job)?;
            Ok(Delivery {
                output: None,
                note: Some(format!("queued as {}", job.id)),
            })
        }
    }

    fn convert_args(
        input: &std::path::Path,
        output: &std::path::Path,
        extra: &[OsString],
    ) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "convert".into(),
            input.into(),
            "--output".into(),
            output.into(),
            "--overwrite".into(),
        ];
        args.extend(extra.iter().cloned());
        args
    }

    /// Reconstruct the global plugin flags so child conversions follow the same policy.
    fn plugin_flags(policy: &RuntimePluginPolicy) -> Vec<OsString> {
        let mut flags: Vec<OsString> = Vec::new();
        if !policy.enabled {
            flags.push("--no-plugins".into());
        }
        flags.push("--plugin-timeout".into());
        flags.push(policy.timeout.as_secs().max(1).to_string().into());
        for kind in policy.allow_capabilities.iter().flatten() {
            flags.extend(["--allow-plugin-kind".into(), kind.into()]);
        }
        for kind in &policy.deny_capabilities {
            flags.extend(["--deny-plugin-kind".into(), kind.into()]);
        }
        flags.push("--plugin-sandbox".into());
        flags.push(policy.sandbox.mode.as_str().into());
        for path in &policy.sandbox.allow_read {
            flags.extend(["--plugin-sandbox-allow-read".into(), path.into()]);
        }
        flags
    }

    fn credential(args: &ImapArgs) -> Result<Credential, CliError> {
        let from_env = |var: &str, file_flag: &str| {
            std::env::var(var)
                .map_err(|_| fail(ExitClass::Usage, format!("set {var} or pass {file_flag}")))
        };
        Ok(match args.auth.as_str() {
            "xoauth2" => match &args.oauth_token_file {
                Some(path) => {
                    Credential::OAuth2TokenFile(tag(ExitClass::Usage, std::path::absolute(path))?)
                }
                None => Credential::OAuth2Token(from_env(TOKEN_ENV, "--oauth-token-file")?),
            },
            _ => match &args.password_file {
                Some(path) => Credential::Password(tag(ExitClass::Usage, read_password(path))?),
                None => Credential::Password(from_env(PASSWORD_ENV, "--password-file")?),
            },
        })
    }

    pub(crate) fn watch_imap(args: ImapArgs, policy: &RuntimePluginPolicy) -> Result<(), CliError> {
        let tls: TlsMode = tag(ExitClass::Usage, args.tls.parse())?;
        let credential = credential(&args)?;
        let senders = tag(
            ExitClass::Usage,
            SenderPolicy::new(&args.allow_from, args.require_dmarc.as_deref()),
        )?;
        let config = ImapConfig {
            host: args.host.clone(),
            port: args.port.unwrap_or(tls.default_port()),
            tls,
            user: args.user.clone(),
            credential,
            mailbox: args.mailbox.clone(),
            search: args.search.clone(),
            ca_file: args.ca_file.clone(),
            io_timeout: IO_TIMEOUT,
        };
        tag(ExitClass::Usage, config.validate())?;

        // Reject bad convert options now rather than failing every message later.
        let (mut sink, base): (Box<dyn MessageSink>, PathBuf) = match &args.queue {
            Some(dir) => {
                let convert = args
                    .convert_args
                    .iter()
                    .map(|a| a.clone().into_string())
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| fail(ExitClass::Usage, "convert options must be UTF-8"))?;
                check_convert_args(&convert)?;
                let queue = tag(ExitClass::Usage, Queue::open(dir))?;
                let base = queue.root().to_path_buf();
                (Box::new(QueueSink { queue, convert }), base)
            }
            None => {
                let probe = convert_args(
                    "message.eml".as_ref(),
                    "message.pdf".as_ref(),
                    &args.convert_args,
                );
                if let Err(e) =
                    Cli::try_parse_from(std::iter::once(OsString::from("anytopdf")).chain(probe))
                {
                    let first = e.to_string();
                    let first = first.lines().next().unwrap_or_default();
                    return Err(fail(
                        ExitClass::Usage,
                        format!("invalid convert options after `--`: {first}"),
                    ));
                }
                let dir = args
                    .output_dir
                    .as_ref()
                    .expect("clap requires --output-dir without --queue");
                let output_dir = tag(
                    ExitClass::Usage,
                    std::fs::create_dir_all(dir)
                        .and_then(|()| std::path::absolute(dir))
                        .with_context(|| format!("prepare --output-dir {}", dir.display())),
                )?;
                let sink = ConvertSink {
                    exe: std::env::current_exe().context("locate the anytopdf executable")?,
                    global: plugin_flags(policy),
                    output_dir: output_dir.clone(),
                    extra: args.convert_args.clone(),
                    timeout: Duration::from_secs(args.convert_timeout),
                };
                (Box::new(sink), output_dir)
            }
        };
        let state_dir = match &args.state_dir {
            Some(dir) => tag(ExitClass::Usage, std::path::absolute(dir))?,
            None => tag(ExitClass::Usage, std::path::absolute(base))?.join(".anytopdf-imap"),
        };
        let watcher = Watcher::new(
            &config.host,
            &config.user,
            &config.mailbox,
            WatchOptions {
                state_dir,
                backfill: args.backfill,
                max_message_bytes: args.max_message_bytes,
                max_attempts: args.max_attempts,
                mark_seen: args.mark_seen,
                move_to: args.move_to.clone(),
                keep_eml: args.keep_eml,
                senders,
                quiet: args.quiet,
            },
        );

        // Connect once up front so bad credentials or TLS problems fail immediately
        // instead of entering the reconnect loop.
        let mut first = Some(tag(ExitClass::Provider, connect(&config))?);
        if args.once {
            let mut mailbox = first.take().expect("connected above");
            let report = tag(
                ExitClass::Provider,
                watcher.poll_once(&mut mailbox, sink.as_mut()),
            )?;
            if !args.quiet {
                eprintln!(
                    "imap: {} {}, {} to retry, {} skipped, {} refused by sender policy",
                    report.delivered.len(),
                    if args.queue.is_some() {
                        "queued"
                    } else {
                        "converted"
                    },
                    report.failed.len(),
                    report.parked.len(),
                    report.rejected.len()
                );
            }
            return Ok(());
        }
        let mut connect_next = || -> Result<Box<dyn Mailbox>> {
            Ok(match first.take() {
                Some(mailbox) => Box::new(mailbox),
                None => Box::new(connect(&config)?),
            })
        };
        watcher.run(
            &mut connect_next,
            sink.as_mut(),
            Duration::from_secs(args.poll_interval),
        )?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::collections::BTreeSet;

        #[test]
        fn child_conversions_inherit_plugin_policy() {
            let policy = RuntimePluginPolicy {
                enabled: false,
                timeout: Duration::from_secs(30),
                allow_capabilities: Some(BTreeSet::from(["importer".to_string()])),
                deny_capabilities: BTreeSet::from(["renderer".to_string()]),
                sandbox: anytopdf_core::SandboxPolicy {
                    mode: anytopdf_core::SandboxMode::Strict,
                    allow_read: vec!["/opt/models".into()],
                },
            };
            let flags: Vec<String> = plugin_flags(&policy)
                .into_iter()
                .map(|f| f.to_string_lossy().into_owned())
                .collect();
            assert_eq!(
                flags,
                [
                    "--no-plugins",
                    "--plugin-timeout",
                    "30",
                    "--allow-plugin-kind",
                    "importer",
                    "--deny-plugin-kind",
                    "renderer",
                    "--plugin-sandbox",
                    "strict",
                    "--plugin-sandbox-allow-read",
                    "/opt/models"
                ]
            );
        }

        #[test]
        fn child_conversions_do_not_receive_the_password() {
            let sink = ConvertSink {
                exe: "anytopdf".into(),
                global: Vec::new(),
                output_dir: ".".into(),
                extra: vec!["--ocr".into(), "off".into()],
                timeout: Duration::from_secs(1),
            };
            let command = sink.command("m.eml".as_ref(), "m.pdf".as_ref());
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| key == PASSWORD_ENV && value.is_none())
            );
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| key == TOKEN_ENV && value.is_none())
            );
            let args: Vec<_> = command.get_args().collect();
            assert_eq!(args[0], "convert");
            assert_eq!(args.last().unwrap(), &"off");
        }
    }
}
