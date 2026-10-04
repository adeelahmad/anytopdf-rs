use anytopdf_core::schema;
use anytopdf_pdf::{EmbeddedFile, read_embedded_files};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (!(crc & 1)).wrapping_add(1));
        }
    }
    !crc
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend(data);
    out.extend(&body);
    out.extend(crc32(&body).to_be_bytes());
}

/// A width x height 8-bit grayscale PNG using stored deflate blocks.
fn write_png(path: &Path, width: u32, height: u32) {
    let mut raw = Vec::new();
    for _ in 0..height {
        raw.push(0);
        raw.extend(std::iter::repeat_n(128u8, width as usize));
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + *byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    let mut z = vec![0x78, 0x01, 0x01];
    z.extend((raw.len() as u16).to_le_bytes());
    z.extend((!(raw.len() as u16)).to_le_bytes());
    z.extend(&raw);
    z.extend(((b << 16) | a).to_be_bytes());
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend(width.to_be_bytes());
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 0, 0, 0, 0]);
    png_chunk(&mut png, b"IHDR", &ihdr);
    png_chunk(&mut png, b"IDAT", &z);
    png_chunk(&mut png, b"IEND", &[]);
    fs::write(path, png).unwrap();
}

fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend(((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

fn page_count(pdf: &[u8]) -> u64 {
    let text = String::from_utf8_lossy(pdf);
    regex::Regex::new(r"/Type\s*/Page\b")
        .unwrap()
        .find_iter(&text)
        .count() as u64
}

fn convert(inputs: &[&Path], pdf: &Path, extra: &[&str], dump: Option<&Path>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    cmd.arg("--no-plugins")
        .env("PATH", "")
        .env_remove("SOURCE_DATE_EPOCH")
        .arg("convert")
        .args(inputs)
        .args(["--ocr", "off", "-o"])
        .arg(pdf);
    if let Some(dump) = dump {
        cmd.arg("--dump-graph").arg(dump);
    }
    let out = cmd.args(extra).output().unwrap();
    assert!(
        out.status.success(),
        "convert failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn load_schema(name: &str) -> Value {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root
        .ancestors()
        .map(|dir| dir.join("schemas").join(format!("{name}.schema.json")))
        .find(|candidate| candidate.is_file())
        .expect("schema file exists");
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn assert_valid(name: &str, instance: &Value) {
    let errors = schema::validate(&load_schema(name), instance);
    assert!(errors.is_empty(), "{name} schema errors: {errors:?}");
}

fn attachments(pdf: &Path) -> Vec<EmbeddedFile> {
    read_embedded_files(&fs::read(pdf).unwrap()).unwrap()
}

fn find<'a>(files: &'a [EmbeddedFile], name: &str) -> &'a EmbeddedFile {
    files
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("attachment {name} missing; found {:?}", names(files)))
}

fn names(files: &[EmbeddedFile]) -> Vec<&str> {
    files.iter().map(|f| f.name.as_str()).collect()
}

fn json_of(file: &EmbeddedFile) -> Value {
    serde_json::from_slice(&file.bytes).unwrap()
}

fn strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
        Value::Object(o) => o.values().for_each(|v| strings(v, out)),
        _ => {}
    }
}

fn setup(dir: &Path) -> PathBuf {
    let notes = dir.join("notes.txt");
    fs::write(&notes, "Identity fixture\n").unwrap();
    notes
}

#[test]
fn converted_pdf_embeds_schema_valid_manifest_and_chunks() {
    let tmp = tempfile::tempdir().unwrap();
    let notes = setup(tmp.path());
    let pdf = tmp.path().join("out.pdf");
    convert(&[&notes], &pdf, &[], None);

    let files = attachments(&pdf);
    let mut got = names(&files);
    got.sort();
    assert_eq!(got, ["anytopdf-chunks.json", "anytopdf-manifest.json"]);
    let manifest = json_of(find(&files, "anytopdf-manifest.json"));
    let chunks = json_of(find(&files, "anytopdf-chunks.json"));
    assert_valid("manifest", &manifest);
    assert_valid("chunks", &chunks);
    let source = &manifest["sources"][0];
    let digest = sha256_hex(
        b"Identity fixture
",
    );
    assert_eq!(source["sha256"], digest.as_str());
    assert_eq!(source["size"], 17);
}

#[test]
fn embedded_chunks_trace_every_unit_to_pages() {
    let tmp = tempfile::tempdir().unwrap();
    let notes = setup(tmp.path());
    let png = tmp.path().join("pic.png");
    write_png(&png, 4, 3);
    let pdf = tmp.path().join("out.pdf");
    let dump = tmp.path().join("graph.json");
    convert(&[&notes, &png], &pdf, &[], Some(&dump));

    let files = attachments(&pdf);
    let manifest = json_of(find(&files, "anytopdf-manifest.json"));
    let chunks = json_of(find(&files, "anytopdf-chunks.json"));
    let page_count = page_count(&fs::read(&pdf).unwrap());
    let source_ids: Vec<&Value> = manifest["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| &s["id"])
        .collect();
    let list = chunks["chunks"].as_array().unwrap();
    assert!(!list.is_empty(), "chunks must not be empty");
    for chunk in list {
        let first = chunk["pages"]["first"].as_u64().unwrap();
        let last = chunk["pages"]["last"].as_u64().unwrap();
        assert!(
            first >= 1 && last < page_count,
            "pages {first}..{last} of {page_count}"
        );
        assert!(
            source_ids.contains(&&chunk["source_id"]),
            "unknown source_id"
        );
    }
    let graph: Value = serde_json::from_slice(&fs::read(&dump).unwrap()).unwrap();
    assert_eq!(
        manifest["units"].as_array().unwrap().len(),
        graph["units"].as_array().unwrap().len()
    );
}

#[test]
fn share_profile_manifest_has_no_absolute_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let notes = setup(&dir);
    let share = dir.join("share.pdf");
    convert(&[&notes], &share, &["--profile", "share"], None);
    let files = attachments(&share);
    let mut all = Vec::new();
    for name in ["anytopdf-manifest.json", "anytopdf-chunks.json"] {
        strings(&json_of(find(&files, name)), &mut all);
    }
    let root = dir.to_string_lossy().to_string();
    assert!(all.iter().all(|s| !s.contains(&root)), "share leaks {root}");

    let archive = dir.join("archive.pdf");
    convert(&[&notes], &archive, &[], None);
    let files = attachments(&archive);
    let manifest = json_of(find(&files, "anytopdf-manifest.json"));
    let source = &manifest["sources"][0];
    let has_path = source["path"].as_str().is_some_and(|p| p.contains(&root))
        || source["metadata"]["source.path"]
            .as_str()
            .is_some_and(|p| p.contains(&root));
    assert!(has_path, "archive profile should record the source path");
}

#[test]
fn convert_writes_no_sidecar_when_embedding_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let notes = setup(tmp.path());
    let pdf = tmp.path().join("out.pdf");
    let out = convert(&[&notes], &pdf, &[], None);
    let files = attachments(&pdf);
    assert_eq!(files.len(), 2, "precondition: both attachments embedded");
    assert!(!tmp.path().join("out.pdf.manifest.json").exists());
    assert!(!tmp.path().join("out.pdf.chunks.json").exists());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("[manifest.sidecar]"));
}
