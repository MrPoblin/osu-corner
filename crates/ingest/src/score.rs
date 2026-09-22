//! The standardised score (§8 step 5a): what osu! shows today for a score that was set under
//! ScoreV1.
//!
//! # Why this is an estimate, and which half of it is not
//!
//! The conversion needs the shape of the play's combos, and **a `.osr` never recorded it** — six
//! judgement counts and a max combo say nothing about whether a combo portion came from one 1,000
//! chain or ten 100-chains. osu!'s own conversion is labelled the same way, *"we are constructing a
//! 'best possible' score from the statistics provided because it's the best we can do."*
//!
//! So the work splits:
//!
//! - [`frame`] computes the **V1 reference frame** for a map — what a *full combo* play would have
//!   scored, in its three portions. It is exact, and it is checkable against osu!, which reports the
//!   legacy V1 total for every stable-era play. On a perfect full combo the frame reconstructs that
//!   number with no estimator involved.
//! - The conversion on top infers the combo shape from the play's own max combo and accuracy, and is
//!   therefore an estimate for anything that is not a perfect FC.
//!
//! **It is not readable from osu!'s API, which is why it must be an estimate.** Measured: every
//! endpoint reachable for a stable-era play reports the *legacy* value and nothing else, and
//! `/api/v2/scores/{id}` returns nothing at all for a stable-era id. The owner's decision
//! (2026-09-22) is to port it, label it, and validate the half that can be validated.
//!
//! # Where the algorithms come from
//!
//! Ported from lazer at `master`, and the functions are laid out in its order so each value can be
//! checked against the C# line by line:
//!
//! ```text
//! osu.Game.Rulesets.Osu/Difficulty/OsuLegacyScoreSimulator.cs      the frame
//! osu.Game/Rulesets/Scoring/Legacy/LegacyScoreAttributes.cs        its shape
//! osu.Game/Database/StandardisedScoreMigrationTools.cs            the conversion
//! osu.Game.Rulesets.Osu/Scoring/OsuScoreMultiplierCalculatorV2.cs the standardised multipliers
//! ```
//!
//! One piece is **not** ported: the peppy-star score multiplier comes from `rosu-pp`'s public
//! `OsuDifficultyAttributes::legacy_score_base_multiplier`, which is the same number computed with no
//! mods applied — exactly what lazer's simulator uses for its own reference frame.

// The frame is built and pinned by tests, but nothing *in production* calls it yet: the conversion
// that consumes it is the next piece of 5a. Module-wide rather than item-by-item so that wiring the
// conversion deletes one line instead of five — and named here so it cannot be mistaken for a
// permanent exemption.
#![allow(dead_code)]

use rosu_map::Beatmap;
use rosu_map::section::hit_objects::{
    HitObjectKind, SliderEvent, SliderEventType, SliderEventsIter,
};

/// The V1 values a beatmap can produce, over a **full combo** play.
///
/// The three portions are separate because the conversion treats them differently: the accuracy
/// portion is scaled by the play's accuracy, the combo portion is rescaled onto the standardised
/// curve, and the bonus portion is carried across at its ratio.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Frame {
    /// The plain sum of every object's base value — 300 for a circle, 30 for a slider's head, tail
    /// and each repeat, 10 for each tick. Exact, and independent of how the play went.
    pub accuracy_score: i64,
    /// The combo-multiplied portion, as a maximum. The conversion multiplies this by the legacy mod
    /// multiplier after rounding, which is why it is stored unmultiplied here.
    pub combo_score: i64,
    /// The bonus portion: spinner ticks, worth 100 and 1100 each.
    pub bonus_score: i64,
    /// `standardised_bonus / legacy_bonus`, or 0 when the map has no spinners. The conversion needs
    /// the ratio rather than the standardised total.
    pub bonus_ratio: f64,
    /// The combo a full combo would reach.
    pub max_combo: i32,
}

