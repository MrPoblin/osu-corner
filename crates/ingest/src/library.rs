//! Building the working set: the local copy of what the library actually uses.
//!
//! Ingest never works inside a game folder. It reads the installs only to find what is new, and
//! everything downstream reads `library/` instead — which is why a pp rebalance re-reads 321 MB
//! sequentially rather than random-seeking across a 32 GB store, and why the pipeline still works
//! after the game has deleted something.
//!
//! ```text
//! library/
//!   maps/<beatmap md5>.osu                only maps a play references
//!   replays/<beatmap md5>-<filetime>.osr  one per play
//!   state.db                              the ledger
//! ```
//!
//! **The filenames are the identity.** A beatmap is named by its MD5 and a replay by its play
//! key, so the same map or play found in two installs — or staged twice by two runs — collapses to
//! one file with no comparison step. That is the whole mechanism behind "unique exporting".

use crate::collect::{self, Known};
use crate::ledger::{Blob, Kind, Ledger};
use crate::mirror::{Fetched, Fetcher};
use crate::pp;
use crate::{Source, SourceKind};
use osu_core::osr;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// The ledger `source` under which a beatmap osu! was asked about and did not know is recorded.
/// It is a source like any other, so a run that wants to ask again deletes the ledger.
const ASKED_SOURCE: &str = "osu-api";

