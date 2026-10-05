#[path = "common/process.rs"]
mod process;
use process::command;

/// Converts a text file as the print helper would, with its job variables set,
/// and returns the extracted manifest source.
fn convert_as_print_job(profile: &str) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("job.txt");
    std::fs::write(&input, "Printed page.\n").unwrap();
    let output = dir.path().join("job.pdf");
    let out = command()
        .args(["convert", "--ocr", "off", "--profile", profile, "-o"])
        .arg(&output)
        .arg(&input)
        .env("ANYTOPDF_PRINT_JOB_ID", "12")
        .env("ANYTOPDF_PRINT_JOB_NAME", "Receipt")
        .env("ANYTOPDF_PRINT_USER", "adeel")
        .env("ANYTOPDF_PRINT_FORMAT", "image/pwg-raster")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = command()
        .arg("extract")
        .arg(&output)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    doc["manifest"]["sources"][0].clone()
}

#[test]
fn print_job_variables_reach_the_manifest_under_the_archive_profile() {
    let source = convert_as_print_job("archive");
    let metadata = &source["metadata"];
    assert_eq!(metadata["print.job-id"], "12");
    assert_eq!(metadata["print.job-name"], "Receipt");
    assert_eq!(metadata["print.user"], "adeel");
    assert_eq!(metadata["print.format"], "image/pwg-raster");
}

#[test]
fn share_profile_drops_print_job_details() {
    let source = convert_as_print_job("share");
    let metadata = source["metadata"].as_object().unwrap();
    assert!(
        !metadata.keys().any(|k| k.starts_with("print.")),
        "{metadata:?}"
    );
}

#[test]
fn doctor_reports_printing_components() {
    let out = command().args(["doctor", "--json"]).output().unwrap();
    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let printing = doc["printing"].as_array().expect("printing array");
    let names: Vec<_> = printing
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["remote-front", "anytopdf-printer", "tailscale"]);
    assert_eq!(printing[0]["available"], true);
}