/// osu!'s per-mod score multiplier, which scales the three portions into a V1 total.
///
/// Lazer keeps this as `GetLegacyScoreMultiplier` on the simulator rather than in a table, because a
/// few values depend on the mod's *settings*: `DT` at anything other than 1.5× carries a penalty, and
/// `NF` and `HR` are worth more under ScoreV2. `score_v2` here means the `SV2` mod, which is what
/// lazer branches on.
pub fn legacy_multiplier(mods: &[String], score_v2: bool) -> f64 {
    let mut multiplier = 1.0;

    for acronym in mods {
        multiplier *= match acronym.as_str() {
            "NF" => {
                if score_v2 {
                    1.0
                } else {
                    0.5
                }
            }
            "EZ" => 0.5,
            "HT" | "DC" => 0.3,
            "HD" => 1.06,
            "HR" => {
                if score_v2 {
                    1.10
                } else {
                    1.06
                }
            }
            "DT" | "NC" => {
                if score_v2 {
                    1.20
                } else {
                    1.12
                }
            }
            "FL" => 1.12,
            "SO" => 0.9,
            // Neither of these can be scored at all, and lazer returns immediately rather than
            // continuing to multiply: a play with either mod has a multiplier of exactly zero.
            "RX" | "AP" => return 0.0,
            _ => 1.0,
        };
    }

    multiplier
}

/// A map's V1 reference frame.
///
/// `score_multiplier` is the peppy-star multiplier, which `rosu-pp` exposes as
/// `OsuDifficultyAttributes::legacy_score_base_multiplier` and computes with **no mods applied** —
/// lazer's own comment on that choice is *"Apparently, this is how lazer wants to it /shrug"*.
///
/// The map is taken by `&mut` because generating a slider's ticks needs its curve, which is built
/// lazily.
pub fn frame(map: &mut Beatmap, score_multiplier: f64) -> Frame {
    // Read the map-level values first: the walk below needs `&mut map.hit_objects`, so nothing may
    // borrow the map while it runs.
    let mut frame = Frame::default();
    let mut combo: i32 = 0;
    let mut standardised_bonus: i64 = 0;
    let mut ticks: Vec<SliderEvent> = Vec::new();
    let (slider_multiplier, slider_tick_rate, format_version) = (
        map.slider_multiplier,
        map.slider_tick_rate,
        map.format_version,
    );

    // The timing and difficulty points are looked up per object, from the immutable map, before the
    // mutable walk. Two passes rather than one borrow fight.
    //
    // The fall-backs are rosu-map's own defaults for a map with no control point at that time: a
    // beat length of `60_000 / 60` ms (one beat at 60 BPM) and a slider velocity of 1.0 with ticks
    // generated. Spelled out rather than named because rosu-map keeps those constants behind a
    // private module, and they are two numbers rather than a table.
    let timings: Vec<(f64, f64, bool)> = map
        .hit_objects
        .iter()
        .map(|object| {
            let beat_len = map
                .control_points
                .timing_point_at(object.start_time)
                .map_or(60_000.0 / 60.0, |point| point.beat_len);
            let (velocity, generate) = map
                .control_points
                .difficulty_point_at(object.start_time)
                .map_or((1.0, true), |point| {
                    (point.slider_velocity, point.generate_ticks)
                });
            (beat_len, velocity, generate)
        })
        .collect();

    for (object, (beat_len, slider_velocity, generate_ticks)) in
        map.hit_objects.iter_mut().zip(timings)
    {
        match &mut object.kind {
            HitObjectKind::Circle(_) => {
                add_combo_score(&mut frame, combo, 300, score_multiplier);
                frame.accuracy_score += 300;
                combo += 1;
            }
            HitObjectKind::Slider(slider) => {
                // Nested objects are judged before the slider itself, and each of them extends the
                // combo without being multiplied by it — which is why their count matters to every
                // object after them.
                let velocity = 100.0 * slider_multiplier
                    / precision_adjusted_beat_len(slider_velocity, beat_len);
                // A slider tick is a *spatial* interval: the distance is derived from the beat
                // length and the map's tick rate, then the events are generated along the path.
                let mut tick_dist = velocity * beat_len / slider_tick_rate;
                if format_version < 8 {
                    tick_dist /= slider_velocity;
                }
                let span_count = slider.span_count();
                // Building the curve is what makes the path length available; it is cached inside
                // `SliderPath`, so asking twice would cost nothing, but it is asked once anyway.
                let path_dist = slider.path.curve().dist();
                let span_duration = path_dist / velocity;

                let events = SliderEventsIter::new(
                    object.start_time,
                    span_duration,
                    velocity,
                    if generate_ticks {
                        tick_dist
                    } else {
                        f64::INFINITY
                    },
                    path_dist,
                    span_count,
                    &mut ticks,
                )
                .filter(|event| event.kind == SliderEventType::Tick)
                .count() as i32;

                // head, tail and each repeat at 30, then every tick at 10.
                let big = 2 + slider.repeat_count;
                for index in 0..big + events {
                    frame.accuracy_score += if index < big { 30 } else { 10 };
                    combo += 1;
                }

                add_combo_score(&mut frame, combo, 300, score_multiplier);
                frame.accuracy_score += 300;
            }
            HitObjectKind::Spinner(spinner) => {
                // Lazer replays the spinner's own ticks here, with two constants standing in for the
                // worst case, because the real rotation count is not recorded anywhere:
                // *"this will have the final effect of slightly underestimating bonus score achieved
                // on stable when converting from score V1."*
                let seconds = spinner.duration / 1000.0;
                let possible = (seconds * (477.0 / 60.0) * 2.0) as i32;
                let required = (seconds * 3.0) as i32;
                let before_bonus = required + 3;

                for i in 0..=possible {
                    if i > before_bonus && (i - before_bonus) % 2 == 0 {
                        frame.bonus_score += 1100;
                        standardised_bonus += 50;
                    } else if i > 1 && i % 2 == 0 {
                        frame.bonus_score += 100;
                        standardised_bonus += 10;
                    }
                }

                add_combo_score(&mut frame, combo, 300, score_multiplier);
                frame.accuracy_score += 300;
                combo += 1;
            }
            // A hold note belongs to osu!mania and never reaches this, because the frame is built per
            // ruleset and this is the osu! one.
            HitObjectKind::Hold(_) => {}
        }
    }

    frame.bonus_ratio = if frame.bonus_score == 0 {
        0.0
    } else {
        standardised_bonus as f64 / frame.bonus_score as f64
    };
    frame.max_combo = combo;
    frame
}

