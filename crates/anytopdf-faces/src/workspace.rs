//! Hand-off between the embedding plugin and the host.
//!
//! The plugin tags every face annotation it embedded with [`REF_ATTR`] and
//! writes the vectors to `face-id/*.json` in the job workspace, so embeddings
//! never enter the graph, the PDF or its attachments. The host reads them
//! back, matches them against the face index and removes the tags.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

pub const DIR: &str = "face-id";
/// Annotation attribute linking a face to its embedding in the workspace.
pub const REF_ATTR: &str = "face.ref";
/// Detector attribute with five normalized landmark points (see
/// [`crate::align::parse_landmarks`]).
pub const LANDMARKS_ATTR: &str = "landmarks";
/// Optional detector attribute: an aligned 112x112 crop, absolute or relative
/// to the job workspace.
pub const CROP_ATTR: &str = "crop";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingFile {
    /// Identifies the embedding model; vectors from different models are
    /// never compared.
    pub model: String,
    pub faces: Vec<FaceEmbedding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceEmbedding {
    #[serde(rename = "ref")]
    pub face_ref: String,
    pub embedding: Vec<f32>,
}

/// An embedding read back from the workspace.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedded {
    pub model: String,
    pub vector: Vec<f32>,
}

/// Writes one embedding file atomically and returns its path.
pub fn write(workspace: &Path, file: &EmbeddingFile) -> Result<PathBuf> {
    let dir = workspace.join(DIR);
    fs::create_dir_all(&dir).context("create face-id directory")?;
    let path = dir.join(format!("{}.json", uuid::Uuid::new_v4()));
    let partial = path.with_extension("json.tmp");
    fs::write(&partial, serde_json::to_vec(file)?).context("write embeddings")?;
    fs::rename(&partial, &path).context("publish embeddings")?;
    Ok(path)
}

/// Reads every embedding file in the workspace, keyed by face reference.
pub fn read_all(workspace: &Path) -> Result<HashMap<String, Embedded>> {
    let mut out = HashMap::new();
    let Ok(entries) = fs::read_dir(workspace.join(DIR)) else {
        return Ok(out);
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let file: EmbeddingFile = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("parse {}", path.display()))?;
        for face in file.faces {
            out.insert(
                face.face_ref,
                Embedded {
                    model: file.model.clone(),
                    vector: face.embedding,
                },
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_round_trip_and_ignore_partial_writes() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            &EmbeddingFile {
                model: "m1".into(),
                faces: vec![FaceEmbedding {
                    face_ref: "a".into(),
                    embedding: vec![1.0, 0.0],
                }],
            },
        )
        .unwrap();
        fs::write(dir.path().join(DIR).join("x.json.tmp"), "{").unwrap();
        let all = read_all(dir.path()).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all["a"].model, "m1");
        assert!(read_all(&dir.path().join("missing")).unwrap().is_empty());
    }
}
