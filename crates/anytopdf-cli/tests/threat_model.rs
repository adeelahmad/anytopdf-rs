//! Tests that pin the answers to the threat model's open questions (Q4, Q9, Q17).
use std::{
    fs,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

fn anytopdf() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    cmd.env("PATH", "").env_remove("SOURCE_DATE_EPOCH");
    cmd
}

/// Runs the command, killing it if it outlives `limit`, so a hang fails the test instead of CI.
fn output_within(mut cmd: Command, limit: Duration) -> Output {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > limit {
            child.kill().unwrap();
            panic!("anytopdf did not finish within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

/// A classic-xref PDF whose objects are numbered from 1; `trailer` adds trailer entries and
/// may use `{xref}` for the xref table's own offset.
fn raw_pdf(objects: &[&str], trailer: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        out.extend(format!("{offset:010} 00000 n \n").as_bytes());
    }
    let trailer = trailer.replace("{xref}", &xref.to_string());
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R {trailer} >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

const PAGES: &str = "<< /Type /Pages /Kids [] /Count 0 >>";

#[test]
fn extract_terminates_with_input_error_on_cyclic_pdfs() {
    let cases: [(&str, Vec<u8>); 6] = [
        (
            "names reference loop",
            raw_pdf(
                &[
                    "<< /Type /Catalog /Pages 2 0 R /Names 3 0 R >>",
                    PAGES,
                    "4 0 R",
                    "3 0 R",
                ],
                "",
            ),
        ),
        (
            "embedded-files tree points at itself",
            raw_pdf(
                &[
                    "<< /Type /Catalog /Pages 2 0 R /Names 3 0 R >>",
                    PAGES,
                    "<< /EmbeddedFiles 3 0 R >>",
                ],
                "",
            ),
        ),
        (
            "page tree contains itself",
            raw_pdf(
                &[
                    "<< /Type /Catalog /Pages 2 0 R >>",
                    "<< /Type /Pages /Kids [2 0 R] /Count 1 >>",
                ],
                "",
            ),
        ),
        (
            "xref Prev chain loops",
            raw_pdf(
                &["<< /Type /Catalog /Pages 2 0 R >>", PAGES],
                "/Prev {xref}",
            ),
        ),
        (
            "stream length refers to its own stream",
            raw_pdf(
                &[
                    "<< /Type /Catalog /Pages 2 0 R >>",
                    PAGES,
                    "<< /Length 3 0 R >>\nstream\nabc\nendstream",
                ],
                "",
            ),
        ),
        (
            "catalog is a reference to itself",
            raw_pdf(&["1 0 R", PAGES], ""),
        ),
    ];
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in cases {
        let pdf = dir.path().join("cyclic.pdf");
        fs::write(&pdf, bytes).unwrap();
        let mut cmd = anytopdf();
        cmd.arg("--no-plugins")
            .arg("extract")
            .arg(&pdf)
            .arg("--json");
        let out = output_within(cmd, Duration::from_secs(20));
        assert_eq!(
            out.status.code(),
            Some(3),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stdout.is_empty(), "{name}: printed a document");
    }
}

#[cfg(unix)]
#[test]
fn overwrite_through_a_hard_link_leaves_the_input_intact() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("notes.txt");
    let alias = dir.path().join("alias.pdf");
    fs::write(&input, "hard link fixture\n").unwrap();
    fs::hard_link(&input, &alias).unwrap();
    let out = anytopdf()
        .arg("--no-plugins")
        .arg("convert")
        .arg(&input)
        .args(["--ocr", "off", "--overwrite", "-o"])
        .arg(&alias)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read(&input).unwrap(), b"hard link fixture\n");
    assert!(fs::read(&alias).unwrap().starts_with(b"%PDF-"));
}

#[cfg(unix)]
fn write_failing_plugin(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let plugin = dir.join("anytopdf-plugin-broken");
    let manifest = r#"{"protocol":1,"name":"broken","version":"1","capabilities":[{"kind":"importer","extensions":["broken"],"mime_types":[],"priority":90}]}"#;
    fs::write(
        &plugin,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --anytopdf-manifest ]; then echo '{manifest}'; exit 0; fi\nexit 9\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&plugin, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn share_profile_redacts_the_plugin_directory_from_failures() {
    let inputs = tempfile::tempdir().unwrap();
    let plugins = tempfile::tempdir().unwrap();
    write_failing_plugin(plugins.path());
    let doc = inputs.path().join("doc.broken");
    let notes = inputs.path().join("notes.txt");
    fs::write(&doc, "x").unwrap();
    fs::write(&notes, "kept\n").unwrap();
    let out = anytopdf()
        .env("ANYTOPDF_PLUGIN_PATH", plugins.path())
        .arg("convert")
        .arg(&doc)
        .arg(&notes)
        .args(["--ocr", "off", "--profile", "share", "--json", "-o"])
        .arg(inputs.path().join("out.pdf"))
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("anytopdf-plugin-broken"), "{text}");
    for form in [
        plugins.path().to_path_buf(),
        plugins.path().canonicalize().unwrap(),
    ] {
        let form = form.to_string_lossy().into_owned();
        assert!(!text.contains(&form), "leaked {form}: {text}");
    }
}
