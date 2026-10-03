use anyhow::Result;
use anytopdf_core::*;

pub struct SceneAnnotationEnricher;

impl Plugin for SceneAnnotationEnricher {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "scene-provenance".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "unit-enricher".into(),
            extensions: vec![],
            mime_types: vec![],
            priority: -50,
        }
    }
}

impl UnitEnricher for SceneAnnotationEnricher {
    fn supports(&self, _graph: &DocumentGraph, unit: &Unit) -> bool {
        unit.kind == UnitKind::Visual && unit.metadata.contains_key("video.frame-selection")
    }

    fn enrich_unit(
        &self,
        _ctx: &JobContext,
        _graph: &DocumentGraph,
        unit: &mut Unit,
    ) -> Result<Vec<String>> {
        if let Some(reason) = unit.metadata.get("video.frame-selection") {
            unit.annotations.push(Annotation::text(
                AnnotationKind::Scene,
                "scene-provenance",
                format!("scene/keyframe reason: {reason}"),
            ));
        }
        Ok(vec![])
    }
}
