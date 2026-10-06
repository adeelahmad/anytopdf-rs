use crate::{CHUNK_KIND, Document};
use anyhow::{Context, Result, bail};
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `PRAGMA user_version` of the layout below. Later layouts (the face index,
/// embedding models) migrate forward from it.
const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE documents (
    id INTEGER PRIMARY KEY,
    pdf TEXT NOT NULL UNIQUE,
    pdf_sha256 TEXT NOT NULL,
    origin TEXT NOT NULL,
    profile TEXT,
    collection TEXT,
    indexed_at INTEGER NOT NULL
);
CREATE INDEX documents_collection ON documents(collection);
CREATE TABLE sources (
    document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL,
    name TEXT NOT NULL,
    path TEXT,
    media_type TEXT,
    sha256 TEXT,
    PRIMARY KEY (document_id, source_id)
);
CREATE TABLE entries (
    id INTEGER PRIMARY KEY,
    document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL,
    unit_id TEXT NOT NULL,
    unit_kind TEXT NOT NULL,
    kind TEXT NOT NULL,
    text TEXT NOT NULL,
    provider TEXT,
    confidence REAL,
    x REAL, y REAL, width REAL, height REAL,
    frame INTEGER,
    start_seconds REAL, end_seconds REAL,
    page_first INTEGER, page_last INTEGER,
    attributes TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX entries_document_unit ON entries(document_id, unit_id);
CREATE INDEX entries_kind ON entries(kind);
CREATE VIRTUAL TABLE entries_fts USING fts5(
    text, content='entries', content_rowid='id', tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER entries_fts_insert AFTER INSERT ON entries BEGIN
    INSERT INTO entries_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER entries_fts_delete AFTER DELETE ON entries BEGIN
    INSERT INTO entries_fts(entries_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TABLE embeddings (
    document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    unit_id TEXT NOT NULL,
    model TEXT NOT NULL,
    dim INTEGER NOT NULL,
    vector BLOB NOT NULL,
    PRIMARY KEY (document_id, unit_id, model)
);
";

const HIT_COLUMNS: &str =
    "d.pdf, d.collection, e.source_id, s.name, s.path, e.unit_id, e.unit_kind,
    e.kind, e.provider, e.confidence, e.text, e.x, e.y, e.width, e.height, e.frame,
    e.start_seconds, e.end_seconds, e.page_first, e.page_last, e.attributes";

pub struct Index {
    conn: Connection,
    path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    /// Search text (see [`crate::fts_query`]); `None` lists entries matching the filters.
    pub text: Option<String>,
    /// Only these entry kinds (`face`, `object`, `chunk`, ...); empty means all.
    pub kinds: Vec<String>,
    /// Only face entries recognised as this person (case-insensitive).
    pub person: Option<String>,
    pub collection: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HitSource {
    pub id: String,
    pub name: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PageSpan {
    pub first: i64,
    pub last: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimeSpan {
    pub start_seconds: f64,
    pub end_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HitRegion {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hit {
    pub pdf: String,
    pub collection: Option<String>,
    pub source: HitSource,
    pub unit_id: String,
    pub unit_kind: String,
    pub kind: String,
    pub provider: Option<String>,
    pub confidence: Option<f64>,
    pub text: String,
    /// The text around the match, with matched terms in `[` `]`.
    pub snippet: String,
    pub pages: Option<PageSpan>,
    pub time: Option<TimeSpan>,
    pub region: Option<HitRegion>,
    pub frame: Option<i64>,
    pub attributes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DocumentSummary {
    pub pdf: String,
    pub pdf_sha256: String,
    pub origin: String,
    pub profile: Option<String>,
    pub collection: Option<String>,
    pub indexed_at: i64,
    pub sources: i64,
    pub entries: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Neighbour {
    pub pdf: String,
    pub unit_id: String,
    pub similarity: f64,
    pub pages: Option<PageSpan>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn pdf_key(pdf: &Path) -> String {
    pdf.display().to_string()
}

impl Index {
    /// Open the index at `path`, creating it (and its directory) when missing.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create index directory {}", dir.display()))?;
        }
        Self::connect(path)
    }

    /// Open an index that must already exist (for searching).
    pub fn open_existing(path: &Path) -> Result<Self> {
        if !path.is_file() {
            bail!(
                "no search index at {}; build one with `anytopdf convert --index` or `anytopdf index add`",
                path.display()
            );
        }
        Self::connect(path)
    }

    fn connect(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("cannot open search index {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let mut index = Self {
            conn,
            path: path.to_path_buf(),
        };
        index.migrate()?;
        // WAL lets searches run while a queue worker is indexing.
        let _: String = index
            .conn
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        Ok(index)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        match version {
            SCHEMA_VERSION => Ok(()),
            0 => {
                let tables: i64 =
                    self.conn
                        .query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0))?;
                if tables > 0 {
                    bail!(
                        "{} is a database but not an anytopdf search index",
                        self.path.display()
                    );
                }
                let tx = self.conn.transaction()?;
                tx.execute_batch(SCHEMA)
                    .context("cannot create the search index tables")?;
                tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
                tx.commit()?;
                Ok(())
            }
            newer => bail!(
                "search index {} has layout version {newer}, newer than this anytopdf understands ({SCHEMA_VERSION}); upgrade anytopdf",
                self.path.display()
            ),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Record `doc`, replacing everything previously recorded for the same PDF path.
    pub fn add(&mut self, doc: &Document) -> Result<DocumentSummary> {
        let pdf = pdf_key(&doc.pdf);
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM documents WHERE pdf = ?1", [&pdf])?;
        tx.execute(
            "INSERT INTO documents (pdf, pdf_sha256, origin, profile, collection, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                pdf,
                doc.pdf_sha256,
                doc.origin.as_str(),
                doc.profile,
                doc.collection,
                now()
            ],
        )?;
        let id = tx.last_insert_rowid();
        {
            let mut insert = tx.prepare(
                "INSERT OR REPLACE INTO sources (document_id, source_id, name, path, media_type, sha256)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for s in &doc.sources {
                insert.execute(params![id, s.id, s.name, s.path, s.media_type, s.sha256])?;
            }
            let mut insert = tx.prepare(
                "INSERT INTO entries (document_id, source_id, unit_id, unit_kind, kind, text,
                    provider, confidence, x, y, width, height, frame, start_seconds, end_seconds,
                    page_first, page_last, attributes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            )?;
            for e in &doc.entries {
                let region = e.region.map(|r| r.map(Some)).unwrap_or([None; 4]);
                insert.execute(params![
                    id,
                    e.source_id,
                    e.unit_id,
                    e.unit_kind,
                    e.kind,
                    e.text,
                    e.provider,
                    e.confidence,
                    region[0],
                    region[1],
                    region[2],
                    region[3],
                    e.frame,
                    e.time.map(|t| t.0),
                    e.time.map(|t| t.1),
                    e.pages.map(|p| p.0 as i64),
                    e.pages.map(|p| p.1 as i64),
                    serde_json::to_string(&e.attributes)?,
                ])?;
            }
        }
        tx.commit()?;
        self.document(&doc.pdf)?
            .context("indexed document vanished")
    }

    /// The recorded state of one PDF, if it is indexed.
    pub fn document(&self, pdf: &Path) -> Result<Option<DocumentSummary>> {
        Ok(self
            .documents_where("WHERE d.pdf = ?1", vec![Sql::Text(pdf_key(pdf))])?
            .pop())
    }

    /// Every indexed PDF, optionally only one collection's.
    pub fn documents(&self, collection: Option<&str>) -> Result<Vec<DocumentSummary>> {
        match collection {
            Some(c) => self.documents_where("WHERE d.collection = ?1", vec![Sql::Text(c.into())]),
            None => self.documents_where("", Vec::new()),
        }
    }

    fn documents_where(&self, filter: &str, values: Vec<Sql>) -> Result<Vec<DocumentSummary>> {
        let sql = format!(
            "SELECT d.pdf, d.pdf_sha256, d.origin, d.profile, d.collection, d.indexed_at,
                (SELECT COUNT(*) FROM sources s WHERE s.document_id = d.id),
                (SELECT COUNT(*) FROM entries e WHERE e.document_id = d.id)
             FROM documents d {filter} ORDER BY d.pdf"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values), |r| {
            Ok(DocumentSummary {
                pdf: r.get(0)?,
                pdf_sha256: r.get(1)?,
                origin: r.get(2)?,
                profile: r.get(3)?,
                collection: r.get(4)?,
                indexed_at: r.get(5)?,
                sources: r.get(6)?,
                entries: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Move an indexed PDF into another collection (or none).
    pub fn set_collection(&mut self, pdf: &Path, collection: Option<&str>) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE documents SET collection = ?2 WHERE pdf = ?1",
            params![pdf_key(pdf), collection],
        )? > 0)
    }

    /// Forget one PDF; returns whether it was indexed.
    pub fn remove(&mut self, pdf: &Path) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM documents WHERE pdf = ?1", [pdf_key(pdf)])?
            > 0)
    }

    /// Search entries. With text, results are ranked by relevance and a unit's
    /// `chunk` entry is left out when one of its annotations matched on its own
    /// (the annotation carries the region and time); without text, entries matching
    /// the filters are listed in document, page and time order.
    pub fn search(&self, query: &SearchQuery) -> Result<Vec<Hit>> {
        let mut filters = Vec::new();
        let mut values = Vec::new();
        let fts = query.text.as_deref().and_then(crate::fts_query);
        if query.text.is_some() && fts.is_none() {
            return Ok(Vec::new());
        }
        if let Some(fts) = &fts {
            values.push(Sql::Text(fts.clone()));
        }
        if !query.kinds.is_empty() {
            let marks: Vec<String> = query
                .kinds
                .iter()
                .map(|k| {
                    values.push(Sql::Text(k.clone()));
                    format!("?{}", values.len())
                })
                .collect();
            filters.push(format!("e.kind IN ({})", marks.join(", ")));
        } else if fts.is_some() {
            filters.push(format!(
                "NOT (e.kind = '{CHUNK_KIND}' AND EXISTS (
                    SELECT 1 FROM hits h2 JOIN entries e2 ON e2.id = h2.id
                    WHERE e2.document_id = e.document_id AND e2.unit_id = e.unit_id
                      AND e2.kind <> '{CHUNK_KIND}'))"
            ));
        }
        if let Some(person) = &query.person {
            values.push(Sql::Text(person.clone()));
            filters.push(format!(
                "e.kind = 'face' AND json_extract(e.attributes, '$.person') = ?{} COLLATE NOCASE",
                values.len()
            ));
        }
        if let Some(collection) = &query.collection {
            values.push(Sql::Text(collection.clone()));
            filters.push(format!("d.collection = ?{}", values.len()));
        }
        if fts.is_none() && filters.is_empty() {
            bail!("give search text or at least one filter");
        }
        values.push(Sql::Integer(query.limit.max(1) as i64));
        let limit = format!("?{}", values.len());
        let mut filter = filters.join(" AND ");
        if filter.is_empty() {
            filter = "1".into();
        }
        let sql = if fts.is_some() {
            format!(
                "WITH hits AS MATERIALIZED (
                    SELECT rowid AS id, bm25(entries_fts) AS score,
                        snippet(entries_fts, 0, '[', ']', '…', 16) AS snippet
                    FROM entries_fts WHERE entries_fts MATCH ?1
                 )
                 SELECT {HIT_COLUMNS}, h.snippet
                 FROM hits h JOIN entries e ON e.id = h.id
                 JOIN documents d ON d.id = e.document_id
                 LEFT JOIN sources s ON s.document_id = e.document_id AND s.source_id = e.source_id
                 WHERE {filter}
                 ORDER BY h.score, e.id LIMIT {limit}"
            )
        } else {
            format!(
                "SELECT {HIT_COLUMNS}, NULL
                 FROM entries e JOIN documents d ON d.id = e.document_id
                 LEFT JOIN sources s ON s.document_id = e.document_id AND s.source_id = e.source_id
                 WHERE {filter}
                 ORDER BY d.pdf, e.page_first, e.start_seconds, e.id LIMIT {limit}"
            )
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values), hit)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Store one embedding for a unit of an indexed PDF, replacing an earlier one
    /// from the same model.
    pub fn put_embedding(
        &mut self,
        pdf: &Path,
        unit_id: &str,
        model: &str,
        vector: &[f32],
    ) -> Result<()> {
        let id: i64 = self
            .conn
            .query_row(
                "SELECT id FROM documents WHERE pdf = ?1",
                [pdf_key(pdf)],
                |r| r.get(0),
            )
            .optional()?
            .with_context(|| format!("{} is not indexed", pdf.display()))?;
        let bytes: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
        self.conn.execute(
            "INSERT OR REPLACE INTO embeddings (document_id, unit_id, model, dim, vector)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, unit_id, model, vector.len() as i64, bytes],
        )?;
        Ok(())
    }

    /// The `k` units whose `model` embeddings are most cosine-similar to `vector`.
    pub fn nearest(
        &self,
        model: &str,
        vector: &[f32],
        k: usize,
        collection: Option<&str>,
    ) -> Result<Vec<Neighbour>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.pdf, m.unit_id, m.vector,
                (SELECT MIN(page_first) FROM entries e WHERE e.document_id = m.document_id AND e.unit_id = m.unit_id),
                (SELECT MAX(page_last) FROM entries e WHERE e.document_id = m.document_id AND e.unit_id = m.unit_id)
             FROM embeddings m JOIN documents d ON d.id = m.document_id
             WHERE m.model = ?1 AND m.dim = ?2 AND (?3 IS NULL OR d.collection = ?3)",
        )?;
        let rows = stmt.query_map(params![model, vector.len() as i64, collection], |r| {
            let blob: Vec<u8> = r.get(2)?;
            let first: Option<i64> = r.get(3)?;
            let last: Option<i64> = r.get(4)?;
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                blob,
                first.zip(last),
            ))
        })?;
        let mut found = Vec::new();
        for row in rows {
            let (pdf, unit_id, blob, pages) = row?;
            let other: Vec<f32> = blob
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            found.push(Neighbour {
                pdf,
                unit_id,
                similarity: cosine(vector, &other),
                pages: pages.map(|(first, last)| PageSpan { first, last }),
            });
        }
        found.sort_by(|a, b| b.similarity.total_cmp(&a.similarity));
        found.truncate(k);
        Ok(found)
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x), f64::from(*y));
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

fn hit(r: &Row<'_>) -> rusqlite::Result<Hit> {
    let text: String = r.get(10)?;
    let region: [Option<f64>; 4] = [r.get(11)?, r.get(12)?, r.get(13)?, r.get(14)?];
    let start: Option<f64> = r.get(16)?;
    let end: Option<f64> = r.get(17)?;
    let first: Option<i64> = r.get(18)?;
    let last: Option<i64> = r.get(19)?;
    let attributes: String = r.get(20)?;
    let snippet: Option<String> = r.get(21)?;
    Ok(Hit {
        pdf: r.get(0)?,
        collection: r.get(1)?,
        source: HitSource {
            id: r.get(2)?,
            name: r.get(3)?,
            path: r.get(4)?,
        },
        unit_id: r.get(5)?,
        unit_kind: r.get(6)?,
        kind: r.get(7)?,
        provider: r.get(8)?,
        confidence: r.get(9)?,
        snippet: snippet.unwrap_or_else(|| shorten(&text, 160)),
        text,
        pages: first
            .zip(last)
            .map(|(first, last)| PageSpan { first, last }),
        time: start.zip(end).map(|(start_seconds, end_seconds)| TimeSpan {
            start_seconds,
            end_seconds,
        }),
        region: match region {
            [Some(x), Some(y), Some(width), Some(height)] => Some(HitRegion {
                x,
                y,
                width,
                height,
            }),
            _ => None,
        },
        frame: r.get(15)?,
        attributes: serde_json::from_str(&attributes).unwrap_or_default(),
    })
}

fn shorten(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::sample_graph;
    use anytopdf_core::{ChunkSet, Manifest};

    fn index() -> (tempfile::TempDir, Index) {
        let dir = tempfile::tempdir().unwrap();
        let index = Index::open(&dir.path().join("nested/index.sqlite")).unwrap();
        (dir, index)
    }

    fn graph_doc(pdf: &str) -> Document {
        let (graph, report) = sample_graph();
        Document::from_graph(Path::new(pdf), b"%PDF-1", &graph, &report)
    }

    fn query(text: &str) -> SearchQuery {
        SearchQuery {
            text: Some(text.into()),
            limit: 20,
            ..SearchQuery::default()
        }
    }

    #[test]
    fn search_returns_file_page_time_and_region_for_an_annotation() {
        let (_dir, mut index) = index();
        index.add(&graph_doc("/out/meeting.pdf")).unwrap();
        let hits = index.search(&query("alice")).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        let hit = &hits[0];
        assert_eq!(hit.kind, "face");
        assert_eq!(hit.pdf, "/out/meeting.pdf");
        assert_eq!(hit.source.name.as_deref(), Some("meeting.mp4"));
        assert_eq!(hit.pages, Some(PageSpan { first: 1, last: 1 }));
        assert_eq!(hit.time.as_ref().unwrap().start_seconds, 725.0);
        assert!((hit.region.as_ref().unwrap().width - 0.3).abs() < 1e-6);
        assert_eq!(hit.snippet, "[Alice]");
    }

    #[test]
    fn chunk_entries_answer_only_when_no_annotation_of_their_unit_matched() {
        let (_dir, mut index) = index();
        index.add(&graph_doc("/out/meeting.pdf")).unwrap();
        let kinds = |q: &str| -> Vec<String> {
            index
                .search(&query(q))
                .unwrap()
                .into_iter()
                .map(|h| h.kind)
                .collect()
        };
        // "budget" matches the OCR line of the frame and the visible minutes text.
        let mut found = kinds("budget");
        found.sort();
        assert_eq!(found, ["chunk", "ocr"]);
        // A phrase spanning two annotations of one unit only matches its chunk.
        assert_eq!(kinds("\"alice quarterly\""), ["chunk"]);
        assert!(kinds("nothing-like-this").is_empty());
    }

    #[test]
    fn kind_person_and_collection_filters_narrow_results() {
        let (_dir, mut index) = index();
        let mut work = graph_doc("/out/work.pdf");
        work.collection = Some("work".into());
        index.add(&work).unwrap();
        index.add(&graph_doc("/out/home.pdf")).unwrap();
        let search = |q: SearchQuery| index.search(&q).unwrap();
        let faces = search(SearchQuery {
            kinds: vec!["face".into()],
            limit: 20,
            ..SearchQuery::default()
        });
        assert_eq!(faces.len(), 2);
        let alice = search(SearchQuery {
            person: Some("ALICE".into()),
            collection: Some("work".into()),
            limit: 20,
            ..SearchQuery::default()
        });
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0].pdf, "/out/work.pdf");
        assert_eq!(alice[0].collection.as_deref(), Some("work"));
        let ocr = search(SearchQuery {
            kinds: vec!["ocr".into()],
            ..query("budget")
        });
        assert_eq!(ocr.len(), 2);
        assert!(ocr.iter().all(|h| h.kind == "ocr"));
        assert!(index.search(&SearchQuery::default()).is_err());
    }

    #[test]
    fn re_adding_a_pdf_replaces_its_entries_and_remove_forgets_it() {
        let (_dir, mut index) = index();
        let first = index.add(&graph_doc("/out/m.pdf")).unwrap();
        let again = index.add(&graph_doc("/out/m.pdf")).unwrap();
        assert_eq!(first.entries, again.entries);
        assert_eq!(index.documents(None).unwrap().len(), 1);
        assert_eq!(index.search(&query("alice")).unwrap().len(), 1);
        assert!(
            index
                .set_collection(Path::new("/out/m.pdf"), Some("x"))
                .unwrap()
        );
        assert_eq!(index.documents(Some("x")).unwrap().len(), 1);
        assert!(index.remove(Path::new("/out/m.pdf")).unwrap());
        assert!(!index.remove(Path::new("/out/m.pdf")).unwrap());
        assert!(index.search(&query("alice")).unwrap().is_empty());
        assert!(index.documents(None).unwrap().is_empty());
    }

    #[test]
    fn pdfs_indexed_from_chunks_are_searchable_by_text() {
        let (_dir, mut index) = index();
        let (graph, report) = sample_graph();
        let doc = Document::from_chunks(
            Path::new("/old/m.pdf"),
            b"%PDF-1",
            &Manifest::build(&graph, &report),
            &ChunkSet::build(&graph, &report),
        );
        let summary = index.add(&doc).unwrap();
        assert_eq!(summary.origin, "pdf");
        let hits = index.search(&query("alice")).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, CHUNK_KIND);
        assert_eq!(hits[0].pages, Some(PageSpan { first: 1, last: 1 }));
    }

    #[test]
    fn reopening_keeps_data_and_newer_layouts_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("i.sqlite");
        Index::open(&path)
            .unwrap()
            .add(&graph_doc("/out/m.pdf"))
            .unwrap();
        let index = Index::open_existing(&path).unwrap();
        assert_eq!(index.search(&query("alice")).unwrap().len(), 1);
        drop(index);
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(conn);
        let err = Index::open(&path).err().unwrap().to_string();
        assert!(err.contains("newer"), "{err}");
        assert!(Index::open_existing(&dir.path().join("missing.sqlite")).is_err());
        let other = dir.path().join("other.sqlite");
        Connection::open(&other)
            .unwrap()
            .execute_batch("CREATE TABLE notes (body TEXT)")
            .unwrap();
        let err = Index::open(&other).err().unwrap().to_string();
        assert!(err.contains("not an anytopdf search index"), "{err}");
        let tables: i64 = Connection::open(&other)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tables, 1, "a foreign database must be left untouched");
    }

    #[test]
    fn embeddings_rank_units_by_cosine_similarity() {
        let (_dir, mut index) = index();
        let doc = graph_doc("/out/m.pdf");
        index.add(&doc).unwrap();
        let pdf = Path::new("/out/m.pdf");
        let units: Vec<&str> = vec![&doc.entries[0].unit_id, &doc.entries[3].unit_id];
        index
            .put_embedding(pdf, units[0], "clip", &[1.0, 0.0])
            .unwrap();
        index
            .put_embedding(pdf, units[1], "clip", &[0.6, 0.8])
            .unwrap();
        let near = index.nearest("clip", &[0.0, 1.0], 5, None).unwrap();
        assert_eq!(near.len(), 2);
        assert_eq!(near[0].unit_id, units[1]);
        assert!((near[0].similarity - 0.8).abs() < 1e-6);
        assert_eq!(near[0].pages, Some(PageSpan { first: 2, last: 2 }));
        assert!(
            index
                .nearest("other", &[0.0, 1.0], 5, None)
                .unwrap()
                .is_empty()
        );
        assert!(
            index
                .put_embedding(Path::new("/nope.pdf"), units[0], "clip", &[1.0])
                .is_err()
        );
    }
}
