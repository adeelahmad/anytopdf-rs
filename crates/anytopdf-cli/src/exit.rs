#![allow(dead_code)]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitClass {
    Success,
    Internal,
    Usage,
    Input,
    Provider,
    Strict,
    Render,
    FailFast,
}

impl ExitClass {
    pub const ALL: [ExitClass; 8] = [
        ExitClass::Success,
        ExitClass::Internal,
        ExitClass::Usage,
        ExitClass::Input,
        ExitClass::Provider,
        ExitClass::Strict,
        ExitClass::Render,
        ExitClass::FailFast,
    ];

    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn name(self) -> &'static str {
        match self {
            ExitClass::Success => "success",
            ExitClass::Internal => "internal",
            ExitClass::Usage => "usage",
            ExitClass::Input => "input",
            ExitClass::Provider => "provider",
            ExitClass::Strict => "strict",
            ExitClass::Render => "render",
            ExitClass::FailFast => "fail-fast",
        }
    }
}

#[derive(Debug)]
pub struct CliError {
    pub class: ExitClass,
    pub error: anyhow::Error,
}

impl<E: Into<anyhow::Error>> From<E> for CliError {
    fn from(error: E) -> Self {
        CliError {
            class: ExitClass::Internal,
            error: error.into(),
        }
    }
}

/// Tag a fallible result with the exit class its failure maps to.
pub fn tag<T, E: Into<anyhow::Error>>(
    class: ExitClass,
    result: Result<T, E>,
) -> Result<T, CliError> {
    result.map_err(|e| CliError {
        class,
        error: e.into(),
    })
}

pub fn fail(class: ExitClass, message: impl std::fmt::Display) -> CliError {
    CliError {
        class,
        error: anyhow::anyhow!("{message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn exit_codes_are_distinct_and_documented_in_readme() {
        let readme = include_str!("../../../README.md");
        let expected = [
            "success",
            "internal",
            "usage",
            "input",
            "provider",
            "strict",
            "render",
            "fail-fast",
        ];
        let codes: BTreeSet<u8> = ExitClass::ALL.iter().map(|c| c.code()).collect();
        assert_eq!(codes, (0..=7).collect::<BTreeSet<u8>>());
        assert_eq!(ExitClass::ALL.len(), 8);
        for (index, class) in ExitClass::ALL.iter().enumerate() {
            assert_eq!(class.code() as usize, index, "{class:?}");
            assert_eq!(class.name(), expected[index]);
            let row = format!("| {} | {} |", class.code(), class.name());
            assert!(
                readme.lines().any(|line| line.starts_with(&row)),
                "README lacks row {row}"
            );
        }
    }
}
