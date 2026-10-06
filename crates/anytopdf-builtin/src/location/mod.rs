//! Location enrichment: a GPS fix from source metadata, reverse geocoded
//! offline, plus place names mentioned in OCR text, captions and transcripts.

mod gazetteer;
mod gps;

use anyhow::Result;
use anytopdf_core::*;
use gazetteer::{Gazetteer, PlaceInfo};
use gps::GpsFix;
use std::collections::HashSet;
use std::str::FromStr;

/// A GPS fix farther than this from every known town is labelled with its
/// coordinates instead of a place name.
const MAX_PLACE_KM: f64 = 50.0;

const PROVIDER: &str = "location";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocationMode {
    /// GPS fixes and place names found in text.
    #[default]
    On,
    /// GPS fixes only.
    Gps,
    Off,
}

impl FromStr for LocationMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "on" => Ok(Self::On),
            "gps" => Ok(Self::Gps),
            "off" | "none" => Ok(Self::Off),
            other => Err(format!(
                "unknown location mode {other:?}; expected on, gps or off"
            )),
        }
    }
}

pub struct LocationEnricher {
    mode: LocationMode,
}

impl LocationEnricher {
    pub fn new(mode: LocationMode) -> Self {
        Self { mode }
    }
}

impl Plugin for LocationEnricher {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: PROVIDER.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "unit-enricher".into(),
            extensions: vec![],
            mime_types: vec!["*/*".into()],
            priority: 0,
        }
    }
}

impl UnitEnricher for LocationEnricher {
    fn supports(&self, _graph: &DocumentGraph, _unit: &Unit) -> bool {
        self.mode != LocationMode::Off
    }

    fn enrich_unit(
        &self,
        _ctx: &JobContext,
        graph: &DocumentGraph,
        unit: &mut Unit,
    ) -> Result<Vec<String>> {
        let gps = gps_target(graph, unit)
            .and_then(|source| gps::gps_fix(&source.metadata))
            .filter(|_| !has_location(unit, "gps"));
        let segments = if self.mode == LocationMode::On && !has_location(unit, "text") {
            text_segments(unit)
        } else {
            Vec::new()
        };
        if gps.is_none() && segments.is_empty() {
            return Ok(Vec::new());
        }
        let gazetteer = Gazetteer::embedded()?;
        let mut found = Vec::new();
        if let Some(fix) = gps {
            found.push(gps_annotation(gazetteer, &fix));
        }
        found.extend(text_annotations(gazetteer, &segments));
        unit.annotations.extend(found);
        Ok(Vec::new())
    }
}

/// A source's GPS fix is recorded once, on its first unit.
fn gps_target<'a>(graph: &'a DocumentGraph, unit: &Unit) -> Option<&'a SourceRecord> {
    let first = graph.units.iter().find(|u| u.source_id == unit.source_id)?;
    (first.id == unit.id)
        .then(|| graph.source(unit.source_id))
        .flatten()
}

fn has_location(unit: &Unit, source: &str) -> bool {
    unit.annotations.iter().any(|a| {
        a.kind == AnnotationKind::Location
            && a.attributes.get("source").map(String::as_str) == Some(source)
    })
}

fn place_attributes(place: &PlaceInfo, out: &mut Metadata) {
    out.insert("place".into(), place.display());
    out.insert("place_kind".into(), place.kind.into());
    if place.kind == "city" {
        out.insert("city".into(), place.name.clone());
    }
    if let Some(region) = place.region.as_ref().filter(|r| !r.is_empty()) {
        out.insert("region".into(), region.clone());
    } else if place.kind == "region" {
        out.insert("region".into(), place.name.clone());
    }
    out.insert("country".into(), place.country.clone());
    out.insert("country_code".into(), place.country_code.clone());
    out.insert("gazetteer".into(), gazetteer::ATTRIBUTION.into());
}

fn gps_annotation(gazetteer: &Gazetteer, fix: &GpsFix) -> Annotation {
    let coordinates = format!("{:.5}, {:.5}", fix.latitude, fix.longitude);
    let mut annotation = Annotation::text(AnnotationKind::Location, PROVIDER, coordinates);
    let attributes = &mut annotation.attributes;
    attributes.insert("source".into(), "gps".into());
    attributes.insert("latitude".into(), format!("{:.6}", fix.latitude));
    attributes.insert("longitude".into(), format!("{:.6}", fix.longitude));
    if let Some(altitude) = fix.altitude {
        attributes.insert("altitude_m".into(), format!("{altitude:.1}"));
    }
    attributes.insert("origin".into(), fix.origin.clone());
    if let Some((place, km)) = gazetteer.nearest(fix.latitude, fix.longitude) {
        attributes.insert("distance_km".into(), format!("{km:.1}"));
        if km <= MAX_PLACE_KM {
            place_attributes(&place, attributes);
            annotation.text = place.display();
        } else {
            attributes.insert("nearest_place".into(), place.display());
        }
    }
    annotation
}

