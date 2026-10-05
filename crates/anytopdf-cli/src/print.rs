use crate::cli::{PrintCommand, RemoteArgs};
use crate::exit::{CliError, ExitClass, fail, tag};
use anyhow::Context;
use anytopdf_print::{
    Allowlist, Exposure, Front, FrontConfig, ServiceSpec, TAILNET_RANGES, Users, check, load_tls,
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