pub fn build(
    sources: &[&Source],
    work: &Path,
    mirrors: &[String],
    limit: usize,
    dry_run: bool,
) -> Result<(), String> {
    let roots = collect::roots(sources);

    if dry_run {
        println!("\n--dry-run: nothing will be written and the ledger will not be updated.");
    } else {
        for dir in ["maps", "replays"] {
            let path = work.join(dir);
            fs::create_dir_all(&path)
                .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
        }
    }

    // A dry run must not create so much as a ledger file. With nothing on disk there is nothing
    // known, which is exactly what a first run would see anyway.
    let ledger_path = work.join("state.db");
    let ledger_target = if dry_run && !ledger_path.exists() {
        PathBuf::from(":memory:")
    } else {
        ledger_path
    };

    let mut ledger = Ledger::open(&ledger_target)?;

    // Reads only, so a dry run may use it too: knowing which maps a mirror could supply is part
    // of knowing what this run would do.
    let mut fetcher = Fetcher::new(work, mirrors);

    // ---------------------------------------------------------------- collect

    let mut inspected = Vec::new();
    let mut skipped = 0u64;
    let mut keyless = 0u64;

    for source in sources {
        let key = source.path.display().to_string();
        let known: Known = ledger.known(&key)?;
        let started = Instant::now();

        let found = match source.kind {
            SourceKind::Lazer => collect::lazer(&key, &source.path, &known)?,
            SourceKind::Stable => collect::stable(&key, &source.path, &known)?,
        };

        println!("\n{}  ({})", source.path.display(), kind_name(source.kind));
        println!(
            "  {:>7} new, {:>7} already known, {:>7} other   [{:.1}s]",
            found.inspected.len(),
            found.skipped,
            found.other,
            started.elapsed().as_secs_f64()
        );

        inspected.extend(found.inspected);
        skipped += found.skipped;
        keyless += found.keyless;
    }

    // Recorded even in a dry run: the ledger is then in memory, so the staging plan below can
    // still be worked out without anything reaching disk. Skipping this is what made an earlier
    // dry run report zero of everything.
    ledger.record(&inspected)?;

    // ---------------------------------------------------------------- replays

    let replays = ledger.replays()?;
    let staged_already = read_dir_names(&work.join("replays"))?;

    let mut plan: Vec<(&str, &str, &str)> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut duplicates = 0u64;

    for (source, id, key) in &replays {
        if !seen.insert(key) {
            // The same play, filed by a second install. Named by key, so it is already covered.
            duplicates += 1;
            continue;
        }
        if !staged_already.contains(&format!("{key}.osr")) {
            plan.push((source, id, key));
        }
    }

    let mut staged_replays = 0u64;
    for (source, id, key) in &plan {
        let from = source_path(&roots, source, id)?;
        if stage(work, "replays", &format!("{key}.osr"), &from, dry_run)? {
            staged_replays += 1;
        }
    }

    // ------------------------------------------------------------------- maps

    let needed: HashSet<&str> = replays
        .iter()
        .filter_map(|(_, _, key)| key.split_once('-').map(|(md5, _)| md5))
        .collect();

    let have: HashSet<String> = read_dir_names(&work.join("maps"))?
        .into_iter()
        .filter_map(|name| name.strip_suffix(".osu").map(str::to_owned))
        .collect();

    let mut map_plan: Vec<(&str, String, String)> = Vec::new();
    // Maps no install holds. These may still be reachable — see below.
    let mut unheld: Vec<&str> = Vec::new();
    for md5 in &needed {
        if have.contains(*md5) {
            continue;
        }
        match ledger.map_source(md5)? {
            Some((source, id)) => map_plan.push((md5, source, id)),
            None => unheld.push(md5),
        }
    }

    let mut staged_maps = 0u64;
    for (md5, source, id) in &map_plan {
        let from = source_path(&roots, source, id)?;
        if stage(work, "maps", &format!("{md5}.osu"), &from, dry_run)? {
            staged_maps += 1;
        }
    }

    // ------------------------------------------------- maps on no disk: the mirrors
    //
    // Three outcomes, kept apart because two of them are answers and one is a "try again later".
    // A map osu! has already been asked about is not asked again: with thousands missing, which
    // is what anyone who prunes maps they have played will have, re-asking every sync would turn
    // a 7-second run into an hour. The `osu-api` source is a ledger source like any other, so a
    // run that wants to ask again deletes `state.db`.

    let already_asked = ledger.known(ASKED_SOURCE)?;
    let mut fetched = 0u64;
    // Seeded from the ledger, not just from this run: "osu! does not know this one" is a recorded
    // answer, so a later run must still report it. Collecting only this run's results made the
    // second run quietly drop the explanation and call the maps "not looked for".
    let mut unknown: Vec<&str> = unheld
        .iter()
        .copied()
        .filter(|md5| already_asked.contains_key(*md5))
        .collect();
    let mut unavailable: Vec<&str> = Vec::new();
    let mut deferred = 0usize;

    if !unheld.is_empty() && fetcher.configured() {
        let askable: Vec<&str> = unheld
            .iter()
            .copied()
            .filter(|md5| !already_asked.contains_key(*md5))
            .collect();
        let to_ask: Vec<&str> = askable.iter().copied().take(limit).collect();
        // Only a map the limit stopped us reaching is deferred. A map already answered is not
        // waiting for anything, and saying otherwise sends the reader to raise a limit that is
        // not the reason.
        deferred = askable.len().saturating_sub(to_ask.len());

        if !to_ask.is_empty() {
            // Said before starting, not after: paced to one request a second this is minutes, and
            // a silent multi-minute run reads as a hang.
            println!(
                "\n  looking for {} of {} beatmaps no install holds, one request a second…",
                to_ask.len(),
                unheld.len()
            );
        }

        let mut asked = Vec::new();
        for md5 in &to_ask {
            // `beatmap` verifies the bytes against the MD5 we asked for, so anything it returns is
            // safe to keep and anything else is refused on our behalf.
            match fetcher.beatmap(md5) {
                Fetched::Map(bytes) => {
                    if stage_bytes(work, "maps", &format!("{md5}.osu"), &bytes, dry_run)? {
                        fetched += 1;
                    }
                }
                Fetched::Unknown => {
                    unknown.push(md5);
                    // Remembered so the next run does not pay for the same answer. Paid for a
                    // dry run too, because a dry run's ledger is in memory and goes nowhere.
                    asked.push(Blob {
                        source: ASKED_SOURCE.to_owned(),
                        id: (*md5).to_owned(),
                        kind: Kind::Other,
                        md5: None,
                        key: None,
                        size: 0,
                        mtime: 0,
                    });
                }
                Fetched::Unavailable => unavailable.push(md5),
            }
        }
        if !asked.is_empty() {
            ledger.record(&asked)?;
        }
    } else if !unheld.is_empty() {
        unavailable = unheld.clone();
    }

    // ------------------------------------------------------------ stars and pp
    //
    // Every play is priced for real rather than sampled, because this is what the index stores.
    // Plays are grouped by map first so a map is decoded once instead of once per play — a map
    // averages 2.65 plays here (§16) and decoding is the expensive half.
    let pricing = Instant::now();
    let mut by_map: HashMap<String, Vec<pp::Play>> = HashMap::new();
    let mut unpriced = 0u64;

    // Driven by the **staged filenames**, which are the deduplicated set: the ledger holds a row
    // per `.osr` per source, so its 15,632 replays collapse to the 8,000 plays the index will
    // contain. Pricing the ledger's rows priced half the library twice and took 74 seconds to do
    // it. These names are also exactly what the index reads, so the two cannot disagree.
    for entry in fs::read_dir(work.join("replays"))
        .map_err(|error| format!("cannot read {}\\replays: {error}", work.display()))?
    {
        let name = entry.map_err(|error| error.to_string())?.file_name();
        let Some(key) = name.to_str().and_then(|name| name.strip_suffix(".osr")) else {
            continue;
        };
        let Some((md5, _)) = key.split_once('-') else {
            unpriced += 1;
            continue;
        };
        let Ok(bytes) = fs::read(work.join("replays").join(format!("{key}.osr"))) else {
            unpriced += 1;
            continue;
        };
        match osr::parse(&bytes) {
            Ok(header) => by_map.entry(md5.to_owned()).or_default().push(pp::Play {
                mods: header.mods,
                counts: header.counts,
                max_combo: header.max_combo,
                mode: header.mode,
                mods_names: header.mods_names.clone(),
                version: header.version,
            }),
            Err(_) => unpriced += 1,
        }
    }

    let maps_priced = by_map.len();
    let mut priced = 0u64;
    let mut stars: Vec<f64> = Vec::new();
    let mut pps: Vec<f64> = Vec::new();

    for (md5, plays) in &by_map {
        let Ok(bytes) = fs::read(work.join("maps").join(format!("{md5}.osu"))) else {
            unpriced += plays.len() as u64;
            continue;
        };
        let Ok(map) = pp::Map::parse(&bytes) else {
            unpriced += plays.len() as u64;
            continue;
        };
        for play in plays {
            match map.attributes(play) {
                Ok(attributes) => {
                    priced += 1;
                    stars.push(attributes.stars);
                    pps.push(attributes.pp);
                }
                // Refused as too suspicious, or not calculable. Counted rather than hidden.
                Err(_) => unpriced += 1,
            }
        }
    }

    let span = |values: &mut Vec<f64>| match (
        values.iter().cloned().fold(f64::INFINITY, f64::min),
        values.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    ) {
        (low, high) if low.is_finite() && high.is_finite() => (low, high),
        _ => (0.0, 0.0),
    };
    let (star_low, star_high) = span(&mut stars);
    let (pp_low, pp_high) = span(&mut pps);
    let pricing_seconds = pricing.elapsed().as_secs_f64();

    // ----------------------------------------------------------------- report

    println!("\n{}", work.display());
    println!(
        "  replays  {:>6} staged, {:>5} duplicate copies of a play already taken, {:>6} known",
        staged_replays,
        duplicates,
        replays.len()
    );
    println!(
        "  maps     {:>6} staged, {:>5} already held, {:>6} needed, {:>5} not held by any disk",
        staged_maps,
        needed
            .len()
            .saturating_sub(unknown.len() + unavailable.len() + fetched as usize + map_plan.len()),
        needed.len(),
        unheld.len()
    );
    if fetched > 0 {
        println!(
            "  fetched  {fetched:>6} beatmaps no install held, from a mirror and MD5-verified"
        );
    }
    println!(
        "  ledger   {:>6} rows, {} {}{skipped} blobs already known so they were never opened",
        ledger.total()?,
        inspected.len(),
        if dry_run {
            "would be added this run; "
        } else {
            "added this run; "
        }
    );
    println!(
        "  priced   {priced:>6} plays over {maps_priced} maps, {unpriced} not priced   \
         [{pricing_seconds:.1}s]  stars {star_low:.2}–{star_high:.2}, pp {pp_low:.1}–{pp_high:.1}"
    );
    println!("  by       {}", pp::ROSU_PP);

    if keyless > 0 {
        println!(
            "\n  WARNING: {keyless} replays parsed but carry no usable timestamp, so they have no\n  \
             play key and were not staged. That is a malformed file, not a normal one."
        );
    }
    // The three mirror outcomes are reported separately, because they mean different things and
    // only one of them is a fact about the map. Reporting a rate limit as "this map is gone" was
    // the bug: it reads exactly like the true case and nothing in the output distinguishes them.
    if !unknown.is_empty() || !unavailable.is_empty() {
        let on = |set: &[&str]| {
            replays
                .iter()
                .filter(|(_, _, key)| {
                    key.split_once('-')
                        .is_some_and(|(md5, _)| set.contains(&md5))
                })
                .count()
        };

        if !unknown.is_empty() {
            println!(
                "\n  {unknown_n} beatmaps, carrying {plays} of {total} plays, are **not known to\n  \
                 osu!** and will not be asked about again. Their replays are staged but cannot be\n  \
                 priced, because the map a score was set on is not on this machine.\n  \
                 This is not proof the map is deleted — the checksum lookup does not serve\n  \
                 unranked or graveyarded maps, and the mirrors do carry those — only that the map\n  \
                 cannot be named from here. Each play still carries its own mode, mods and score.\n  \
                 To ask again, delete {ledger} and run once more.\n  \
                 First few: {first}",
                ledger = work.join("state.db").display(),
                first = unknown
                    .iter()
                    .take(5)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", "),
                total = replays.len(),
                plays = on(&unknown),
                unknown_n = unknown.len()
            );
        }

        if !unavailable.is_empty() {
            println!(
                "\n  {n} beatmaps, carrying {plays} of {total} plays, could not be looked up at\n  \
                 all — rate-limited or unreachable. **Nothing is known about them**, so they are\n  \
                 asked about again next run. Nothing was recorded.",
                n = unavailable.len(),
                plays = on(&unavailable),
                total = replays.len()
            );
        }
    }

    if deferred > 0 {
        println!(
            "\n  {deferred} beatmaps were not looked for this run — the per-run limit is {limit}.\n  \
             Raise it with `--fetch-limit N`, or `--fetch-limit 0` for no limit. They are looked\n  \
             for next run either way."
        );
    }

    if !unheld.is_empty() && !fetcher.configured() {
        println!(
            "\n  {n} beatmaps are on no disk and were **not looked for**, because fetching them\n  \
             needs an osu! application: put OSU_CLIENT_ID and OSU_CLIENT_SECRET in .dev.vars\n  \
             beside the config and run again. Their replays are staged meanwhile, and nothing\n  \
             else about the run changes.",
            n = unheld.len()
        );
    }

    if dry_run {
        println!("\n--dry-run: nothing was written. Run without it to build the working set.");
    }

    Ok(())
}

