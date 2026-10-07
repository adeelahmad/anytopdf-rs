//! The host step that turns embedded faces into people.
//!
//! Each face is compared with every person in the index (enrolled people and
//! earlier unnamed clusters). The best match at or above the threshold names
//! the face; otherwise the face starts a new `person-N` cluster that later
//! faces in this run, and later runs, can join. Two faces in one unit never
//! resolve to the same person. Only confident matches (at or above
//! [`LEARN_THRESHOLD`]) add the face to the person's stored embeddings, so a
//! borderline match cannot pull later faces toward the wrong person.

use crate::index::{FaceIndex, Person, Sighting};
use crate::vector;
use crate::workspace::{Embedded, REF_ATTR};
use anyhow::Result;
use anytopdf_core::{Anchor, AnnotationKind, DocumentGraph, Unit, Uuid, content_unit_id};
use std::collections::{BTreeMap, HashMap, HashSet, hash_map::Entry};

/// Cosine similarity at which a face counts as a known person. Suits
/// ArcFace-style 512-d embeddings; other models may need their own value.
pub const DEFAULT_THRESHOLD: f32 = 0.40;

/// Cosine similarity a match needs before the face is remembered as another
/// example of that person. Faces that start a new person are always kept.
pub const LEARN_THRESHOLD: f32 = 0.60;

/// Display name (or `person-N` label) of the recognized person.
pub const PERSON_ATTR: &str = "person";
/// Stable `person-N` label, unchanged when the person is named later.
pub const PERSON_ID_ATTR: &str = "person.id";
/// Cosine similarity of the match; absent when the face started a new person.
pub const SIMILARITY_ATTR: &str = "person.similarity";
/// Unit metadata marking the generated "who is where" page.
pub const SUMMARY_KEY: &str = "anytopdf.summary";
pub const SUMMARY_PEOPLE: &str = "people";

#[derive(Debug, Clone)]
pub struct Options {
    pub threshold: f32,
    /// Similarity at or above which a matched face joins the person's stored
    /// embeddings; never below `threshold`.
    pub learn_threshold: f32,
}

