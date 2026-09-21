//! Walking the game folders.
//!
//! Nothing here writes. It answers "what is in these installs that the ledger has not seen", and
//! reads enough of each new file to file it: a beatmap gets its MD5, a replay gets its play key,
//! everything else is recorded as not ours so it is never opened again.
//!
//! The two clients could hardly be less alike. Lazer's store is content-addressed with no
//! filenames, so every new blob has to be opened — and that walk is parallelised, because it is
//! the one genuinely expensive thing in the pipeline. Stable has real names on real files, so a
//! file whose size and mtime are unchanged is not opened at all.

use crate::Source;
use crate::ledger::{Blob, Kind};
use osu_core::{Blob as Classified, osr, osu};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// What the ledger already knows about one source, keyed by ledger id, with the size and mtime
/// needed to tell whether a named file has changed since.
pub type Known = HashMap<String, (u64, i64)>;

#[derive(Default)]
pub struct Found {
    pub inspected: Vec<Blob>,
    /// Blobs the ledger already had, so this run never opened them.
    pub skipped: u64,
    /// Files that were neither a beatmap nor a usable replay.
    pub other: u64,
    /// Replays whose header parsed but whose timestamp gives no usable key. Should be zero.
    pub keyless: u64,
}

impl Found {
    fn merge(mut self, other: Found) -> Self {
        self.inspected.extend(other.inspected);
        self.skipped += other.skipped;
        self.other += other.other;
        self.keyless += other.keyless;
        self
    }

    fn absorb(&mut self, other: Found) {
        *self = std::mem::take(self).merge(other);
    }
}

/// Read one new blob whole and work out what it is.
///
/// Whole rather than a head, because a beatmap's MD5 needs every byte and a replay's key sits
/// after a variable-length string. These files are small and each is read exactly once.
fn inspect(source: &str, id: String, path: &Path, size: u64, mtime: i64) -> io::Result<Blob> {
    // Read only the first 64 bytes and classify from those. Most blobs in a lazer store are audio,
    // images or skin files that are neither a beatmap nor a replay, and reading them whole to
    // discover that cost 31 GB on one real store — the whole reason HEAD_LEN exists.
    let mut file = fs::File::open(path)?;
    let mut bytes = vec![0u8; osu_core::HEAD_LEN];
    let mut filled = 0;
    while filled < bytes.len() {
        match file.read(&mut bytes[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    bytes.truncate(filled);

    let (kind, md5, key) = match osu_core::classify(&bytes) {
        Classified::Other => (Kind::Other, None, None),

        // Both of these need the whole file: a beatmap's MD5 covers every byte, and a replay's
        // timestamp sits after a variable-length string. The open handle is reused, so it is one
        // open and two reads.
        Classified::Osu => {
            file.read_to_end(&mut bytes)?;
            (Kind::Osu, Some(osu::md5(&bytes)), None)
        }
        Classified::Osr => {
            file.read_to_end(&mut bytes)?;
            match osr::parse(&bytes) {
                Ok(header) => (Kind::Osr, None, header.key()),
                // The header shape was right but the body was not, so it is filed as not-ours: it
                // gets counted, and it is never read again.
                Err(_) => (Kind::Other, None, None),
            }
        }
    };

    Ok(Blob {
        source: source.to_owned(),
        id,
        kind,
        md5,
        key,
        size,
        mtime,
    })
}

fn tally(found: &mut Found, blob: &Blob) {
    match blob.kind {
        Kind::Other => found.other += 1,
        Kind::Osr if blob.key.is_none() => found.keyless += 1,
        _ => {}
    }
}

// --------------------------------------------------------------------------------- lazer

/// `files/<a>/<ab>/<sha256>` — 16 shards, then 256 beneath them.
pub fn lazer(source: &str, root: &Path, known: &Known) -> Result<Found, String> {
    let store = root.join("files");
    if !store.is_dir() {
        return Err(format!(
            "{} is not a lazer data folder — no files/ directory in it.\n\
             If the install moved, fix `path` in osu-corner.local.toml; if it is gone, set \
             `enabled = false`.",
            root.display()
        ));
    }

    let mut shards = Vec::new();
    for level1 in dirs_in(&store).map_err(|e| e.to_string())? {
        shards.extend(dirs_in(&level1).map_err(|e| e.to_string())?);
    }
    if shards.is_empty() {
        return Err(format!("{} holds no shard directories", store.display()));
    }

    let per_shard = shards
        .par_iter()
        .map(|shard| scan_shard(source, shard, known))
        .collect::<Vec<Result<Found, String>>>();

    let mut total = Found::default();
    for shard in per_shard {
        total.absorb(shard?);
    }
    Ok(total)
}

fn scan_shard(source: &str, shard: &Path, known: &Known) -> Result<Found, String> {
    let mut found = Found::default();

    for entry in fs::read_dir(shard).map_err(|e| format!("{}: {e}", shard.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            continue;
        }

        // The store's filename is the content's SHA-256, so it can never go stale and needs no
        // size or mtime check — presence in the ledger is the whole test.
        let id = entry.file_name().to_string_lossy().into_owned();
        if known.contains_key(&id) {
            found.skipped += 1;
            continue;
        }

        let metadata = entry.metadata().map_err(|e| e.to_string())?;
        let blob = match inspect(source, id, &entry.path(), metadata.len(), 0) {
            Ok(blob) => blob,
            // A file that vanished between readdir and open is not an error; lazer's cleanup can
            // be running while we look.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("{}: {error}", entry.path().display())),
        };

        tally(&mut found, &blob);
        found.inspected.push(blob);
    }

    Ok(found)
}

fn dirs_in(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            out.push(entry.path());
        }
    }
    Ok(out)
}

// -------------------------------------------------------------------------------- stable

/// `Songs/**/*.osu` and `Data/r/*.osr`, both named for real.
pub fn stable(source: &str, root: &Path, known: &Known) -> Result<Found, String> {
    if !root.is_dir() {
        return Err(format!(
            "{} does not exist.\n\
             If the install moved, fix `path` in osu-corner.local.toml; if it is gone, set \
             `enabled = false`.",
            root.display()
        ));
    }

    let mut found = Found::default();

    match beatmap_dirs(&root.join("Songs")) {
        Ok(dirs) => {
            let per_dir = dirs
                .par_iter()
                .map(|dir| scan_songs_dir(source, root, dir, known))
                .collect::<Vec<Result<Found, String>>>();
            for dir in per_dir {
                found.absorb(dir?);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            println!(
                "  note: no Songs/ in {}, so it contributes no maps",
                root.display()
            );
        }
        Err(error) => return Err(error.to_string()),
    }

    match scan_replay_dir(source, root, &root.join("Data").join("r"), known) {
        Ok(replays) => found.absorb(replays),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            println!(
                "  note: no Data/r/ in {}, so it contributes no replays",
                root.display()
            );
        }
        Err(error) => return Err(error.to_string()),
    }

    Ok(found)
}

/// Every directory beneath `Songs/`, including `Songs/` itself. Laid out breadth-first so the
/// per-directory work can be handed to the pool in one go.
fn beatmap_dirs(songs: &Path) -> io::Result<Vec<PathBuf>> {
    if !songs.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is not a directory", songs.display()),
        ));
    }

    let mut out = vec![songs.to_path_buf()];
    let mut next = 0;
    while next < out.len() {
        let current = out[next].clone();
        next += 1;
        out.extend(dirs_in(&current)?);
    }
    Ok(out)
}

