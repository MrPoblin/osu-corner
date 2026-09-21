//! The ledger: what has already been looked at, so a re-sync does not repeat the work.
//!
//! It does one job. Reading lazer's store costs an open per file, and hashing the whole `.osu`
//! corpus costs a read per file; both are worth doing once and never again. So the ledger records
//! **a row per blob inspected** — its identity, what it turned out to be, and for a beatmap its
//! MD5 — and a later run only reads what is genuinely new.
//!
//! Around 144K rows for this machine's lazer store, which is about 5 MB of local SQLite. That is
//! the price of never re-reading 144,475 files.

use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use std::path::Path;

/// Bump when the schema changes **or when the meaning of a stored value changes**; an old file is
/// then refused rather than silently trusted. Version 2 is the play key's epoch being corrected.
const SCHEMA_VERSION: i64 = 2;

/// What a blob turned out to be. Stored as numbers, so the values must not be reordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Other = 0,
    Osu = 1,
    Osr = 2,
}

/// One inspected blob, ready to be written.
#[derive(Debug, Clone)]
pub struct Blob {
    pub source: String,
    /// The store's content hash for lazer, or the path relative to the install for stable.
    pub id: String,
    pub kind: Kind,
    /// Set for a beatmap: the MD5 of its bytes.
    pub md5: Option<String>,
    /// Set for a replay: its play key.
    pub key: Option<String>,
    pub size: u64,
    /// Seconds since the Unix epoch. Meaningless for lazer, whose ids are content hashes and so
    /// can never go stale — the field is only read back for named files.
    pub mtime: i64,
}

pub struct Ledger {
    conn: Connection,
}

impl Ledger {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path)
            .map_err(|error| format!("cannot open {}: {error}", path.display()))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS blob (
                 source TEXT    NOT NULL,
                 id     TEXT    NOT NULL,
                 kind   INTEGER NOT NULL,
                 md5    TEXT,
                 key    TEXT,
                 size   INTEGER,
                 mtime  INTEGER,
                 PRIMARY KEY (source, id)
             );
             CREATE INDEX IF NOT EXISTS blob_kind ON blob (kind);
             CREATE INDEX IF NOT EXISTS blob_md5  ON blob (md5);",
        )
        .map_err(|error| format!("cannot prepare {}: {error}", path.display()))?;

        let found: Option<String> = conn
            .query_row("SELECT v FROM meta WHERE k = 'schema'", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|error| error.to_string())?;

        match found.as_deref() {
            None => {
                conn.execute(
                    "INSERT INTO meta (k, v) VALUES ('schema', ?1)",
                    params![SCHEMA_VERSION.to_string()],
                )
                .map_err(|error| error.to_string())?;
            }
            Some(v) if v != SCHEMA_VERSION.to_string() => {
                return Err(format!(
                    "{} was written by a different version (schema {v}, this build wants \
                     {SCHEMA_VERSION}).\nDelete it and run again — it is a cache of what has \
                     already been read, not a record of anything that cannot be rebuilt.",
                    path.display()
                ));
            }
            Some(_) => {}
        }

        Ok(Self { conn })
    }

    /// Which blobs from one source are already known, with the size and mtime needed to tell
    /// whether a named file has changed since.
    pub fn known(&self, source: &str) -> Result<HashMap<String, (u64, i64)>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, size, mtime FROM blob WHERE source = ?1")
            .map_err(|error| error.to_string())?;

        let rows = stmt
            .query_map(params![source], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?.unwrap_or(0).max(0) as u64,
                    row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                ))
            })
            .map_err(|error| error.to_string())?;

        let mut out = HashMap::new();
        for row in rows {
            let (id, size, mtime) = row.map_err(|error| error.to_string())?;
            out.insert(id, (size, mtime));
        }
        Ok(out)
    }

    /// Write a batch in one transaction — one commit per run rather than one per file.
    pub fn record(&mut self, blobs: &[Blob]) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO blob (source, id, kind, md5, key, size, mtime)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT (source, id) DO UPDATE SET
                         kind = excluded.kind, md5 = excluded.md5, key = excluded.key,
                         size = excluded.size, mtime = excluded.mtime",
                )
                .map_err(|error| error.to_string())?;

            for blob in blobs {
                stmt.execute(params![
                    blob.source,
                    blob.id,
                    blob.kind as i64,
                    blob.md5,
                    blob.key,
                    blob.size as i64,
                    blob.mtime,
                ])
                .map_err(|error| error.to_string())?;
            }
        }
        tx.commit().map_err(|error| error.to_string())
    }

    /// Every replay the ledger knows about, as `(source, id, play key)`.
    pub fn replays(&self) -> Result<Vec<(String, String, String)>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT source, id, key FROM blob WHERE kind = ?1 AND key IS NOT NULL")
            .map_err(|error| error.to_string())?;

        let rows = stmt
            .query_map(params![Kind::Osr as i64], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|error| error.to_string())?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    /// Where a beatmap with this MD5 can be read from, once it has been hashed.
    pub fn map_source(&self, md5: &str) -> Result<Option<(String, String)>, String> {
        self.conn
            .query_row(
                "SELECT source, id FROM blob WHERE kind = ?1 AND md5 = ?2 LIMIT 1",
                params![Kind::Osu as i64, md5],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| error.to_string())
    }

    /// How many rows the ledger holds, for the report.
    pub fn total(&self) -> Result<u64, String> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM blob", [], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        Ok(count.max(0) as u64)
    }
}
