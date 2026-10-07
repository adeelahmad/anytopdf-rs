//! `anytopdf faces …` and the `--recognize-faces` step of `convert`.
//!
//! Detection and embedding happen in runtime plugins (a face detector and
//! `anytopdf-plugin-face-id`). This module owns the local face index: it
//! enrolls people from photos, names the embedded faces of a conversion,
//! clusters faces that match nobody as `person-N`, and searches past
//! conversions by photo. Identities come only from people the user enrolls
//! or names.

use crate::cli::{FacesArgs, FacesCommand};
use crate::convert::registry;
use crate::exit::{CliError, ExitClass, fail, tag};
use anyhow::{Context, Result};
use anytopdf_builtin::{BuiltinOptions, OcrMode};
use anytopdf_core::{
    AnnotationKind, Diagnostic, DiagnosticCode, DocumentGraph, Pipeline, RuntimePluginPolicy,
};
use anytopdf_faces::{
    index::{FaceIndex, Person},
    paths, recognize,
    vector::cosine,
    workspace::{self, Embedded},
};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

pub(crate) fn index_path(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    explicit
        .map(Path::to_path_buf)
        .or_else(paths::default_index)
        .ok_or_else(|| {
            fail(
                ExitClass::Usage,
                "no data directory for the face index; pass --face-index or set ANYTOPDF_FACE_INDEX",
            )
        })
}

/// Names the embedded faces of a finished pipeline run and adds the
/// "People in …" pages. Failures become warnings: the PDF is still written.
pub(crate) fn recognize_run(
    graph: &mut DocumentGraph,
    workspace: &Path,
    index: &Path,
    threshold: f32,
) -> (Vec<Diagnostic>, Option<recognize::Outcome>) {
    let embeddings = match workspace::read_all(workspace) {
        Ok(e) => e,
        Err(e) => {
            recognize::strip_refs(graph);
            return (
                vec![Diagnostic::new(
                    DiagnosticCode::EnrichmentFailed,
                    format!("face recognition skipped: {e:#}"),
                )],
                None,
            );
        }
    };
    let faces = graph
        .units
        .iter()
        .flat_map(|u| &u.annotations)
        .filter(|a| a.kind == AnnotationKind::Face)
        .count();
    if embeddings.is_empty() {
        let message = if faces == 0 {
            "face recognition found no faces; it needs a face detection plugin and \
             anytopdf-plugin-face-id on ANYTOPDF_PLUGIN_PATH"
        } else {
            "face recognition found faces but no embeddings; install anytopdf-plugin-face-id \
             and set ANYTOPDF_FACE_EMBED_MODEL"
        };
        recognize::strip_refs(graph);
        return (
            vec![Diagnostic::new(DiagnosticCode::ProviderMissing, message)],
            None,
        );
    }
    let result = FaceIndex::open(index).and_then(|mut index| {
        recognize::recognize(
            graph,
            &embeddings,
            &mut index,
            &recognize::Options::with_threshold(threshold),
        )
    });
    recognize::strip_refs(graph);
    match result {
        Ok(outcome) => {
            recognize::add_people_summaries(graph);
            (Vec::new(), Some(outcome))
        }
        Err(e) => (
            vec![Diagnostic::new(
                DiagnosticCode::EnrichmentFailed,
                format!("face recognition failed: {e:#}"),
            )],
            None,
        ),
    }
}