/// One object's contribution to the combo portion.
///
/// The `scoreIncrease / 25` is **integer division in lazer, on purpose**, with the comment
/// *"intentional to match osu-stable"* — so it is integer division here too, and the truncation to
/// `i64` at the end matches C#'s cast.
fn add_combo_score(frame: &mut Frame, combo: i32, score_increase: i32, score_multiplier: f64) {
    let factor = f64::from(combo.max(0).saturating_sub(1)) * f64::from(score_increase / 25);
    frame.combo_score += (factor * score_multiplier) as i64;
}

/// osu!'s beat-length adjustment for the map's slider velocity.
///
/// Ported rather than called: `rosu-pp` has exactly this function, but its `util` module is private.
/// The `f32` round-trip is lazer's, and the 10–10,000 clamp is what stops a negative slider velocity
/// from producing a nonsense multiplier.
fn precision_adjusted_beat_len(slider_velocity: f64, beat_len: f64) -> f64 {
    let as_beat_len = -100.0 / slider_velocity;

    let multiplier = if as_beat_len < 0.0 {
        f64::from(((-as_beat_len) as f32).clamp(10.0, 10_000.0)) / 100.0
    } else {
        1.0
    };

    beat_len * multiplier
}

/// osu!'s standardised (V2) score multiplier, which is **not** the legacy one.
///
/// Verified by reading `OsuScoreMultiplierCalculatorV2`: `HR` is 1.09 rather than 1.06, `DT` is 1.23
/// rather than 1.12, `HT` is 0.55 rather than 0.3, `SO` is 0.95, and `RX`/`AP` are 0.1 rather than
/// zero. The conversion always takes V2, because it calls `CreateScoreMultiplierCalculator` with no
/// score and that only returns V1 below `TotalScoreVersion` `30000017`.
///
/// `rate` is the `speed_change` setting for `DT`/`NC`/`HT`/`DC` when the file recorded one, and the
/// mod's default otherwise — a stable-era `.osr` carries no settings at all, and stable only ever
/// produced the default rates.
///
/// Returns `None` for a mod whose multiplier depends on a setting that is not available, so the caller
/// stores no score rather than a wrong one. Nothing reachable from a stable-era file does that today;
/// it is here so that adding one cannot silently produce a wrong number.
pub fn standardised_multiplier(acronyms: &[String], rate: Option<f64>) -> Option<f64> {
    let mut multiplier = 1.0;
    let has = |name: &str| acronyms.iter().any(|acronym| acronym == name);

    for acronym in acronyms {
        multiplier *= match acronym.as_str() {
            "EZ" => 0.8,
            "NF" => 0.5,
            "HT" | "DC" => half_time_multiplier(rate.unwrap_or(0.75)),
            "HR" => 1.09,
            "DT" | "NC" => double_time_multiplier(rate.unwrap_or(1.5)),
            // Hidden is worth less when another mod already gives away the timing.
            "HD" => {
                let timing_told = ["WG", "GR", "DF", "RP", "DP"].iter().any(|name| has(name));
                1.04 - if timing_told { 0.02 } else { 0.0 }
            }
            "FL" => 1.2,
            "SO" => 0.95,
            "RX" | "AP" => 0.1,
            // Everything else osu! defines at 1.0, including `SD`, `PF`, `TD`, `V2` and the mania key
            // mods, which osu! does not use. A `CL` reaching here would be a bug rather than a mod:
            // Classic is *inferred* for display and is not part of the file's own mod list.
            _ => 1.0,
        };
    }

    Some(multiplier)
}

