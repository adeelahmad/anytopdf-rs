use crate::Cli;
use clap::CommandFactory;
use std::ffi::OsString;

pub(crate) fn normalize_args(args: Vec<OsString>) -> Vec<OsString> {
    let cmd = Cli::command();
    let mut out = args;
    let mut i = 1;
    while i < out.len() {
        let Some(token) = out[i].to_str().map(str::to_owned) else {
            break;
        };
        if matches!(token.as_str(), "-h" | "--help" | "-V" | "--version") {
            return out;
        }
        if let Some(long) = token.strip_prefix("--").filter(|l| !l.is_empty()) {
            let name = long.split('=').next().unwrap_or(long);
            let global = cmd
                .get_arguments()
                .find(|a| a.is_global_set() && a.get_long() == Some(name));
            if let Some(arg) = global {
                let takes_value = arg.get_action().takes_values();
                i += if takes_value && !long.contains('=') {
                    2
                } else {
                    1
                };
                continue;
            }
        }
        if cmd.get_subcommands().any(|s| s.get_name() == token) || token == "help" {
            return out;
        }
        out.insert(i, OsString::from("convert"));
        for arg in &mut out[i + 1..] {
            if arg == "--" {
                break;
            }
            if let Some(v) = arg.to_str().and_then(|a| a.strip_prefix("-o=")) {
                *arg = OsString::from(format!("--output={v}"));
            }
        }
        return out;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Vec<String> {
        normalize_args(args.iter().map(OsString::from).collect())
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn normalize_inserts_convert_only_before_first_non_global_token() {
        let cases: &[(&[&str], &[&str])] = &[
            (
                &["anytopdf", "a.txt", "-o", "x.pdf"],
                &["anytopdf", "convert", "a.txt", "-o", "x.pdf"],
            ),
            (
                &[
                    "anytopdf",
                    "--no-plugins",
                    "--plugin-timeout",
                    "5",
                    "-o",
                    "x.pdf",
                    "a.txt",
                ],
                &[
                    "anytopdf",
                    "--no-plugins",
                    "--plugin-timeout",
                    "5",
                    "convert",
                    "-o",
                    "x.pdf",
                    "a.txt",
                ],
            ),
            (&["anytopdf", "doctor"], &["anytopdf", "doctor"]),
            (
                &["anytopdf", "--no-plugins", "probe", "f"],
                &["anytopdf", "--no-plugins", "probe", "f"],
            ),
            (&["anytopdf", "convert", "a"], &["anytopdf", "convert", "a"]),
            (&["anytopdf", "--help"], &["anytopdf", "--help"]),
            (&["anytopdf", "-V"], &["anytopdf", "-V"]),
            (&["anytopdf"], &["anytopdf"]),
            (
                &["anytopdf", "./doctor"],
                &["anytopdf", "convert", "./doctor"],
            ),
            (
                &["anytopdf", "--", "doctor"],
                &["anytopdf", "convert", "--", "doctor"],
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(run(input), v(expected), "input {input:?}");
        }
    }
}
