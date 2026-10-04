use std::{fmt, path::PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticCode {
    InputNotFile,
    InputUnreadable,
    InputUnsupported,
    ImportFailed,
    ImportInvalid,
    ImportDuplicateId,
    EnrichmentFailed,
    ProviderMissing,
    ProviderFailed,
    OcrFallback,
    FramesNotImported,
    LossyDecode,
    TranscriptAmbiguous,
    CaptionUnreadable,
    RenderWarning,
    ManifestSidecar,
    ExtractVersionMismatch,
    PluginWarning,
    PluginDiscovery,
}

impl DiagnosticCode {
    pub const ALL: &[DiagnosticCode] = &[
        Self::InputNotFile,
        Self::InputUnreadable,
        Self::InputUnsupported,
        Self::ImportFailed,
        Self::ImportInvalid,
        Self::ImportDuplicateId,
        Self::EnrichmentFailed,
        Self::ProviderMissing,
        Self::ProviderFailed,
        Self::OcrFallback,
        Self::FramesNotImported,
        Self::LossyDecode,
        Self::TranscriptAmbiguous,
        Self::CaptionUnreadable,
        Self::RenderWarning,
        Self::ManifestSidecar,
        Self::ExtractVersionMismatch,
        Self::PluginWarning,
        Self::PluginDiscovery,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::InputNotFile => "input.not-file",
            Self::InputUnreadable => "input.unreadable",
            Self::InputUnsupported => "input.unsupported",
            Self::ImportFailed => "import.failed",
            Self::ImportInvalid => "import.invalid",
            Self::ImportDuplicateId => "import.duplicate-id",
            Self::EnrichmentFailed => "enrichment.failed",
            Self::ProviderMissing => "provider.missing",
            Self::ProviderFailed => "provider.failed",
            Self::OcrFallback => "ocr.fallback",
            Self::FramesNotImported => "input.frames-not-imported",
            Self::LossyDecode => "input.lossy-decode",
            Self::TranscriptAmbiguous => "transcript.ambiguous",
            Self::CaptionUnreadable => "caption.unreadable",
            Self::RenderWarning => "render.warning",
            Self::ManifestSidecar => "manifest.sidecar",
            Self::ExtractVersionMismatch => "extract.version-mismatch",
            Self::PluginWarning => "plugin.warning",
            Self::PluginDiscovery => "plugin.discovery",
        }
    }

    pub fn severity(self) -> Severity {
        match self {
            Self::ProviderMissing | Self::OcrFallback | Self::ManifestSidecar => Severity::Info,
            _ => Severity::Warning,
        }
    }

    pub fn is_skip(self) -> bool {
        matches!(
            self,
            Self::InputNotFile
                | Self::InputUnreadable
                | Self::InputUnsupported
                | Self::ImportFailed
                | Self::ImportInvalid
                | Self::ImportDuplicateId
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub severity: Severity,
    pub message: String,
    pub input: Option<PathBuf>,
    pub provider_exhausted: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ProvidersExhausted {
    pub message: String,
}

impl Diagnostic {
    pub fn new(code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: code.severity(),
            message: message.into(),
            input: None,
            provider_exhausted: false,
        }
    }

    pub fn for_input(
        code: DiagnosticCode,
        input: impl Into<PathBuf>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            input: Some(input.into()),
            ..Self::new(code, message)
        }
    }

    pub fn from_wire(wire: &str) -> Self {
        wire.strip_prefix('[')
            .and_then(|rest| rest.split_once("] "))
            .and_then(|(code, message)| {
                DiagnosticCode::ALL
                    .iter()
                    .find(|c| c.as_str() == code)
                    .map(|c| Self::new(*c, message))
            })
            .unwrap_or_else(|| Self::new(DiagnosticCode::PluginWarning, wire))
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code.as_str(), self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DiagnosticCode::*;

    const SKIPS: [DiagnosticCode; 6] = [
        InputNotFile,
        InputUnreadable,
        InputUnsupported,
        ImportFailed,
        ImportInvalid,
        ImportDuplicateId,
    ];

    fn well_formed(s: &str) -> bool {
        let mut parts = s.split('.');
        let ok = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase() || c == '-');
        let first = parts
            .next()
            .is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase()));
        let rest: Vec<&str> = parts.collect();
        first && !rest.is_empty() && rest.iter().all(|p| ok(p))
    }

    #[test]
    fn codes_have_unique_stable_strings() {
        assert_eq!(DiagnosticCode::ALL.len(), 19);
        let strings: Vec<&str> = DiagnosticCode::ALL.iter().map(|c| c.as_str()).collect();
        let unique: std::collections::BTreeSet<&str> = strings.iter().copied().collect();
        assert_eq!(unique.len(), strings.len(), "codes must be pairwise unique");
        for s in &strings {
            assert!(well_formed(s), "malformed code {s:?}");
        }
        assert_eq!(InputUnsupported.as_str(), "input.unsupported");
        assert_eq!(ProviderMissing.as_str(), "provider.missing");
        assert_eq!(OcrFallback.as_str(), "ocr.fallback");
        assert_eq!(FramesNotImported.as_str(), "input.frames-not-imported");
        assert_eq!(LossyDecode.as_str(), "input.lossy-decode");
        assert_eq!(TranscriptAmbiguous.as_str(), "transcript.ambiguous");
        assert_eq!(PluginWarning.as_str(), "plugin.warning");
    }

    #[test]
    fn only_optional_provider_notices_are_informational() {
        assert_eq!(DiagnosticCode::ALL.len(), 19);
        let info = [ProviderMissing, OcrFallback, ManifestSidecar];
        for code in DiagnosticCode::ALL {
            let expected = if info.contains(code) {
                Severity::Info
            } else {
                Severity::Warning
            };
            assert_eq!(code.severity(), expected, "{code:?}");
            assert_eq!(code.is_skip(), SKIPS.contains(code), "{code:?}");
            assert_eq!(Diagnostic::new(*code, "m").severity, code.severity());
        }
        assert_eq!(ProviderMissing.severity(), Severity::Info);
        assert_eq!(OcrFallback.severity(), Severity::Info);
        assert_eq!(ManifestSidecar.severity(), Severity::Info);
        assert_eq!(RenderWarning.severity(), Severity::Warning);
        assert!(InputUnsupported.is_skip());
        assert!(!ProviderMissing.is_skip());
        assert_eq!(
            Diagnostic::new(ProviderMissing, "m").severity,
            Severity::Info
        );
    }

    #[test]
    fn wire_form_round_trips_every_code() {
        assert_eq!(DiagnosticCode::ALL.len(), 19);
        for code in DiagnosticCode::ALL {
            let wire = Diagnostic::new(*code, "msg with [brackets]").to_string();
            let parsed = Diagnostic::from_wire(&wire);
            assert_eq!(parsed.code, *code, "{wire}");
            assert_eq!(parsed.message, "msg with [brackets]", "{wire}");
        }
    }

    #[test]
    fn unknown_or_unprefixed_strings_become_plugin_warnings() {
        let plain = Diagnostic::from_wire("plain v1 warning");
        assert_eq!(plain.code, PluginWarning);
        assert_eq!(plain.message, "plain v1 warning");
        let unknown = Diagnostic::from_wire("[not.a-code] x");
        assert_eq!(unknown.code, PluginWarning);
        assert_eq!(unknown.message, "[not.a-code] x");
    }
}
