//! The local face index: enrolled people, unnamed clusters and sightings.
//!
//! It is a SQLite file in the user's data directory. Embeddings stay in it and
//! are never written into a PDF.

use crate::vector;
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::{fs, path::Path, time::Duration};

/// Observed (not enrolled) embeddings kept per person; older ones are dropped.
pub const MAX_OBSERVED: usize = 32;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS people (
    id INTEGER PRIMARY KEY,
    label TEXT NOT NULL UNIQUE,
    name TEXT UNIQUE COLLATE NOCASE,
    created INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS embeddings (
    id INTEGER PRIMARY KEY,
    person_id INTEGER NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    model TEXT NOT NULL,
    vector BLOB NOT NULL,
    origin TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS embeddings_model ON embeddings(model);
CREATE TABLE IF NOT EXISTS sightings (
    id INTEGER PRIMARY KEY,
    person_id INTEGER NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    model TEXT NOT NULL,
    vector BLOB NOT NULL,
    source_path TEXT NOT NULL,
    source_sha256 TEXT,
    start_seconds REAL,
    end_seconds REAL,
    frame INTEGER,
    x REAL, y REAL, w REAL, h REAL,
    unit INTEGER NOT NULL,
    similarity REAL,
    recorded INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sightings_person ON sightings(person_id);
";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Person {
    pub id: i64,
    /// Stable `person-N` label, assigned once and never reused.
    pub label: String,
    pub name: Option<String>,
    pub embeddings: usize,
    pub sightings: usize,
}

impl Person {
    /// The name when the user gave one, else the label.
    pub fn display(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.label)
    }
}

/// Where a face was seen.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Sighting {
    pub source_path: String,
    pub source_sha256: Option<String>,
    pub start_seconds: Option<f64>,
    pub end_seconds: Option<f64>,
    pub frame: Option<u32>,
    /// Normalized x, y, width, height.
    pub region: Option<[f32; 4]>,
    /// Position of the unit among its source's units.
    pub unit: usize,
    pub similarity: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct StoredSighting {
    pub person_id: i64,
    pub sighting: Sighting,
    pub vector: Vec<f32>,
}

pub struct FaceIndex {
    conn: Connection,
}

impl FaceIndex {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)
                .with_context(|| format!("create face index directory {}", dir.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open face index {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        conn.execute_batch(SCHEMA)
            .context("create face index schema")?;
        Ok(Self { conn })
    }

    /// Runs `f` in one write transaction; an error rolls everything back.
    pub fn transaction<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        match f(self) {
            Ok(value) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(value)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    pub fn people(&self) -> Result<Vec<Person>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.label, p.name,
                    (SELECT COUNT(*) FROM embeddings e WHERE e.person_id = p.id),
                    (SELECT COUNT(*) FROM sightings s WHERE s.person_id = p.id)
             FROM people p ORDER BY p.name IS NULL, p.name, p.id",
        )?;
        let rows = stmt.query_map([], person_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn person_by_id(&self, id: i64) -> Result<Person> {
        self.people()?
            .into_iter()
            .find(|p| p.id == id)
            .context("person not found")
    }

    /// Finds a person by name (case-insensitive) or `person-N` label.
    pub fn person(&self, key: &str) -> Result<Option<Person>> {
        let key = key.trim();
        Ok(self.people()?.into_iter().find(|p| {
            p.label == key
                || p.name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(key))
        }))
    }

    fn require(&self, key: &str) -> Result<Person> {
        self.person(key)?
            .with_context(|| format!("no person named or labelled {key:?} in the face index"))
    }

    /// Creates a person with the next free `person-N` label.
    pub fn create_person(&mut self, name: Option<&str>) -> Result<Person> {
        let name = name.map(check_name).transpose()?;
        let next: i64 = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'next_label'", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        let label = format!("person-{next}");
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES ('next_label', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [(next + 1).to_string()],
        )?;
        self.conn.execute(
            "INSERT INTO people (label, name, created) VALUES (?1, ?2, ?3)",
            params![label, name, now()],
        )?;
        self.person_by_id(self.conn.last_insert_rowid())
    }

    /// Adds enrolled embeddings for `name`, creating the person if needed.
    pub fn enroll(&mut self, name: &str, model: &str, vectors: &[Vec<f32>]) -> Result<Person> {
        let name = check_name(name)?;
        self.transaction(|index| {
            let person = match index.person(&name)? {
                Some(p) if p.name.is_some() => p,
                _ => index.create_person(Some(&name))?,
            };
            for v in vectors {
                index.insert_embedding(person.id, model, v, "enrolled")?;
            }
            index.person_by_id(person.id)
        })
    }

    fn insert_embedding(&self, person: i64, model: &str, v: &[f32], origin: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO embeddings (person_id, model, vector, origin) VALUES (?1, ?2, ?3, ?4)",
            params![
                person,
                model,
                vector::to_bytes(&vector::normalize(v.to_vec())),
                origin
            ],
        )?;
        Ok(())
    }

    /// Remembers an embedding seen in a document so later runs recognize the
    /// same person, keeping at most [`MAX_OBSERVED`] per person and model.
    pub fn add_observed(&mut self, person: i64, model: &str, v: &[f32]) -> Result<()> {
        self.insert_embedding(person, model, v, "observed")?;
        self.conn.execute(
            "DELETE FROM embeddings WHERE id IN (
                SELECT id FROM embeddings
                WHERE person_id = ?1 AND model = ?2 AND origin = 'observed'
                ORDER BY id DESC LIMIT -1 OFFSET ?3)",
            params![person, model, MAX_OBSERVED as i64],
        )?;
        Ok(())
    }

    /// Every stored embedding for `model`, with its person.
    pub fn gallery(&self, model: &str) -> Result<Vec<(i64, Vec<f32>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT person_id, vector FROM embeddings WHERE model = ?1")?;
        let rows = stmt.query_map([model], |r| {
            Ok((r.get(0)?, vector::from_bytes(&r.get::<_, Vec<u8>>(1)?)))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn add_sighting(
        &mut self,
        person: i64,
        model: &str,
        v: &[f32],
        s: &Sighting,
    ) -> Result<()> {
        let region = s.region.map(|r| r.map(f64::from));
        self.conn.execute(
            "INSERT INTO sightings (person_id, model, vector, source_path, source_sha256,
                 start_seconds, end_seconds, frame, x, y, w, h, unit, similarity, recorded)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                person,
                model,
                vector::to_bytes(&vector::normalize(v.to_vec())),
                s.source_path,
                s.source_sha256,
                s.start_seconds,
                s.end_seconds,
                s.frame,
                region.map(|r| r[0]),
                region.map(|r| r[1]),
                region.map(|r| r[2]),
                region.map(|r| r[3]),
                s.unit as i64,
                s.similarity.map(f64::from),
                now(),
            ],
        )?;
        Ok(())
    }

    /// Sightings recorded with `model`, in recording order.
    pub fn sightings(&self, model: &str) -> Result<Vec<StoredSighting>> {
        let mut stmt = self.conn.prepare(
            "SELECT person_id, vector, source_path, source_sha256, start_seconds, end_seconds,
                    frame, x, y, w, h, unit, similarity
             FROM sightings WHERE model = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([model], |r| {
            let region: [Option<f64>; 4] = [r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?];
            Ok(StoredSighting {
                person_id: r.get(0)?,
                vector: vector::from_bytes(&r.get::<_, Vec<u8>>(1)?),
                sighting: Sighting {
                    source_path: r.get(2)?,
                    source_sha256: r.get(3)?,
                    start_seconds: r.get(4)?,
                    end_seconds: r.get(5)?,
                    frame: r.get(6)?,
                    region: match region {
                        [Some(x), Some(y), Some(w), Some(h)] => {
                            Some([x as f32, y as f32, w as f32, h as f32])
                        }
                        _ => None,
                    },
                    unit: r.get::<_, i64>(11)? as usize,
                    similarity: r.get::<_, Option<f64>>(12)?.map(|v| v as f32),
                },
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Changes a person's name; fails if another person already has it.
    pub fn rename(&mut self, key: &str, new_name: &str) -> Result<Person> {
        let new_name = check_name(new_name)?;
        let person = self.require(key)?;
        if let Some(other) = self.person(&new_name)?
            && other.id != person.id
        {
            bail!(
                "{} already exists; use `anytopdf faces merge {} {}` to combine them",
                other.display(),
                person.display(),
                other.display()
            );
        }
        self.conn.execute(
            "UPDATE people SET name = ?1 WHERE id = ?2",
            params![new_name, person.id],
        )?;
        self.person_by_id(person.id)
    }

    /// Names a person; if the name is taken, merges this person into the
    /// existing one instead.
    pub fn name(&mut self, key: &str, new_name: &str) -> Result<Person> {
        let new_name = check_name(new_name)?;
        match self.person(&new_name)? {
            Some(existing) => self.merge(key, &existing.label),
            None => self.rename(key, &new_name),
        }
    }

    /// Moves every embedding and sighting of `from` onto `into` and removes `from`.
    pub fn merge(&mut self, from: &str, into: &str) -> Result<Person> {
        let (from, into) = (self.require(from)?, self.require(into)?);
        if from.id == into.id {
            bail!("cannot merge {} into itself", from.display());
        }
        self.transaction(|index| {
            for table in ["embeddings", "sightings"] {
                index.conn.execute(
                    &format!("UPDATE {table} SET person_id = ?1 WHERE person_id = ?2"),
                    params![into.id, from.id],
                )?;
            }
            index
                .conn
                .execute("DELETE FROM people WHERE id = ?1", [from.id])?;
            index.person_by_id(into.id)
        })
    }

    /// Deletes a person with all their embeddings and sightings.
    pub fn forget(&mut self, key: &str) -> Result<Person> {
        let person = self.require(key)?;
        self.conn
            .execute("DELETE FROM people WHERE id = ?1", [person.id])?;
        Ok(person)
    }
}

fn person_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Person> {
    Ok(Person {
        id: r.get(0)?,
        label: r.get(1)?,
        name: r.get(2)?,
        embeddings: r.get::<_, i64>(3)? as usize,
        sightings: r.get::<_, i64>(4)? as usize,
    })
}

/// Names are free text, but must not look like a `person-N` label.
pub fn check_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        bail!("a person's name must be 1 to 100 printable characters");
    }
    if name
        .strip_prefix("person-")
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    {
        bail!("{name:?} is reserved for unnamed people; choose a real name");
    }
    Ok(name.to_string())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sighting(path: &str) -> Sighting {
        Sighting {
            source_path: path.into(),
            start_seconds: Some(1.5),
            region: Some([0.1, 0.2, 0.3, 0.4]),
            ..Default::default()
        }
    }

    #[test]
    fn labels_are_never_reused_after_forget() {
        let mut index = FaceIndex::open_in_memory().unwrap();
        let first = index.create_person(None).unwrap();
        assert_eq!(first.label, "person-1");
        index.forget("person-1").unwrap();
        assert_eq!(index.create_person(None).unwrap().label, "person-2");
    }

    #[test]
    fn enroll_adds_to_an_existing_name_case_insensitively() {
        let mut index = FaceIndex::open_in_memory().unwrap();
        index.enroll("Alice", "m", &[vec![1.0, 0.0]]).unwrap();
        let alice = index.enroll("alice", "m", &[vec![0.0, 1.0]]).unwrap();
        assert_eq!(alice.name.as_deref(), Some("Alice"));
        assert_eq!(alice.embeddings, 2);
        assert_eq!(index.people().unwrap().len(), 1);
        assert_eq!(index.gallery("m").unwrap().len(), 2);
        assert!(index.gallery("other-model").unwrap().is_empty());
    }

    #[test]
    fn naming_a_cluster_with_a_taken_name_merges_it() {
        let mut index = FaceIndex::open_in_memory().unwrap();
        let alice = index.enroll("Alice", "m", &[vec![1.0, 0.0]]).unwrap();
        let cluster = index.create_person(None).unwrap();
        index.add_observed(cluster.id, "m", &[0.9, 0.1]).unwrap();
        index
            .add_sighting(cluster.id, "m", &[0.9, 0.1], &sighting("a.mp4"))
            .unwrap();
        let merged = index.name(&cluster.label, "ALICE").unwrap();
        assert_eq!(merged.id, alice.id);
        assert_eq!((merged.embeddings, merged.sightings), (2, 1));
        assert!(index.person(&cluster.label).unwrap().is_none());

        let bob = index.create_person(None).unwrap();
        let named = index.name(&bob.label, "Bob").unwrap();
        assert_eq!(named.display(), "Bob");
        assert_eq!(named.label, bob.label);
    }

    #[test]
    fn rename_refuses_a_name_in_use_and_reserved_labels() {
        let mut index = FaceIndex::open_in_memory().unwrap();
        index.enroll("Alice", "m", &[vec![1.0]]).unwrap();
        index.enroll("Bob", "m", &[vec![1.0]]).unwrap();
        assert!(index.rename("Bob", "alice").is_err());
        assert!(index.rename("Bob", "person-7").is_err());
        assert!(index.rename("Bob", "  ").is_err());
        assert_eq!(index.rename("Bob", "Robert").unwrap().display(), "Robert");
    }

    #[test]
    fn forget_removes_embeddings_and_sightings() {
        let mut index = FaceIndex::open_in_memory().unwrap();
        let alice = index.enroll("Alice", "m", &[vec![1.0, 0.0]]).unwrap();
        index
            .add_sighting(alice.id, "m", &[1.0, 0.0], &sighting("a.jpg"))
            .unwrap();
        index.forget("alice").unwrap();
        assert!(index.gallery("m").unwrap().is_empty());
        assert!(index.sightings("m").unwrap().is_empty());
        assert!(index.forget("alice").is_err());
    }

    #[test]
    fn observed_embeddings_are_capped_per_person() {
        let mut index = FaceIndex::open_in_memory().unwrap();
        let alice = index.enroll("Alice", "m", &[vec![1.0, 0.0]]).unwrap();
        for i in 0..(MAX_OBSERVED + 5) {
            index.add_observed(alice.id, "m", &[1.0, i as f32]).unwrap();
        }
        // The enrolled embedding survives the cap.
        assert_eq!(index.gallery("m").unwrap().len(), MAX_OBSERVED + 1);
    }

    #[test]
    fn sightings_round_trip_and_the_file_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("faces.sqlite");
        {
            let mut index = FaceIndex::open(&path).unwrap();
            let p = index.create_person(None).unwrap();
            index
                .add_sighting(p.id, "m", &[3.0, 4.0], &sighting("clip.mp4"))
                .unwrap();
        }
        let index = FaceIndex::open(&path).unwrap();
        let stored = index.sightings("m").unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].sighting, sighting("clip.mp4"));
        assert_eq!(stored[0].vector, vec![0.6, 0.8]);
    }

    #[test]
    fn failed_transactions_roll_back() {
        let mut index = FaceIndex::open_in_memory().unwrap();
        let result: Result<()> = index.transaction(|i| {
            i.create_person(None)?;
            bail!("boom")
        });
        assert!(result.is_err());
        assert!(index.people().unwrap().is_empty());
    }
}
