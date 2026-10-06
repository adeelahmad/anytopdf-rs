use crate::{AnnotationKind, DocumentGraph, Metadata};
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    #[default]
    Archive,
    Share,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Document,
    GraphDump,
}

/// Unit metadata key marking generated summary pages (see `anytopdf-faces`).
const SUMMARY_KEY: &str = "anytopdf.summary";

const VOLATILE_SUFFIXES: &[&str] = &[
    "FileAccessDate",
    "FileModifyDate",
    "FileInodeChangeDate",
    "FileCreateDate",
    "FilePermissions",
    "Directory",
];

const SHARE_SUFFIXES: &[&str] = &[
    "FileType",
    "FileTypeExtension",
    "MIMEType",
    "ImageWidth",
    "ImageHeight",
    "ImageSize",
    "Megapixels",
    "Duration",
    "duration",
    "width",
    "height",
    "codec_name",
    "codec_type",
    "format_name",
    "nb_frames",
    "FrameCount",
    "PageCount",
    "Orientation",
    "dpi",
];

fn key_suffix(key: &str) -> &str {
    key.rsplit([':', '.']).next().unwrap_or(key)
}

fn keep_key(profile: Profile, channel: Channel, key: &str) -> bool {
    let volatile = VOLATILE_SUFFIXES.contains(&key_suffix(key));
    match profile {
        Profile::Archive => channel == Channel::GraphDump || !volatile,
        Profile::Share => {
            !volatile
                && (key == "source.filename"
                    || key == "fs.size-bytes"
                    || SHARE_SUFFIXES.contains(&key_suffix(key)))
        }
    }
}

fn filter_metadata(profile: Profile, channel: Channel, metadata: &Metadata) -> Metadata {
    metadata
        .iter()
        .filter(|(k, _)| keep_key(profile, channel, k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

impl Profile {
    pub fn as_str(&self) -> &'static str {
        match self {
            Profile::Archive => "archive",
            Profile::Share => "share",
        }
    }

    pub fn filter(&self, graph: &DocumentGraph, channel: Channel) -> DocumentGraph {
        let mut out = graph.clone();
        for source in &mut out.sources {
            source.metadata = filter_metadata(*self, channel, &source.metadata);
        }
        for unit in &mut out.units {
            unit.metadata = filter_metadata(*self, channel, &unit.metadata);
            for annotation in &mut unit.annotations {
                annotation.attributes = filter_metadata(*self, channel, &annotation.attributes);
            }
        }
        if *self == Profile::Share {
            for source in &mut out.sources {
                source.path = PathBuf::from(crate::basename(&source.path));
            }
            // Recognized names and the "People in …" pages identify people.
            let summaries: Vec<_> = graph
                .units
                .iter()
                .filter(|u| u.metadata.get(SUMMARY_KEY).is_some_and(|v| v == "people"))
                .map(|u| u.id)
                .collect();
            for (unit, original) in out.units.iter_mut().zip(&graph.units) {
                for (annotation, before) in unit.annotations.iter_mut().zip(&original.annotations) {
                    if before.kind == AnnotationKind::Face
                        && before.attributes.contains_key("person")
                    {
                        annotation.text = "face".into();
                    }
                }
            }
            out.units.retain(|u| !summaries.contains(&u.id));
            for unit in &mut out.units {
                unit.visual_path = None;
                unit.annotations
                    .retain(|a| a.kind != AnnotationKind::Location);
            }
        }
        out
    }
}

impl FromStr for Profile {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "archive" => Ok(Profile::Archive),
            "share" => Ok(Profile::Share),
            other => Err(format!(
                "unknown profile {other:?}; expected archive or share"
            )),
        }
    }
}

