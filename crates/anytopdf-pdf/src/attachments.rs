use anyhow::{Context, Result};
use lopdf::{Dictionary, Document, Object, Stream, dictionary};

/// Attachment names shared by every renderer and by `extract`.
pub const MANIFEST_FILE: &str = "anytopdf-manifest.json";
pub const CHUNKS_FILE: &str = "anytopdf-chunks.json";

pub struct EmbeddedFile {
    pub name: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

pub fn embed_files(pdf: &[u8], files: &[EmbeddedFile]) -> Result<Vec<u8>> {
    let mut doc = Document::load_mem(pdf).context("input is not a valid PDF")?;
    let mut sorted: Vec<&EmbeddedFile> = files.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut names = Vec::new();
    let mut af = Vec::new();
    for f in sorted {
        let mut sdict = Dictionary::new();
        sdict.set("Type", Object::Name(b"EmbeddedFile".to_vec()));
        sdict.set("Subtype", Object::Name(f.mime_type.as_bytes().to_vec()));
        let stream_id = doc.add_object(Stream::new(sdict, f.bytes.clone()));
        let name = Object::string_literal(f.name.as_str());
        let spec = dictionary! {
            "Type" => "Filespec",
            "F" => name.clone(),
            "UF" => name.clone(),
            "AFRelationship" => "Data",
            "EF" => dictionary! { "F" => stream_id },
        };
        let spec_id = doc.add_object(spec);
        names.push(name);
        names.push(Object::Reference(spec_id));
        af.push(Object::Reference(spec_id));
    }
    let tree_id = doc.add_object(dictionary! { "Names" => names });
    let root_id = doc.trailer.get(b"Root")?.as_reference()?;
    let catalog = doc.get_object_mut(root_id)?.as_dict_mut()?;
    let mut names_dict = match catalog.get(b"Names") {
        Ok(Object::Dictionary(d)) => d.clone(),
        _ => Dictionary::new(),
    };
    names_dict.set("EmbeddedFiles", Object::Reference(tree_id));
    catalog.set("Names", names_dict);
    catalog.set("AF", af);
    let mut out = Vec::new();
    doc.save_to(&mut out)?;
    Ok(out)
}

pub fn read_embedded_files(pdf: &[u8]) -> Result<Vec<EmbeddedFile>> {
    let doc = Document::load_mem(pdf).context("input is not a valid PDF")?;
    let catalog = doc.catalog()?;
    let Ok(names) = catalog.get(b"Names").and_then(|o| doc.dereference(o)) else {
        return Ok(Vec::new());
    };
    let Ok(tree) = names
        .1
        .as_dict()?
        .get(b"EmbeddedFiles")
        .and_then(|o| doc.dereference(o))
    else {
        return Ok(Vec::new());
    };
    let pairs = tree.1.as_dict()?.get(b"Names")?.as_array()?;
    let mut out = Vec::new();
    for pair in pairs.chunks(2) {
        let [name, spec] = pair else { continue };
        let spec = doc.dereference(spec)?.1.as_dict()?;
        let ef = doc.dereference(spec.get(b"EF")?)?.1.as_dict()?;
        let stream = doc.dereference(ef.get(b"F")?)?.1.as_stream()?;
        let mime = stream.dict.get(b"Subtype")?.as_name()?;
        out.push(EmbeddedFile {
            name: String::from_utf8_lossy(name.as_str()?).into_owned(),
            mime_type: String::from_utf8_lossy(mime).into_owned(),
            // Other writers (pdfa among them) compress attachments.
            bytes: if stream.dict.has(b"Filter") {
                stream
                    .decompressed_content()
                    .context("decode embedded file")?
            } else {
                stream.content.clone()
            },
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SearchablePdfRenderer;
    use anytopdf_core::*;

    fn plain_pdf() -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        let source = SourceRecord::new(dir.path().join("a.txt"));
        let graph = DocumentGraph {
            units: vec![Unit::text(source.id, "Identity fixture".into())],
            sources: vec![source],
            ..Default::default()
        };
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        let out = dir.path().join("plain.pdf");
        SearchablePdfRenderer {
            dpi: 144.0,
            unicode_font: None,
        }
        .render(&ctx, &graph, &out)
        .unwrap();
        std::fs::read(out).unwrap()
    }

    fn files() -> Vec<EmbeddedFile> {
        vec![
            EmbeddedFile {
                name: "anytopdf-manifest.json".into(),
                mime_type: "application/json".into(),
                bytes: br#"{"a":1}"#.to_vec(),
            },
            EmbeddedFile {
                name: "anytopdf-chunks.json".into(),
                mime_type: "application/json".into(),
                bytes: br#"{"b":[2]}"#.to_vec(),
            },
        ]
    }

    fn page_contents(bytes: &[u8]) -> Vec<Vec<u8>> {
        let doc = lopdf::Document::load_mem(bytes).unwrap();
        doc.get_pages()
            .values()
            .map(|id| doc.get_page_content(*id))
            .collect()
    }

    #[test]
    fn embedded_files_round_trip_byte_for_byte() {
        let embedded = embed_files(&plain_pdf(), &files()).unwrap();
        let read = read_embedded_files(&embedded);
        assert!(
            read.is_ok(),
            "read_embedded_files failed: {:?}",
            read.as_ref().err()
        );
        let read = read.unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].name, "anytopdf-chunks.json");
        assert_eq!(read[1].name, "anytopdf-manifest.json");
        for f in &read {
            assert_eq!(f.mime_type, "application/json");
        }
        assert_eq!(read[0].bytes, br#"{"b":[2]}"#);
        assert_eq!(read[1].bytes, br#"{"a":1}"#);
    }

    #[test]
    fn embedding_preserves_pages_and_content() {
        let plain = plain_pdf();
        let embedded = embed_files(&plain, &files()).unwrap();
        assert!(embedded.starts_with(b"%PDF-"));
        assert!(embedded != plain, "embedding must change the document");
        let before = lopdf::Document::load_mem(&plain).unwrap();
        let after = lopdf::Document::load_mem(&embedded).unwrap();
        assert_eq!(before.get_pages().len(), after.get_pages().len());
        assert!(page_contents(&plain) == page_contents(&embedded));
    }

    #[test]
    fn embedding_is_deterministic() {
        let plain = plain_pdf();
        let a = embed_files(&plain, &files()).unwrap();
        let b = embed_files(&plain, &files()).unwrap();
        assert!(a != plain, "embedding must change the document");
        assert!(a == b, "embedding must be byte-identical");
    }

    #[test]
    fn pdf_without_attachments_reads_as_empty() {
        let read = read_embedded_files(&plain_pdf());
        assert!(
            read.is_ok(),
            "read_embedded_files failed: {:?}",
            read.as_ref().err()
        );
        assert!(read.unwrap().is_empty());
    }

    #[test]
    fn non_pdf_input_is_rejected() {
        assert!(embed_files(b"not a pdf", &[]).is_err());
        assert!(read_embedded_files(b"not a pdf").is_err());
    }
}
