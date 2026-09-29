//! The ledger: what has already been looked at, so a re-sync does not repeat the work.
//!
//! It does one job. Reading lazer's store costs an open per file, and hashing the whole `.osu`
//! corpus costs a read per file; both are worth doing once and never again. So the ledger records
//! **a row per blob inspected** — its identity, what it turned out to be, and for a beatmap its
//! MD5 — and a later run only reads what is genuinely new.
//!
//! It also records what has been **published**: which plays have their bytes in the store, and the digest
//! of each index file as uploaded, so a rerun uploads what changed rather than everything.
//!
//! # Schema 3, and why it is the size it is
//!
//! Version 2 measured **48.9 MB for 175,631 rows**, which is worth knowing before anyone adds a
//! column: `blob` was 22.5 MB and its `PRIMARY KEY (source, id)` index another **21.3 MB**, with
//! `blob_md5` 3.4 MB and `blob_kind` 1.8 MB. Two of those columns held hex **text** for hashes whose
//! bytes are half as long, and `source` repeated an absolute path that is the same string on every
//! row — 5.3 MB of it, twice over once the index is counted.
//!
//! So version 3 normalises. `source` is an integer into a `source` table; a hash `id` and every
//! `md5` are stored as **their bytes**; and `blob` is `WITHOUT ROWID`, which removes the separate
//! primary-key index entirely by making the table itself the clustered one. The `md5` index is now
//! **partial** (`WHERE kind = 1`) because one query is all that uses it. The published records below
//! ride this same bump, deliberately:
//!
//! > a bump is not free. An old file is *refused* rather than silently trusted, so the caller
//! > re-walks every file from scratch — 11 m 51 s on this machine. Paying that twice, once for the
//! > normalising and again for the first column that records something new, would be the only real
//! > mistake available here.

use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Bump when the schema changes **or when the meaning of a stored value changes**; an old file is
/// then refused rather than silently trusted. Version 2 was the play key's epoch being corrected.
/// **Version 3 is the normalising above plus the published records** — one bump, not two.
const SCHEMA_VERSION: i64 = 3;

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

/// One play whose bytes are in the store.
#[derive(Debug, Clone)]
pub struct Published {
    /// The play key — the staged replay's own name, and the same key the index and the viewer use.
    pub play: String,
    /// Where those bytes actually sit: the online score id when osu! gave one, else the play key.
    pub object: String,
    pub bytes: u64,
}

pub struct Ledger {
    conn: Connection,
}

