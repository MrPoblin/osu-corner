//! The published profile snapshot.
//!
//! **Why this exists at all.** The Worker's live `/api/osu/profile` is the good path, but its
//! upstream call leaves from Cloudflare's shared egress addresses and osu! rate-limits per IP. So
//! there are windows in which the Worker cannot reach the API at all and answers `503`, however
//! correct its code is — measured, not assumed: the same build and the same credentials answer `200`
//! from a residential address and `503` from the edge in the same minute, and a `curl` with the
//! same bearer token shows `x-ratelimit-remaining: 1199`.
//!
//! A card that cannot be refreshed is a card that cannot be **updated**, so the profile is
//! published the same way the index is: by the one part of this system whose address osu! answers.
//! The frontend tries the Worker first and falls back to `${public_base}/profile-<mode>.json`, so
//! the live path still wins whenever it works and the snapshot is the floor.
//!
//! **Projected, not verbatim.** The file is written through `osu_core::profile::project`, the same
//! allowlist the Worker serves through. A snapshot of the raw response would publish exactly the
//! fields the allowlist exists to withhold.

use crate::mirror::Fetcher;
use osu_core::profile;
use std::path::Path;

/// Unix seconds — the unit the Worker's `fetched_at` and the index header both use.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

/// The rulesets, named as **osu!** names them. `catch` is `fruits` upstream, which is also the name
/// the frontend asks for (`apiMode` in `apps/corner/src/osu.ts`).
pub const MODES: [&str; 4] = ["osu", "taiko", "fruits", "mania"];

/// Fetch and write one `profile-<mode>.json` per ruleset into the working set.
///
/// A ruleset osu! did not answer for is reported and skipped, and any file already there is left
/// alone: a stale profile is better than none, and failing the run over it would cost the index and
/// the replays, which are the run's actual product.
///
/// A dry run fetches and reports without writing, the same promise `index::write` keeps.
pub fn write(
    work: &Path,
    user_id: i64,
    fetcher: &mut Fetcher,
    dry_run: bool,
) -> Result<Vec<String>, String> {
    let mut written = Vec::new();

    for mode in MODES {
        let Some(body) = fetcher.profile(user_id, mode) else {
            println!(
                "  {:<17}{mode}: osu! did not answer, keeping any earlier file",
                "profile"
            );
            continue;
        };

        let Some(projected) = profile::project(&body) else {
            println!(
                "  {:<17}{mode}: unrecognised response, keeping any earlier file",
                "profile"
            );
            continue;
        };

        // Stamped with the **fetch**, not with the data. "Last updated" has to mean "we last
        // managed to ask osu!", or a profile that has not changed in months would look broken while
        // it is in fact perfectly current — and a card that cries wolf is a card whose warning gets
        // ignored when it matters.
        let Some(stamped) = profile::with_fetched_at(&projected, now()) else {
            println!(
                "  {:<17}{mode}: could not be stamped, keeping any earlier file",
                "profile"
            );
            continue;
        };

        let file = format!("profile-{mode}.json");

        if !dry_run {
            std::fs::write(work.join(&file), stamped.as_bytes())
                .map_err(|error| format!("cannot write {file}: {error}"))?;
        }

        println!(
            "  {:<17}{mode}  {:>6.0} KB",
            "profile",
            stamped.len() as f64 / 1024.0
        );
        written.push(file);
    }

    Ok(written)
}
