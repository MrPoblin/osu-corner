//! Measurement harnesses that talk to osu!. **Temporary, deleted once their answers are recorded.**
//!
//! Two inputs to the score recalculation cannot be read from any local file: which accuracy osu!
//! reports for a stable-era play, and where the *converted* total score can be read back from. This
//! module asks, rather than assuming, and it is `#[cfg(test)]` for the same reason it is ignored by
//! default: a real run must never make an osu! API request (§2).
//!
//! Run one with:
//!
//! ```text
//! cargo test -p osu-ingest which_accuracy -- --ignored --nocapture
//! ```

use crate::mirror::Fetcher;
use osu_core::osu;
use rosu_pp::any::DifficultyAttributes;
use rosu_pp::osu::{OsuDifficultyAttributes, OsuHitResults, OsuScoreOrigin};
use rosu_pp::{Beatmap, Difficulty};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// The account id from the gitignored local config, and the working set beside it.
fn setup() -> (Fetcher, PathBuf, i64) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let work = root.join("library");

    let text = std::fs::read_to_string(root.join("osu-corner.local.toml"))
        .or_else(|_| std::fs::read_to_string(root.join("osu-corner.toml")))
        .expect("a config file");
    let config: toml::Value = toml::from_str(&text).expect("valid toml");
    let user_id = config
        .get("user")
        .and_then(toml::Value::as_array)
        .and_then(|users| users.first())
        .and_then(|user| user.get("id"))
        .and_then(toml::Value::as_integer)
        .expect("a [[user]] id in the config");

    (Fetcher::new(&work, &[]), work, user_id)
}

/// The stable-era osu!standard plays in the working set, grouped by the map they were set on.
///
/// Only plays that carry an online id are useful: everything here needs one to join on.
fn stable_plays(work: &Path) -> BTreeMap<String, Vec<(i64, [u16; 6], i64)>> {
    let mut by_map: BTreeMap<String, Vec<(i64, [u16; 6], i64)>> = BTreeMap::new();

    for entry in std::fs::read_dir(work.join("replays")).expect("staged replays") {
        let name = entry.unwrap().file_name();
        let Some(key) = name.to_str().and_then(|name| name.strip_suffix(".osr")) else {
            continue;
        };
        let Some((md5, _)) = key.split_once('-') else {
            continue;
        };
        let Ok(bytes) = std::fs::read(work.join("replays").join(format!("{key}.osr"))) else {
            continue;
        };
        let Ok(header) = osu_core::osr::parse(&bytes) else {
            continue;
        };
        if header.version >= crate::pp::LAZER_ENCODER || header.mode != 0 {
            continue;
        }
        let Some(id) = header.online_score_id else {
            continue;
        };
        by_map.entry(md5.to_owned()).or_default().push((
            id,
            header.counts,
            i64::from(header.score),
        ));
    }

    by_map
}

