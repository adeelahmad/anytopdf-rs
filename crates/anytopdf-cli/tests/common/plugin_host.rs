use std::process::Command;

/// The CLI with runtime plugin discovery left on, unlike `process::command`.
pub fn anytopdf_with_plugins() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    command.env("PATH", "").env_remove("ANYTOPDF_PLUGIN_PATH");
    command
}