// ------------------------------------------------------------------------------- helpers

/// Turn a ledger row back into a path in that source's install.
fn source_path(
    roots: &HashMap<String, (SourceKind, PathBuf)>,
    source: &str,
    id: &str,
) -> Result<PathBuf, String> {
    let (kind, root) = roots.get(source).ok_or_else(|| {
        format!(
            "the ledger has entries from {source}, which is no longer an enabled [[source]].\n\
             Re-enable it, or delete the ledger to start again."
        )
    })?;

    Ok(match kind {
        SourceKind::Lazer => {
            // `files/<first hex digit>/<first two>/<whole hash>`
            if id.len() < 3 {
                return Err(format!("{id:?} is not a store hash"));
            }
            root.join("files").join(&id[0..1]).join(&id[0..2]).join(id)
        }
        SourceKind::Stable => root.join(id.replace('/', std::path::MAIN_SEPARATOR_STR)),
    })
}

/// Copy one file into the working set. Returns whether it was staged; a source file that vanished
/// mid-run is not an error, because lazer's cleanup can be running while we work.
fn guarded_path(work: &Path, dir: &str, name: &str) -> Result<PathBuf, String> {
    // Every write in this tool goes through here. Writing outside the working set is the one
    // mistake that could damage a game install, so it is checked rather than trusted: the name
    // must be one this program generates, and the joined path must stay inside the working set.
    if !is_generated(name) {
        return Err(format!(
            "refusing to write {name:?} — not a name this program generates"
        ));
    }
    let to = work.join(dir).join(name);
    if !to.starts_with(work) {
        return Err(format!(
            "refusing to write outside {}: {}",
            work.display(),
            to.display()
        ));
    }
    Ok(to)
}

