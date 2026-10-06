use std::process::Command;

pub fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    // A non-file override hides tools found outside PATH (Chrome in Program Files).
    command
        .arg("--no-plugins")
        .env("PATH", "")
        .env("ANYTOPDF_CHROME", "anytopdf-test-no-chrome")
        .env("ANYTOPDF_YT_DLP", "anytopdf-test-no-yt-dlp");
    command
}