/// `0.55` at the default 0.75×: `(int)(speed · 20) / 20 · 1.4 − 0.5`.
fn half_time_multiplier(speed_change: f64) -> f64 {
    (speed_change * 20.0) as i32 as f64 / 20.0 * 1.4 - 0.5
}

/// `1.23` at the default 1.5×: `(floor(speed · 10) / 10 − 1) · 0.46 + 1`, less `0.01` for a rate
/// other than 1.0× or 1.5×.
fn double_time_multiplier(speed_change: f64) -> f64 {
    let value = (speed_change * 10.0) as i32 as f64 / 10.0;
    let penalty = if value == 1.5 || value == 1.0 {
        0.0
    } else {
        0.01
    };

    (value - 1.0) * 0.46 + 1.0 - penalty
}

/// The play's mods as acronyms, from whichever source the file has.
///
/// A lazer-era play's blob names them completely, in osu!'s own order. A stable-era play has no blob,
/// so its mods are the legacy bitfield, expanded in **bit order** — which is the order osu! displays
/// them in, and neither source gives it: `rosu-mods` spells a Hidden+DoubleTime play `DTHD`, and
/// osu!'s own API returned `HR` before `HD` on one replay.
///
/// `CL` is deliberately **not** added here. Stable *is* Classic, but the mod is not in the file, and
/// this list feeds the multiplier — where Classic would scale every stable-era play by 0.96 or 0.985
/// for a mod the client never applied. Whether osu!'s own conversion includes it is a separate,
/// still-open question; it is not assumed either way.
pub fn acronyms(play: &crate::pp::Play) -> Vec<String> {
    let mut names: Vec<String> = if play.mods_names.is_empty() {
        BITS.iter()
            .filter(|(bit, _)| play.mods & bit != 0)
            .map(|(_, acronym)| (*acronym).to_owned())
            .collect()
    } else {
        play.mods_names.clone()
    };

    // osu! carries the mod it implies as well — Nightcore is DoubleTime and Perfect is SuddenDeath —
    // but shows and returns only the stronger one, so the implied mod leaves the set.
    if names.iter().any(|name| name == NC) {
        names.retain(|name| name != DT);
    }
    if names.iter().any(|name| name == PF) {
        names.retain(|name| name != SD);
    }

    // The bitflag order first, then anything this table has never heard of — lazer keeps adding mods,
    // and dropping an unknown one would make a badge quietly wrong.
    let mut ordered: Vec<String> = BITS
        .iter()
        .filter(|(_, acronym)| names.iter().any(|name| name == acronym))
        .map(|(_, acronym)| (*acronym).to_owned())
        .collect();
    for name in &names {
        if !BITS.iter().any(|(_, acronym)| acronym == name) {
            ordered.push(name.clone());
        }
    }

    ordered
}