impl Options {
    pub fn with_threshold(threshold: f32) -> Self {
        Self {
            threshold,
            ..Self::default()
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            threshold: DEFAULT_THRESHOLD,
            learn_threshold: LEARN_THRESHOLD,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Face annotations that carried an embedding reference.
    pub faces: usize,
    /// Faces matched to someone already in the index or seen earlier this run.
    pub recognized: usize,
    /// New unnamed people created for faces that matched nobody.
    pub new_people: usize,
    /// References whose embedding was missing from the workspace.
    pub missing: usize,
}

/// Names every embedded face in `graph` and records each sighting in `index`.
pub fn recognize(
    graph: &mut DocumentGraph,
    embeddings: &HashMap<String, Embedded>,
    index: &mut FaceIndex,
    opts: &Options,
) -> Result<Outcome> {
    index.transaction(|index| {
        let mut outcome = Outcome::default();
        let mut galleries: HashMap<String, Vec<(i64, Vec<f32>)>> = HashMap::new();
        let mut people: HashMap<i64, Person> = HashMap::new();
        let mut positions: HashMap<Uuid, usize> = HashMap::new();
        let DocumentGraph { sources, units, .. } = graph;
        for unit in units.iter_mut() {
            let position = positions.entry(unit.source_id).or_default();
            let unit_position = *position;
            *position += 1;
            let source = sources.iter().find(|s| s.id == unit.source_id);
            let frame = match unit.anchor {
                Some(Anchor::Region { frame, .. }) => frame,
                _ => None,
            };
            let mut taken = HashSet::new();
            for annotation in &mut unit.annotations {
                if annotation.kind != AnnotationKind::Face {
                    continue;
                }
                let Some(face_ref) = annotation.attributes.remove(REF_ATTR) else {
                    continue;
                };
                outcome.faces += 1;
                let Some(embedded) = embeddings.get(&face_ref) else {
                    outcome.missing += 1;
                    continue;
                };
                let v = vector::normalize(embedded.vector.clone());
                let gallery = match galleries.entry(embedded.model.clone()) {
                    Entry::Occupied(e) => e.into_mut(),
                    Entry::Vacant(e) => e.insert(index.gallery(&embedded.model)?),
                };
                let matched = best_match(gallery, &v, &taken, opts.threshold);
                let (person_id, similarity) = match matched {
                    Some((id, score)) => {
                        outcome.recognized += 1;
                        (id, Some(score))
                    }
                    None => {
                        outcome.new_people += 1;
                        (index.create_person(None)?.id, None)
                    }
                };
                taken.insert(person_id);
                let learn = opts.learn_threshold.max(opts.threshold);
                if similarity.is_none_or(|score| score >= learn) {
                    gallery.push((person_id, v.clone()));
                    index.add_observed(person_id, &embedded.model, &v)?;
                }
                let time = annotation.time_range.or(unit.time_range);
                let region = annotation.region.map(|r| [r.x, r.y, r.width, r.height]);
                index.add_sighting(
                    person_id,
                    &embedded.model,
                    &v,
                    &Sighting {
                        source_path: source
                            .map(|s| s.path.display().to_string())
                            .unwrap_or_default(),
                        source_sha256: source.and_then(|s| s.sha256.clone()),
                        start_seconds: time.map(|t| t.start_seconds),
                        end_seconds: time.map(|t| t.end_seconds),
                        frame,
                        region,
                        unit: unit_position,
                        similarity,
                    },
                )?;
                let person = match people.entry(person_id) {
                    Entry::Occupied(e) => e.into_mut(),
                    Entry::Vacant(e) => e.insert(index.person_by_id(person_id)?),
                };
                annotation
                    .attributes
                    .insert(PERSON_ATTR.into(), person.display().into());
                annotation
                    .attributes
                    .insert(PERSON_ID_ATTR.into(), person.label.clone());
                if let Some(score) = similarity {
                    annotation
                        .attributes
                        .insert(SIMILARITY_ATTR.into(), format!("{score:.3}"));
                }
                annotation.text = match annotation.text.trim() {
                    "" => person.display().to_string(),
                    text => format!("{text} {}", person.display()),
                };
            }
        }
        Ok(outcome)
    })
}

/// The best person at or above `threshold`, skipping people already
/// assigned in this unit.
fn best_match(
    gallery: &[(i64, Vec<f32>)],
    v: &[f32],
    taken: &HashSet<i64>,
    threshold: f32,
) -> Option<(i64, f32)> {
    let mut best: BTreeMap<i64, f32> = BTreeMap::new();
    for (person, stored) in gallery {
        let score = vector::cosine(stored, v);
        let entry = best.entry(*person).or_insert(f32::MIN);
        *entry = entry.max(score);
    }
    best.into_iter()
        .filter(|(person, score)| !taken.contains(person) && *score >= threshold)
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

/// Removes embedding references from faces that were not recognized, so they
/// never reach the PDF.
pub fn strip_refs(graph: &mut DocumentGraph) {
    for unit in &mut graph.units {
        for annotation in &mut unit.annotations {
            annotation.attributes.remove(REF_ATTR);
        }
    }
}

/// Appends a visible "People in …" text unit after each source that has
/// recognized faces, listing each person with the times (or pages) where
/// they appear. Returns how many pages were added.
pub fn add_people_summaries(graph: &mut DocumentGraph) -> usize {
    let mut added = 0;
    for source in graph.sources.clone() {
        let mut appearances: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut count = 0;
        for unit in graph.units.iter().filter(|u| u.source_id == source.id) {
            count += 1;
            for annotation in &unit.annotations {
                let Some(person) = annotation.attributes.get(PERSON_ATTR) else {
                    continue;
                };
                let at = match annotation.time_range.or(unit.time_range) {
                    Some(t) => clock(t.start_seconds),
                    None => format!("page {count}"),
                };
                let list = appearances.entry(person.clone()).or_default();
                if !list.contains(&at) {
                    list.push(at);
                }
            }
        }
        if appearances.is_empty() {
            continue;
        }
        let name = source
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut text = format!("People in {name}\n\n");
        for (person, at) in &appearances {
            text.push_str(&format!("{person}: {}\n", at.join(", ")));
        }
        let mut unit = Unit::text(source.id, text);
        unit.id = content_unit_id(source.id, count);
        unit.anchor = Some(unit.default_anchor(&source));
        unit.metadata
            .insert(SUMMARY_KEY.into(), SUMMARY_PEOPLE.into());
        let after = graph
            .units
            .iter()
            .rposition(|u| u.source_id == source.id)
            .map_or(graph.units.len(), |i| i + 1);
        graph.units.insert(after, unit);
        added += 1;
    }
    added
}

fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anytopdf_core::{Annotation, Region, SourceRecord, TimeRange};

    fn face(face_ref: &str, seconds: f64) -> Annotation {
        let mut a = Annotation::text(AnnotationKind::Face, "faces", "face");
        a.region = Some(Region {
            x: 0.1,
            y: 0.1,
            width: 0.2,
            height: 0.2,
        });
        a.time_range = Some(TimeRange::point(seconds));
        a.attributes.insert(REF_ATTR.into(), face_ref.into());
        a
    }

    fn graph(frames: Vec<Vec<Annotation>>) -> DocumentGraph {
        let source = SourceRecord::new("/videos/party.mp4".into());
        let units = frames
            .into_iter()
            .enumerate()
            .map(|(i, annotations)| {
                let mut u = Unit::visual(source.id, format!("/w/{i}.png").into());
                u.id = content_unit_id(source.id, i);
                u.annotations = annotations;
                u
            })
            .collect();
        DocumentGraph {
            sources: vec![source],
            units,
            metadata: Default::default(),
        }
    }

    fn embedded(pairs: &[(&str, [f32; 3])]) -> HashMap<String, Embedded> {
        pairs
            .iter()
            .map(|(r, v)| {
                (
                    r.to_string(),
                    Embedded {
                        model: "m".into(),
                        vector: v.to_vec(),
                    },
                )
            })
            .collect()
    }

    fn people(graph: &DocumentGraph) -> Vec<Option<String>> {
        graph
            .units
            .iter()
            .flat_map(|u| &u.annotations)
            .map(|a| a.attributes.get(PERSON_ATTR).cloned())
            .collect()
    }

    #[test]
    fn enrolled_people_are_named_and_unknowns_cluster() {
        let mut index = FaceIndex::open_in_memory().unwrap();
        index.enroll("Alice", "m", &[vec![1.0, 0.0, 0.0]]).unwrap();
        let mut g = graph(vec![
            vec![face("a1", 1.0), face("u1", 1.0)],
            vec![face("a2", 6.0), face("u2", 6.0)],
            vec![face("z", 9.0)],
        ]);
        let vectors = embedded(&[
            ("a1", [0.95, 0.1, 0.0]),
            ("a2", [0.9, 0.0, 0.2]),
            ("u1", [0.0, 1.0, 0.0]),
            ("u2", [0.1, 0.95, 0.0]),
            ("z", [0.0, 0.0, 1.0]),
        ]);
        let outcome = recognize(&mut g, &vectors, &mut index, &Options::default()).unwrap();
        assert_eq!(
            outcome,
            Outcome {
                faces: 5,
                recognized: 3,
                new_people: 2,
                missing: 0
            }
        );
        assert_eq!(
            people(&g),
            [
                Some("Alice".into()),
                Some("person-2".into()),
                Some("Alice".into()),
                Some("person-2".into()),
                Some("person-3".into()),
            ]
        );
        let first = &g.units[0].annotations[0];
        assert_eq!(first.text, "face Alice");
        assert!(!first.attributes.contains_key(REF_ATTR));
        assert!(first.attributes.contains_key(SIMILARITY_ATTR));
        assert_eq!(index.sightings("m").unwrap().len(), 5);
    }

    #[test]
    fn a_named_cluster_is_recognized_in_the_next_run() {
        let mut index = FaceIndex::open_in_memory().unwrap();
        let mut g = graph(vec![vec![face("x", 2.0)]]);
        recognize(
            &mut g,
            &embedded(&[("x", [0.0, 1.0, 0.0])]),
            &mut index,
            &Options::default(),
        )
        .unwrap();
        assert_eq!(people(&g), [Some("person-1".into())]);
        index.name("person-1", "Bob").unwrap();

        let mut next = graph(vec![vec![face("y", 3.0)]]);
        recognize(
            &mut next,
            &embedded(&[("y", [0.05, 0.98, 0.0])]),
            &mut index,
            &Options::default(),
        )
        .unwrap();
        assert_eq!(people(&next), [Some("Bob".into())]);
        assert_eq!(
            next.units[0].annotations[0].attributes[PERSON_ID_ATTR],
            "person-1"
        );
    }

    #[test]
    fn two_faces_in_one_frame_are_never_the_same_person() {
        let mut index = FaceIndex::open_in_memory().unwrap();
        index.enroll("Alice", "m", &[vec![1.0, 0.0, 0.0]]).unwrap();
        let mut g = graph(vec![vec![face("a", 1.0), face("b", 1.0)]]);
        recognize(
            &mut g,
            &embedded(&[("a", [1.0, 0.0, 0.0]), ("b", [0.99, 0.05, 0.0])]),
            &mut index,
            &Options::default(),
        )
        .unwrap();
        assert_eq!(people(&g), [Some("Alice".into()), Some("person-2".into())]);
    }

    #[test]
    fn missing_embeddings_and_other_models_do_not_match() {
        let mut index = FaceIndex::open_in_memory().unwrap();
        index
            .enroll("Alice", "other", &[vec![1.0, 0.0, 0.0]])
            .unwrap();
        let mut g = graph(vec![vec![face("a", 1.0), face("gone", 1.0)]]);
        let outcome = recognize(
            &mut g,
            &embedded(&[("a", [1.0, 0.0, 0.0])]),
            &mut index,
            &Options::default(),
        )
        .unwrap();
        assert_eq!((outcome.new_people, outcome.missing), (1, 1));
        assert_eq!(people(&g), [Some("person-2".into()), None]);
        assert!(!g.units[0].annotations[1].attributes.contains_key(REF_ATTR));
    }

    #[test]
    fn summary_lists_each_person_after_their_source() {
        let mut index = FaceIndex::open_in_memory().unwrap();
        index.enroll("Alice", "m", &[vec![1.0, 0.0, 0.0]]).unwrap();
        let mut g = graph(vec![vec![face("a", 5.0)], vec![face("b", 65.0)]]);
        recognize(
            &mut g,
            &embedded(&[("a", [1.0, 0.0, 0.0]), ("b", [1.0, 0.1, 0.0])]),
            &mut index,
            &Options::default(),
        )
        .unwrap();
        assert_eq!(add_people_summaries(&mut g), 1);
        g.validate().unwrap();
        let summary = g.units.last().unwrap();
        assert_eq!(summary.metadata[SUMMARY_KEY], SUMMARY_PEOPLE);
        assert_eq!(
            summary.visible_text.as_deref(),
            Some("People in party.mp4\n\nAlice: 0:05, 1:05\n")
        );
    }

    #[test]
    fn borderline_matches_are_named_but_not_learned() {
        let mut index = FaceIndex::open_in_memory().unwrap();
        index.enroll("Alice", "m", &[vec![1.0, 0.0, 0.0]]).unwrap();
        // "b" matches Alice at 0.50: named, but not stored. "c" sits close to
        // "b" and far from Alice, so it must not reach Alice through "b".
        let mut g = graph(vec![
            vec![face("a", 1.0)],
            vec![face("b", 2.0)],
            vec![face("c", 3.0)],
        ]);
        let outcome = recognize(
            &mut g,
            &embedded(&[
                ("a", [0.98, 0.0, 0.2]),
                ("b", [0.5, 0.866, 0.0]),
                ("c", [0.1, 0.995, 0.0]),
            ]),
            &mut index,
            &Options::default(),
        )
        .unwrap();
        assert_eq!((outcome.recognized, outcome.new_people), (2, 1));
        assert_eq!(
            people(&g),
            [
                Some("Alice".into()),
                Some("Alice".into()),
                Some("person-2".into())
            ]
        );
        // Enrolled photo + the confident match "a" + new person "c".
        assert_eq!(index.gallery("m").unwrap().len(), 3);
        assert_eq!(index.sightings("m").unwrap().len(), 3);
    }

    #[test]
    fn strip_refs_removes_every_reference() {
        let mut g = graph(vec![vec![face("a", 1.0)]]);
        strip_refs(&mut g);
        assert!(g.units[0].annotations[0].attributes.is_empty());
        assert_eq!(clock(3725.0), "1:02:05");
    }
}
