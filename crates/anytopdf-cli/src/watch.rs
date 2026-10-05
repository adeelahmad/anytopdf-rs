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
    use anyhow::{Context, Result, bail};
    use anytopdf_core::CommandExt;
    use anytopdf_imap::{
        Delivery, ImapConfig, Mailbox, MessageSink, SpooledMessage, TlsMode, WatchOptions, Watcher,
        connect, read_password,
    };
    use clap::Parser;
    use std::{ffi::OsString, path::PathBuf, process::Command, time::Duration};

    const PASSWORD_ENV: &str = "ANYTOPDF_IMAP_PASSWORD";
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
                // Plugins run by the converter must not inherit the mailbox password.
                .env_remove(PASSWORD_ENV);
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
        flags
    }

    pub(crate) fn watch_imap(args: ImapArgs, policy: &RuntimePluginPolicy) -> Result<(), CliError> {
        let tls: TlsMode = tag(ExitClass::Usage, args.tls.parse())?;
        let password = match &args.password_file {
            Some(path) => tag(ExitClass::Usage, read_password(path))?,
            None => std::env::var(PASSWORD_ENV).map_err(|_| {
                fail(
                    ExitClass::Usage,
                    format!("set {PASSWORD_ENV} or pass --password-file"),
                )
            })?,
        };
        let config = ImapConfig {
            host: args.host.clone(),
            port: args.port.unwrap_or(tls.default_port()),
            tls,
            user: args.user.clone(),
            password,
            mailbox: args.mailbox.clone(),
            search: args.search.clone(),
            ca_file: args.ca_file.clone(),
            io_timeout: IO_TIMEOUT,
        };
        tag(ExitClass::Usage, config.validate())?;

        // Reject bad convert options now rather than failing every message later.
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

        let output_dir = tag(
            ExitClass::Usage,
            std::fs::create_dir_all(&args.output_dir)
                .and_then(|()| std::path::absolute(&args.output_dir))
                .with_context(|| format!("prepare --output-dir {}", args.output_dir.display())),
        )?;
        let state_dir = match &args.state_dir {
            Some(dir) => tag(ExitClass::Usage, std::path::absolute(dir))?,
            None => output_dir.join(".anytopdf-imap"),
        };
        let mut sink = ConvertSink {
            exe: std::env::current_exe().context("locate the anytopdf executable")?,
            global: plugin_flags(policy),
            output_dir,
            extra: args.convert_args.clone(),
            timeout: Duration::from_secs(args.convert_timeout),
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
                watcher.poll_once(&mut mailbox, &mut sink),
            )?;
            if !args.quiet {
                eprintln!(
                    "imap: {} converted, {} to retry, {} skipped",
                    report.delivered.len(),
                    report.failed.len(),
                    report.parked.len()
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
            &mut sink,
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
                    "renderer"
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
            let args: Vec<_> = command.get_args().collect();
            assert_eq!(args[0], "convert");
            assert_eq!(args.last().unwrap(), &"off");
        }
    }
}
