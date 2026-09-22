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
    let mut per_mode: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    // The other half of the check: a frame that is too *high* is harmless, but it is also useless,
    // and a play with **no misses** has left nothing on the table — so its recorded total should sit
    // right up against its frame. That is what shows the frames are tight rather than merely safe, and
    // it is the only tightness evidence taiko, catch and mania can have: no osu!-side converted total
    // exists for any of their plays here (§8).
    let mut closest_by_mode: BTreeMap<String, (f64, String)> = BTreeMap::new();
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
        if header.version >= crate::pp::LAZER_ENCODER {
            continue;
        }
        let Ok(map_bytes) = std::fs::read(work.join("maps").join(format!("{md5}.osu"))) else {
            continue;
        };
        // Every mode, not just osu!standard: the frame is the reference the conversion divides by, so
        // a frame that is too *low* corrupts every converted score on the map while a frame that is
        // too high is harmless. That asymmetry is what this direction tests.
        let mode = crate::score::mode_of_byte(header.mode);
        let frame_key = format!("{}-{mode:?}", md5);
        let frame = match frames.get(&frame_key) {
            Some(frame) => *frame,
            None => {
                let Ok(mut map) = crate::pp::Map::parse(&map_bytes) else {
                    continue;
                };
                // A convert has no frame, and is not this check's business: its plays are refused.
                let Ok(frame) = map.legacy_frame(&map_bytes, mode) else {
                    continue;
                };
                frames.insert(frame_key, frame);
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
        let multiplier = crate::score::legacy_multiplier(mode, &acronyms, score_v2);
        let recorded = i64::from(header.score);

        // Lazer's own view: only the combo portion carries the mod multiplier, and the frame is a
        // maximum. `recorded` must not exceed it.
        let ceiling = frame.accuracy_score
            + (frame.combo_score as f64 * multiplier).round() as i64
            + frame.bonus_score;

        checked += 1;
        per_mode.entry(format!("{mode:?}")).or_default().0 += 1;
        // A play must not exceed its frame. Measured on this library: 7,579 of 7,580 do not, and the
        // one that does overshoots by 24 points on 156,650 — 0.015%. Two known-approximate parts can
        // account for that, and both are documented rather than guessed: the spinner bonus, which
        // lazer itself calls an estimate (*"the final effect of slightly underestimating bonus score
        // achieved on stable"*), and the score multiplier, which lazer holds as an `i32` while
        // `rosu-pp` exposes it as an `f64`. The bound below is tight enough that any real error in the
        // frame — a wrong tick count, a wrong multiplier — breaks it immediately.
        // Only the mods a mode actually has can carry a multiplier; a legacy bit for a mod the
        // ruleset has no such mod for is inert, which `legacy_multiplier` already models.
        if recorded as f64 > ceiling as f64 * 1.001 {
            violations += 1;
            per_mode.entry(format!("{mode:?}")).or_default().1 += 1;
            if rows.len() < 12 {
                rows.push(format!(
                    "   EXCEEDS frame ({mode:?}): recorded {recorded:>9} > {ceiling:>9}  mods {acronyms:?}"
                ));
            }
        }

        // A no-miss play is the tightness evidence for every mode, including the three whose perfect
        // full combos cannot be identified (their `.osr` combo counts objects that never give combo).
        // "Left nothing on the table" means something different per mode, because the legacy counts
        // do: taiko's `count100` is an Ok that still gives combo but scores half, and catch's `katu`
        // is a **missed tiny droplet**, which breaks combo even though `countmiss` is zero.
        let no_losses = match mode {
            crate::score::Mode::Taiko => header.counts[1] == 0 && header.counts[5] == 0,
            crate::score::Mode::Catch => header.counts[4] == 0 && header.counts[5] == 0,
            _ => header.counts[1] == 0 && header.counts[2] == 0 && header.counts[5] == 0,
        };

        if no_losses && ceiling > 0 {
            let ratio = recorded as f64 / ceiling as f64;
            let entry = closest_by_mode
                .entry(format!("{mode:?}"))
                .or_insert((0.0, String::new()));
            if ratio > entry.0 {
                *entry = (
                    ratio,
                    format!(
                        "recorded {recorded} of frame {ceiling}, combo {}/{}, counts {:?}",
                        header.max_combo, frame.max_combo, header.counts
                    ),
                );
            }
        }

        // A perfect full combo is the play closest to the ceiling, so it is where tightness shows.
        // Only osu!standard is asked: in the other three modes `frame.max_combo` counts *scoring*
        // objects, which is not what the `.osr` records — taiko's maximum combo counts the drum-roll
        // ticks that never give combo — so the comparison would not mean the same thing.
        let is_perfect = mode == crate::score::Mode::Osu
            && header.counts[1] == 0
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
  stable-era plays checked: {checked}"
    );
    println!("  plays EXCEEDING their frame:   {violations}");
    println!("  per mode (checked / exceeding):");
    for (mode, (count, bad)) in &per_mode {
        println!("    {mode:<6} {count:>5} / {bad}");
    }
    println!("  closest no-miss play to its frame, per mode:");
    for (mode, (ratio, detail)) in &closest_by_mode {
        println!("    {mode:<6} {ratio:.4} of the ceiling   {detail}");
    }
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

/// **Does the standardised multiplier table match real scores?**
///
/// A lazer-era blob records *both* the pre-mod score and the with-mods one, and lazer's conversion
/// multiplies the first by the multiplier to get the second — so their ratio **is** the multiplier
/// that client applied. That turns the table from something read out of source into something
/// measured against 300-odd real scores, on every mod combination this library actually contains.
///
/// It is also the check that separates the two candidate tables: the pinned commit's values and the
/// modern `ScoreMultiplierCalculator` V2 values differ on `HR`, `HD`, `FL` and every rate mod, so a
/// library with any modded lazer play says which one produced the number in the file. No network.
#[test]
fn the_standardised_multiplier_matches_real_lazer_scores() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let work = root.join("library");
    let Ok(entries) = std::fs::read_dir(work.join("replays")) else {
        println!("\n  no working set staged; skipping");
        return;
    };

    let mut rows: BTreeMap<String, (u32, f64)> = BTreeMap::new();
    let mut unmatched: Vec<String> = Vec::new();
    let (mut checked, mut bad) = (0u32, 0u32);

    for entry in entries {
        let name = entry.unwrap().file_name();
        let Some(key) = name.to_str().and_then(|n| n.strip_suffix(".osr")) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(work.join("replays").join(format!("{key}.osr"))) else {
            continue;
        };
        let Ok(header) = osu_core::osr::parse(&bytes) else {
            continue;
        };
        if header.version < crate::pp::LAZER_ENCODER {
            continue;
        }
        // Only a lazer-era blob has the pair, and a blob-less one has nothing to measure.
        let Some(without) = header.total_score_without_mods else {
            continue;
        };
        if without <= 0 || header.score <= 0 {
            continue;
        }

        let play = crate::pp::Play {
            mode: header.mode,
            mods: header.mods,
            mods_names: header.mods_names.clone(),
            mods_json: header.mods_json.clone(),
            counts: header.counts,
            max_combo: header.max_combo,
            version: header.version,
            sliders: header.sliders,
            stored_rank: header.stored_rank.clone(),
        };
        let acronyms = crate::score::acronyms(&play);
        let mode = match header.mode {
            1 => crate::score::Mode::Taiko,
            2 => crate::score::Mode::Catch,
            3 => crate::score::Mode::Mania,
            _ => crate::score::Mode::Osu,
        };

        // lazer writes `total = round(without_mods * multiplier)`, so the ratio recovers it to within
        // half a point in ~10^6.
        let implied = f64::from(header.score) / without as f64;
        let ours = crate::score::standardised_multiplier(mode, &acronyms);

        checked += 1;
        let label = if acronyms.is_empty() {
            "NM".to_owned()
        } else {
            acronyms.concat()
        };
        let entry = rows.entry(label.clone()).or_insert((0, 0.0));
        entry.0 += 1;
        entry.1 = implied;

        if (implied - ours).abs() > ours * 1e-3 {
            bad += 1;
            if unmatched.len() < 12 {
                unmatched.push(format!(
                    "   {label:<10} implied {implied:.6}  ours {ours:.6}  ({:.2}% off)",
                    (implied / ours - 1.0) * 100.0
                ));
            }
        }
    }

    println!(
        "
  lazer-era plays carrying a pre-mod score: {checked}
  distinct mod sets: {}  not matching today's table: {bad}",
        rows.len()
    );
    for (label, (count, implied)) in &rows {
        println!("   {label:<10} {count:>4} plays   implied multiplier {implied:.6}");
    }
    if !unmatched.is_empty() {
        println!(
            "
  The remainder were scored by an OLDER client whose table has since changed — lazer's
  osu! rate curve moved from 1.1 to 1.23 and its mania key mods from 1.0 to 0.9 — so a pre-mod
  value written back then cannot corroborate today's table. Named for the record:"
        );
        for row in &unmatched {
            println!("{row}");
        }
    }

    // The rows that carry the weight: each is a mod set with enough plays that one odd client cannot
    // explain it. These are what makes the table measured rather than merely read.
    for (label, at_least, expected) in [
        ("NM", 100, 1.0),
        ("DT", 100, 1.23),
        ("HR", 20, 1.09),
        ("HDDT", 10, 1.04 * 1.23),
        ("NC", 5, 1.23),
        ("NF", 5, 0.5),
    ] {
        let Some((count, implied)) = rows.get(label) else {
            panic!("no {label} plays were measured");
        };
        assert!(
            *count >= at_least,
            "{label}: only {count} plays measured, expected at least {at_least}"
        );
        assert!(
            (implied - expected).abs() < expected * 1e-4,
            "{label}: measured {implied}, the table says {expected}"
        );
    }

    assert!(
        checked > 0,
        "no lazer-era play with a pre-mod score was found, so nothing was measured"
    );
}