/// The legacy mod bits and their acronyms, **in bit order**.
const SD: &str = "SD";
const DT: &str = "DT";
const NC: &str = "NC";
const PF: &str = "PF";

const BITS: [(i32, &str); 31] = [
    (1 << 0, "NF"),
    (1 << 1, "EZ"),
    (1 << 2, "TD"),
    (1 << 3, "HD"),
    (1 << 4, "HR"),
    (1 << 5, SD),
    (1 << 6, DT),
    (1 << 7, "RX"),
    (1 << 8, "HT"),
    (1 << 9, NC),
    (1 << 10, "FL"),
    (1 << 11, "AT"),
    (1 << 12, "SO"),
    (1 << 13, "AP"),
    (1 << 14, PF),
    (1 << 15, "4K"),
    (1 << 16, "5K"),
    (1 << 17, "6K"),
    (1 << 18, "7K"),
    (1 << 19, "8K"),
    (1 << 20, "FI"),
    (1 << 21, "RD"),
    (1 << 22, "CM"),
    (1 << 23, "TP"),
    (1 << 24, "9K"),
    (1 << 25, "CO"),
    (1 << 26, "1K"),
    (1 << 27, "3K"),
    (1 << 28, "2K"),
    (1 << 29, "V2"),
    (1 << 30, "MR"),
];

/// `ScoreProcessor.COMBO_EXPONENT`, read from lazer's source rather than inferred from the shape of
/// the formula. osu! is the only ruleset that uses 0.5.
const COMBO_EXPONENT: f64 = 0.5;