fn write(to: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::write(to, bytes).map_err(|error| format!("cannot write {}: {error}", to.display()))
}

fn stage(work: &Path, dir: &str, name: &str, from: &Path, dry_run: bool) -> Result<bool, String> {
    let to = guarded_path(work, dir, name)?;

    if dry_run {
        return Ok(true);
    }

    let bytes = match fs::read(from) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("cannot read {}: {error}", from.display())),
    };

    write(&to, &bytes)?;
    Ok(true)
}

/// The same guard, for bytes that came from a mirror rather than from a file on disk. Kept
/// separate from [`stage`] only because there is no source path to read; the check and the write
/// are the same two functions, so there is still exactly one place a name can escape through.
fn stage_bytes(
    work: &Path,
    dir: &str,
    name: &str,
    bytes: &[u8],
    dry_run: bool,
) -> Result<bool, String> {
    let to = guarded_path(work, dir, name)?;

    if dry_run {
        return Ok(true);
    }

    write(&to, bytes)?;
    Ok(true)
}

/// The only two names this program ever writes: a beatmap named by its MD5, and a replay named by
/// its play key.
///
/// Both are built from hex digits and a decimal timestamp, so neither can contain a path
/// separator or a `..`. Checking it here means a bug upstream cannot escape the working set.
fn is_generated(name: &str) -> bool {
    if let Some(md5) = name.strip_suffix(".osu") {
        return md5.len() == 32 && md5.bytes().all(|byte| byte.is_ascii_hexdigit());
    }
    if let Some(stem) = name.strip_suffix(".osr") {
        let Some((md5, filetime)) = stem.split_once('-') else {
            return false;
        };
        return md5.len() == 32
            && md5.bytes().all(|byte| byte.is_ascii_hexdigit())
            && !filetime.is_empty()
            && filetime.bytes().all(|byte| byte.is_ascii_digit());
    }
    false
}