pub fn strip_workspace_paths(graph: &DocumentGraph, workspace: &Path) -> DocumentGraph {
    let mut out = graph.clone();
    for unit in &mut out.units {
        if unit
            .visual_path
            .as_deref()
            .is_some_and(|p| p.starts_with(workspace))
        {
            unit.visual_path = None;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Annotation, AnnotationKind, Metadata, SourceRecord, Unit};
    use std::path::PathBuf;

    fn meta(pairs: &[(&str, &str)]) -> Metadata {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn photo_graph(dir: &Path) -> DocumentGraph {
        let abs = dir.join("secret-dir").join("photo.jpg");
        let mut source = SourceRecord::new(abs.clone());
        source.metadata = meta(&[
            ("exiftool.GPS:GPSLatitude", "51.5"),
            ("exiftool.Composite:GPSPosition", "51.5 0.1"),
            ("exiftool.ExifIFD:SerialNumber", "SN-123"),
            ("exiftool.ExifIFD:BodySerialNumber", "BSN-9"),
            ("exiftool.ExifIFD:OwnerName", "Alice Owner"),
            ("exiftool.IFD0:Artist", "Bob Artist"),
            ("source.path", abs.to_str().unwrap()),
            ("exiftool.File:ImageWidth", "640"),
            ("source.filename", "photo.jpg"),
        ]);
        let mut unit = Unit::visual(source.id, dir.join("ws").join("image-1.png"));
        unit.annotations.push(Annotation {
            kind: AnnotationKind::Location,
            text: "51.5, 0.1".into(),
            provider: "exiftool".into(),
            confidence: None,
            region: None,
            time_range: None,
            attributes: Metadata::new(),
        });
        DocumentGraph {
            sources: vec![source],
            units: vec![unit],
            metadata: Metadata::new(),
        }
    }

    #[test]
    fn share_drops_gps_serial_owner_artist_and_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        let graph = photo_graph(dir.path());
        let before = serde_json::to_string(&graph).unwrap();
        for channel in [Channel::Document, Channel::GraphDump] {
            let out = Profile::Share.filter(&graph, channel);
            let json = serde_json::to_string(&out).unwrap();
            for needle in [
                "51.5",
                "SN-123",
                "BSN-9",
                "Alice Owner",
                "Bob Artist",
                "secret-dir",
            ] {
                assert!(
                    !json.contains(needle),
                    "{channel:?} leaked {needle}: {json}"
                );
            }
            assert_eq!(out.sources[0].path, PathBuf::from("photo.jpg"));
            assert_eq!(
                out.sources[0]
                    .metadata
                    .get("exiftool.File:ImageWidth")
                    .map(String::as_str),
                Some("640")
            );
            assert_eq!(
                out.sources[0]
                    .metadata
                    .get("source.filename")
                    .map(String::as_str),
                Some("photo.jpg")
            );
            assert!(out.units.iter().all(|u| u.visual_path.is_none()));
            assert!(
                out.units
                    .iter()
                    .flat_map(|u| &u.annotations)
                    .all(|a| a.kind != AnnotationKind::Location)
            );
        }
        assert_eq!(serde_json::to_string(&graph).unwrap(), before);
    }

    #[test]
    fn archive_document_keeps_gps_but_drops_volatile_filesystem_fields() {
        let dir = tempfile::tempdir().unwrap();
        let mut graph = photo_graph(dir.path());
        for key in [
            "exiftool.System:FileAccessDate",
            "exiftool.System:FileModifyDate",
            "exiftool.System:FileInodeChangeDate",
            "exiftool.System:FilePermissions",
            "exiftool.System:Directory",
        ] {
            graph.sources[0].metadata.insert(key.into(), "x".into());
        }
        let out = Profile::Archive.filter(&graph, Channel::Document);
        let m = &out.sources[0].metadata;
        assert!(m.contains_key("exiftool.GPS:GPSLatitude"));
        assert!(m.contains_key("source.path"));
        for key in [
            "exiftool.System:FileAccessDate",
            "exiftool.System:FileModifyDate",
            "exiftool.System:FileInodeChangeDate",
            "exiftool.System:FilePermissions",
            "exiftool.System:Directory",
        ] {
            assert!(!m.contains_key(key), "{key} survived");
        }
    }

    #[test]
    fn archive_graph_dump_keeps_volatile_fields() {
        let dir = tempfile::tempdir().unwrap();
        let mut graph = photo_graph(dir.path());
        graph.sources[0]
            .metadata
            .insert("exiftool.System:FileAccessDate".into(), "x".into());
        graph.sources[0]
            .metadata
            .insert("exiftool.System:Directory".into(), "/d".into());
        let out = Profile::Archive.filter(&graph, Channel::GraphDump);
        assert_eq!(out.sources[0].metadata, graph.sources[0].metadata);
    }

    #[test]
    fn profile_names_parse_and_default_to_archive() {
        assert_eq!("archive".parse::<Profile>(), Ok(Profile::Archive));
        assert_eq!("share".parse::<Profile>(), Ok(Profile::Share));
        assert!("Share".parse::<Profile>().is_err());
        assert!("public".parse::<Profile>().is_err());
        assert_eq!(Profile::default(), Profile::Archive);
        for p in [Profile::Archive, Profile::Share] {
            assert_eq!(p.as_str().parse::<Profile>(), Ok(p));
        }
    }

    #[test]
    fn strip_workspace_paths_removes_only_workspace_visuals() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        let other = dir.path().join("other");
        let source = SourceRecord::new(dir.path().join("a.png"));
        let inside = Unit::visual(source.id, ws.join("image-1.png"));
        let outside = Unit::visual(source.id, other.join("keep.png"));
        let graph = DocumentGraph {
            sources: vec![source],
            units: vec![inside, outside],
            metadata: Metadata::new(),
        };
        let out = strip_workspace_paths(&graph, &ws);
        assert_eq!(out.units[0].visual_path, None);
        assert_eq!(out.units[1].visual_path, Some(other.join("keep.png")));
    }

    #[test]
    fn share_drops_recognized_names_and_people_pages() {
        let dir = tempfile::tempdir().unwrap();
        let mut graph = photo_graph(dir.path());
        let mut face = Annotation::text(AnnotationKind::Face, "faces", "face Alice");
        face.attributes = meta(&[("person", "Alice"), ("person.id", "person-1")]);
        graph.units[0].annotations.push(face);
        graph.units[0]
            .annotations
            .push(Annotation::text(AnnotationKind::Face, "faces", "2 faces"));
        let mut summary = Unit::text(
            graph.sources[0].id,
            "People in photo.jpg\n\nAlice: page 1".into(),
        );
        summary.metadata = meta(&[("anytopdf.summary", "people")]);
        graph.units.push(summary);

        let archive = Profile::Archive.filter(&graph, Channel::Document);
        assert_eq!(archive.units.len(), 2);
        assert_eq!(archive.units[0].annotations[1].text, "face Alice");

        for channel in [Channel::Document, Channel::GraphDump] {
            let shared = Profile::Share.filter(&graph, channel);
            assert_eq!(shared.units.len(), 1);
            let json = serde_json::to_string(&shared).unwrap();
            assert!(
                !json.contains("Alice") && !json.contains("person-1"),
                "{json}"
            );
        }
    }
}