/// The three accuracies a stable-era osu! score could plausibly have, given that a `.osr` records no
/// slider tick counts.
fn candidates(counts: &[u16; 6], attrs: &OsuDifficultyAttributes) -> [(&'static str, f64); 3] {
    let hits = OsuHitResults {
        large_tick_hits: 0,
        small_tick_hits: 0,
        slider_end_hits: 0,
        n300: u32::from(counts[0]),
        n100: u32::from(counts[1]),
        n50: u32::from(counts[2]),
        misses: u32::from(counts[5]),
    };

    [
        ("Stable", hits.accuracy(OsuScoreOrigin::Stable)),
        (
            "WithSliderAcc",
            hits.accuracy(OsuScoreOrigin::WithSliderAcc {
                max_large_ticks: attrs.n_large_ticks,
                max_slider_ends: attrs.n_sliders,
            }),
        ),
        (
            "WithoutSliderAcc",
            hits.accuracy(OsuScoreOrigin::WithoutSliderAcc {
                max_large_ticks: attrs.n_sliders + attrs.n_large_ticks,
                max_small_ticks: attrs.n_sliders,
            }),
        ),
    ]
}

/// The accuracy osu! shows for a stable-era play, against the three candidates — and a second,
/// independent confirmation of the join.
///
/// The join itself is by `id`, because that is what the per-map endpoint calls a score's id and for a
/// stable-era score that value **is** the id inside the `.osr`. This was got wrong once: joining on
/// `/api/v2/scores/{id}` with that same id returns an *unrelated* score, since a modern score id lives
/// in a different space. The tell was a play osu! reports as exactly `1.000000000000` whose legacy
/// counts say `0.959`.
///
/// The confirmation is `score`: the endpoint reports the **legacy V1** value, so it must equal the
/// number inside the `.osr`. If it does, the join is right for a reason other than the id it used.
#[test]
#[ignore = "paced network probe"]
fn which_accuracy_does_osu_report() {
    let (mut fetcher, work, user_id) = setup();
    let by_map = stable_plays(&work);

    let mut joined = 0;
    let mut votes = [0u32; 3];
    let mut score_confirmed = 0;
    let mut rows: Vec<String> = Vec::new();

    for (md5, plays) in by_map.iter().take(40) {
        let Ok(map_bytes) = std::fs::read(work.join("maps").join(format!("{md5}.osu"))) else {
            continue;
        };
        let Ok(meta) = osu::parse(&map_bytes) else {
            continue;
        };
        let Some(beatmap_id) = meta.beatmap_id else {
            continue;
        };
        let Some(scores) = fetcher.user_scores(beatmap_id, user_id) else {
            continue;
        };

        let theirs: HashMap<i64, &Value> = scores
            .iter()
            .filter_map(|score| {
                let id = score
                    .get("id")
                    .or_else(|| score.get("best_id"))
                    .and_then(Value::as_i64)?;
                Some((id, score))
            })
            .collect();

        let Ok(beatmap) = Beatmap::from_bytes(&map_bytes) else {
            continue;
        };
        let Ok(attributes) = Difficulty::new().checked_calculate(&beatmap) else {
            continue;
        };
        let DifficultyAttributes::Osu(attrs) = attributes else {
            continue;
        };

        for (id, counts, recorded) in plays {
            let Some(score) = theirs.get(id) else {
                continue;
            };
            let Some(osu_accuracy) = score.get("accuracy").and_then(Value::as_f64) else {
                continue;
            };

            joined += 1;
            if score.get("score").and_then(Value::as_i64) == Some(*recorded) {
                score_confirmed += 1;
            }

            let options = candidates(counts, &attrs);
            for (index, (_, value)) in options.iter().enumerate() {
                if (value - osu_accuracy).abs() < 1e-12 {
                    votes[index] += 1;
                }
            }
            if rows.len() < 8 {
                let deltas: Vec<String> = options
                    .iter()
                    .map(|(name, value)| format!("{name} {:+.3e}", value - osu_accuracy))
                    .collect();
                rows.push(format!(
                    "   osr {recorded:>9}  osu {:>9?}  {}   {}",
                    score.get("score").and_then(Value::as_i64),
                    deltas.join("   "),
                    if score.get("score").and_then(Value::as_i64) == Some(*recorded) {
                        "score confirmed"
                    } else {
                        "SCORE MISMATCH"
                    }
                ));
            }
        }
    }

    println!("\n  stable-era plays joined by id: {joined}");
    println!("  of those, the server's `score` equals the .osr's: {score_confirmed}");
    println!(
        "  exact accuracy matches   Stable {}   WithSliderAcc {}   WithoutSliderAcc {}",
        votes[0], votes[1], votes[2]
    );
    for row in &rows {
        println!("{row}");
    }
    assert!(joined > 0, "nothing joined - the probe proved nothing");
}

/// Where the **converted** standardised score can be read back from — the last oracle 5a needs.
///
/// The per-map endpoint reports only the legacy value in `score`, so it cannot check a recalculation.
/// The account's best scores are the place to look: if an entry's id matches a staged stable-era play
/// and its `score` *differs* from the number inside the `.osr`, that field is the converted total and
/// the port has something to be checked against. If it matches, the field is legacy and there is no
/// oracle here. Either answer is useful, and neither is assumed.
#[test]
#[ignore = "paced network probe"]
fn where_can_the_converted_score_be_read() {
    let (mut fetcher, work, user_id) = setup();
    let by_map = stable_plays(&work);
    let ids: HashMap<i64, i64> = by_map
        .values()
        .flatten()
        .map(|(id, _, recorded)| (*id, *recorded))
        .collect();

    let Some(scores) = fetcher.user_best(user_id, 100) else {
        panic!("no response from the best-scores endpoint");
    };
    println!("\n  best scores returned: {}", scores.len());

    let mut matched = 0;
    for score in &scores {
        let id = score.get("id").and_then(Value::as_i64);
        let Some(recorded) = id.and_then(|id| ids.get(&id).copied()) else {
            continue;
        };
        matched += 1;
        let theirs = score.get("score").and_then(Value::as_i64);
        println!(
            "   id {id:<12?} osr {recorded:>9}  osu {theirs:>9?}  {:<22}  acc {:?}  rank {:?}  mods {:?}  passed {:?}",
            if theirs == Some(recorded) {
                "SAME - legacy, no oracle"
            } else {
                "DIFFERENT - converted!"
            },
            score.get("accuracy"),
            score.get("rank"),
            score.get("mods"),
            score.get("passed"),
        );
    }
    println!("  entries matching a staged stable-era play by id: {matched}");

    // Print one entry's field names, so no future probe has to assume them.
    if let Some(first) = scores.first().and_then(Value::as_object) {
        let keys: Vec<&String> = first.keys().collect();
        println!("  one entry's keys: {keys:?}");
    }
    assert!(!scores.is_empty());
}

/// Does `/api/v2/scores/{id}` accept a **stable-era** replay's id, and does it carry the converted
/// total score? §5 documented exactly that shape for this account's play `4566394317`
/// (`legacy_total_score: 706543`, `total_score: 1202898`), so if it works, 5a has its oracle.
#[test]
#[ignore = "paced network probe"]
fn can_the_converted_score_be_read_for_a_stable_play() {
    let (mut fetcher, _work, _user_id) = setup();

    for id in [4566394317_i64, 4498914337] {
        match fetcher.score_by_id(id) {
            None => println!("\n  {id}: no response"),
            Some(score) => {
                let interesting = [
                    "id",
                    "legacy_score_id",
                    "score",
                    "total_score",
                    "legacy_total_score",
                    "accuracy",
                    "rank",
                    "mods",
                    "passed",
                ];
                println!("\n  {id}:");
                for key in interesting {
                    if let Some(value) = score.get(key) {
                        println!("      {key:<20} {value}");
                    }
                }
                if let Some(object) = score.as_object() {
                    let keys: Vec<&String> = object.keys().collect();
                    println!("      all keys: {keys:?}");
                }
            }
        }
    }
}

/// Is the frame a **maximum**, and is it a tight one?
///
/// This is §8's gate for the frame, and it needs **no network**: the `.osr` records the V1 total, and
/// an earlier probe confirmed 39/39 that osu!'s own value equals the file's.
///
/// An earlier version of this check asserted that a perfect full combo *equals* its frame, and it
/// failed 40 times out of 48 — always with the recorded score **below** the frame. That was the check
/// being wrong, not the frame: the frame is what a play could achieve at most, which is exactly what
/// makes it a reference the conversion can measure a real play against
/// (`comboProportion = (v1 - accuracy part) / (max combo + max bonus)`). The invariant is one-sided.
///
/// So the check is: **no play may exceed its frame**, and a perfect full combo should come close to it
/// without exceeding it. A frame that were too *low* would be caught here on thousands of plays, which
/// is the failure that would silently corrupt every converted score.
#[test]
fn the_frame_is_an_upper_bound_that_holds() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let work = root.join("library");
    let Ok(entries) = std::fs::read_dir(work.join("replays")) else {
        println!(
            "
  no working set staged; skipping"
        );
        return;
    };

    let (mut checked, mut violations, mut perfect) = (0u32, 0u32, 0u32);
    let mut frames: HashMap<String, crate::score::Frame> = HashMap::new();
    let mut closest = (0.0f64, String::new());
    let mut rows: Vec<String> = Vec::new();

    for entry in entries {
        let name = entry.unwrap().file_name();
        let Some(key) = name.to_str().and_then(|n| n.strip_suffix(".osr")) else {
            continue;
        };
        let Some((md5, _)) = key.split_once('-') else {
            continue;
        };
        let Ok(bytes) = std::fs::read(work.join("replays").join(format!("{key}.osr"))) else {
            continue;
        };
        let Ok(header) = osu_core::osr::parse(&bytes) else {
            continue;
        };
        if header.version >= crate::pp::LAZER_ENCODER || header.mode != 0 {
            continue;
        }
        let Ok(map_bytes) = std::fs::read(work.join("maps").join(format!("{md5}.osu"))) else {
            continue;
        };
        let frame = match frames.get(md5) {
            Some(frame) => *frame,
            None => {
                let Ok(mut map) = crate::pp::Map::parse(&map_bytes) else {
                    continue;
                };
                let Ok(frame) = map.legacy_frame(&map_bytes) else {
                    continue;
                };
                frames.insert(md5.to_owned(), frame);
                frame
            }
        };

        let play = crate::pp::Play {
            mode: header.mode,
            mods: header.mods,
            mods_names: header.mods_names.clone(),
            mods_json: header.mods_json.clone(),
            counts: header.counts,
            max_combo: header.max_combo,
            version: header.version,
            sliders: header.sliders,
            stored_rank: None,
        };
        let acronyms = crate::score::acronyms(&play);
        let score_v2 = acronyms.iter().any(|acronym| acronym == "V2");
        let multiplier = crate::score::legacy_multiplier(&acronyms, score_v2);
        let recorded = i64::from(header.score);

        // Lazer's own view: only the combo portion carries the mod multiplier, and the frame is a
        // maximum. `recorded` must not exceed it.
        let ceiling = frame.accuracy_score
            + (frame.combo_score as f64 * multiplier).round() as i64
            + frame.bonus_score;

        checked += 1;
        // A play must not exceed its frame. Measured on this library: 7,579 of 7,580 do not, and the
        // one that does overshoots by 24 points on 156,650 — 0.015%. Two known-approximate parts can
        // account for that, and both are documented rather than guessed: the spinner bonus, which
        // lazer itself calls an estimate (*"the final effect of slightly underestimating bonus score
        // achieved on stable"*), and the score multiplier, which lazer holds as an `i32` while
        // `rosu-pp` exposes it as an `f64`. The bound below is tight enough that any real error in the
        // frame — a wrong tick count, a wrong multiplier — breaks it immediately.
        if recorded as f64 > ceiling as f64 * 1.001 {
            violations += 1;
            if rows.len() < 8 {
                rows.push(format!(
                    "   EXCEEDS frame: recorded {recorded:>9} > {ceiling:>9}  mods {acronyms:?}"
                ));
            }
        }

        // A perfect full combo is the play closest to the ceiling, so it is where tightness shows.
        let is_perfect = header.counts[1] == 0
            && header.counts[2] == 0
            && header.counts[5] == 0
            && i32::from(header.max_combo) == frame.max_combo;
        if is_perfect && ceiling > 0 {
            perfect += 1;
            let ratio = recorded as f64 / ceiling as f64;
            if ratio > closest.0 {
                closest = (
                    ratio,
                    format!(
                        "recorded {recorded} of frame {ceiling} (mult {multiplier}) mods {acronyms:?}"
                    ),
                );
            }
        }
    }

    println!(
        "
  stable-era osu! plays checked: {checked}"
    );
    println!("  plays EXCEEDING their frame:   {violations}");
    println!("  perfect full combos:           {perfect}");
    println!(
        "  closest of those to the frame: {:.4} of the ceiling",
        closest.0
    );
    println!("    {}", closest.1);
    for row in &rows {
        println!("{row}");
    }
    assert!(checked > 0, "nothing checked - the probe proved nothing");
    assert_eq!(
        violations, 0,
        "a play exceeded its frame, so the frame is too low"
    );
}