/// osu!'s conversion from a recorded V1 total to the standardised score osu! shows today.
///
/// Only one step infers anything, and it is the reason this whole function is an estimate: the V1
/// total is split into the part the accuracy explains and the part combo and bonus explain, and that
/// remainder is rescaled onto the standardised curve by **two** estimates — one from the play's
/// longest combo, one spreading the remainder evenly over the objects that give combo — combined 30/70
/// and capped at 1.2× their mean. Everything else is arithmetic on numbers the map or the `.osr`
/// supplies, which is why the frame half of this is checkable and this half is not.
///
/// `rate` is the `DT`/`HT` speed setting when the file recorded one. Returns `None` when a mod's
/// multiplier cannot be resolved, so a play gets **no** score rather than a wrong one.
pub fn convert_osu(
    frame: &Frame,
    v1_total: i64,
    accuracy: f64,
    max_combo: u32,
    misses: u32,
    acronyms: &[String],
    rate: Option<f64>,
) -> Option<i64> {
    let score_v2 = acronyms.iter().any(|acronym| acronym == "V2");
    let legacy = legacy_multiplier(acronyms, score_v2);
    let standardised = standardised_multiplier(acronyms, rate)?;

    // The accuracy portion of the play is known exactly: the frame's maximum, scaled by the play's
    // accuracy. What is *not* known is how the rest of the V1 total was distributed, which is the
    // inference below.
    let max_combo_score = (frame.combo_score as f64 * legacy).round() as i64;
    let accuracy_score = frame.accuracy_score as f64 * accuracy;

    let combo_proportion = if max_combo_score + frame.bonus_score > 0 {
        // The combo and bonus portions cannot be separated, so the bonus stays in the ratio.
        (v1_total as f64 - accuracy_score).max(0.0) / (max_combo_score + frame.bonus_score) as f64
    } else if legacy == 0.0 {
        0.0
    } else {
        1.0
    };

    // The bonus is whatever exceeded what the map could otherwise produce.
    let maximum_legacy_base = frame.accuracy_score + max_combo_score;
    let bonus_proportion = ((v1_total - maximum_legacy_base) as f64 * frame.bonus_ratio).max(0.0);
    let accuracy_part = 500_000.0 * accuracy.powi(5);

    let without_mods = if max_combo == 0 || accuracy == 0.0 {
        // Nothing to distribute: a play with no combo or no accuracy has no combo portion at all.
        (accuracy_part + bonus_proportion).round()
    } else if max_combo_score + frame.bonus_score == 0 {
        // No combo score to convert means the map has no combo-giving objects, or the mods zeroed the
        // multiplier. Either way the combo proportion stands in directly.
        (500_000.0 * combo_proportion + accuracy_part + bonus_proportion).round()
    } else {
        let maximum_legacy_combo = f64::from(frame.max_combo);
        // The maximum *magnitude* of the combo portion, on both curves. Only ratios of these are used,
        // which is why the constants drop out.
        let maximum_v1 = maximum_legacy_combo.powi(2);
        let maximum_std = maximum_legacy_combo.powf(1.0 + COMBO_EXPONENT);

        let longest_v1 = f64::from(max_combo).powi(2);
        let longest_std = f64::from(max_combo).powf(1.0 + COMBO_EXPONENT);

        // How much the play's combo portion is worth in V1 terms. Dividing by accuracy lessens the
        // impact of accuracy on it, and the clamp from below covers near-FC plays whose accuracy fell
        // off at the end.
        let combo_v1 = (maximum_v1 * combo_proportion / accuracy).max(longest_v1);

        // Estimate one: repeat the longest combo as often as it fits.
        let occurrences = (combo_v1 / longest_v1).floor();
        let remaining_v1 = combo_v1 - occurrences * longest_v1;
        let score_based =
            occurrences * longest_std + remaining_v1.sqrt().powf(1.0 + COMBO_EXPONENT);

        // Estimate two: spread the remainder evenly over the remaining objects that give combo.
        // Assuming `n` equal combos of length `x`, the remainder is `n·x²` and the object count is
        // `n·x`, so dividing gives `x` directly.
        let remaining_objects = maximum_legacy_combo - f64::from(max_combo) - f64::from(misses);
        let length = if remaining_objects > 0.0 {
            (combo_v1 - longest_v1) / remaining_objects
        } else {
            0.0
        };
        let object_based = longest_std + remaining_objects * length.powf(COMBO_EXPONENT);

        let score_based = score_based.clamp(0.0, maximum_std);
        let object_based = object_based.clamp(0.0, maximum_std);
        let lower = score_based.min(object_based);
        let upper = score_based.max(object_based);
        let estimated = (0.3 * lower + 0.7 * upper).min(1.2 * (lower + upper) / 2.0);

        (500_000.0 * (estimated / maximum_std) * accuracy + accuracy_part + bonus_proportion)
            .round()
    };

    // Lazer throws here rather than storing a negative total, and refusing is the same instinct.
    if without_mods < 0.0 {
        return None;
    }

    Some((without_mods * standardised).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A map with one circle, one slider with a tick, and a spinner — enough to exercise every arm of
    /// the walk and the combo bookkeeping between them.
    fn sample() -> Beatmap {
        let text = "\
osu file format v14
[General]
Mode: 0
[TimingPoints]
1000,500,4,2,0,100,1,0
[Difficulty]
SliderMultiplier:1.4
SliderTickRate:1
[Metadata]
Title:test
Artist:test
Creator:test
Version:test
BeatmapID:1
BeatmapSetID:2
[HitObjects]
100,100,1000,1,0,0:0:0:0:
100,100,2000,2,0,B|200:200,1,140,0|0,0:0|0:0,0:0:0:0:
256,192,4000,12,0,6000,0:0:0:0:
";
        Beatmap::from_bytes(text.as_bytes()).expect("parses")
    }

    /// The frame's shape, with the multiplier at 1 so the arithmetic is readable: one circle is 300,
    /// a slider's head, tick and tail are 30 + 10 + 30 plus the slider's own 300, and a spinner's base
    /// is 300.
    #[test]
    fn the_frame_adds_up_in_lazers_order() {
        let mut map = sample();
        let frame = frame(&mut map, 1.0);

        // 300 circle + (30 head + 30 tail + 300 slider) + 300 spinner. The slider is 140 px at a
        // 140 px tick distance, so its only tick would land on the tail and the tail leniency drops
        // it — the tick arm is exercised by the longer slider below instead.
        assert_eq!(frame.accuracy_score, 300 + 360 + 300);
        // Combo is 4: the circle, the slider's head and tail (the slider itself adds none, because
        // its head already did), and the spinner. A map's combo counts nested parts, not objects.
        assert_eq!(frame.max_combo, 4);
        // The spinner earns bonus points, which is why the ratio exists at all.
        assert!(frame.bonus_score > 0, "the spinner paid no bonus");
        assert!(frame.bonus_ratio > 0.0 && frame.bonus_ratio < 1.0);
    }

    /// A slider long enough to carry ticks: they add 10 to the accuracy portion and one to the combo
    /// each, which is why a map's tick count moves every object after it.
    #[test]
    fn a_long_slider_generates_ticks() {
        let text = "\
osu file format v14
[General]
Mode: 0
[TimingPoints]
1000,500,4,2,0,100,1,0
[Difficulty]
SliderMultiplier:1.4
SliderTickRate:1
[Metadata]
Title:test
Artist:test
Creator:test
Version:test
BeatmapID:1
BeatmapSetID:2
[HitObjects]
100,100,1000,1,0,0:0:0:0:
100,100,2000,2,0,B|200:200,1,700,0|0,0:0|0:0,0:0:0:0:
";
        let mut map = Beatmap::from_bytes(text.as_bytes()).expect("parses");
        let frame = frame(&mut map, 1.0);
        println!(
            "\n  long slider: accuracy {}  combo {}  bonus {}  ratio {}",
            frame.accuracy_score, frame.max_combo, frame.bonus_score, frame.bonus_ratio
        );

        // The tick distance is 140 px, so a 700 px path carries ticks at 140, 280, 420 and 560 — the
        // fifth would land exactly on the tail, which the tail leniency drops. Measured, not assumed:
        // 300 (circle) + 30 head + 4 x 10 ticks + 30 tail + 300 (slider) = 700, and a combo of 7.
        assert_eq!(frame.accuracy_score, 700);
        assert_eq!(frame.max_combo, 7);
        // A map with no spinner pays no bonus and divides by nothing.
        assert_eq!(frame.bonus_score, 0);
        assert_eq!(frame.bonus_ratio, 0.0);
    }

    /// A map with nothing to multiply still gets a combo portion when its combo is long enough, and
    /// the multiplier scales that portion — never the accuracy portion, which lazer adds afterwards
    /// and unmultiplied.
    #[test]
    fn the_multiplier_scales_only_the_combo_portion() {
        let mut map = sample();
        let plain = frame(&mut map, 1.0);
        let doubled = frame(&mut map, 2.0);

        assert_eq!(plain.accuracy_score, doubled.accuracy_score);
        assert_eq!(plain.bonus_score, doubled.bonus_score);
        assert!(doubled.combo_score > plain.combo_score);
    }

    /// A perfect full combo is the one case where the inference has nothing to infer, and the answer
    /// is therefore checkable: osu!'s standardised score is 500,000 of combo plus 500,000 of accuracy,
    /// so a perfect play lands at 1,000,000 and only the bonus can push it above.
    #[test]
    fn a_perfect_full_combo_converts_to_a_full_score() {
        let mut map = sample();
        let frame = frame(&mut map, 1.0);

        // The V1 total a perfect play on this map would have recorded.
        let v1_total = frame.accuracy_score + frame.combo_score + frame.bonus_score;
        let converted = convert_osu(&frame, v1_total, 1.0, frame.max_combo as u32, 0, &[], None)
            .expect("a nomod play converts");

        assert!(
            (1_000_000..1_100_000).contains(&converted),
            "a perfect play should land at the standardised ceiling, got {converted}"
        );

        // And a worse play on the same map must score less. This is the property that would break
        // first if the combo-shape estimate were inverted.
        let worse = convert_osu(
            &frame,
            v1_total * 8 / 10,
            0.95,
            frame.max_combo as u32 - 1,
            0,
            &[],
            None,
        )
        .expect("converts");
        assert!(worse < converted, "{worse} should be below {converted}");
    }

    /// The standardised multiplier is applied on top, so the same play scores higher with DoubleTime
    /// — which is the whole reason that table had to be read from source rather than assumed.
    #[test]
    fn double_time_scores_higher_than_nomod() {
        let mut map = sample();
        let frame = frame(&mut map, 1.0);
        let v1_total = frame.accuracy_score + frame.combo_score + frame.bonus_score;

        let nomod = convert_osu(&frame, v1_total, 1.0, frame.max_combo as u32, 0, &[], None)
            .expect("converts");
        let dt = convert_osu(
            &frame,
            v1_total,
            1.0,
            frame.max_combo as u32,
            0,
            &["DT".to_owned()],
            None,
        )
        .expect("converts");

        assert!(dt > nomod, "DT {dt} should beat nomod {nomod}");
    }

    /// `HD` alone is 1.04, and less when another mod already gives away the timing.
    #[test]
    fn hidden_is_worth_less_when_another_mod_tells_the_timing() {
        let alone = standardised_multiplier(&["HD".to_owned()], None).expect("resolves");
        let with_wiggle =
            standardised_multiplier(&["HD".to_owned(), "WG".to_owned()], None).expect("resolves");

        assert!(
            (alone - 1.04).abs() < 1e-12,
            "HD alone should be 1.04, got {alone}"
        );
        assert!(with_wiggle < alone, "HD with Wiggle should be worth less");
    }

    /// The rate-dependent multipliers, which are the only reason `speed_change` had to be captured
    /// for pp in step 4a and are needed a second time here.
    #[test]
    fn double_time_and_half_time_follow_the_played_rate() {
        // Defaults: 1.23 at 1.5x, 0.55 at 0.75x.
        assert!((double_time_multiplier(1.5) - 1.23).abs() < 1e-12);
        assert!((half_time_multiplier(0.75) - 0.55).abs() < 1e-12);
        // A non-default rate carries a 0.01 penalty.
        assert!(double_time_multiplier(1.4) < 1.23);
        // 1.0x is explicitly not penalised.
        assert!((double_time_multiplier(1.0) - 1.0).abs() < 1e-12);
    }

    /// A play that cannot earn anything: Relax and Autopilot zero the multiplier outright rather than
    /// scaling it, which is lazer returning early instead of continuing to multiply.
    #[test]
    fn relax_and_autopilot_zero_the_legacy_multiplier() {
        assert_eq!(
            legacy_multiplier(&["HD".to_owned(), "DT".to_owned()], false),
            1.06 * 1.12
        );
        assert_eq!(legacy_multiplier(&["NF".to_owned()], false), 0.5);
        assert_eq!(legacy_multiplier(&["NF".to_owned()], true), 1.0);
        assert_eq!(
            legacy_multiplier(&["RX".to_owned(), "HD".to_owned()], false),
            0.0
        );
        assert_eq!(legacy_multiplier(&["AP".to_owned()], false), 0.0);
    }
}