pub(crate) fn faces(args: FacesArgs, policy: &RuntimePluginPolicy) -> Result<(), CliError> {
    let path = index_path(args.face_index.as_deref())?;
    let open = || tag(ExitClass::Input, FaceIndex::open(&path));
    match args.command {
        FacesCommand::Enroll { name, images } => {
            tag(ExitClass::Usage, anytopdf_faces::index::check_name(&name))?;
            let person = enroll(&mut open()?, &name, &images, policy)?;
            println!(
                "Enrolled {} ({}) with {} face(s)",
                person.display(),
                person.label,
                person.embeddings
            );
        }
        FacesCommand::Import { dir } => {
            let mut index = open()?;
            let mut people = 0;
            let mut entries: Vec<_> = tag(ExitClass::Input, std::fs::read_dir(&dir))?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            entries.sort();
            for person_dir in entries {
                let name = person_dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if name.starts_with('.') {
                    continue;
                }
                let mut images: Vec<PathBuf> = std::fs::read_dir(&person_dir)?
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.is_file())
                    .collect();
                images.sort();
                match enroll(&mut index, &name, &images, policy) {
                    Ok(person) => {
                        people += 1;
                        println!(
                            "Enrolled {} with {} face(s)",
                            person.display(),
                            person.embeddings
                        );
                    }
                    Err(e) => eprintln!("warning: {name}: {:#}", e.error),
                }
            }
            if people == 0 {
                return Err(fail(
                    ExitClass::Input,
                    format!("no person enrolled from {}", dir.display()),
                ));
            }
        }
        FacesCommand::List { json } => {
            let people = open()?.people()?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({"people": people}))?
                );
            } else if people.is_empty() {
                println!("The face index is empty: {}", path.display());
            } else {
                for p in &people {
                    println!(
                        "{:<24} {:<12} {:>4} face(s) {:>6} sighting(s)",
                        p.name.as_deref().unwrap_or("(unnamed)"),
                        p.label,
                        p.embeddings,
                        p.sightings
                    );
                }
            }
        }
        FacesCommand::Rename { person, new_name } => {
            let p = tag(ExitClass::Usage, open()?.rename(&person, &new_name))?;
            println!("Renamed {person} to {}", p.display());
        }
        FacesCommand::Name { person, name } => {
            let p = tag(ExitClass::Usage, open()?.name(&person, &name))?;
            println!("{person} is now {} ({})", p.display(), p.label);
        }
        FacesCommand::Merge { from, into } => {
            let p = tag(ExitClass::Usage, open()?.merge(&from, &into))?;
            println!("Merged {from} into {}", p.display());
        }
        FacesCommand::Forget { person } => {
            let p = tag(ExitClass::Usage, open()?.forget(&person))?;
            println!(
                "Forgot {} with {} face(s) and {} sighting(s)",
                p.display(),
                p.embeddings,
                p.sightings
            );
        }
        FacesCommand::Find {
            photos,
            threshold,
            json,
        } => {
            let index = open()?;
            let queries = embed_photos(&photos, policy)?;
            let mut report = Vec::new();
            for (photo, embedded) in queries {
                let Some(embedded) = embedded else {
                    eprintln!("warning: no face found in {}", photo.display());
                    continue;
                };
                let hits = find(&index, &embedded, threshold)?;
                if !json {
                    println!("{}:", photo.display());
                    if hits.is_empty() {
                        println!("  no matches");
                    }
                    for hit in &hits {
                        println!("  {}", describe(hit));
                    }
                }
                report.push(json!({"photo": photo, "matches": hits}));
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({"results": report}))?
                );
            }
        }
    }
    Ok(())
}

fn enroll(
    index: &mut FaceIndex,
    name: &str,
    images: &[PathBuf],
    policy: &RuntimePluginPolicy,
) -> Result<Person, CliError> {
    let mut model = None;
    let mut vectors = Vec::new();
    for (photo, embedded) in embed_photos(images, policy)? {
        match embedded {
            Some(e) if model.as_ref().is_none_or(|m| *m == e.model) => {
                model = Some(e.model);
                vectors.push(e.vector);
            }
            Some(_) => eprintln!(
                "warning: {} was embedded with a different model; skipped",
                photo.display()
            ),
            None => eprintln!("warning: no face found in {}", photo.display()),
        }
    }
    let Some(model) = model else {
        return Err(fail(
            ExitClass::Provider,
            "no face could be embedded; check the face detection plugin, \
             anytopdf-plugin-face-id and ANYTOPDF_FACE_EMBED_MODEL with `anytopdf plugins`",
        ));
    };
    tag(ExitClass::Input, index.enroll(name, &model, &vectors))
}

