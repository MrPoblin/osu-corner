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
    hit_samples::{HitSampleDefaultName, HitSampleInfo, HitSampleInfoName},
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

/// Which ruleset a play is in. The variants carry osu!'s own ruleset ids, which is also the order of
/// the `switch` in `StandardisedScoreMigrationTools.convertFromLegacyTotalScore`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Osu = 0,
    Taiko = 1,
    Catch = 2,
    Mania = 3,
}

/// osu!'s per-mod V1 score multiplier, which scales the three portions into a V1 total.
///
/// Each ruleset has its **own** table — lazer keeps these on each `ILegacyScoreSimulator` rather than
/// in one shared place — and the differences are not cosmetic: catch's `DT` is 1.06 where osu!'s is
/// 1.12, catch's `HR` is 1.12 where osu!'s is 1.06, and mania's `HT` is 0.5 where the others' is 0.3.
/// Reading one ruleset's table and using it for all four would be wrong four different ways.
///
/// A few values depend on the mod's *settings*: `NF`, `HR` and `DT` are worth more under ScoreV2.
/// `score_v2` here means the `SV2` mod, which is what lazer branches on.
pub fn legacy_multiplier(mode: Mode, mods: &[String], score_v2: bool) -> f64 {
    let mut multiplier = 1.0;

    for acronym in mods {
        multiplier *= match acronym.as_str() {
            // NoFail is 0.5 everywhere except under ScoreV2, and Easy is 0.5 everywhere.
            "NF" => {
                if score_v2 {
                    1.0
                } else {
                    0.5
                }
            }
            "EZ" => 0.5,
            // Mania halves for HalfTime where the other three use 0.3.
            "HT" | "DC" => {
                if mode == Mode::Mania {
                    0.5
                } else {
                    0.3
                }
            }
            // Catch drops Hidden to 1.0 under ScoreV2; the others do not.
            "HD" => {
                if mode == Mode::Catch && score_v2 {
                    1.0
                } else {
                    1.06
                }
            }
            // **Catch and osu! swap these two**: catch's HardRock is the 1.12 and its DoubleTime the
            // 1.06, the reverse of osu!, taiko and mania.
            "HR" => {
                if mode == Mode::Catch {
                    1.12
                } else if mode == Mode::Osu && score_v2 {
                    1.10
                } else {
                    1.06
                }
            }
            "DT" | "NC" => {
                if mode == Mode::Catch {
                    1.06
                } else if mode == Mode::Osu && score_v2 {
                    1.20
                } else {
                    1.12
                }
            }
            "FL" => 1.12,
            // SpunOut is osu!-only, so it is a no-op in the other three tables.
            "SO" => {
                if mode == Mode::Osu {
                    0.9
                } else {
                    1.0
                }
            }
            // The two unscoreable mods, which are osu!-only as well. Lazer returns *immediately*
            // rather than continuing to multiply, so the multiplier is exactly zero and not a product.
            "RX" | "AP" => {
                if mode == Mode::Osu {
                    return 0.0;
                }
                1.0
            }
            // Catch is the only one of the four whose Relax is unscoreable, and it returns 0 like osu!'s.
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
                    / precision_adjusted_beat_len(slider_velocity, beat_len, 1_000.0);
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
                add_osu_spinner(
                    &mut frame,
                    &mut standardised_bonus,
                    &mut combo,
                    spinner.duration,
                    score_multiplier,
                );
            }
            // A hold note is osu!mania's encoding of `IHasDuration`, and lazer's converter turns any
            // such object into **this** ruleset's duration object — for osu! that is a spinner. So a
            // hold reaches this frame only on a convert, and it is scored as a spinner rather than
            // skipped.
            HitObjectKind::Hold(hold) => {
                add_osu_spinner(
                    &mut frame,
                    &mut standardised_bonus,
                    &mut combo,
                    hold.duration,
                    score_multiplier,
                );
            }
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
/// The precision-adjusted beat length, used by every legacy ruleset to place slider nodes — and the
/// clamp really does differ per ruleset: **osu! and catch clamp at 1000, taiko and mania at 10000**
/// (`LegacyRulesetExtensions.GetPrecisionAdjustedBeatLength` switches on the ruleset's short name,
/// with `fruits` sharing osu!'s arm). `rosu-pp` has a single copy with the 10000 clamp, so a port that
/// borrowed it for every mode would be wrong for osu! and catch on any map with a slider velocity
/// multiplier below 0.1 — rare, but silently wrong rather than loudly.
fn precision_adjusted_beat_len(slider_velocity: f64, beat_len: f64, highest: f64) -> f64 {
    let as_beat_len = -100.0 / slider_velocity;

    let multiplier = if as_beat_len < 0.0 {
        f64::from(((-as_beat_len) as f32).clamp(10.0, highest as f32)) / 100.0
    } else {
        1.0
    };

    beat_len * multiplier
}

/// The standardised mod multiplier: one function per ruleset, because lazer has one calculator per
/// ruleset and they genuinely disagree.
///
/// **The migration always takes V2**, whichever ruleset it is. `StandardisedScoreMigrationTools` calls
/// `ruleset.CreateScoreMultiplierCalculator(new ScoreMultiplierContext(difficulty))` — note the
/// missing second argument — and `OsuRuleset` only returns the V1 calculator when
/// `context.Score != null && TotalScoreVersion < 30000017`. Passing no score means V2 unconditionally,
/// which is measured rather than assumed: 324 of this library's lazer-era replays record both the
/// pre-mod and the final score, and their ratio is the multiplier their client applied. **162 of
/// those 324 matched the V1 table and every one of the 324 matches V2** — `DT` 1.230000 across 103
/// plays, `HR` 1.090000 across 26, and `HDDT` 1.279200 across 12. The V1 table produced no real score
/// in this library at all.
///
/// Two things are deliberately *not* modelled, because a converted play cannot reach them:
///
/// - **Combinations.** V2 defines several — `HD` beside `WG`/`GR`/`RP`/`DP` is worth 0.02 less, `HD`
///   beside `BL` a flat 1.24 — but every one pairs Hidden with a mod that has no legacy bit, so no
///   stable-era file can express it.
/// - **Settings.** A stable-era file records none, and the migration passes no score, so lazer builds
///   each mod at its default: `EZ` is 0.8 rather than the `max(0.4, 0.8 − 0.1·retries)` a score would
///   pick, and osu!'s `DA` has nothing to adjust against.
///
/// `Classic` is the exception worth naming. It is **not in the file** — osu!'s importer appends it,
/// which `acronyms` does too — and each ruleset prices it differently: osu! at `0.985` when note lock
/// is on and `0.96` otherwise, while the other three return **1.0** whenever no score is passed, which
/// is exactly the migration's case. `0.985` is measured, not read: the two plays §5 recorded produce
/// implied multipliers of `1.04 · 1.23 · 0.985` and `1.23 · 0.985`, to six decimal places.
pub fn standardised_multiplier(mode: Mode, acronyms: &[String]) -> f64 {
    acronyms
        .iter()
        .map(|acronym| match mode {
            Mode::Osu => osu_mod(acronym),
            Mode::Taiko => taiko_mod(acronym),
            Mode::Catch => catch_mod(acronym),
            Mode::Mania => mania_mod(acronym),
        })
        .product()
}

/// `OsuScoreMultiplierCalculatorV2`, with each mod at its default configuration.
fn osu_mod(acronym: &str) -> f64 {
    match acronym {
        "NF" => 0.5,
        // 0.8x base, less 0.1x per extra retry — at the default retry count it is 0.8.
        "EZ" => 0.8,
        "HT" | "DC" => half_time_v2(),
        "HR" => 1.09,
        "DT" | "NC" => double_time_v2(),
        "HD" => 1.04,
        "FL" => 1.2,
        "SO" => 0.95,
        "RX" | "AP" => 0.1,
        "TR" => 1.02,
        "BL" => 1.24,
        "TP" => 0.01,
        "DA" => 0.5,
        "RD" => 0.7,
        "AD" => 0.7,
        // The note-lock branch, which is the one a stable-era play takes: stable had note lock, so the
        // `Classic` its importer appends carries it.
        "CL" => 0.985,
        _ => 1.0,
    }
}

/// `TaikoScoreMultiplierCalculator`. Note that taiko **kept the old rate curve** — its `DT` is 1.1
/// where osu!'s is 1.23 — and that its `Classic` needs a score, so it is 1.0 here.
fn taiko_mod(acronym: &str) -> f64 {
    match acronym {
        "NF" | "EZ" => 0.5,
        "HT" | "DC" => half_time_v1(),
        "SR" => 0.6,
        "HR" | "HD" => 1.06,
        "DT" | "NC" => double_time_v1(),
        "FL" => 1.12,
        "DA" => 0.5,
        "CS" => 0.9,
        "RX" => 0.1,
        "WU" | "WD" | "AS" => 0.5,
        _ => 1.0,
    }
}

/// `CatchScoreMultiplierCalculator`, which is the one that swaps Hidden and HardRock.
fn catch_mod(acronym: &str) -> f64 {
    match acronym {
        "NF" | "EZ" => 0.5,
        "HT" | "DC" => half_time_v1(),
        "HR" => 1.12,
        "DT" | "NC" => double_time_v1(),
        "HD" => 1.06,
        "FL" => 1.12,
        "DA" => 0.5,
        "RX" => 0.1,
        "SY" => 0.8,
        "WU" | "WD" | "AS" => 0.5,
        _ => 1.0,
    }
}

/// `ManiaScoreMultiplierCalculator`. Half of osu!'s list is commented out here: mania has **no**
/// HardRock, DoubleTime, Nightcore, Hidden or Flashlight multiplier at all, which is the single
/// largest trap in the four tables.
fn mania_mod(acronym: &str) -> f64 {
    match acronym {
        "NF" | "EZ" => 0.5,
        "HT" | "DC" => half_time_v1(),
        "NR" => 0.9,
        "DA" => 0.5,
        "CS" => 0.9,
        "HO" => 0.9,
        // The key mods read the score's client version to choose 1.0 or 0.9, and fall back to 0.9 when
        // there is no score — which is the migration's case.
        "1K" | "2K" | "3K" | "4K" | "5K" | "6K" | "7K" | "8K" | "9K" => 0.9,
        "WU" | "WD" | "AS" => 0.5,
        _ => 1.0,
    }
}

/// The V2 rate curve, which only osu! uses: `1.23` at 1.5× and `0.55` at 0.75×.
fn double_time_v2() -> f64 {
    let value = (1.5 * 10.0) as i32 as f64 / 10.0;
    (value - 1.0) * 0.46 + 1.0
}

fn half_time_v2() -> f64 {
    (0.75 * 20.0) as i32 as f64 / 20.0 * 1.4 - 0.5
}

/// The older curve taiko, catch and mania kept: `1.1` at 1.5× and `0.3` at 0.75×. The truncation is
/// lazer's own: it rounds the rate *down* to a multiple of 0.1 before offsetting it, which is why
/// 0.75 becomes 0.7 and the result is 0.3 rather than `0.6 + (0.75 − 1)` = 0.35.
fn double_time_v1() -> f64 {
    let value = (1.5 * 10.0) as i32 as f64 / 10.0 - 1.0;
    1.0 + value / 5.0
}

fn half_time_v1() -> f64 {
    let value = (0.75 * 10.0) as i32 as f64 / 10.0 - 1.0;
    0.6 + value
}

/// The play's mods as acronyms, from whichever source the file has.
///
/// A lazer-era play's blob names them completely, in osu!'s own order. A stable-era play has no blob,
/// so its mods are the legacy bitfield, expanded in **bit order** — which is the order osu! displays
/// them in, and neither source gives it: `rosu-mods` spells a Hidden+DoubleTime play `DTHD`, and
/// osu!'s own API returned `HR` before `HD` on one replay.
///
/// `CL` is deliberately **not** added here. Stable *is* Classic, but the mod is not in the file, and
/// `CL` **is** added for a stable-era play, because osu!'s own importer adds it: `LegacyScoreDecoder`
/// appends `ModClassic` whenever the file's version predates lazer, and `StandardisedScoreMigrationTools`
/// multiplies the multiplier over *that* list — so every stable-era converted score genuinely carries
/// the 0.96. It is **not** added to the V1 side, because lazer's `GetLegacyScoreMultiplier` has no
/// `ModClassic` case: Classic scales the standardised step only. That asymmetry is why the two
/// multiplier functions disagree about a mod the file never recorded, and it is not a bug in either.
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

    // Stable *is* Classic, and osu!'s importer says so explicitly rather than inferring it, so the
    // acronym is part of this list rather than a display-only suffix. A lazer-era play already carries
    // its own `CL` in the blob when it was played with the mod, so only the missing one is added.
    if !play.lazer() && !ordered.iter().any(|name| name == CL) {
        ordered.push(CL.to_owned());
    }

    ordered
}

/// The legacy mod bits and their acronyms, **in bit order**.
const CL: &str = "CL";
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
/// `rate` is not a parameter: a stable-era file cannot express one, and a lazer-era play is never
/// converted, so the pinned commit's default rates are the only reachable ones.
/// Which ruleset a map's own file declares.
pub fn mode_of_map(map: &Beatmap) -> Mode {
    match map.mode {
        rosu_map::section::general::GameMode::Taiko => Mode::Taiko,
        rosu_map::section::general::GameMode::Catch => Mode::Catch,
        rosu_map::section::general::GameMode::Mania => Mode::Mania,
        _ => Mode::Osu,
    }
}

/// Which ruleset a `.osr`'s mode byte declares — the play's mode, which for a *convert* differs from
/// the map's.
pub const fn mode_of_byte(mode: u8) -> Mode {
    match mode {
        1 => Mode::Taiko,
        2 => Mode::Catch,
        3 => Mode::Mania,
        _ => Mode::Osu,
    }
}

/// A map's V1 reference frame, for the ruleset whose plays are being converted.
///
/// **The frame always describes the map the file holds, converted by whichever ruleset's converter the
/// play used** — and that is not the same thing for all four. osu!, catch and mania's converters are
/// 1:1 (a circle is a circle, a slider a slider, `IHasDuration` whatever that ruleset calls a
/// duration), so those frames read the file's objects directly and a convert needs no special case.
/// **taiko is the exception**: its converter can split one slider into a run of hit circles, which
/// changes how many scoring objects the map has, so `frame_taiko` looks at the map's own mode.
///
/// The peppy-star multiplier is computed here for all four modes rather than read from `rosu-pp`. It
/// is the same helper in every simulator's preamble (`CalculateDifficultyPeppyStars`), and `rosu-pp`
/// exposes it for osu!standard alone — so taking it from there for one mode and computing it for the
/// other three would be two sources for one number. That the two agree is checked by the frame gate,
/// which is tight enough on osu!standard's 7,580 stable-era plays to move if the multiplier is off by
/// even one.
///
/// `is_convert` is passed in because only taiko needs it, and only the caller knows which ruleset the
/// play was in.
pub fn frame_for(map: &mut Beatmap, mode: Mode) -> Frame {
    let (object_count, drain) = object_count_and_drain_length(map);
    let peppy_of = |cs: f64| {
        peppy_stars(
            f64::from(map.hp_drain_rate),
            f64::from(map.overall_difficulty),
            cs,
            object_count,
            drain,
        )
    };
    let peppy = peppy_of(f64::from(map.circle_size));

    match mode {
        Mode::Osu => frame(map, f64::from(peppy)),
        // **taiko forces `CircleSize = 2` before deriving the peppy stars.** Stable's
        // `HitObjectManagerTaiko` did exactly that and lazer reproduces it (#38203), so a taiko play's
        // legacy multiplier is derived from a beatmap whose circle size is 2 rather than the map's own
        // — which differs on nearly every map, since 2 is not a typical taiko `CircleSize`.
        Mode::Taiko => frame_taiko(map, peppy_of(2.0)),
        Mode::Catch => frame_catch(map, f64::from(peppy)),
        // mania's frame is the same four constants for every map, which is why this arm ignores the
        // values above entirely.
        Mode::Mania => frame_mania(),
    }
}

/// The column count lazer gives a map, which decides how many columns a **mania convert** is played in
/// and — through `GetLegacyScoreMultiplier` — part of its V1 multiplier besides.
///
/// Only a convert reaches this: a native mania map's column count is just its circle size, and the
/// multiplier returns before any of the rest. What is left is a chain of composition-and-OD rules that
/// osu!stable arrived at and lazer reproduces exactly, so they are ported rather than reasoned about.
///
/// `total_objects` and `end_time_objects` are the *source* map's: how many objects it has, and how many
/// of them are `IHasDuration` — sliders, spinners and holds.
pub fn mania_columns(cs: f64, od: f64, total_objects: i32, end_time_objects: i32) -> i32 {
    // `Math.Round`, so banker's rounding, as everywhere else lazer rounds.
    let rounded_cs = cs.round_ties_even();
    let rounded_od = od.round_ties_even();

    if total_objects > 0 && end_time_objects >= 0 {
        let percent_special_objects = f64::from(end_time_objects) / f64::from(total_objects);

        if percent_special_objects < 0.2 {
            return 7;
        }
        if percent_special_objects < 0.3 || rounded_cs >= 5.0 {
            return if rounded_od > 5.0 { 7 } else { 6 };
        }
        if percent_special_objects > 0.6 {
            return if rounded_od > 4.0 { 5 } else { 4 };
        }
    }

    ((rounded_od as i32) + 1).clamp(4, 7)
}

/// The **convert-only** part of mania's V1 multiplier: a play whose column count the *mod* chose rather
/// than the map is worth 10% less, and a further 4% less for every column it lost against the map.
///
/// Returns 1.0 when nothing changed, which is the common case and the reason the factor is a function
/// rather than a `if` at the call site.
pub fn mania_column_factor(original: i32, actual: i32) -> f64 {
    if actual > original {
        0.9
    } else if actual < original {
        0.9 - 0.04 * f64::from(original - actual)
    } else {
        1.0
    }
}

/// The play's own numbers, as opposed to the map's — everything the reassembly reads from a score
/// rather than from a beatmap.
#[derive(Debug, Clone, Copy)]
pub struct Achieved {
    /// The `.osr`'s recorded total, which for a stable-era play is the V1 one.
    pub v1_total: i64,
    pub accuracy: f64,
    pub max_combo: u32,
    pub misses: u32,
    /// **Catch only.** Lazer derives `fruitTinyScale` from the tiny-droplet maximum against the fruit
    /// maximum, and the droplets-hit ratio from the tiny droplets actually hit. A legacy score records
    /// no separate maxima, so `PopulateMaximumStatistics` derives them by summing every basic result
    /// into `Great` and every small tick into `SmallTickHit` — which for a catch `.osr` is
    /// `(count50 + countkatu, count300 + countmiss)`. The other three rulesets ignore both.
    pub tiny_droplets: (u32, u32),
    pub fruits_max: u32,
}

/// osu!'s conversion from a recorded V1 total to the standardised score osu! shows today, for any of
/// the four rulesets.
///
/// Only one step infers anything, and it is the reason a converted score is an estimate rather than a
/// computation: the V1 total is split into the part the accuracy explains and the part combo and bonus
/// explain, and that remainder is rescaled onto the standardised curve. osu! does that with **two**
/// estimates — one from the play's longest combo, one spreading the remainder evenly over the objects
/// that give combo — combined 30/70 and capped at 1.2× their mean, and only osu! uses them: taiko,
/// catch and mania reassemble straight from `comboProportion`. Everything else is arithmetic on numbers
/// the map or the `.osr` supplies, which is why the frame half of this is checkable and this half is
/// not.
///
/// **A play with no misses has a determined combo shape**, so the estimator is switched off on it and
/// the result is exact — which is what makes §5's two recorded plays usable as a verification of the
/// whole chain rather than of the arithmetic alone.
///
/// `legacy` and `standardised` are passed in rather than resolved here so that the conversion can be
/// asked what a play *would* have scored under a different multiplier — which is how the multiplier
/// table was measured against real scores (§8).
pub fn convert(
    mode: Mode,
    frame: &Frame,
    achieved: &Achieved,
    legacy: f64,
    standardised: f64,
) -> Option<i64> {
    let (v1_total, accuracy, max_combo, misses) = (
        achieved.v1_total,
        achieved.accuracy,
        achieved.max_combo,
        achieved.misses,
    );

    // The accuracy portion of the play is known exactly: the frame's maximum, scaled by the play's
    // accuracy.
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

    let without_mods = match mode {
        Mode::Osu => {
            if max_combo == 0 || accuracy == 0.0 {
                // Nothing to distribute: a play with no combo or no accuracy has no combo portion.
                (accuracy_part + bonus_proportion).round()
            } else if max_combo_score + frame.bonus_score == 0 {
                // No combo score to convert means the map has no combo-giving objects, or the mods
                // zeroed the multiplier. Either way the combo proportion stands in directly.
                (500_000.0 * combo_proportion + accuracy_part + bonus_proportion).round()
            } else {
                let maximum_legacy_combo = f64::from(frame.max_combo);
                // The maximum *magnitude* of the combo portion, on both curves. Only ratios of these
                // are used, which is why the constants drop out.
                let maximum_v1 = maximum_legacy_combo.powi(2);
                let maximum_std = maximum_legacy_combo.powf(1.0 + COMBO_EXPONENT);

                let longest_v1 = f64::from(max_combo).powi(2);
                let longest_std = f64::from(max_combo).powf(1.0 + COMBO_EXPONENT);

                // How much the play's combo portion is worth in V1 terms. Dividing by accuracy
                // lessens the impact of accuracy on it, and the clamp from below covers near-FC plays
                // whose accuracy fell off at the end.
                let combo_v1 = (maximum_v1 * combo_proportion / accuracy).max(longest_v1);

                // Estimate one: repeat the longest combo as often as it fits.
                let occurrences = (combo_v1 / longest_v1).floor();
                let remaining_v1 = combo_v1 - occurrences * longest_v1;
                let score_based =
                    occurrences * longest_std + remaining_v1.sqrt().powf(1.0 + COMBO_EXPONENT);

                // Estimate two: spread the remainder evenly over the remaining objects that give
                // combo. Assuming `n` equal combos of length `x`, the remainder is `n·x²` and the
                // object count is `n·x`, so dividing gives `x` directly.
                let remaining_objects =
                    maximum_legacy_combo - f64::from(max_combo) - f64::from(misses);
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

                (500_000.0 * (estimated / maximum_std) * accuracy
                    + accuracy_part
                    + bonus_proportion)
                    .round()
            }
        }
        // Taiko and mania reassemble straight from the combo proportion — neither has an estimator,
        // which makes them the two modes where a converted score is closest to a computation.
        Mode::Taiko => {
            (250_000.0 * combo_proportion + 750_000.0 * accuracy.powf(3.6) + bonus_proportion)
                .round()
        }
        Mode::Mania => {
            // **The coefficients are the other way round at the pinned commit** — `850000 ×
            // comboProportion + 150000 × accuracy^…` there, and this way round today. The current one
            // is used because the server migrates with whatever lazer it is running; nothing local can
            // check it, because no mania play in this library has an osu!-side converted total (§8).
            (150_000.0 * combo_proportion
                + 850_000.0 * accuracy.powf(2.0 + 2.0 * accuracy)
                + bonus_proportion)
                .round()
        }
        Mode::Catch => {
            // Lazer's own note on this arm: for a *stable* score the fruit maximum ends up including
            // large droplets, because stable counts a missed droplet as a plain miss and there is no
            // separate slot to put it in. It calls that unfixable without a dedicated legacy
            // attribute, and acceptable because the whole conversion is a ballpark; the port inherits
            // both judgements rather than inventing a correction osu! itself does not make.
            let (tiny_hit, tiny_max) = achieved.tiny_droplets;
            let divisor = tiny_max + achieved.fruits_max;
            let fruit_tiny_scale = if divisor == 0 {
                0.0
            } else {
                f64::from(tiny_max) / f64::from(divisor)
            };

            const MAX_TINY_DROPLETS_PORTION: f64 = 400_000.0;

            let catch_combo_portion = 1_000_000.0 - MAX_TINY_DROPLETS_PORTION
                + MAX_TINY_DROPLETS_PORTION * (1.0 - fruit_tiny_scale);
            let droplets_portion = MAX_TINY_DROPLETS_PORTION * fruit_tiny_scale;
            let droplets_hit = if tiny_max == 0 {
                0.0
            } else {
                f64::from(tiny_hit) / f64::from(tiny_max)
            };

            (catch_combo_portion
                * estimate_combo_proportion_for_catch(
                    frame.max_combo,
                    max_combo as i32,
                    misses as i32,
                )
                + droplets_portion * droplets_hit
                + bonus_proportion)
                .round()
        }
    };

    // Lazer throws here rather than storing a negative total, and refusing is the same instinct.
    if without_mods < 0.0 {
        return None;
    }

    Some((without_mods * standardised).round() as i64)
}

/// Catch's own combo-shape estimate, ported verbatim. **It is deliberately not the general method:**
/// in stable ScoreV1 catch's progression is quadratic while the standardised score is logarithmic up
/// to 200 combo and linear after, so linearly rescaling the V1 combo portion *"leads to horribly
/// underestimating it"*. This instead computes the best case for the combo the player is known to have
/// hit with the remaining misses spread evenly over the rest of the objects that give combo — which
/// makes it a worst case for the player, and therefore an upper estimate.
fn estimate_combo_proportion_for_catch(
    beatmap_max_combo: i32,
    score_max_combo: i32,
    score_miss_count: i32,
) -> f64 {
    /// The score a combo of `max_combo` earns at best, as `∫ log₄(t) dt` in three pieces, because the
    /// standardised catch curve changes shape at 2 combo and again at 200.
    fn best_case_combo_total(max_combo: i32) -> f64 {
        if max_combo == 0 {
            return 1.0;
        }

        let mut estimated = 0.5 * f64::from(max_combo.min(2));

        if max_combo <= 2 {
            return estimated;
        }

        let upto_200 = max_combo.min(200);
        estimated += (f64::from(upto_200) * (f64::from(upto_200).ln() - 1.0) + 2.0 - 4.0_f64.ln())
            / 4.0_f64.ln();

        if max_combo <= 200 {
            return estimated;
        }

        estimated += f64::from(max_combo - 200) * 200.0_f64.ln() / 4.0_f64.ln();
        estimated
    }

    /// How much a combo of this length is worth after a miss. Lazer calls it pessimistic, because it
    /// may subtract too much when the miss happened before reaching 200 combo.
    fn dropped_combo_score_after_miss(length_of_combo_after_miss: i32) -> f64 {
        let length = length_of_combo_after_miss.min(200);
        f64::from(length) * (1.0 + 200.0_f64.ln() - f64::from(length).ln()) / 4.0_f64.ln()
    }

    if beatmap_max_combo == 0 {
        return 1.0;
    }
    if score_max_combo == 0 {
        return 0.0;
    }
    if beatmap_max_combo == score_max_combo {
        return 1.0;
    }

    let estimated_best_case_total = best_case_combo_total(beatmap_max_combo);

    let mut remaining_combo = beatmap_max_combo - (score_max_combo + score_miss_count);
    let mut total_dropped_score = 0.0;

    // `score_miss_count` cannot be zero here: the equal-combo case returned above.
    let assumed_length = (f64::from(remaining_combo) / f64::from(score_miss_count)).floor() as i32;

    if assumed_length > 0 {
        let assumed_combos =
            (f64::from(remaining_combo) / f64::from(assumed_length)).floor() as i32;
        total_dropped_score +=
            f64::from(assumed_combos) * dropped_combo_score_after_miss(assumed_length);

        remaining_combo -= assumed_combos * assumed_length;

        if remaining_combo > 0 {
            total_dropped_score += dropped_combo_score_after_miss(remaining_combo);
        }
    } else {
        // So many misses that dividing the remaining combo evenly gives zero length per combo, i.e.
        // every remaining judgement breaks combo. Presume they all missed and gave nothing.
        total_dropped_score = estimated_best_case_total - best_case_combo_total(score_max_combo);
    }

    if estimated_best_case_total == 0.0 {
        1.0
    } else {
        1.0 - (total_dropped_score / estimated_best_case_total).clamp(0.0, 1.0)
    }
}

/// osu!standard's conversion, with both multipliers resolved from the play's own acronyms.
pub fn convert_osu(
    frame: &Frame,
    v1_total: i64,
    accuracy: f64,
    max_combo: u32,
    misses: u32,
    acronyms: &[String],
) -> Option<i64> {
    let score_v2 = acronyms.iter().any(|acronym| acronym == "V2");

    convert(
        Mode::Osu,
        frame,
        &Achieved {
            v1_total,
            accuracy,
            max_combo,
            misses,
            tiny_droplets: (0, 0),
            fruits_max: 0,
        },
        legacy_multiplier(Mode::Osu, acronyms, score_v2),
        standardised_multiplier(Mode::Osu, acronyms),
    )
}

/// `LegacyRulesetExtensions.CalculateDifficultyPeppyStars`, which every simulator's preamble calls.
///
/// Lazer uses C#'s 128-bit `decimal` here to emulate the x87 registers stable's float arithmetic
/// actually ran on, because on a significant number of beatmaps the rounding would otherwise land on
/// the wrong integer — its own comment names *one* ranked map in the whole game that still flips.
/// `rosu-pp` deliberately does not reproduce that (*"we use f64 instead of C#'s decimal type for
/// simplicity reasons and sacrifice precision while doing so"*) and neither does this.
///
/// The rounding is `Math.Round`'s, i.e. banker's rounding — `round_ties_even`, not `round`.
fn peppy_stars(hp: f64, od: f64, cs: f64, object_count: i32, drain_length: i32) -> i32 {
    let object_to_drain_ratio = if drain_length != 0 {
        (f64::from(object_count) / f64::from(drain_length) * 8.0).clamp(0.0, 16.0)
    } else {
        16.0
    };

    ((hp + od + cs + object_to_drain_ratio) / 38.0 * 5.0).round_ties_even() as i32
}

/// The two numbers a simulator's preamble derives from the **base** beatmap: how many objects it has
/// and how long it drains for. `rosu-pp` exposes neither, and both feed the peppy-star multiplier.
///
/// A break's length is rounded *before* it is summed, which is lazer's own ordering.
fn object_count_and_drain_length(map: &Beatmap) -> (i32, i32) {
    let (Some(first), Some(last)) = (map.hit_objects.first(), map.hit_objects.last()) else {
        return (0, 0);
    };

    let break_length: i32 = map
        .breaks
        .iter()
        .map(|period| (period.end_time.round() - period.start_time.round()) as i32)
        .sum();

    let drain =
        (last.start_time.round() as i32 - first.start_time.round() as i32 - break_length) / 1000;

    (map.hit_objects.len() as i32, drain)
}

/// **mania's V1 reference frame, which needs no map at all.**
///
/// `ManiaLegacyScoreSimulator.Simulate` is four constants: a combo score of exactly `1000000` and
/// everything else zero — including `MaxCombo = 0`, with lazer's own comment *"Max combo is
/// mod-dependent, so any value here is insufficient."* Under V1 mania is already capped at a million,
/// which is why `comboProportion` collapses to `legacyTotalScore / (1000000 · multiplier)` and mania
/// is the one mode whose conversion needs nothing from the beatmap.
///
/// The zero `bonus_ratio` is not a placeholder: mania has no bonus results at all, so the ratio really
/// is zero and `bonusProportion` really is zero.
pub fn frame_mania() -> Frame {
    Frame {
        accuracy_score: 0,
        combo_score: 1_000_000,
        bonus_score: 0,
        bonus_ratio: 0.0,
        max_combo: 0,
    }
}

/// One spinner's worth of osu! scoring, by duration alone.
///
/// Lazer replays the spinner's own ticks with two constants standing in for the worst case, because
/// the real rotation count is not recorded anywhere: *"this will have the final effect of slightly
/// underestimating bonus score achieved on stable when converting from score V1."*
///
/// Shared with `Hold` rather than duplicated, because a mania hold note **is** an `IHasDuration` and
/// lazer's converter gives this ruleset a spinner for one — so the two differ only in where the
/// duration comes from.
fn add_osu_spinner(
    frame: &mut Frame,
    standardised_bonus: &mut i64,
    combo: &mut i32,
    duration: f64,
    score_multiplier: f64,
) {
    let seconds = duration / 1000.0;
    let possible = (seconds * (477.0 / 60.0) * 2.0) as i32;
    let required = (seconds * 3.0) as i32;
    let before_bonus = required + 3;

    for i in 0..=possible {
        if i > before_bonus && (i - before_bonus) % 2 == 0 {
            frame.bonus_score += 1100;
            *standardised_bonus += 50;
        } else if i > 1 && i % 2 == 0 {
            frame.bonus_score += 100;
            *standardised_bonus += 10;
        }
    }

    add_combo_score(frame, *combo, 300, score_multiplier);
    frame.accuracy_score += 300;
    *combo += 1;
}

/// Lazer's taiko combo term: `scoreIncrease / 35 * 2 * (peppyStars + 1) * (min(100, combo) / 10)`.
///
/// Every division is deliberate. `min(100, combo) / 10` is the "combo bonus caps at 100" of §8 and
/// yields an integer 0–10, and `scoreIncrease / 35` happens *before* the multiplication, so a base
/// that does not divide evenly is truncated into the term rather than out of it.
fn taiko_combo_term(score_increase: i32, peppy: i32, combo: i32) -> i32 {
    score_increase / 35 * 2 * (peppy + 1) * (combo.min(100) / 10)
}

/// `(int)(value * 1.2f)` — lazer scales kiai in **single** precision and truncates, which is why this
/// is not a plain `f64` multiply.
fn kiai(value: i32) -> i32 {
    (value as f32 * 1.2) as i32
}

/// **taiko's V1 reference frame.**
///
/// A full-combo `TaikoLegacyScoreSimulator.Simulate`, which is the unusual one of the four: its combo
/// term is a *capped, integer* function of the running combo rather than a multiplier applied to a
/// finished total, and it needs the map's kiai sections and each object's `Finish` hitsound — `IsStrong`
/// in lazer is exactly *"has a `Finish` sample"*, and a strong object doubles its score.
///
/// Three arms, and two of them pay bonus rather than accuracy:
///
/// - **`Hit`** — 300 of accuracy, the combo term, and one combo.
/// - **`DrumRoll`** — nothing itself, but its ticks pay 300 of *bonus* each, scaled by kiai at the
///   **roll's** start and by 20% more if the roll is strong. Ticks never give combo. The tick
///   *spacing* is stable's (`getSliderTaikoMinHitDelay`), not the modern client's nested-object
///   generator — see the note on `min_hit_delay` below.
/// - **`Swell`** — 300 of bonus plus its combo term, doubled, and `n + 1` nested ticks of 300 bonus
///   each where `n` comes from the duration. Swells never give combo either, so a taiko map's maximum
///   combo is exactly its number of `Hit` objects.
pub fn frame_taiko(map: &Beatmap, peppy: i32) -> Frame {
    let mut frame = Frame::default();
    let mut combo: i32 = 0;
    let mut standardised_bonus: i64 = 0;

    let slider_multiplier = map.slider_multiplier;
    let slider_tick_rate = map.slider_tick_rate;
    let format_version = map.format_version;

    // **`getSliderTaikoMinHitDelay`** — stable's tick spacing for a drum roll, which is *not* the
    // modern client's `DrumRoll.CreateNestedHitObjects`.
    //
    // This is the one place a legacy simulator is not a straight read of today's objects. The simulator
    // walks this loop itself rather than recursing into a roll's nested ticks, because converting a
    // stable-era play has to reproduce what **stable** scored: lazer replaced its own generator here
    // under the commit message *"not porting stable quirk"* (#38203), so taking the nested ticks would
    // count a roll the way a lazer play would be scored, not the way this play was.
    let min_hit_delay = |beat_len: f64| -> f64 {
        let mut max_rate = if format_version >= 8
            && (slider_tick_rate == 3.0 || slider_tick_rate == 6.0 || slider_tick_rate == 1.5)
        {
            beat_len / 6.0
        } else {
            beat_len / 8.0
        };

        // Stable's clamp to a plausible ms-per-tick range.
        while max_rate < 60.0 {
            max_rate *= 2.0;
        }
        while max_rate > 120.0 {
            max_rate /= 2.0;
        }

        max_rate
    };
    // A play on a map of **this** ruleset is native, and its sliders are always drum rolls. A convert
    // may have them split into runs of hit circles instead, which is the one place taiko's converter
    // changes how many scoring objects a file produces.
    let is_native = mode_of_map(map) == Mode::Taiko;

    /// Everything about one object that the scoring pass reads, resolved up front.
    ///
    /// Resolved in a **single immutable pass** rather than during the walk, because the walk needs no
    /// mutable access at all: a roll's duration comes from the `.osu`'s own stated path length, so the
    /// lazily-built slider curve is never needed here (unlike osu!standard's frame).
    enum Plan {
        Hit {
            start: f64,
            kiai: bool,
        },
        Roll {
            start: f64,
            end: f64,
            min_hit_delay: f64,
            kiai: bool,
        },
        Swell {
            start: f64,
            ticks: i64,
            kiai_at_end: bool,
        },
    }

    // Every variant carries its own start time so that the roll pass below can ask for the *next*
    // object's without knowing which kind it is.
    fn plan_start(plan: &Plan) -> f64 {
        match plan {
            Plan::Hit { start, .. } | Plan::Roll { start, .. } | Plan::Swell { start, .. } => {
                *start
            }
        }
    }

    // A swell's ticks are derived from its duration alone, so a hold — mania's `IHasDuration`, which
    // taiko's converter turns into a swell — shares this exactly.
    let swell = |start: f64, duration: f64| -> Plan {
        let mut half_spins = (duration / 1000.0 * 7.5) as i32;
        half_spins = 1f32.max(half_spins as f32 * 1.65) as i32;
        half_spins = 1.max((half_spins as f32 * 1.5) as i32);

        let kiai_at_end = map
            .control_points
            .effect_point_at(start + duration)
            .is_some_and(|point| point.kiai);

        Plan::Swell {
            start,
            // `for (i = 0; i <= halfSpinsRequiredForCompletion; i++)`.
            ticks: i64::from(half_spins) + 1,
            kiai_at_end,
        }
    };

    let plans: Vec<(Plan, bool)> = map
        .hit_objects
        .iter()
        .flat_map(|object| {
            let strong = has_finish(&object.samples);
            let start = object.start_time;
            let beat_len = map
                .control_points
                .timing_point_at(start)
                .map_or(60_000.0 / 60.0, |point| point.beat_len);
            let kiai_at_start = map
                .control_points
                .effect_point_at(start)
                .is_some_and(|point| point.kiai);

            match &object.kind {
                HitObjectKind::Circle(_) => vec![(
                    Plan::Hit {
                        start,
                        kiai: kiai_at_start,
                    },
                    strong,
                )],
                HitObjectKind::Slider(slider) => {
                    // **The slider velocity comes from the difficulty point, not the slider.** Its own
                    // `velocity` field is already the *computed* velocity — rosu-map fills it with
                    // exactly `100 · SliderMultiplier / precisionAdjustedBeatLength`, which is the same
                    // number lazer's `DrumRoll` builds — so feeding it back in would apply the
                    // adjustment twice and put the tick count out by a factor of several.
                    let slider_velocity = map
                        .control_points
                        .difficulty_point_at(start)
                        .map_or(1.0, |point| point.slider_velocity);
                    let adjusted = precision_adjusted_beat_len(slider_velocity, beat_len, 10_000.0);

                    // `distance` and the roll's velocity, in lazer's own operation order.
                    let spans = f64::from(slider.span_count());
                    let distance = slider.path.expected_dist().unwrap_or(0.0)
                        * f64::from(TAIKO_VELOCITY_MULTIPLIER)
                        * spans;
                    let scoring_distance = f64::from(TAIKO_BASE_SCORING_DISTANCE)
                        * (slider_multiplier * f64::from(TAIKO_VELOCITY_MULTIPLIER))
                        / slider_tick_rate;
                    let taiko_velocity = scoring_distance * slider_tick_rate;
                    let duration = (distance / taiko_velocity * adjusted).floor();

                    // *"If the drum roll is to be split into hit circles, assume the ticks are 1/8
                    // spaced within the duration of one beat."* — and note `osuVelocity` is built from
                    // the **precision-adjusted** beat length, while the tick spacing then uses the raw
                    // one for any map from v8 on. Lazer's comment on that asymmetry: *"osu-stable
                    // always uses the speed-adjusted beatlength to determine the osu! velocity, but only
                    // uses it for conversion if beatmap version < 8"*.
                    let osu_velocity = taiko_velocity * (1000.0 / adjusted);
                    let convert_beat_len = if format_version >= 8 {
                        beat_len
                    } else {
                        adjusted
                    };
                    let tick_spacing = (convert_beat_len / slider_tick_rate).min(duration / spans);

                    // A native map never splits: `if (isForCurrentRuleset) { tickSpacing = 0; return
                    // false; }` is the first thing lazer does here.
                    let split = !is_native
                        && tick_spacing > 0.0
                        && distance / osu_velocity * 1000.0 < 2.0 * convert_beat_len;

                    if split {
                        // Each generated circle takes its samples from the slider's nodes in turn, so
                        // its `IsStrong` comes from that node rather than from the slider as a whole.
                        let nodes = if slider.node_samples.is_empty() {
                            vec![object.samples.clone()]
                        } else {
                            slider.node_samples.clone()
                        };

                        let mut hits = Vec::new();
                        let mut i = 0usize;
                        let mut time = start;
                        let end = start + duration + tick_spacing / 8.0;

                        while time <= end {
                            let samples = &nodes[i % nodes.len()];

                            hits.push((
                                Plan::Hit {
                                    start: time,
                                    kiai: kiai_at_start,
                                },
                                has_finish(samples),
                            ));

                            i += 1;
                            if tick_spacing.abs() < f64::EPSILON {
                                break;
                            }
                            time += tick_spacing;
                        }

                        return hits;
                    }

                    // The roll's tick count cannot be resolved here: stable drops the final tick when
                    // the **next** object follows too closely, so it needs its neighbour. Recorded as a
                    // span and counted in one pass below.
                    vec![(
                        Plan::Roll {
                            start,
                            end: start + duration,
                            min_hit_delay: min_hit_delay(beat_len),
                            kiai: kiai_at_start,
                        },
                        strong,
                    )]
                }
                // Lazer re-derives a swell's rotations from its duration rather than reading the map's
                // `RequiredHits`, and takes the worst case for the player: the minimum rotations, times
                // a 1.65 convert penalty, times 1.5 because it cannot know whether a rate mod was
                // active. Its own comment: *"This way, scores remain beatable at the cost of the
                // conversion being slightly inaccurate."*
                HitObjectKind::Spinner(spinner) => vec![(swell(start, spinner.duration), strong)],
                HitObjectKind::Hold(hold) => vec![(swell(start, hold.duration), strong)],
            }
        })
        .collect();

    // `HittableEndTime`: a roll's last tick is dropped when the next object arrives within one
    // min-hit-delay of the roll's own end. Only this rule needs a *neighbour* — and a roll that follows
    // a roll is measured against that roll's own delay rather than its start time — which is why the
    // tick count is resolved here rather than in the per-object mapping above.
    let roll_ticks: Vec<i32> = (0..plans.len())
        .map(|i| match &plans[i].0 {
            Plan::Roll {
                start,
                end,
                min_hit_delay,
                ..
            } => {
                let next_hittable_start = plans.get(i + 1).map(|(next, _)| match next {
                    Plan::Roll {
                        min_hit_delay: next_delay,
                        ..
                    } => plan_start(next) - *next_delay,
                    _ => plan_start(next),
                });

                let delay = min_hit_delay.trunc();
                let endpoint_hittable = match next_hittable_start {
                    None => true,
                    Some(next_start) => next_start - (*end + delay) > delay,
                };
                let hittable_end = if endpoint_hittable {
                    *end + delay
                } else {
                    *end
                };

                // `for (double i = StartTime; i < hittableEndTime; i += minHitDelay)`.
                let mut ticks = 0i32;
                let mut time = *start;

                while time < hittable_end {
                    ticks += 1;
                    time += min_hit_delay;
                }

                ticks
            }
            _ => 0,
        })
        .collect();

    for (i, (plan, strong)) in plans.into_iter().enumerate() {
        match plan {
            Plan::Hit { kiai: in_kiai, .. } => {
                // Lazer's order matters: the combo term is added, *then* kiai scales the whole thing,
                // *then* a strong object doubles both parts. Note `combo_increase` is the kiai'd total
                // minus the un-kiai'd 300, not a difference of two kiai'd values.
                let base = 300;
                let mut total = base + taiko_combo_term(base, peppy, combo);

                if in_kiai {
                    total = kiai(total);
                }

                let mut combo_increase = total - base;

                if strong {
                    total *= 2;
                    combo_increase *= 2;
                }

                frame.accuracy_score += i64::from(total - combo_increase);
                frame.combo_score += i64::from(combo_increase);
                combo += 1;
            }
            Plan::Roll { kiai: in_kiai, .. } => {
                let ticks = roll_ticks[i];
                // Applied **per tick**, and a tick passes through *both* strong clauses: the fifth more
                // that `DrumRollTick` gets of its own, and then the doubling every strongable object
                // takes — which a tick does, since `DrumRollTick : TaikoStrongableHitObject`. The
                // doubling normally splits across the combo and non-combo portions; a tick has no combo
                // portion, so all of it lands on the bonus.
                let mut per_tick = 300;

                if in_kiai {
                    per_tick = kiai(per_tick);
                }
                if strong {
                    per_tick += per_tick / 5;
                    per_tick *= 2;
                }

                frame.bonus_score += i64::from(per_tick * ticks);
                // `DrumRollTick`'s result is a `SmallBonus`, worth 10 of standardised bonus.
                standardised_bonus += 10 * i64::from(ticks);
            }
            Plan::Swell {
                ticks, kiai_at_end, ..
            } => {
                // The swell's own nested ticks pay legacy bonus only: `SwellTick`'s result is
                // `IgnoreHit`, whose base score is zero. So a swell adds standardised bonus *nothing at
                // all*, which is why a map whose only bonus objects are swells has a bonus ratio of
                // exactly zero.
                frame.bonus_score += 300 * ticks;
                // The ticks are `IgnoreHit` and pay nothing standardised — but the swell *itself* is a
                // `LargeBonus`, worth 50, and it lands in the ratio's numerator.
                standardised_bonus += 50;

                let base = 300;
                let mut total = base + taiko_combo_term(base, peppy, combo);

                if kiai_at_end {
                    total = kiai(total);
                }

                let mut combo_increase = total - base;

                // A swell is always doubled — that is what the "strong" arm of lazer's check means for
                // it — and it never gives combo.
                total *= 2;
                combo_increase *= 2;

                frame.bonus_score += i64::from(total - combo_increase);
                frame.combo_score += i64::from(combo_increase);
            }
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

/// `TaikoBeatmapConverter`'s two constants, which the roll duration is built from.
const TAIKO_VELOCITY_MULTIPLIER: f32 = 1.4;
const TAIKO_BASE_SCORING_DISTANCE: f32 = 100.0;

/// Catch's own combo term: `(int)(max(0, combo − 1) · (scoreIncrease / 25 · scoreMultiplier))`.
/// `scoreIncrease / 25` is integer division, lazer's own, and the cast truncates rather than rounds —
/// which together are what make catch's V1 progression quadratic in combo where the standardised
/// curve it is being converted onto is logarithmic.
fn catch_combo_term(combo: i32, score_increase: i32, score_multiplier: f64) -> i64 {
    let term =
        f64::from(i32::max(0, combo - 1)) * f64::from(score_increase / 25) * score_multiplier;

    term as i64
}

/// **catch's V1 reference frame.**
///
/// The mode whose object model is least like its source file's: a slider becomes a **juice stream**
/// whose head, tail and every repeat are *fruits* — worth 300 and a combo, and the only objects that
/// carry the combo multiplier — while the ticks between them are *droplets*, worth 100 and a combo
/// each, and the tiny droplets generated between events are worth 10 and no combo at all. So a catch
/// map's combo is `fruits + droplets`, which is where `CatchDifficultyAttributes::max_combo` comes
/// from, and the tiny droplets land only in the accuracy portion.
///
/// A spinner becomes a banana shower, and every banana is a `LargeBonus`: 1100 of legacy bonus against
/// 200 of standardised. Only the *ratio* of those two is used downstream, which is why the ratio is a
/// clean `200 / 1100` whenever there is a shower at all — but the legacy *count* is not free, because
/// it sits in the denominator of `comboProportion`, so the shower is counted.
pub fn frame_catch(map: &mut Beatmap, peppy: f64) -> Frame {
    let mut frame = Frame::default();
    let mut combo: i32 = 0;
    let mut standardised_bonus: i64 = 0;
    let mut ticks: Vec<SliderEvent> = Vec::new();

    let slider_multiplier = map.slider_multiplier;
    let slider_tick_rate = map.slider_tick_rate;

    let beat_lengths: Vec<(f64, f64)> = map
        .hit_objects
        .iter()
        .map(|object| {
            let beat_len = map
                .control_points
                .timing_point_at(object.start_time)
                .map_or(60_000.0 / 60.0, |point| point.beat_len);
            // **The slider velocity comes from the difficulty point.** rosu-map's `slider.velocity`
            // field is already the *computed* velocity, so feeding it back into
            // `precision_adjusted_beat_len` would apply the adjustment twice and shrink the tick
            // distance — which is exactly the bug this cross-check against `rosu-pp` caught.
            let slider_velocity = map
                .control_points
                .difficulty_point_at(object.start_time)
                .map_or(1.0, |point| point.slider_velocity);

            (beat_len, slider_velocity)
        })
        .collect();

    let format_version = map.format_version;

    for (idx, object) in map.hit_objects.iter_mut().enumerate() {
        let (beat_len, slider_velocity) = beat_lengths[idx];

        match &mut object.kind {
            HitObjectKind::Circle(_) => {
                frame.accuracy_score += 300;
                frame.combo_score += catch_combo_term(combo, 300, peppy);
                combo += 1;
            }
            HitObjectKind::Slider(slider) => {
                // Catch's velocity uses the `fruits` precision clamp — 10…1000, where lazer's
                // taiko and mania arms use 10…10000.
                let velocity = f64::from(CATCH_BASE_SCORING_DISTANCE) * slider_multiplier
                    / precision_adjusted_beat_len(slider_velocity, beat_len, 1_000.0);
                let path_length = slider.path.curve().dist();
                let span_count = slider.span_count();
                let span_duration = path_length / velocity;
                let mut tick_distance = velocity * beat_len / slider_tick_rate;

                // *"Prior to v8, speed multipliers don't adjust for how many ticks are generated over
                // the same distance. This results in more (or less) ticks being generated in <v8 maps
                // for the same time duration."* — lazer's own note on why this is here.
                if format_version < 8 {
                    tick_distance /= slider_velocity;
                }

                ticks.clear();
                let mut last_event: Option<(i32, f64)> = None;

                for event in SliderEventsIter::new(
                    object.start_time,
                    span_duration,
                    velocity,
                    tick_distance,
                    path_length,
                    span_count,
                    &mut ticks,
                ) {
                    // Lazer generates tiny droplets between consecutive events whose truncated times
                    // are more than 80 ms apart, halving a working interval until it is at most
                    // 100 ms. They pay 10 of accuracy and never give combo.
                    if let Some((last_time, _)) = last_event {
                        let since_last_tick = event.time as i32 - last_time;

                        if since_last_tick > 80 {
                            let mut time_between = f64::from(since_last_tick);
                            while time_between > 100.0 {
                                time_between /= 2.0;
                            }

                            let mut t = time_between;
                            while t < f64::from(since_last_tick) {
                                frame.accuracy_score += 10;
                                t += time_between;
                            }
                        }
                    }

                    last_event = Some((event.time as i32, event.path_progress));

                    match event.kind {
                        // The head, the tail and every repeat are all fruits.
                        SliderEventType::Head | SliderEventType::Tail | SliderEventType::Repeat => {
                            frame.accuracy_score += 300;
                            frame.combo_score += catch_combo_term(combo, 300, peppy);
                            combo += 1;
                        }
                        SliderEventType::Tick => {
                            frame.accuracy_score += 100;
                            combo += 1;
                        }
                        // Lazer's switch has no arm for the last tick, so it adds nothing at all.
                        SliderEventType::LastTick => {}
                    }
                }
            }
            HitObjectKind::Spinner(spinner) => {
                add_banana_shower(
                    &mut frame,
                    &mut standardised_bonus,
                    object.start_time,
                    spinner.duration,
                );
            }
            // A mania hold is an `IHasDuration` too, and catch's converter gives it this ruleset's
            // duration object — a banana shower.
            HitObjectKind::Hold(hold) => {
                add_banana_shower(
                    &mut frame,
                    &mut standardised_bonus,
                    object.start_time,
                    hold.duration,
                );
            }
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

/// One banana shower, which a mania hold also becomes on a convert.
///
/// Every banana is a `LargeBonus`: 1100 of legacy bonus against 200 of standardised, so the *ratio*
/// downstream is a clean `200 / 1100` whenever there is a shower at all — but the legacy count is not
/// free, because it sits in the denominator of `comboProportion`.
fn add_banana_shower(
    frame: &mut Frame,
    standardised_bonus: &mut i64,
    start_time: f64,
    duration: f64,
) {
    let count = banana_count(start_time, duration);

    frame.bonus_score += 1100 * count;
    *standardised_bonus += 200 * count;
}

/// `CatchHitObject`'s base scoring distance, which catch's velocity is built from.
const CATCH_BASE_SCORING_DISTANCE: f32 = 100.0;

/// How many bananas a banana shower generates. Lazer truncates both ends to `int`, halves a **single**
/// precision interval until it is at most 100 ms, and then steps inclusively — so the count is a
/// function of the duration alone.
fn banana_count(start_time: f64, duration: f64) -> i64 {
    let start = start_time as i32;
    let end = (start_time + duration) as i32;
    let mut spacing = duration as f32;

    while spacing > 100.0 {
        spacing /= 2.0;
    }

    if spacing <= 0.0 {
        return 0;
    }

    let mut count = 0i64;
    let mut time = start as f32;

    while time <= end as f32 {
        count += 1;
        time += spacing;
    }

    count
}

/// Lazer's `IsStrong` for a taiko object, which is exactly *"carries a `Finish` sample"* — the same
/// test the game uses to decide whether an object is a big note. Takes the samples rather than the
/// object because a split slider's generated circles each inherit one of the slider's **node** sample
/// lists, so strongness there is a property of a node, not of the file's object.
fn has_finish(samples: &[HitSampleInfo]) -> bool {
    samples.iter().any(|sample| {
        matches!(
            sample.name,
            HitSampleInfoName::Default(HitSampleDefaultName::Finish)
        )
    })
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
        let converted = convert_osu(&frame, v1_total, 1.0, frame.max_combo as u32, 0, &[])
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

        let nomod =
            convert_osu(&frame, v1_total, 1.0, frame.max_combo as u32, 0, &[]).expect("converts");
        let dt = convert_osu(
            &frame,
            v1_total,
            1.0,
            frame.max_combo as u32,
            0,
            &["DT".to_owned()],
        )
        .expect("converts");

        assert!(dt > nomod, "DT {dt} should beat nomod {nomod}");
    }

    /// The four tables disagree in ways that are easy to get backwards, and one of them is not a
    /// rounding difference: `DT` is worth 1.23 in osu! and 1.1 in the other three.
    #[test]
    fn the_four_rulesets_do_not_share_a_multiplier_table() {
        // osu!'s V2 curve against the older one the other three kept.
        assert!((double_time_v2() - 1.23).abs() < 1e-12);
        assert!((half_time_v2() - 0.55).abs() < 1e-12);
        assert!((double_time_v1() - 1.1).abs() < 1e-12);
        assert!((half_time_v1() - 0.3).abs() < 1e-12);

        let dt = |mode| standardised_multiplier(mode, &["DT".to_owned()]);
        assert!((dt(Mode::Osu) - 1.23).abs() < 1e-12, "osu! DT");
        assert!((dt(Mode::Catch) - 1.1).abs() < 1e-12, "catch DT");
        // Mania comments both rate mods out entirely, so DoubleTime is worth nothing at all — the
        // trap that a shared table walks straight into.
        assert_eq!(dt(Mode::Mania), 1.0);

        // Catch swaps the pair: its HardRock is the 1.12 and its osu! counterpart is 1.09.
        assert!((standardised_multiplier(Mode::Catch, &["HR".to_owned()]) - 1.12).abs() < 1e-12);
        assert!((standardised_multiplier(Mode::Osu, &["HR".to_owned()]) - 1.09).abs() < 1e-12);
        // Hidden is 1.04 for osu! and 1.06 for taiko and catch; mania has none.
        assert!((standardised_multiplier(Mode::Osu, &["HD".to_owned()]) - 1.04).abs() < 1e-12);
        assert_eq!(
            standardised_multiplier(Mode::Mania, &["HD".to_owned()]),
            1.0
        );
        // HardRock and Flashlight are 1.0 in mania; osu!'s Easy is 0.8 where everyone else's is 0.5.
        assert_eq!(
            standardised_multiplier(Mode::Mania, &["FL".to_owned()]),
            1.0
        );
        assert!((standardised_multiplier(Mode::Osu, &["EZ".to_owned()]) - 0.8).abs() < 1e-12);
        assert!((standardised_multiplier(Mode::Catch, &["EZ".to_owned()]) - 0.5).abs() < 1e-12);
    }

    /// Classic is appended to every stable-era play and is worth 0.985 in osu! only — the note-lock
    /// branch, because stable had note lock. The other three rulesets return 1.0 for it whenever no
    /// score is passed, which is the migration's case.
    #[test]
    fn classic_is_worth_985_in_osu_and_nothing_elsewhere() {
        let with_classic = |mode| standardised_multiplier(mode, &["CL".to_owned()]);

        assert!((with_classic(Mode::Osu) - 0.985).abs() < 1e-12);
        assert_eq!(with_classic(Mode::Taiko), 1.0);
        assert_eq!(with_classic(Mode::Catch), 1.0);
        assert_eq!(with_classic(Mode::Mania), 1.0);

        // And it scales the standardised product without touching the V1 one, because lazer's
        // `GetLegacyScoreMultiplier` has no `ModClassic` case at all.
        let both = standardised_multiplier(Mode::Osu, &["HD".to_owned(), "CL".to_owned()]);
        assert!((1.04 * 0.985 - both).abs() < 1e-12, "got {both}");
        assert_eq!(
            legacy_multiplier(Mode::Osu, &["HD".to_owned(), "CL".to_owned()], false),
            1.06
        );
    }

    /// A play that cannot earn anything: Relax and Autopilot zero the multiplier outright rather than
    /// scaling it, which is lazer returning early instead of continuing to multiply.
    #[test]
    fn relax_and_autopilot_zero_the_legacy_multiplier() {
        assert_eq!(
            legacy_multiplier(Mode::Osu, &["HD".to_owned(), "DT".to_owned()], false),
            1.06 * 1.12
        );
        assert_eq!(legacy_multiplier(Mode::Osu, &["NF".to_owned()], false), 0.5);
        assert_eq!(legacy_multiplier(Mode::Osu, &["NF".to_owned()], true), 1.0);
        assert_eq!(
            legacy_multiplier(Mode::Osu, &["RX".to_owned(), "HD".to_owned()], false),
            0.0
        );
        assert_eq!(legacy_multiplier(Mode::Osu, &["AP".to_owned()], false), 0.0);
    }
}
