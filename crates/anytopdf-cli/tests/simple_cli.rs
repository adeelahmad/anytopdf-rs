use std::{fs, path::Path, process::Command};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    command.arg("--no-plugins").env("PATH", "");
    command
}

fn write(dir: &Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("Marker text in {name}\nSecond line.")).unwrap();
    path
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn inputs_without_subcommand_convert() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.txt");
    let out = dir.path().join("out.pdf");
    let result = command()
        .arg(&a)
        .args(["--ocr", "off", "-o"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", stderr(&result));
    assert!(fs::read(&out).unwrap().starts_with(b"%PDF-"));
}

#[test]
fn output_flag_forms_are_accepted_anywhere() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.txt");
    let b = write(dir.path(), "b.txt");
    let p = |n: &str| dir.path().join(n).to_string_lossy().into_owned();
    let (a_s, b_s) = (
        a.to_string_lossy().into_owned(),
        b.to_string_lossy().into_owned(),
    );
    let permutations: Vec<Vec<String>> = vec![
        vec!["-o".into(), p("out1.pdf"), a_s.clone(), b_s.clone()],
        vec![a_s.clone(), format!("-o={}", p("out2.pdf")), b_s.clone()],
        vec![a_s.clone(), b_s.clone(), "--output".into(), p("out3.pdf")],
        vec![
            format!("--output={}", p("out4.pdf")),
            a_s.clone(),
            b_s.clone(),
        ],
        vec![
            "convert".into(),
            a_s.clone(),
            format!("--output={}", p("out5.pdf")),
            b_s.clone(),
        ],
    ];
    for (i, args) in permutations.iter().enumerate() {
        let n = i + 1;
        let graph = dir.path().join(format!("g{n}.json"));
        let result = command()
            .args(args)
            .args(["--ocr", "off", "--dump-graph"])
            .arg(&graph)
            .output()
            .unwrap();
        assert!(result.status.success(), "form {n}: {}", stderr(&result));
        assert!(dir.path().join(format!("out{n}.pdf")).exists(), "form {n}");
        let json: serde_json::Value = serde_json::from_slice(&fs::read(&graph).unwrap()).unwrap();
        assert_eq!(json["sources"].as_array().unwrap().len(), 2, "form {n}");
    }
    for bad in ["-o=out2.pdf", "=out2.pdf", "out2.pdf"] {
        let name = dir.path().join(bad);
        // out2.pdf is the legitimate output of form 2; only the malformed names must be absent.
        if bad != "out2.pdf" {
            assert!(!name.exists(), "{bad} must not exist");
        }
    }
}

#[test]
fn existing_subcommands_still_work_first() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.txt");
    let doctor = command().arg("doctor").output().unwrap();
    assert!(doctor.status.success());
    assert!(String::from_utf8_lossy(&doctor.stdout).contains("provider diagnostics"));
    let plugins = command().arg("plugins").output().unwrap();
    assert!(plugins.status.success());
    assert!(String::from_utf8_lossy(&plugins.stdout).contains("text"));
    let probe = command().arg("probe").arg(&a).output().unwrap();
    assert!(probe.status.success());
    serde_json::from_slice::<serde_json::Value>(&probe.stdout).unwrap();
    let c = dir.path().join("c.pdf");
    let convert = command()
        .arg("convert")
        .arg(&a)
        .arg("-o")
        .arg(&c)
        .output()
        .unwrap();
    assert!(convert.status.success(), "{}", stderr(&convert));
    assert!(c.exists());
}

#[test]
fn subcommand_named_paths_via_dot_slash_or_double_dash() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "doctor");
    let a = command()
        .current_dir(dir.path())
        .args(["./doctor", "--ocr", "off", "-o", "a.pdf"])
        .output()
        .unwrap();
    assert!(a.status.success(), "{}", stderr(&a));
    assert!(dir.path().join("a.pdf").exists());
    let b = command()
        .current_dir(dir.path())
        .args(["--ocr", "off", "-o", "b.pdf", "--", "doctor"])
        .output()
        .unwrap();
    assert!(b.status.success(), "{}", stderr(&b));
    assert!(dir.path().join("b.pdf").exists());
    let bare = tempfile::tempdir().unwrap();
    write(bare.path(), "doctor");
    let d = command()
        .current_dir(bare.path())
        .arg("doctor")
        .output()
        .unwrap();
    assert!(d.status.success());
    assert!(String::from_utf8_lossy(&d.stdout).contains("provider diagnostics"));
    let pdfs = fs::read_dir(bare.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "pdf")
        })
        .count();
    assert_eq!(pdfs, 0);
}

#[test]
fn no_arguments_still_shows_help() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.txt");
    let result = Command::new(env!("CARGO_BIN_EXE_anytopdf"))
        .current_dir(dir.path())
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(!result.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        stderr(&result)
    );
    assert!(text.to_lowercase().contains("usage"), "{text}");
    let pdfs = fs::read_dir(dir.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "pdf")
        })
        .count();
    assert_eq!(pdfs, 0);
}
