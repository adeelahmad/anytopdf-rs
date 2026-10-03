use crate::{DocumentGraph, ImportOutcome, JobContext, RenderReport, SourceRecord, Unit};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginDescriptor {
    pub name: String,
    pub version: String,
    pub kind: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub mime_types: Vec<String>,
    #[serde(default)]
    pub priority: i32,
}

pub trait Plugin: Send + Sync {
    fn descriptor(&self) -> PluginDescriptor;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProbeScore(pub u16);

impl ProbeScore {
    pub const NONE: Self = Self(0);
    pub const EXTENSION: Self = Self(300);
    pub const MIME: Self = Self(600);
    pub const MAGIC: Self = Self(800);
    pub const CERTAIN: Self = Self(1000);
}

pub trait Importer: Plugin {
    fn probe(&self, source: &SourceRecord) -> ProbeScore;
    fn import(&self, ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome>;
}

pub trait SourceEnricher: Plugin {
    fn supports(&self, source: &SourceRecord) -> bool;
    fn enrich_source(&self, ctx: &JobContext, source: &mut SourceRecord) -> Result<Vec<String>>;
}

pub trait GraphEnricher: Plugin {
    fn enrich_graph(&self, ctx: &JobContext, graph: &mut DocumentGraph) -> Result<Vec<String>>;
}

pub trait UnitEnricher: Plugin {
    fn supports(&self, graph: &DocumentGraph, unit: &Unit) -> bool;
    fn enrich_unit(
        &self,
        ctx: &JobContext,
        graph: &DocumentGraph,
        unit: &mut Unit,
    ) -> Result<Vec<String>>;
}

pub trait Renderer: Plugin {
    fn render(
        &self,
        ctx: &JobContext,
        graph: &DocumentGraph,
        output: &Path,
    ) -> Result<RenderReport>;
}

pub type ImporterRef = Arc<dyn Importer>;
pub type SourceEnricherRef = Arc<dyn SourceEnricher>;
pub type GraphEnricherRef = Arc<dyn GraphEnricher>;
pub type UnitEnricherRef = Arc<dyn UnitEnricher>;
pub type RendererRef = Arc<dyn Renderer>;