struct Segment<'a> {
    from: &'static str,
    text: std::borrow::Cow<'a, str>,
    time_range: Option<TimeRange>,
}

fn text_segments(unit: &Unit) -> Vec<Segment<'_>> {
    let mut segments = Vec::new();
    if let Some(text) = unit
        .visible_text
        .as_deref()
        .filter(|t| !t.trim().is_empty())
    {
        segments.push(Segment {
            from: "text",
            text: text.into(),
            time_range: unit.time_range,
        });
    }
    let ocr: Vec<&str> = unit
        .annotations
        .iter()
        .filter(|a| a.kind == AnnotationKind::Ocr)
        .map(|a| a.text.as_str())
        .collect();
    if !ocr.is_empty() {
        segments.push(Segment {
            from: "ocr",
            text: ocr.join(" ").into(),
            time_range: unit.time_range,
        });
    }
    for a in &unit.annotations {
        let from = match a.kind {
            AnnotationKind::Caption => "caption",
            AnnotationKind::Transcript => "transcript",
            _ => continue,
        };
        segments.push(Segment {
            from,
            text: a.text.as_str().into(),
            time_range: a.time_range,
        });
    }
    segments
}

fn text_annotations(gazetteer: &Gazetteer, segments: &[Segment<'_>]) -> Vec<Annotation> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for segment in segments {
        for mention in gazetteer.mentions(&segment.text) {
            let place = &mention.place;
            if !seen.insert(place.display()) {
                continue;
            }
            let mut annotation =
                Annotation::text(AnnotationKind::Location, PROVIDER, place.display());
            annotation.time_range = segment.time_range;
            annotation.confidence = Some(match place.kind {
                "city" if !mention.matched.contains(' ') => 0.6,
                "city" => 0.8,
                _ => 0.9,
            });
            let attributes = &mut annotation.attributes;
            attributes.insert("source".into(), "text".into());
            attributes.insert("from".into(), segment.from.into());
            attributes.insert("matched".into(), mention.matched.clone());
            if let (Some(lat), Some(lon)) = (place.latitude, place.longitude) {
                attributes.insert("latitude".into(), format!("{lat:.3}"));
                attributes.insert("longitude".into(), format!("{lon:.3}"));
            }
            place_attributes(place, attributes);
            out.push(annotation);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx() -> JobContext {
        JobContext {
            workspace: std::env::temp_dir(),
            quiet: true,
        }
    }

    fn photo(metadata: &[(&str, &str)]) -> (DocumentGraph, Unit) {
        let mut source = SourceRecord::new(PathBuf::from("photo.jpg"));
        source.metadata = metadata
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let unit = Unit::visual(source.id, PathBuf::from("photo.jpg"));
        let graph = DocumentGraph {
            sources: vec![source],
            units: vec![unit.clone()],
            metadata: Metadata::new(),
        };
        (graph, unit)
    }

    fn locations(unit: &Unit) -> Vec<&Annotation> {
        unit.annotations
            .iter()
            .filter(|a| a.kind == AnnotationKind::Location)
            .collect()
    }

    #[test]
    fn exif_gps_becomes_a_reverse_geocoded_location() {
        let (graph, mut unit) = photo(&[
            ("exiftool.Composite:GPSLatitude", "48 deg 51' 30.00\" N"),
            ("exiftool.Composite:GPSLongitude", "2 deg 17' 40.00\" E"),
        ]);
        LocationEnricher::new(LocationMode::On)
            .enrich_unit(&ctx(), &graph, &mut unit)
            .unwrap();
        let found = locations(&unit);
        assert_eq!(found.len(), 1, "{found:?}");
        let a = found[0];
        assert_eq!(a.provider, "location");
        assert_eq!(a.attributes["source"], "gps");
        assert_eq!(a.attributes["country_code"], "FR");
        assert!(a.text.ends_with("France"), "{}", a.text);
        assert_eq!(a.attributes["latitude"], "48.858333");
        assert_eq!(a.attributes["origin"], "exiftool.Composite:GPSLatitude");
        assert!(a.attributes["gazetteer"].contains("GeoNames"));
    }

    #[test]
    fn remote_fix_is_labelled_with_coordinates() {
        let (graph, mut unit) = photo(&[("ffprobe.format.tags.location", "-48.8767-123.3933/")]);
        LocationEnricher::new(LocationMode::Gps)
            .enrich_unit(&ctx(), &graph, &mut unit)
            .unwrap();
        let a = locations(&unit)[0];
        assert_eq!(a.text, "-48.87670, -123.39330");
        assert!(a.attributes.contains_key("nearest_place"));
        assert!(!a.attributes.contains_key("place"));
    }

    #[test]
    fn gps_is_recorded_only_on_the_first_unit_of_a_source() {
        let (mut graph, first) = photo(&[("ffprobe.format.tags.location", "+51.5007-000.1246/")]);
        let mut second = Unit::visual(first.source_id, PathBuf::from("frame-2.png"));
        graph.units.push(second.clone());
        LocationEnricher::new(LocationMode::On)
            .enrich_unit(&ctx(), &graph, &mut second)
            .unwrap();
        assert!(locations(&second).is_empty());
    }

    #[test]
    fn place_names_in_ocr_and_transcripts_become_text_locations() {
        let (graph, mut unit) = photo(&[]);
        for word in ["Welcome", "to", "San", "Francisco"] {
            unit.annotations
                .push(Annotation::text(AnnotationKind::Ocr, "tesseract", word));
        }
        let mut said = Annotation::text(
            AnnotationKind::Transcript,
            "whisper",
            "we flew in from Tokyo yesterday",
        );
        said.time_range = Some(TimeRange {
            start_seconds: 12.0,
            end_seconds: 15.0,
        });
        unit.annotations.push(said);
        LocationEnricher::new(LocationMode::On)
            .enrich_unit(&ctx(), &graph, &mut unit)
            .unwrap();
        let found = locations(&unit);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].attributes["from"], "ocr");
        assert_eq!(found[0].attributes["matched"], "San Francisco");
        assert_eq!(found[0].attributes["country_code"], "US");
        assert_eq!(found[0].confidence, Some(0.8));
        assert_eq!(found[1].attributes["from"], "transcript");
        assert_eq!(found[1].attributes["country_code"], "JP");
        assert_eq!(found[1].time_range.unwrap().start_seconds, 12.0);
        assert!(found.iter().all(|a| a.attributes["source"] == "text"));
    }

    #[test]
    fn modes_limit_what_is_extracted() {
        let gps = [("ffprobe.format.tags.location", "+48.8566+002.3522/")];
        let (graph, mut unit) = photo(&gps);
        unit.visible_text = Some("Notes from Berlin".into());
        let mut only_gps = unit.clone();
        LocationEnricher::new(LocationMode::Gps)
            .enrich_unit(&ctx(), &graph, &mut only_gps)
            .unwrap();
        let found = locations(&only_gps);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].attributes["source"], "gps");
        assert!(!LocationEnricher::new(LocationMode::Off).supports(&graph, &unit));
        assert_eq!("gps".parse(), Ok(LocationMode::Gps));
        assert_eq!("none".parse(), Ok(LocationMode::Off));
        assert!("auto".parse::<LocationMode>().is_err());
    }

    #[test]
    fn enrichment_is_idempotent() {
        let (graph, mut unit) = photo(&[("ffprobe.format.tags.location", "+48.8566+002.3522/")]);
        unit.visible_text = Some("Notes from Berlin".into());
        let enricher = LocationEnricher::new(LocationMode::On);
        enricher.enrich_unit(&ctx(), &graph, &mut unit).unwrap();
        let once = locations(&unit).len();
        enricher.enrich_unit(&ctx(), &graph, &mut unit).unwrap();
        assert_eq!(once, 2);
        assert_eq!(locations(&unit).len(), 2);
    }

    #[test]
    fn units_without_gps_or_text_are_untouched() {
        let (graph, mut unit) = photo(&[("exiftool.File:ImageWidth", "640")]);
        let warnings = LocationEnricher::new(LocationMode::On)
            .enrich_unit(&ctx(), &graph, &mut unit)
            .unwrap();
        assert!(warnings.is_empty());
        assert!(unit.annotations.is_empty());
    }
}
