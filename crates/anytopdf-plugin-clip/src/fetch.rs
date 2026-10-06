//! `--fetch-model`: download the default CLIP model with pinned checksums.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::Path,
};

/// OpenAI CLIP ViT-B/32 (MIT) as exported to ONNX by Jina AI's clip-as-service
/// (Apache-2.0), plus OpenAI's tokenizer merges.
pub const DEFAULT_FILES: &[ModelFile] = &[
    ModelFile {
        name: "visual.onnx",
        url: "https://clip-as-service.s3.us-east-2.amazonaws.com/models-436c69702d61732d53657276696365/onnx/ViT-B-32/visual.onnx",
        sha256: "06395063c0a5c28b1a8d4bd585261501a878c8f52d1216db6c4cbb651f7c13f1",
    },
    ModelFile {
        name: "textual.onnx",
        url: "https://clip-as-service.s3.us-east-2.amazonaws.com/models-436c69702d61732d53657276696365/onnx/ViT-B-32/textual.onnx",
        sha256: "0af04c287a3be2570eaef7a1ef896d81c1989602df67a8905941afed589e545e",
    },
    ModelFile {
        name: "bpe_simple_vocab_16e6.txt.gz",
        url: "https://raw.githubusercontent.com/openai/CLIP/main/clip/bpe_simple_vocab_16e6.txt.gz",
        sha256: "924691ac288e54409236115652ad4aa250f48203de50a9e4722a6ecd48d6804a",
    },
];

pub struct ModelFile {
    pub name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
}

fn sha256_of(reader: &mut impl Read) -> Result<String> {
    let mut hasher = Sha256::new();
    io::copy(reader, &mut HashWriter(&mut hasher))?;
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

struct HashWriter<'a>(&'a mut Sha256);
impl Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Downloads each file into `dir` unless a copy with the right checksum is
/// already there. `mirror` replaces every URL's directory with another base URL
/// (for air-gapped mirrors). Each file is verified before it is moved into place.
pub fn fetch(dir: &Path, files: &[ModelFile], mirror: Option<&str>) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let agent = ureq::Agent::new_with_defaults();
    for file in files {
        let target = dir.join(file.name);
        if let Ok(mut existing) = File::open(&target)
            && sha256_of(&mut existing)? == file.sha256
        {
            eprintln!("{}: already present", file.name);
            continue;
        }
        let url = match mirror {
            Some(base) => format!("{}/{}", base.trim_end_matches('/'), file.name),
            None => file.url.to_string(),
        };
        eprintln!("{}: downloading {url}", file.name);
        let partial = dir.join(format!("{}.part", file.name));
        let result = (|| -> Result<()> {
            let mut response = agent
                .get(&url)
                .call()
                .with_context(|| format!("GET {url}"))?;
            let mut reader = response.body_mut().as_reader();
            let mut out = File::create(&partial)?;
            let mut hasher = Sha256::new();
            let mut buffer = vec![0u8; 1 << 16];
            loop {
                let n = reader.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
                out.write_all(&buffer[..n])?;
            }
            out.sync_all()?;
            let digest = hex(&hasher.finalize());
            if digest != file.sha256 {
                bail!(
                    "{} checksum mismatch: expected {}, got {digest}",
                    file.name,
                    file.sha256
                );
            }
            Ok(())
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(&partial);
            return Err(e);
        }
        fs::rename(&partial, &target)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::BufRead, io::BufReader, net::TcpListener, thread};

    /// Serves `body` for every request on a local port.
    fn serve(body: &'static [u8], requests: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming().take(requests) {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap() > 2 {
                    line.clear();
                }
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            }
        });
        format!("http://{address}")
    }

    fn file(sha256: &'static str) -> ModelFile {
        ModelFile {
            name: "model.onnx",
            url: "http://invalid.example/model.onnx",
            sha256,
        }
    }

    // SHA-256 of b"weights".
    const WEIGHTS: &str = "9a129038d9a00aed0cf6a7ea059ca50a813449061ab87848cf1a13eafdf33b2c";

    #[test]
    fn downloads_and_verifies_from_a_mirror() {
        assert_eq!(sha256_of(&mut &b"weights"[..]).unwrap(), WEIGHTS);
        let dir = tempfile::tempdir().unwrap();
        let mirror = serve(b"weights", 1);
        fetch(dir.path(), &[file(WEIGHTS)], Some(&mirror)).unwrap();
        assert_eq!(fs::read(dir.path().join("model.onnx")).unwrap(), b"weights");
        assert!(!dir.path().join("model.onnx.part").exists());
        // A verified copy is not downloaded again (the server is gone now).
        fetch(dir.path(), &[file(WEIGHTS)], Some("http://127.0.0.1:9")).unwrap();
    }

    #[test]
    fn rejects_a_checksum_mismatch_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let mirror = serve(b"tampered", 1);
        let err = fetch(dir.path(), &[file(WEIGHTS)], Some(&mirror)).unwrap_err();
        assert!(format!("{err:#}").contains("checksum mismatch"));
        assert!(!dir.path().join("model.onnx").exists());
        assert!(!dir.path().join("model.onnx.part").exists());
    }
}
