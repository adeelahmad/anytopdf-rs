use std::path::Path;
use std::process::Command;

#[test]
fn bootstrap_regressions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("CLI crate has a workspace root");
    let python = std::env::var_os("PYTHON").unwrap_or_else(|| {
        ["python3", "python"]
            .into_iter()
            .find(|candidate| {
                Command::new(candidate)
                    .args(["-c", "import sys; assert sys.version_info >= (3, 11)"])
                    .output()
                    .is_ok_and(|output| output.status.success())
            })
            .expect("bootstrap regression suite requires Python 3.11+")
            .into()
    });
    let output = Command::new(python)
        .args([
            "-m",
            "unittest",
            "discover",
            "-s",
            "tests",
            "-p",
            "test_bootstrap.py",
            "-v",
        ])
        .current_dir(root)
        .output()
        .expect("run bootstrap regression suite");
    assert!(
        output.status.success(),
        "bootstrap regression suite failed ({}):\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