/// Runs detection and embedding on each photo and returns the embedding of
/// its largest face.
fn embed_photos(
    photos: &[PathBuf],
    policy: &RuntimePluginPolicy,
) -> Result<Vec<(PathBuf, Option<Embedded>)>, CliError> {
    if photos.is_empty() {
        return Err(fail(ExitClass::Usage, "no photos given"));
    }
    for photo in photos {
        if !photo.is_file() {
            return Err(fail(
                ExitClass::Input,
                format!("photo not found: {}", photo.display()),
            ));
        }
    }
    let opts = BuiltinOptions {
        ocr: anytopdf_builtin::OcrOptions {
            mode: OcrMode::Off,
            ..Default::default()
        },
        ..BuiltinOptions::default()
    };
    let (registry, _) = registry(opts, policy);
    let inputs: Vec<PathBuf> = photos
        .iter()
        .map(|p| p.canonicalize())
        .collect::<std::io::Result<_>>()?;
    let run = tag(
        ExitClass::Input,
        Pipeline::new(registry).ingest(&inputs, true),
    )?;
    let embeddings = tag(
        ExitClass::Provider,
        workspace::read_all(&run.context.workspace),
    )?;
    Ok(photos
        .iter()
        .zip(&inputs)
        .map(|(photo, input)| {
            let largest = largest_face(&run.graph, input, &embeddings);
            (photo.clone(), largest)
        })
        .collect())
}

fn largest_face(
    graph: &DocumentGraph,
    input: &Path,
    embeddings: &HashMap<String, Embedded>,
) -> Option<Embedded> {
    let source = graph.sources.iter().find(|s| s.path == input)?;
    graph
        .units
        .iter()
        .filter(|u| u.source_id == source.id)
        .flat_map(|u| &u.annotations)
        .filter_map(|a| {
            let e = embeddings.get(a.attributes.get(workspace::REF_ATTR)?)?;
            let area = a.region.map_or(0.0, |r| r.width * r.height);
            Some((area, e))
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, e)| e.clone())
}

#[derive(Debug, serde::Serialize)]
struct Hit {
    source: String,
    start_seconds: Option<f64>,
    frame: Option<u32>,
    unit: usize,
    person: String,
    label: String,
    similarity: f32,
}

/// Every recorded sighting of the face in `query`: sightings that resemble
/// it directly, plus every sighting of the best-matching person.
fn find(index: &FaceIndex, query: &Embedded, threshold: f32) -> Result<Vec<Hit>> {
    let people: BTreeMap<i64, Person> = index.people()?.into_iter().map(|p| (p.id, p)).collect();
    let mut best_person: Option<(i64, f32)> = None;
    for (person, v) in index.gallery(&query.model)? {
        let score = cosine(&v, &query.vector);
        if score >= threshold && best_person.is_none_or(|(_, s)| score > s) {
            best_person = Some((person, score));
        }
    }
    let mut hits = Vec::new();
    for stored in index.sightings(&query.model)? {
        let score = cosine(&stored.vector, &query.vector);
        let same_person = best_person.is_some_and(|(p, _)| p == stored.person_id);
        if score < threshold && !same_person {
            continue;
        }
        let person = people
            .get(&stored.person_id)
            .context("sighting of an unknown person")?;
        let s = stored.sighting;
        hits.push(Hit {
            source: s.source_path,
            start_seconds: s.start_seconds,
            frame: s.frame,
            unit: s.unit,
            person: person.display().to_string(),
            label: person.label.clone(),
            similarity: score,
        });
    }
    hits.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then(
                a.start_seconds
                    .unwrap_or(0.0)
                    .total_cmp(&b.start_seconds.unwrap_or(0.0)),
            )
            .then(a.unit.cmp(&b.unit))
    });
    Ok(hits)
}

fn describe(hit: &Hit) -> String {
    let at = match (hit.start_seconds, hit.frame) {
        (Some(t), _) => format!("at {}", clock(t)),
        (None, Some(f)) => format!("frame {}", f + 1),
        (None, None) => format!("page {}", hit.unit + 1),
    };
    format!(
        "{} {at} ({}, similarity {:.2})",
        hit.source, hit.person, hit.similarity
    )
}

fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{}:{:02}:{:02}", total / 3600, total / 60 % 60, total % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anytopdf_faces::index::Sighting;

    fn embedded(v: [f32; 3]) -> Embedded {
        Embedded {
            model: "m".into(),
            vector: v.to_vec(),
        }
    }

    fn sighting(path: &str, t: f64) -> Sighting {
        Sighting {
            source_path: path.into(),
            start_seconds: Some(t),
            ..Default::default()
        }
    }

    #[test]
    fn find_by_photo_lists_direct_matches_and_the_matched_persons_sightings() {
        let mut index = FaceIndex::open_in_memory().unwrap();
        let alice = index.enroll("Alice", "m", &[vec![1.0, 0.0, 0.0]]).unwrap();
        // Alice seen from an angle the query does not resemble directly.
        index
            .add_sighting(alice.id, "m", &[0.0, 1.0, 0.0], &sighting("b.mp4", 70.0))
            .unwrap();
        index
            .add_sighting(alice.id, "m", &[1.0, 0.0, 0.0], &sighting("a.mp4", 5.0))
            .unwrap();
        let other = index.create_person(None).unwrap();
        index
            .add_sighting(other.id, "m", &[0.0, 0.0, 1.0], &sighting("a.mp4", 9.0))
            .unwrap();

        let hits = find(&index, &embedded([0.98, 0.1, 0.0]), 0.4).unwrap();
        let found: Vec<(&str, Option<f64>, &str)> = hits
            .iter()
            .map(|h| (h.source.as_str(), h.start_seconds, h.person.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                ("a.mp4", Some(5.0), "Alice"),
                ("b.mp4", Some(70.0), "Alice")
            ]
        );
        assert_eq!(
            describe(&hits[1]).split(" (").next(),
            Some("b.mp4 at 0:01:10")
        );

        // An unenrolled stranger is still found through raw sightings.
        let hits = find(&index, &embedded([0.0, 0.1, 1.0]), 0.4).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].label, other.label);
        assert!(
            find(&index, &embedded([0.7, 0.7, 0.0]), 0.99)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn recognize_run_without_embeddings_reports_a_missing_provider() {
        let dir = tempfile::tempdir().unwrap();
        let mut graph = DocumentGraph::default();
        let (diagnostics, outcome) =
            recognize_run(&mut graph, dir.path(), &dir.path().join("f.sqlite"), 0.4);
        assert!(outcome.is_none());
        assert_eq!(diagnostics[0].code, DiagnosticCode::ProviderMissing);
        assert!(!dir.path().join("f.sqlite").exists());
    }

    #[test]
    fn recognize_run_names_faces_and_adds_a_people_page() {
        use anytopdf_core::{Annotation, SourceRecord, Unit};
        let dir = tempfile::tempdir().unwrap();
        let index_path = dir.path().join("faces.sqlite");
        FaceIndex::open(&index_path)
            .unwrap()
            .enroll("Alice", "m", &[vec![1.0, 0.0, 0.0]])
            .unwrap();
        workspace::write(
            dir.path(),
            &workspace::EmbeddingFile {
                model: "m".into(),
                faces: vec![workspace::FaceEmbedding {
                    face_ref: "r1".into(),
                    embedding: vec![0.9, 0.1, 0.0],
                }],
            },
        )
        .unwrap();
        let source = SourceRecord::new("/photos/beach.jpg".into());
        let mut unit = Unit::visual(source.id, "/photos/beach.jpg".into());
        let mut face = Annotation::text(AnnotationKind::Face, "faces", "face");
        face.attributes
            .insert(workspace::REF_ATTR.into(), "r1".into());
        unit.annotations.push(face);
        let mut graph = DocumentGraph {
            sources: vec![source],
            units: vec![unit],
            metadata: Default::default(),
        };
        let (diagnostics, outcome) = recognize_run(&mut graph, dir.path(), &index_path, 0.4);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(outcome.unwrap().recognized, 1);
        assert_eq!(graph.units[0].annotations[0].text, "face Alice");
        assert_eq!(
            graph.units[1].visible_text.as_deref(),
            Some("People in beach.jpg\n\nAlice: page 1\n")
        );
        let people = FaceIndex::open(&index_path).unwrap().people().unwrap();
        assert_eq!(people[0].sightings, 1);
    }
}
