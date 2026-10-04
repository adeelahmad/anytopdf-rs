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

fn run(
    inputs: &[&Path],
    pdf: &Path,
    json: Option<&Path>,
    extra: &[&str],
    epoch: Option<&str>,
) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    cmd.arg("--no-plugins")
        .env("PATH", "")
        .env_remove("SOURCE_DATE_EPOCH");
    if let Some(epoch) = epoch {
        cmd.env("SOURCE_DATE_EPOCH", epoch);
    }
    cmd.arg("convert")
        .args(inputs)
        .args(["--ocr", "off", "-o"])
        .arg(pdf);
    if let Some(json) = json {
        cmd.arg("--dump-graph").arg(json);
    }
    cmd.args(extra).output().unwrap()
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn load(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
        serde_json::Value::Object(o) => o.values().for_each(|v| strings(v, out)),
        _ => {}
    }
}

fn all_strings(value: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    strings(value, &mut out);
    out
}

fn page_count(pdf: &Path) -> u32 {
    let re = regex::bytes::Regex::new(r"/Type\s*/Page[^s]").unwrap();
    re.find_iter(&fs::read(pdf).unwrap()).count() as u32
}

fn pdftotext(pdf: &Path, page: Option<u32>) -> Option<String> {
    let exe = which::which("pdftotext").ok()?;
    let mut cmd = Command::new(exe);
    if let Some(p) = page {
        cmd.args(["-f", &p.to_string(), "-l", &p.to_string()]);
    }
    let out = cmd.arg(pdf).arg("-").output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Independent FIPS 180-4 SHA-256, so the test does not reuse production hashing.
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
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
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

fn fixture_dir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes.txt");
    fs::write(&notes, b"Identity fixture\n").unwrap();
    (dir, notes)
}

#[test]
fn same_inputs_and_source_date_epoch_give_identical_pdf_and_dump_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes.txt");
    fs::write(&notes, b"Identity fixture\n").unwrap();
    let image = dir.path().join("image.png");
    write_png(&image, 4, 3);
    let epoch = Some("1700000000");
    let paths = |n: &str| {
        (
            dir.path().join(format!("{n}.pdf")),
            dir.path().join(format!("{n}.json")),
        )
    };
    let (apdf, ajson) = paths("a");
    let (bpdf, bjson) = paths("b");
    ok(&run(&[&notes, &image], &apdf, Some(&ajson), &[], epoch));
    ok(&run(&[&notes, &image], &bpdf, Some(&bjson), &[], epoch));
    assert!(
        fs::read(&apdf).unwrap() == fs::read(&bpdf).unwrap(),
        "pdf bytes differ"
    );
    assert!(
        fs::read(&ajson).unwrap() == fs::read(&bjson).unwrap(),
        "dump bytes differ"
    );

    fs::write(&notes, b"Identity fixturf\n").unwrap();
    let (cpdf, cjson) = paths("c");
    ok(&run(&[&notes, &image], &cpdf, Some(&cjson), &[], epoch));
    let (a, c) = (load(&ajson), load(&cjson));
    assert_ne!(a["sources"][0]["id"], c["sources"][0]["id"]);
    assert_ne!(a["sources"][0]["sha256"], c["sources"][0]["sha256"]);
    assert_eq!(a["sources"][1]["id"], c["sources"][1]["id"]);
}

#[test]
fn share_profile_keeps_absolute_paths_out_of_graph_dump() {
    let (dir, notes) = fixture_dir();
    let root = dir.path().to_string_lossy().into_owned();
    let (pdf, json) = (dir.path().join("s.pdf"), dir.path().join("s.json"));
    ok(&run(
        &[&notes],
        &pdf,
        Some(&json),
        &["--profile", "share"],
        None,
    ));
    let dump = load(&json);
    let leaks: Vec<_> = all_strings(&dump)
        .into_iter()
        .filter(|s| s.contains(&root))
        .collect();
    assert!(leaks.is_empty(), "share dump leaks: {leaks:?}");
    assert_eq!(dump["sources"][0]["path"], "notes.txt");

    let (pdf, json) = (dir.path().join("d.pdf"), dir.path().join("d.json"));
    ok(&run(&[&notes], &pdf, Some(&json), &[], None));
    let dump = load(&json);
    let path = dump["sources"][0]["metadata"]["source.path"]
        .as_str()
        .unwrap_or("");
    assert!(
        path.contains(&root),
        "archive dump lacks source.path under {root}: {path:?}"
    );
}

#[test]
fn graph_dump_never_references_the_deleted_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("image.png");
    write_png(&image, 4, 3);
    let (pdf, json) = (dir.path().join("o.pdf"), dir.path().join("o.json"));
    ok(&run(&[&image], &pdf, Some(&json), &[], None));
    let dump = load(&json);
    for unit in dump["units"].as_array().unwrap() {
        assert!(unit["visual_path"].is_null(), "visual_path kept: {unit}");
    }
    let root = &std::fs::canonicalize(dir.path()).unwrap();
    for s in all_strings(&dump) {
        assert!(!s.contains("anytopdf-"), "workspace prefix leaked: {s}");
        let p = Path::new(&s);
        if p.is_absolute() {
            assert!(
                p.starts_with(root) || !p.exists(),
                "dangling absolute path: {s}"
            );
        }
    }
}

#[test]
fn provenance_page_text_matches_independent_sha256() {
    let (dir, notes) = fixture_dir();
    let digest = sha256_hex(b"Identity fixture\n");
    let root = dir.path().to_string_lossy().into_owned();
    for profile in ["archive", "share"] {
        let pdf = dir.path().join(format!("{profile}.pdf"));
        ok(&run(&[&notes], &pdf, None, &["--profile", profile], None));
        let pages = page_count(&pdf);
        let (Some(first), Some(last)) = (pdftotext(&pdf, Some(1)), pdftotext(&pdf, Some(pages)))
        else {
            println!("POPPLER-UNAVAILABLE: skipping text assertions for profile {profile}");
            continue;
        };
        assert!(first.contains("Identity fixture"), "page 1: {first}");
        assert!(
            last.contains(&format!("SHA-256: {digest}")),
            "last page: {last}"
        );
        assert!(
            last.contains(&format!("Profile: {profile}")),
            "last page: {last}"
        );
        assert!(last.contains("Source: notes.txt"), "last page: {last}");
        assert!(!last.contains(&root), "last page leaks temp dir: {last}");
    }
}

#[test]
fn no_provenance_page_flag_omits_the_page_but_keeps_page_count_of_content() {
    let (dir, notes) = fixture_dir();
    let (with, without) = (dir.path().join("with.pdf"), dir.path().join("without.pdf"));
    ok(&run(&[&notes], &with, None, &[], None));
    ok(&run(
        &[&notes],
        &without,
        None,
        &["--no-provenance-page"],
        None,
    ));
    assert_eq!(page_count(&with), page_count(&without) + 1);
    match pdftotext(&without, None) {
        Some(text) => assert!(
            !text.contains("SHA-256:"),
            "provenance text present: {text}"
        ),
        None => println!("POPPLER-UNAVAILABLE: skipping text assertion"),
    }
}

#[test]
fn invalid_source_date_epoch_is_a_usage_error() {
    let (dir, notes) = fixture_dir();
    let pdf = dir.path().join("bad.pdf");
    let out = run(&[&notes], &pdf, None, &[], Some("yesterday"));
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!pdf.exists(), "output written despite usage error");
}