impl Ledger {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path)
            .map_err(|error| format!("cannot open {}: {error}", path.display()))?;

        // `WITHOUT ROWID` on `blob` and `published` is the load-bearing part of the normalising: the
        // primary key *is* the table, so the second full copy of every id that `sqlite_autoindex`
        // used to hold does not exist.
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS source (
                 id   INTEGER PRIMARY KEY,
                 path TEXT    NOT NULL UNIQUE
             );
             CREATE TABLE IF NOT EXISTS blob (
                 source INTEGER NOT NULL REFERENCES source (id),
                 id     BLOB    NOT NULL,
                 kind   INTEGER NOT NULL,
                 md5    BLOB,
                 key    TEXT,
                 size   INTEGER,
                 mtime  INTEGER,
                 PRIMARY KEY (source, id)
             ) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS blob_kind ON blob (kind);
             CREATE INDEX IF NOT EXISTS blob_md5  ON blob (md5) WHERE kind = 1;
             CREATE TABLE IF NOT EXISTS published (
                 play   TEXT PRIMARY KEY,
                 object TEXT    NOT NULL,
                 bytes  INTEGER NOT NULL,
                 at     INTEGER NOT NULL
             ) WITHOUT ROWID;",
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

    /// Make a source addressable by an integer, which is what `blob` stores. Idempotent, so it is
    /// safe on every run; call it before `known` or `record` for that source.
    pub fn register(&mut self, source: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO source (path) VALUES (?1)",
                params![source],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Which blobs from one source are already known, with the size and mtime needed to tell
    /// whether a named file has changed since.
    pub fn known(&self, source: &str) -> Result<HashMap<String, (u64, i64)>, String> {
        let source_id = self.source_id(source)?;
        let mut stmt = self
            .conn
            .prepare("SELECT id, size, mtime FROM blob WHERE source = ?1")
            .map_err(|error| error.to_string())?;

        let rows = stmt
            .query_map(params![source_id], |row| {
                Ok((
                    stored_id(row.get_ref(0)?)?,
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
        let mut ids: HashMap<&str, i64> = HashMap::new();
        for blob in blobs {
            if !ids.contains_key(blob.source.as_str()) {
                let id = self.source_id(&blob.source)?;
                ids.insert(&blob.source, id);
            }
        }

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
                let source = *ids
                    .get(blob.source.as_str())
                    .ok_or_else(|| format!("{:?} was never registered as a source", blob.source))?;

                stmt.execute(params![
                    source,
                    id_value(&blob.id),
                    blob.kind as i64,
                    blob.md5.as_deref().and_then(hex_bytes),
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
            .prepare(
                "SELECT source.path, blob.id, blob.key
                   FROM blob JOIN source ON source.id = blob.source
                  WHERE blob.kind = ?1 AND blob.key IS NOT NULL",
            )
            .map_err(|error| error.to_string())?;

        let rows = stmt
            .query_map(params![Kind::Osr as i64], |row| {
                Ok((row.get(0)?, stored_id(row.get_ref(1)?)?, row.get(2)?))
            })
            .map_err(|error| error.to_string())?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    /// Where a beatmap with this MD5 can be read from, once it has been hashed.
    pub fn map_source(&self, md5: &str) -> Result<Option<(String, String)>, String> {
        // A malformed MD5 can match nothing, so it is a miss rather than an error.
        let Some(md5) = hex_bytes(md5) else {
            return Ok(None);
        };

        self.conn
            .query_row(
                "SELECT source.path, blob.id
                   FROM blob JOIN source ON source.id = blob.source
                  WHERE blob.kind = ?1 AND blob.md5 = ?2 LIMIT 1",
                params![Kind::Osu as i64, md5],
                |row| Ok((row.get(0)?, stored_id(row.get_ref(1)?)?)),
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

    /// The play key of every play whose bytes are already in the store, mapped to its object key.
    ///
    /// Keyed by the **play**, not by a blob, and that is the reason it is its own table: one play
    /// can be filed by two installs — stable's copy and lazer's — so a fact stored on `blob` would
    /// exist twice and could disagree with itself. A play is published or it is not.
    pub fn published(&self) -> Result<HashMap<String, String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT play, object FROM published")
            .map_err(|error| error.to_string())?;

        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|error| error.to_string())?;

        let mut out = HashMap::new();
        for row in rows {
            let (play, object) = row.map_err(|error| error.to_string())?;
            out.insert(play, object);
        }
        Ok(out)
    }

    /// Record what a run has put in the store, in one transaction.
    pub fn record_published(&mut self, rows: &[Published]) -> Result<(), String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_secs() as i64)
            .unwrap_or(0);

        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO published (play, object, bytes, at) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (play) DO UPDATE SET
                         object = excluded.object, bytes = excluded.bytes, at = excluded.at",
                )
                .map_err(|error| error.to_string())?;

            for row in rows {
                stmt.execute(params![row.play, row.object, row.bytes as i64, now])
                    .map_err(|error| error.to_string())?;
            }
        }
        tx.commit().map_err(|error| error.to_string())
    }

    /// The digest of an index file **as last uploaded**, or `None` if it has never been uploaded.
    ///
    /// Kept in `meta` rather than in a table of its own: there are four of them, they are identified
    /// by name, and a table for four rows would be a table for four rows.
    pub fn artifact(&self, name: &str) -> Result<Option<String>, String> {
        self.conn
            .query_row(
                "SELECT v FROM meta WHERE k = ?1",
                params![format!("storage:{name}")],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())
    }

    /// Remember the digest of an index file this run uploaded, so the next run can skip it.
    pub fn record_artifact(&mut self, name: &str, digest: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO meta (k, v) VALUES (?1, ?2)
                 ON CONFLICT (k) DO UPDATE SET v = excluded.v",
                params![format!("storage:{name}"), digest],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn source_id(&self, source: &str) -> Result<i64, String> {
        self.conn
            .query_row(
                "SELECT id FROM source WHERE path = ?1",
                params![source],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("{source:?} was never registered as a source"))
    }
}

/// A stored id back to the string the walker computes.
///
/// The **type is the marker**, which is why it works without a column for it: a hash id is written
/// as its bytes (`Blob`) and anything else — stable's path relative to the install — as text. The
/// two are unambiguous because a path always contains a separator or a dot and so can never be all
/// hex, and because the hex form round-trips exactly even if it were.
fn stored_id(value: ValueRef<'_>) -> rusqlite::Result<String> {
    match value {
        ValueRef::Blob(bytes) => Ok(osu_core::hex(bytes)),
        ValueRef::Text(text) => Ok(String::from_utf8_lossy(text).into_owned()),
        other => Err(rusqlite::Error::InvalidColumnType(
            0,
            "blob.id".to_owned(),
            other.data_type(),
        )),
    }
}

/// An id on its way in: bytes when it is a hash, text when it is a path. The inverse of `stored_id`.
fn id_value(id: &str) -> Value {
    match hex_bytes(id) {
        Some(bytes) => Value::Blob(bytes),
        None => Value::Text(id.to_owned()),
    }
}

/// Hex to bytes, or `None` when the string is not a whole number of hex digits.
fn hex_bytes(hex: &str) -> Option<Vec<u8>> {
    if hex.is_empty() || !hex.len().is_multiple_of(2) {
        return None;
    }

    (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test gets its own file, because `temp_dir` is shared and the tests run together.
    fn open(name: &str) -> Ledger {
        let path =
            std::env::temp_dir().join(format!("osu-ledger-{name}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        Ledger::open(&path).expect("a fresh ledger opens")
    }

    fn blob(source: &str, id: &str, md5: Option<&str>, key: Option<&str>) -> Blob {
        Blob {
            source: source.to_owned(),
            id: id.to_owned(),
            kind: if key.is_some() { Kind::Osr } else { Kind::Osu },
            md5: md5.map(str::to_owned),
            key: key.map(str::to_owned),
            size: 42,
            mtime: 7,
        }
    }

    /// The whole normalising rests on these two round-tripping, in both shapes the walker produces.
    #[test]
    fn both_id_shapes_survive_a_round_trip() {
        let mut ledger = open("ids");
        let hash = "0000f20e2568b57a60444dd428ae440e57f6426e74f54c2e0e0c6d21b27fb0ce";
        let path = "Songs/137294 -45 - A/-45 - A (Urushi38) [Hyper].osu";

        ledger.register("lazer").unwrap();
        ledger.register("stable").unwrap();
        ledger
            .record(&[
                blob(
                    "lazer",
                    hash,
                    Some("477990bba544108ad74438ac77f937ea"),
                    None,
                ),
                blob("stable", path, None, None),
            ])
            .unwrap();

        assert_eq!(
            ledger.known("lazer").unwrap().keys().collect::<Vec<_>>(),
            vec![&hash.to_owned()],
            "a 64-character hash id comes back as the same string"
        );
        assert_eq!(
            ledger.known("stable").unwrap().keys().collect::<Vec<_>>(),
            vec![&path.to_owned()],
            "a path id comes back as the same string"
        );
    }

    #[test]
    fn a_map_is_found_by_its_md5_and_a_replay_by_its_key() {
        let mut ledger = open("lookup");
        ledger.register("stable").unwrap();
        let md5 = "477990bba544108ad74438ac77f937ea";
        let play = "477990bba544108ad74438ac77f937ea-133295220694499848";

        ledger
            .record(&[
                blob("stable", "Songs/x/x.osu", Some(md5), None),
                blob("stable", "Data/r/x.osr", None, Some(play)),
            ])
            .unwrap();

        let (source, id) = ledger.map_source(md5).unwrap().expect("the map is known");
        assert_eq!(source, "stable");
        assert_eq!(id, "Songs/x/x.osu");
        assert!(ledger.map_source("not hex").unwrap().is_none());
        assert!(ledger.map_source(&"0".repeat(32)).unwrap().is_none());

        assert_eq!(
            ledger.replays().unwrap(),
            vec![(
                "stable".to_owned(),
                "Data/r/x.osr".to_owned(),
                play.to_owned()
            )]
        );
    }

    /// The published record is per **play**, so a play filed by both installs publishes once.
    #[test]
    fn publishing_is_recorded_once_per_play() {
        let mut ledger = open("published");
        let play = "477990bba544108ad74438ac77f937ea-133295220694499848";

        ledger.register("stable").unwrap();
        ledger
            .record(&[
                blob("stable", "a.osr", None, Some(play)),
                blob("stable", "b.osr", None, Some(play)),
            ])
            .unwrap();
        assert_eq!(ledger.replays().unwrap().len(), 2, "two files, one play");

        ledger
            .record_published(&[Published {
                play: play.to_owned(),
                object: "13329522".to_owned(),
                bytes: 40_000,
            }])
            .unwrap();

        let published = ledger.published().unwrap();
        assert_eq!(published.len(), 1);
        assert_eq!(published[play], "13329522");
    }

    #[test]
    fn an_index_digest_is_remembered_until_it_changes() {
        let mut ledger = open("artifact");
        assert_eq!(ledger.artifact("index-osu.json").unwrap(), None);
        ledger.record_artifact("index-osu.json", "aaa").unwrap();
        assert_eq!(
            ledger.artifact("index-osu.json").unwrap().as_deref(),
            Some("aaa")
        );
        ledger.record_artifact("index-osu.json", "bbb").unwrap();
        assert_eq!(
            ledger.artifact("index-osu.json").unwrap().as_deref(),
            Some("bbb")
        );
    }

    /// A ledger from an earlier schema is **refused**, not upgraded in place: the caller re-walks
    /// only after being told, which is what stops a stale file being quietly trusted.
    #[test]
    fn a_ledger_from_another_version_is_refused() {
        let path = std::env::temp_dir().join(format!("osu-ledger-old-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
                 INSERT INTO meta (k, v) VALUES ('schema', '2');",
            )
            .unwrap();
        }

        let error = match Ledger::open(&path) {
            Ok(_) => panic!("schema 2 must be refused"),
            Err(error) => error,
        };
        assert!(error.contains("schema 2"), "{error}");
        assert!(error.contains("Delete it and run again"), "{error}");
    }

    #[test]
    fn a_source_must_be_registered_before_it_can_be_recorded() {
        let mut ledger = open("unregistered");
        let error = ledger
            .record(&[blob("nowhere", "abc", None, None)])
            .expect_err("an unregistered source is a mistake, not a silent new row");
        assert!(error.contains("never registered"), "{error}");
    }

    #[test]
    fn hex_helpers_round_trip_and_reject_what_is_not_hex() {
        assert_eq!(hex_bytes("00ff10").unwrap(), vec![0, 255, 16]);
        assert_eq!(osu_core::hex(&[0, 255, 16]), "00ff10");
        assert!(hex_bytes("abc").is_none(), "odd length");
        assert!(hex_bytes("").is_none());
        assert!(hex_bytes("xyz0").is_none());
        assert!(
            hex_bytes("Songs/x.osu").is_none(),
            "so a path is stored as text, never as bytes"
        );
    }
}
