use crate::DiagnosticCode;
use std::path::PathBuf;

// agentic:shim (whole file: replaced by the real observer types in GREEN)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Discover,
    Import,
    Enrich,
    Render,
}

impl Stage {
    pub const ALL: [Stage; 4] = [Stage::Discover, Stage::Import, Stage::Enrich, Stage::Render];

    pub fn as_str(self) -> &'static str {
        let _ = self;
        ""
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PipelineEvent {
    StageStarted(Stage),
    StageFinished(Stage),
    SourceStarted {
        index: usize,
        path: PathBuf,
    },
    SourceImported {
        index: usize,
        path: PathBuf,
        units: usize,
    },
    SourceSkipped {
        index: usize,
        path: PathBuf,
        code: DiagnosticCode,
    },
    UnitStarted {
        unit: usize,
        source: usize,
        enricher: String,
    },
    UnitFinished {
        unit: usize,
        source: usize,
        enricher: String,
    },
}

pub trait PipelineObserver {
    fn on_event(&mut self, event: &PipelineEvent);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_names_are_stable_and_ordered() {
        assert_eq!(
            Stage::ALL.map(Stage::as_str),
            ["discover", "import", "enrich", "render"]
        );
    }
}
