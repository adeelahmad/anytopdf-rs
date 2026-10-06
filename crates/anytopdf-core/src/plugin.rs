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

    /// Imports a container (email, archive) whose members are themselves
    /// importable files. The pipeline calls this instead of [`Importer::import`]
    /// and supplies `members`, which imports files the importer extracted into
    /// the job workspace with whichever registered importer probes best.
    /// Importers without members keep the default.
    fn import_with_members(
        &self,
        ctx: &JobContext,
        source: SourceRecord,
        members: &dyn MemberImporter,
    ) -> Result<ImportOutcome> {
        let _ = members;
        self.import(ctx, source)
    }
}

/// Imports files a container importer extracted into the job workspace.
pub trait MemberImporter {
    /// Probes and imports `path`, which must lie inside `ctx.workspace`. The
    /// returned units belong to a throwaway source; callers re-parent them onto
    /// the container source.
    fn import_member(&self, ctx: &JobContext, path: &Path) -> Result<ImportOutcome>;

    /// Bytes that members extracted for the current top-level input may still
    /// occupy, across every nesting level. Extractors stop reading a member
    /// at this bound.
    fn remaining_bytes(&self) -> u64 {
        u64::MAX
    }

    /// Records one extracted member of `bytes`; fails once the top-level
    /// input's member count or byte budget is exhausted.
    fn charge(&self, bytes: u64) -> Result<()> {
        let _ = bytes;
        Ok(())
    }
}

/// A [`MemberImporter`] that refuses every member, for importing a container
/// outside the pipeline.
pub struct NoMembers;

impl MemberImporter for NoMembers {
    fn import_member(&self, _ctx: &JobContext, path: &Path) -> Result<ImportOutcome> {
        anyhow::bail!("no member importer available for {}", path.display())
    }
}

pub trait SourceEnricher: Plugin {
    fn supports(&self, source: &SourceRecord) -> bool;
    fn enrich_source(&self, ctx: &JobContext, source: &mut SourceRecord) -> Result<Vec<String>>;
}

/// When a graph enricher runs relative to the unit enrichers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GraphPhase {
    /// Before unit enrichers, so they see the enriched graph (the default).
    #[default]
    BeforeUnits,
    /// After unit enrichers, for summaries over their annotations.
    AfterUnits,
}

pub trait GraphEnricher: Plugin {
    fn enrich_graph(&self, ctx: &JobContext, graph: &mut DocumentGraph) -> Result<Vec<String>>;

    fn phase(&self) -> GraphPhase {
        GraphPhase::BeforeUnits
    }
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