fn read_dir_names(dir: &Path) -> Result<HashSet<String>, String> {
    match fs::read_dir(dir) {
        Ok(entries) => {
            let mut out = HashSet::new();
            for entry in entries {
                let entry = entry.map_err(|error| error.to_string())?;
                out.insert(entry.file_name().to_string_lossy().into_owned());
            }
            Ok(out)
        }
        // A working set that does not exist yet simply has nothing staged.
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(HashSet::new()),
        Err(error) => Err(format!("cannot read {}: {error}", dir.display())),
    }
}

fn kind_name(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Lazer => "lazer",
        SourceKind::Stable => "stable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_generated_names_are_writable() {
        assert!(is_generated("144e76e9bd39f65370d54689255f31ec.osu"));
        assert!(is_generated(
            "144e76e9bd39f65370d54689255f31ec-133295220694499848.osr"
        ));

        // The shapes a path bug or a hostile filename would try.
        assert!(!is_generated("../../etc/passwd"));
        assert!(!is_generated("..\\..\\windows\\system32\\x.osu"));
        assert!(!is_generated("/absolute/osu-corner.toml"));
        assert!(!is_generated(
            "144e76e9bd39f65370d54689255f31ec-1332/../x.osr"
        ));
        assert!(!is_generated("144e76e9bd39f65370d54689255f31ec.osr"));
        assert!(!is_generated("144e76e9bd39f65370d54689255f31e.osu"));
        assert!(!is_generated("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz.osu"));
        assert!(!is_generated("144e76e9bd39f65370d54689255f31ec-abc.osr"));
        assert!(!is_generated("144e76e9bd39f65370d54689255f31ec-"));
        assert!(!is_generated(""));
    }

    #[test]
    fn staging_refuses_anything_it_would_not_have_named() {
        let work = Path::new("/tmp/library");
        let nowhere = Path::new("/nonexistent");
        assert!(stage(work, "maps", "../escape.osu", nowhere, false).is_err());
        assert!(stage(work, "maps", "not-a-name.osu", nowhere, false).is_err());
        assert!(stage(work, "replays", "x.osr", nowhere, false).is_err());
    }
}
