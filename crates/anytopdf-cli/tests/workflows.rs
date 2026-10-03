use std::path::Path;
use std::process::Command;

#[test]
fn workflow_regressions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("CLI crate must be two directories below repository root");
    let python = std::env::var_os("PYTHON").unwrap_or_else(|| {
        ["python3", "python"]
            .into_iter()
            .find(|candidate| {
                Command::new(candidate)
                    .args(["-c", "import sys; sys.exit(sys.version_info < (3, 11))"])
                    .output()
                    .is_ok_and(|output| output.status.success())
            })
            .expect("Python 3.11+ must be available as python3 or python, or set PYTHON")
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
            "test_workflows.py",
            "-v",
        ])
        .current_dir(root)
        .output()
        .expect("Python 3.11+ must be available to run workflow tests");
    assert!(
        output.status.success(),
        "workflow regression suite failed ({:?})\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