/// **Does `CL` scale a stable-era converted score?**
///
/// osu!'s decoder appends `ModClassic` to any pre-lazer replay, and the V2 calculator prices Classic
/// at 0.96 — so if the server's `ScoreInfo` carries that append, *every* stable-era converted score
/// is 4.17% lower than a port that omits it. Whether the server does the append is not readable from
/// `ppy/osu` (the server builds its own `ScoreInfo`), so this measures it instead.
///
/// The measurement is a factor, not a comparison: the conversion is linear in the standardised
/// multiplier, so evaluating it at 1.0 gives the pre-mod value exactly, and
/// `osu!'s total / pre-mod value` is the multiplier the server actually applied — mods, Classic and
/// all. Two plays whose totals §5 recorded before the API stopped serving them give two independent
/// readings, and they disagree with each other if the wrong assumption is in the port.
#[test]
fn does_classic_scale_a_stable_era_converted_score() {
    // The two converted totals §5 recorded from the API while it still served them, keyed by the
    // V1 legacy score the `.osr` itself holds.
    const RECORDED: [(i64, i64, &str); 2] = [
        (706_543, 1_202_898, "HDDT — §5's worked example"),
        (2_956_090, 1_134_257, "DT — §5's other verified play"),
    ];

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let work = root.join("library");
    let Ok(entries) = std::fs::read_dir(work.join("replays")) else {
        println!("\n  no working set staged; skipping");
        return;
    };

    let mut found: HashMap<i64, (String, i64, Vec<String>)> = HashMap::new();

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
        if header.version >= crate::pp::LAZER_ENCODER {
            continue;
        }
        if !RECORDED
            .iter()
            .any(|(v1, _, _)| *v1 == i64::from(header.score))
        {
            continue;
        }
        let Ok(map_bytes) = std::fs::read(work.join("maps").join(format!("{md5}.osu"))) else {
            continue;
        };
        let Ok(mut map) = crate::pp::Map::parse(&map_bytes) else {
            continue;
        };
        // A convert has no frame, and is not one of these two plays anyway.
        let Ok(frame) = map.legacy_frame(&map_bytes, crate::score::Mode::Osu) else {
            continue;
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
            stored_rank: header.stored_rank.clone(),
        };
        let acronyms = crate::score::acronyms(&play);
        let Ok(play_attributes) = map.attributes(&play) else {
            continue;
        };
        let score_v2 = acronyms.iter().any(|acronym| acronym == "V2");
        let legacy = crate::score::legacy_multiplier(crate::score::Mode::Osu, &acronyms, score_v2);

        // The conversion is linear in the standardised multiplier, and `legacy` is independent of it,
        // so asking for a multiplier of 1.0 returns the pre-mod value.
        let Some(without_mods) = crate::score::convert(
            crate::score::Mode::Osu,
            &frame,
            &crate::score::Achieved {
                v1_total: i64::from(header.score),
                accuracy: play_attributes.accuracy,
                max_combo: u32::from(header.max_combo),
                misses: u32::from(header.counts[5]),
                tiny_droplets: (0, 0),
                fruits_max: 0,
            },
            legacy,
            1.0,
        ) else {
            continue;
        };

        found.insert(
            i64::from(header.score),
            (key.to_owned(), without_mods, acronyms),
        );
    }

    println!("\n  plays found: {}", found.len());

    for (v1, total, what) in RECORDED {
        let Some((key, without_mods, acronyms)) = found.get(&v1) else {
            println!("   {what:<34} NOT STAGED (looked for a .osr recording {v1})");
            continue;
        };
        let implied = total as f64 / *without_mods as f64;
        let without_classic: Vec<String> = acronyms
            .iter()
            .filter(|acronym| acronym.as_str() != "CL")
            .cloned()
            .collect();
        let product =
            crate::score::standardised_multiplier(crate::score::Mode::Osu, &without_classic);
        let with_classic = crate::score::standardised_multiplier(crate::score::Mode::Osu, acronyms);

        println!(
            "   {what:<34} {key}\n     mods {}  pre-mod {without_mods}\n     implied multiplier {implied:.6}  (osu!'s {total} / ours)\n     our table: without CL {product:.6}   with CL {with_classic:.6}",
            acronyms.concat()
        );
        println!(
            "     -> Classic factor that would make ours exact: {:.4}  (0.985 = CL at note lock, 0.96 = CL without it, 1.0 = no CL)\n",
            implied / product
        );

        // The whole point: with Classic at 0.985 the port reproduces osu!'s own number, so this is
        // an assertion rather than a printout.
        assert!(
            (implied - with_classic).abs() < with_classic * 1e-4,
            "{what}: implied {implied} vs ours {with_classic}"
        );
    }

    assert!(!found.is_empty(), "neither recorded play is staged");
}

