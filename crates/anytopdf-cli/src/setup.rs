//! `anytopdf setup whisper`: install a checksummed whisper.cpp model into the
//! user data dir and record it, so the bundled Whisper plugin turns itself on.

use crate::cli::SetupWhisperArgs;
use crate::exit::{CliError, ExitClass, fail, tag};
use anyhow::{Context, Result};
use anytopdf_core::{RuntimePluginPolicy, discover_runtime_plugins_detailed, user_data_dir};
use anytopdf_plugin_whisper::{backend, setup};
use std::{
    fs,
    io::{IsTerminal, Write},
    path::Path,
    time::Duration,
};

pub(crate) fn whisper(
    args: SetupWhisperArgs,
    policy: &RuntimePluginPolicy,
) -> Result<(), CliError> {
    let model = setup::find_model(&args.model).ok_or_else(|| {
        fail(
            ExitClass::Usage,
            format!("unknown Whisper model {:?}", args.model),
        )
    })?;
    let data_dir = user_data_dir().ok_or_else(|| {
        fail(
            ExitClass::Usage,
            "no home folder to install into; set ANYTOPDF_DATA_DIR",
        )
    })?;
    let destination = setup::whisper_dir(&data_dir).join(setup::file_name(model));
    let mut err = std::io::stderr();

    let existing = (!args.force && args.from.is_none() && destination.is_file())
        .then(|| verify_existing(&destination, model))
        .flatten();
    let digests = match existing {
        Some(digests) => {
            let _ = writeln!(
                err,
                "{} is already installed and matches its checksum",
                destination.display()
            );
            digests
        }
        None => match &args.from {
            Some(from) => {
                let file = tag(
                    ExitClass::Input,
                    fs::File::open(from).with_context(|| format!("open {}", from.display())),
                )?;
                let _ = writeln!(
                    err,
                    "installing {} from {}",
                    setup::file_name(model),
                    from.display()
                );
                tag(
                    ExitClass::Input,
                    setup::install(file, &destination, Some(model.sha1), |_| {}),
                )?
            }
            None => {
                let url = setup::download_url(&args.base_url, model);
                tag(ExitClass::Provider, download(&url, &destination, model))?
            }
        },
    };
    setup::write_record(
        &data_dir,
        &setup::Record {
            schema_version: setup::RECORD_SCHEMA.into(),
            model: model.name.into(),
            path: destination.clone(),
            sha256: digests.sha256.clone(),
        },
    )?;
    println!(
        "Whisper model {} installed at {}",
        model.name,
        destination.display()
    );
    println!("SHA-256 {}", digests.sha256);
    report(policy);
    Ok(())
}

/// Re-hashes an installed model; `None` means it must be fetched again.
fn verify_existing(path: &Path, model: &setup::Model) -> Option<setup::Digests> {
    setup::digest_file(path)
        .ok()
        .filter(|d| d.sha1.eq_ignore_ascii_case(model.sha1))
}

fn download(url: &str, destination: &Path, model: &setup::Model) -> Result<setup::Digests> {
    let mut err = std::io::stderr();
    let _ = writeln!(
        err,
        "downloading {} (about {} MiB) from {url}",
        setup::file_name(model),
        model.size_mib
    );
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(120)))
        .user_agent(concat!("anytopdf/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let response = agent
        .get(url)
        .call()
        .with_context(|| format!("download {url}"))?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let reader = response.into_body().into_reader();
    let live = err.is_terminal();
    let mut shown = 0;
    let digests = setup::install(reader, destination, Some(model.sha1), |done| {
        let percent = total.map(|t| done * 100 / t.max(1));
        let step = percent.unwrap_or(done >> 24);
        if live && step != shown {
            shown = step;
            let _ = match percent {
                Some(p) => write!(err, "\r  {p:>3}% of {} MiB", total.unwrap_or(0) >> 20),
                None => write!(err, "\r  {} MiB", done >> 20),
            };
        }
    })
    .with_context(|| format!("install {}", setup::file_name(model)))?;
    if live {
        let _ = writeln!(err);
    }
    Ok(digests)
}

/// Says whether transcription now works, and exactly what is still missing.
fn report(policy: &RuntimePluginPolicy) {
    let readiness =
        backend::readiness(&backend::Config::from_env(), |name| which::which(name).ok());
    let found = discover_runtime_plugins_detailed(policy);
    let plugin = found
        .plugins
        .iter()
        .chain(&found.idle)
        .find(|p| p.manifest.name == "whisper");
    match (plugin, readiness.ready) {
        (Some(_), true) => println!(
            "Transcription is ready ({}). Audio and video conversions now include a transcript.",
            readiness.detail
        ),
        (None, _) if !policy.enabled => {
            println!(
                "Runtime plugins are disabled by --no-plugins, so nothing will be transcribed."
            )
        }
        (None, _) => println!(
            "Still missing: the anytopdf-plugin-whisper executable. Release archives ship it in \
             plugins/ beside anytopdf; from source, run `cargo build --release -p \
             anytopdf-plugin-whisper` and add target/release to ANYTOPDF_PLUGIN_PATH."
        ),
        (Some(_), false) => println!("Still missing: {}", readiness.detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::BufRead, net::TcpListener, thread};

    /// Serves `body` once over plain HTTP and returns the base URL.
    fn serve_once(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        format!("http://{address}")
    }

    #[test]
    fn download_streams_into_place_and_checks_the_published_sha1() {
        let dir = tempfile::tempdir().unwrap();
        let model = setup::Model {
            name: "test",
            size_mib: 1,
            sha1: "a9993e364706816aba3e25717850c26c9cd0d89d",
        };
        let destination = dir.path().join("ggml-test.bin");
        let url = format!("{}/ggml-test.bin", serve_once(b"abc".to_vec()));
        let digests = download(&url, &destination, &model).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"abc");
        assert_eq!(digests.sha1, model.sha1);

        let tampered = format!("{}/ggml-test.bin", serve_once(b"abd".to_vec()));
        let other = dir.path().join("ggml-other.bin");
        let error = download(&tampered, &other, &model).unwrap_err();
        assert!(
            format!("{error:#}").contains("checksum mismatch"),
            "{error:#}"
        );
        assert!(!other.exists());
    }

    #[test]
    fn installed_models_are_rehashed_before_reuse() {
        let dir = tempfile::tempdir().unwrap();
        let model = setup::Model {
            name: "test",
            size_mib: 1,
            sha1: "a9993e364706816aba3e25717850c26c9cd0d89d",
        };
        let path = dir.path().join("ggml-test.bin");
        fs::write(&path, b"abc").unwrap();
        assert!(verify_existing(&path, &model).is_some());
        fs::write(&path, b"corrupt").unwrap();
        assert!(verify_existing(&path, &model).is_none());
        assert_eq!(fs::read(&path).unwrap(), b"corrupt");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn unreachable_mirrors_fail_without_leaving_files() {
        let dir = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/ggml-base.bin", listener.local_addr().unwrap());
        drop(listener);
        let destination = dir.path().join("ggml-base.bin");
        assert!(download(&url, &destination, &setup::MODELS[2]).is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