fn scan_songs_dir(source: &str, root: &Path, dir: &Path, known: &Known) -> Result<Found, String> {
    let mut found = Found::default();

    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_file()
            || !has_extension(&entry.path(), "osu")
        {
            continue;
        }

        let Some(id) = relative_id(root, &entry.path()) else {
            continue;
        };
        let metadata = entry.metadata().map_err(|e| e.to_string())?;
        let mtime = modified_at(&metadata);

        // A named file can change, so it is skipped only while its size and mtime still agree.
        if known.get(&id) == Some(&(metadata.len(), mtime)) {
            found.skipped += 1;
            continue;
        }

        let blob = match inspect(source, id, &entry.path(), metadata.len(), mtime) {
            Ok(blob) => blob,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("{}: {error}", entry.path().display())),
        };
        tally(&mut found, &blob);
        found.inspected.push(blob);
    }

    Ok(found)
}

fn scan_replay_dir(source: &str, root: &Path, dir: &Path, known: &Known) -> io::Result<Found> {
    let mut found = Found::default();

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || !has_extension(&entry.path(), "osr") {
            continue;
        }

        let Some(id) = relative_id(root, &entry.path()) else {
            continue;
        };
        let metadata = entry.metadata()?;
        let mtime = modified_at(&metadata);

        if known.get(&id) == Some(&(metadata.len(), mtime)) {
            found.skipped += 1;
            continue;
        }

        let blob = match inspect(source, id, &entry.path(), metadata.len(), mtime) {
            Ok(blob) => blob,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        tally(&mut found, &blob);
        found.inspected.push(blob);
    }

    Ok(found)
}

// ------------------------------------------------------------------------------- helpers

/// The ledger id for a named file: its path relative to the install, with `/` separators so the
/// ledger keeps the same meaning if it is ever read from another platform.
fn relative_id(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

fn has_extension(path: &Path, wanted: &str) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case(wanted))
}

fn modified_at(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

/// Sources keyed by the string the ledger stores, so a row can be turned back into a path.
pub fn roots(sources: &[&Source]) -> HashMap<String, (crate::SourceKind, PathBuf)> {
    sources
        .iter()
        .map(|source| {
            (
                source.path.display().to_string(),
                (source.kind, source.path.clone()),
            )
        })
        .collect()
}
