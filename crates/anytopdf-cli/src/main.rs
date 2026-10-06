mod argv;
mod capabilities;
mod capture;
mod cli;
mod commands;
mod config;
mod convert;
mod environment;
mod events;
mod exit;
mod extract;
mod mcp;
mod naming;
mod print;
mod publish;
mod queue;
mod watch;
use anytopdf_core::{RuntimePluginPolicy, SandboxPolicy, validate_sandbox_policy};
use clap::FromArgMatches;
use cli::{CaptureCommand, Cli, Commands, QueueCommand, WatchSource};
use commands::{doctor, plugins, probe};
use convert::convert;
use exit::{CliError, ExitClass};
use std::{process::ExitCode, time::Duration};

fn main() -> ExitCode {
    let parsed = cli::command()
        .try_get_matches_from(argv::normalize_args(std::env::args_os().collect()))
        .and_then(|matches| Ok((Cli::from_arg_matches(&matches)?, matches)));
    let (mut cli, matches) = match parsed {
        Ok(parsed) => parsed,
        Err(e) => {
            let class = if e.use_stderr() {
                ExitClass::Usage
            } else {
                ExitClass::Success
            };
            let _ = e.print();
            return ExitCode::from(class.code());
        }
    };
    let events = match &cli.command {
        Commands::Convert(a) => a.events,
        Commands::Capture(CaptureCommand::Screen(a)) => a.convert.iter().any(|x| x == "--events"),
        _ => false,
    };
    let configured = configure(&mut cli, &matches);
    match configured.and_then(|resolved| run(cli, resolved)) {
        Ok(()) => ExitCode::from(ExitClass::Success.code()),
        Err(e) => {
            if !events {
                use std::io::Write;
                let _ = writeln!(std::io::stderr(), "error: {:#}", e.error);
            }
            ExitCode::from(e.class.code())
        }
    }
}

/// Resolves the layered configuration and writes it into `cli`.
fn configure(cli: &mut Cli, matches: &clap::ArgMatches) -> Result<config::Resolved, CliError> {
    let resolved = exit::tag(
        ExitClass::Usage,
        config::Resolved::load(cli, matches, &config::Environment::current()),
    )?;
    exit::tag(ExitClass::Usage, config::apply(cli, matches, &resolved))?;
    Ok(resolved)
}

fn run(cli: Cli, resolved: config::Resolved) -> Result<(), CliError> {
    let forwarded = mcp::Forwarded::from_cli(&cli);
    let config_flags = config::forward_flags(&cli);
    let sandbox_mode = cli.sandbox_mode();
    let policy = RuntimePluginPolicy {
        enabled: !cli.no_plugins,
        timeout: Duration::from_secs(cli.plugin_timeout),
        allow_capabilities: (!cli.allow_plugin_kind.is_empty())
            .then(|| cli.allow_plugin_kind.into_iter().collect()),
        deny_capabilities: cli.deny_plugin_kind.into_iter().collect(),
        sandbox: SandboxPolicy {
            mode: sandbox_mode,
            allow_read: cli.plugin_sandbox_allow_read,
        },
        options: std::sync::Arc::new(resolved.tables()),
    };
    match cli.command {
        Commands::Convert(args) => convert(*args, &policy),
        Commands::Doctor { json } => Ok(doctor(json)?),
        Commands::Plugins { json } => {
            check_sandbox(&policy)?;
            Ok(plugins(&policy, json)?)
        }
        Commands::Extract { pdf, .. } => {
            let doc = extract::extract(&pdf)?;
            println!("{}", serde_json::to_string_pretty(&doc)?);
            Ok(())
        }
        Commands::Capabilities { json: true } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&capabilities::capabilities()?)?
            );
            Ok(())
        }
        Commands::Capabilities { json: false } => {
            let probe = environment::Probe::detect(&policy);
            print!("{}", environment::render(&environment::build(&probe)));
            Ok(())
        }
        Commands::Probe { input, .. } => {
            check_sandbox(&policy)?;
            probe(&input, &policy)
        }
        Commands::Watch {
            source: WatchSource::Imap(args),
        } => {
            check_sandbox(&policy)?;
            watch::watch_imap(*args, &policy, config_flags)
        }
        Commands::Queue { command } => {
            if matches!(command, QueueCommand::Work(_)) {
                check_sandbox(&policy)?;
            }
            queue::run(command, forwarded.0)
        }
        Commands::Mcp => Ok(mcp::serve(forwarded)?),
        Commands::Config { json: true, .. } => {
            println!("{}", serde_json::to_string_pretty(&resolved.report())?);
            Ok(())
        }
        Commands::Config { defaults, .. } => {
            let shown = if defaults {
                config::Resolved::defaults()
            } else {
                resolved
            };
            print!("{}", shown.render(!defaults));
            Ok(())
        }
        Commands::Print(command) => print::print(command),
        Commands::Capture(CaptureCommand::Screen(args)) => {
            capture::screen(*args, &policy, &resolved)
        }
    }
}

/// Refuses a sandbox level the platform cannot enforce before any plugin runs.
fn check_sandbox(policy: &RuntimePluginPolicy) -> Result<(), CliError> {
    if policy.enabled {
        exit::tag(ExitClass::Usage, validate_sandbox_policy(&policy.sandbox))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::{checked_destination, publish_output};

    #[test]
    fn source_is_protected_even_with_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("source.txt");
        std::fs::write(&input, "keep").unwrap();
        assert!(checked_destination(&input, &[input.canonicalize().unwrap()], true).is_err());
    }

    #[test]
    fn no_arguments_shows_help_without_converting_current_directory() {
        assert_eq!(
            <Cli as clap::Parser>::try_parse_from(["anytopdf"])
                .unwrap_err()
                .kind(),
            clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }

    #[test]
    fn publication_does_not_clobber_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.pdf");
        std::fs::write(&path, "keep").unwrap();
        assert!(publish_output(&path, b"replace", false).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "keep");
    }

    fn help_offenders(cmd: &clap::Command, path: &str, out: &mut Vec<String>) {
        for arg in cmd.get_arguments() {
            if matches!(arg.get_id().as_str(), "help" | "version") {
                continue;
            }
            let has = |s: Option<&clap::builder::StyledStr>| {
                s.is_some_and(|text| !text.to_string().trim().is_empty())
            };
            if !has(arg.get_help()) && !has(arg.get_long_help()) {
                out.push(format!("{path} arg `{}`", arg.get_id()));
            }
        }
        for sub in cmd.get_subcommands() {
            if sub.get_name() == "help" {
                continue;
            }
            let sub_path = format!("{path} {}", sub.get_name());
            let about = sub.get_about().map(|a| a.to_string()).unwrap_or_default();
            if about.trim().is_empty() {
                out.push(format!("{sub_path} subcommand about"));
            }
            help_offenders(sub, &sub_path, out);
        }
    }

    #[test]
    fn every_subcommand_and_argument_has_help() {
        let mut offenders = Vec::new();
        help_offenders(&cli::command(), "anytopdf", &mut offenders);
        assert!(
            offenders.is_empty(),
            "missing help text: {}",
            offenders.join(", ")
        );
    }
}
