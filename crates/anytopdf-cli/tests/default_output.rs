use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

#[path = "common/output.rs"]
mod output;
use output::stderr;

struct Env {
    cwd: tempfile::TempDir,
    inputs: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        Self {
            cwd: tempfile::tempdir().unwrap(),
            inputs: tempfile::tempdir().unwrap(),
        }
    }

    fn cwd(&self) -> PathBuf {
        self.cwd.path().canonicalize().unwrap()
    }

    fn input(&self, name: &str) -> PathBuf {
        let path = self.inputs.path().canonicalize().unwrap().join(name);
        fs::write(&path, format!("Marker text in {name}\nSecond line.")).unwrap();
        path
    }

    fn run(&self, args: &[&Path], extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_anytopdf"))
            .arg("--no-plugins")
            .env("PATH", "")
            .current_dir(self.cwd())
            .args(args)
            .args(["--ocr", "off"])
            .args(extra)
            .output()
            .unwrap()
    }
}

#[test]
fn single_input_defaults_to_stem_pdf_in_current_directory() {
    let env = Env::new();
    let notes = env.input("notes.txt");
    let out = env.run(&[&notes], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let pdf = env.cwd().join("notes.pdf");
    assert!(pdf.exists(), "notes.pdf missing; stderr: {}", stderr(&out));
    assert!(fs::read(&pdf).unwrap().starts_with(b"%PDF-"));
    assert!(stderr(&out).contains("notes.pdf"), "{}", stderr(&out));
}

#[test]
fn several_inputs_default_to_anytopdf_pdf() {
    let env = Env::new();
    let (a, b) = (env.input("a.txt"), env.input("b.txt"));
    let out = env.run(&[&a, &b], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(env.cwd().join("anytopdf.pdf").exists(), "{}", stderr(&out));
}

#[test]
fn existing_default_name_gets_next_free_number() {
    let env = Env::new();
    let notes = env.input("notes.txt");
    fs::write(env.cwd().join("notes.pdf"), "keep").unwrap();
    for n in 1..=2 {
        let out = env.run(&[&notes], &[]);
        assert!(out.status.success(), "run {n}: {}", stderr(&out));
        let name = format!("notes-{n}.pdf");
        assert!(env.cwd().join(&name).exists(), "run {n}: {name} missing");
        assert!(stderr(&out).contains(&name), "run {n}: {}", stderr(&out));
    }
    assert_eq!(fs::read(env.cwd().join("notes.pdf")).unwrap(), b"keep");
}

#[test]
fn overwrite_replaces_the_default_name() {
    let env = Env::new();
    let notes = env.input("notes.txt");
    fs::write(env.cwd().join("notes.pdf"), "keep").unwrap();
    let out = env.run(&[&notes], &["--overwrite"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let pdf = fs::read(env.cwd().join("notes.pdf")).unwrap();
    assert!(pdf.starts_with(b"%PDF-"), "notes.pdf not replaced");
    assert!(!env.cwd().join("notes-1.pdf").exists());
}

#[test]
fn explicit_existing_output_is_still_refused() {
    let env = Env::new();
    let notes = env.input("notes.txt");
    fs::write(env.cwd().join("out.pdf"), "keep").unwrap();
    let out = env.run(&[&notes], &["-o", "out.pdf"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert_eq!(fs::read(env.cwd().join("out.pdf")).unwrap(), b"keep");
}