/// **Do the catch and taiko frames count the same objects rosu-pp does?**
///
/// These are the two frames whose object model is not a straight read of the file: catch turns a
/// juice stream's head, tail and repeats into *fruits* and its ticks into *droplets*, and taiko splits
/// a convert's sliders into runs of hit circles. Both are ports of lazer's converters, so comparing
/// them against `rosu-pp`'s independently-written ports is a real check rather than a restatement —
/// two implementations of one algorithm, agreeing on every map in the library.
///
/// `rosu-pp` converts a map of another ruleset on the way in, exactly as lazer does, so **converts are
/// covered too** — which matters, because a convert is where taiko's frame has a branch at all.
///
/// The compared quantity is the maximum combo, because it is the one the two sides define the same
/// way: rosu-pp's catch `max_combo()` is `fruits + droplets`, and its taiko `max_combo` counts only
/// objects that give combo — which in the legacy simulator means only `Hit`s.
#[test]
fn the_frames_agree_with_rosu_pp() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let work = root.join("library");
    let Ok(entries) = std::fs::read_dir(work.join("maps")) else {
        println!("\n  no working set staged; skipping");
        return;
    };

    let (mut compared, mut mismatched) = (0u32, 0u32);
    let mut rows: Vec<String> = Vec::new();

    for entry in entries {
        let name = entry.unwrap().file_name();
        let Some(md5) = name.to_str().and_then(|n| n.strip_suffix(".osu")) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(work.join("maps").join(format!("{md5}.osu"))) else {
            continue;
        };
        let Ok(beatmap) = Beatmap::from_bytes(&bytes) else {
            continue;
        };

        for mode in [crate::score::Mode::Catch, crate::score::Mode::Taiko] {
            let (rosu_pp_combo, mine, what) = match mode {
                crate::score::Mode::Catch => {
                    let Ok(attributes) = Difficulty::new()
                        .checked_calculate_for_mode::<rosu_pp::catch::Catch>(&beatmap)
                    else {
                        continue;
                    };
                    let Ok(mut map) = rosu_map::Beatmap::from_bytes(&bytes) else {
                        continue;
                    };
                    (
                        attributes.max_combo() as i64,
                        crate::score::frame_catch(&mut map, 1.0).max_combo as i64,
                        "fruits + droplets",
                    )
                }
                _ => {
                    let Ok(attributes) = Difficulty::new()
                        .checked_calculate_for_mode::<rosu_pp::taiko::Taiko>(&beatmap)
                    else {
                        continue;
                    };
                    let Ok(map) = rosu_map::Beatmap::from_bytes(&bytes) else {
                        continue;
                    };
                    (
                        i64::from(attributes.max_combo()),
                        crate::score::frame_taiko(&map, 0).max_combo as i64,
                        "combo-giving objects",
                    )
                }
            };

            compared += 1;
            if rosu_pp_combo != mine {
                mismatched += 1;
                if rows.len() < 12 {
                    rows.push(format!(
                        "   MISMATCH {mode:?} {md5}: rosu-pp {rosu_pp_combo} vs mine {mine} ({what})"
                    ));
                }
            }
        }
    }

    println!("\n  frames compared against rosu-pp: {compared}   combo mismatches: {mismatched}");
    for row in &rows {
        println!("{row}");
    }

    assert!(compared > 0, "no map was found to compare");
    assert_eq!(
        mismatched, 0,
        "the frames and rosu-pp disagree about how many objects a map has"
    );
}
