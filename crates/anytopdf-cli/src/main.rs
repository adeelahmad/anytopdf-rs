mod argv;
mod capabilities;
mod cli;
mod commands;
mod convert;
mod events;
mod exit;
mod extract;
mod naming;
mod publish;
mod queue;
use anytopdf_core::RuntimePluginPolicy;
use clap::Parser;
use cli::{Cli, Commands};
use commands::{doctor, plugins, probe};
use convert::convert;
use exit::{CliError, ExitClass};
use std::{process::ExitCode, time::Duration};

fn main() -> ExitCode {
    let cli = match Cli::try_parse_from(argv::normalize_args(std::env::args_os().collect())) {
        Ok(cli) => cli,
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
    let events = matches!(&cli.command, Commands::Convert(a) if a.events);
    match run(cli) {
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

fn run(cli: Cli) -> Result<(), CliError> {
    let mut global_args = vec![format!("--plugin-timeout={}", cli.plugin_timeout)];
    if cli.no_plugins {
        global_args.push("--no-plugins".into());
    }
    for kind in &cli.allow_plugin_kind {
        global_args.push(format!("--allow-plugin-kind={kind}"));
    }
    for kind in &cli.deny_plugin_kind {
        global_args.push(format!("--deny-plugin-kind={kind}"));
    }
    let policy = RuntimePluginPolicy {
        enabled: !cli.no_plugins,
        timeout: Duration::from_secs(cli.plugin_timeout),
        allow_capabilities: (!cli.allow_plugin_kind.is_empty())
            .then(|| cli.allow_plugin_kind.into_iter().collect()),
        deny_capabilities: cli.deny_plugin_kind.into_iter().collect(),
    };
    match cli.command {
        Commands::Convert(args) => convert(*args, &policy),
        Commands::Doctor { json } => Ok(doctor(json)?),
        Commands::Plugins { json } => Ok(plugins(&policy, json)?),
        Commands::Extract { pdf, .. } => {
            let doc = extract::extract(&pdf)?;
            println!("{}", serde_json::to_string_pretty(&doc)?);
            Ok(())
        }
        Commands::Capabilities { .. } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&capabilities::capabilities()?)?
            );
            Ok(())
        }
        Commands::Probe { input, .. } => probe(&input, &policy),
        Commands::Queue { command } => queue::run(command, global_args),
    }
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
            Cli::try_parse_from(["anytopdf"]).unwrap_err().kind(),
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
        use clap::CommandFactory;
        let mut offenders = Vec::new();
        help_offenders(&Cli::command(), "anytopdf", &mut offenders);
        assert!(
            offenders.is_empty(),
            "missing help text: {}",
            offenders.join(", ")
        );
    }
}
