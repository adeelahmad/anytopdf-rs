use crate::cli::{PrintCommand, RemoteArgs};
use crate::exit::{CliError, ExitClass, fail, tag};
use anyhow::Context;
use anytopdf_print::{
    Allowlist, Exposure, Front, FrontConfig, Receipts, ServiceSpec, TAILNET_RANGES, Users, check,
    load_tls,
};
use std::sync::Arc;
use std::time::Duration;

pub(crate) fn print(command: PrintCommand) -> Result<(), CliError> {
    match command {
        PrintCommand::Remote(args) => remote(*args),
        PrintCommand::Passwd {
            user,
            users,
            delete,
        } => {
            let mut list = tag(ExitClass::Input, Users::load_or_default(&users))?;
            if delete {
                if !list.remove(&user) {
                    return Err(fail(
                        ExitClass::Input,
                        format!("no print user named {user:?}"),
                    ));
                }
            } else {
                let mut password = String::new();
                std::io::stdin()
                    .read_line(&mut password)
                    .context("cannot read the password from stdin")?;
                let password = password.trim_end_matches(['\r', '\n']);
                tag(ExitClass::Usage, list.set_password(&user, password))?;
            }
            tag(ExitClass::Input, list.save(&users))?;
            Ok(())
        }
        PrintCommand::DnsSd {
            domain,
            host,
            port,
            name,
            addresses,
            json,
        } => {
            let spec = ServiceSpec::new(&name, &host, port);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&spec.json(&domain, &addresses))?
                );
            } else {
                print!("{}", spec.zone(&domain, &addresses));
            }
            Ok(())
        }
        PrintCommand::Advertise {
            port,
            name,
            host,
            addresses,
        } => {
            if !host.trim_end_matches('.').ends_with(".local") {
                return Err(fail(ExitClass::Usage, "--host must end in .local"));
            }
            let spec = ServiceSpec::new(&name, &host, port);
            let _advertisement = tag(
                ExitClass::Provider,
                anytopdf_print::advertise(&spec, &host, &addresses),
            )?;
            eprintln!(
                "advertising {name:?} on the local network as {}; stop with Ctrl-C",
                spec.url()
            );
            loop {
                std::thread::park();
            }
        }
        PrintCommand::Url { host, port } => {
            println!("{}", ServiceSpec::new("anytopdf", &host, port).url());
            Ok(())
        }
    }
}

fn remote(args: RemoteArgs) -> Result<(), CliError> {
    let mut cidrs = args.allow.clone();
    if args.allow_tailnet {
        cidrs.extend(TAILNET_RANGES.iter().map(|s| s.to_string()));
    }
    let allow = tag(ExitClass::Usage, Allowlist::parse(&cidrs))?;
    let users = match &args.users {
        Some(path) => tag(ExitClass::Input, Users::load(path))?,
        None => Users::new(),
    };
    let problems = check(&Exposure {
        listen: args.listen,
        upstream: args.upstream,
        has_users: !users.is_empty(),
        allow: &allow,
        allow_public_bind: args.allow_public_bind,
    });
    if !problems.is_empty() {
        let text: Vec<String> = problems.iter().map(ToString::to_string).collect();
        return Err(fail(
            ExitClass::Usage,
            format!("refusing to start the remote front: {}", text.join("; ")),
        ));
    }
    let tls = tag(ExitClass::Input, load_tls(&args.tls_cert, &args.tls_key))?;
    let receipts = match &args.receipts {
        Some(path) => Some(tag(ExitClass::Input, Receipts::open(path))?),
        None => None,
    };
    let front = tag(
        ExitClass::Provider,
        Front::bind(FrontConfig {
            listen: args.listen,
            upstream: args.upstream,
            tls,
            users: Arc::new(users),
            allow,
            max_connections: usize::from(args.max_connections),
            idle_timeout: Duration::from_secs(args.idle_timeout),
            receipts,
        }),
    )?;
    let addr = front.local_addr()?;
    eprintln!(
        "remote printing on ipps://{addr}/{} -> {}",
        anytopdf_print::RESOURCE,
        args.upstream
    );
    Ok(front.serve()?)
}

pub(crate) struct PrintingStatus {
    pub(crate) name: &'static str,
    pub(crate) available: bool,
    pub(crate) path: Option<std::path::PathBuf>,
    pub(crate) detail: &'static str,
}

/// What `doctor` reports about printing: the built-in remote front, the
/// print helper and Tailscale (for the tailnet address and `tailscale cert`).
pub(crate) fn printing_status() -> Vec<PrintingStatus> {
    let helper = which::which("anytopdf-printer").ok();
    let tailscale = which::which("tailscale").ok();
    vec![
        PrintingStatus {
            name: "remote-front",
            available: true,
            path: None,
            detail: "built in: anytopdf print remote",
        },
        PrintingStatus {
            name: "anytopdf-printer",
            available: helper.is_some(),
            detail: if helper.is_some() {
                "IPP print helper the remote front forwards to"
            } else {
                "not on PATH; build helpers/anytopdf-printer to receive print jobs"
            },
            path: helper,
        },
        PrintingStatus {
            name: "tailscale",
            available: tailscale.is_some(),
            detail: if tailscale.is_some() {
                "tailnet address for --listen; tailscale cert for --tls-cert"
            } else {
                "not on PATH; listen on a WireGuard address instead"
            },
            path: tailscale,
        },
    ]
}

/// Print-job details the print helper passes to `convert` in `ANYTOPDF_PRINT_*`
/// variables, as source metadata for the manifest and provenance page. Empty
/// unless a job id is present, so ordinary conversions are unaffected. The
/// `share` profile drops these keys like any other non-allowlisted metadata.
pub(crate) fn job_metadata(lookup: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    if lookup("ANYTOPDF_PRINT_JOB_ID").is_none_or(|id| id.trim().is_empty()) {
        return Vec::new();
    }
    [
        ("ANYTOPDF_PRINT_JOB_ID", "print.job-id"),
        ("ANYTOPDF_PRINT_JOB_NAME", "print.job-name"),
        ("ANYTOPDF_PRINT_USER", "print.user"),
        ("ANYTOPDF_PRINT_FORMAT", "print.format"),
    ]
    .into_iter()
    .filter_map(|(var, key)| {
        let value = lookup(var)?;
        let value = value.trim();
        (!value.is_empty()).then(|| (key.to_string(), value.to_string()))
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::job_metadata;

    #[test]
    fn job_metadata_needs_a_job_id_and_skips_blank_values() {
        assert!(job_metadata(|_| None).is_empty());
        let env = |key: &str| match key {
            "ANYTOPDF_PRINT_JOB_ID" => Some("12".to_string()),
            "ANYTOPDF_PRINT_USER" => Some("adeel".to_string()),
            "ANYTOPDF_PRINT_JOB_NAME" => Some(" ".to_string()),
            _ => None,
        };
        assert_eq!(
            job_metadata(env),
            [
                ("print.job-id".to_string(), "12".to_string()),
                ("print.user".to_string(), "adeel".to_string())
            ]
        );
    }
}
