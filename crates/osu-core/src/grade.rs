//! The letter a play earned.
//!
//! There is no single rule, because **the two clients grade differently and the site shows each
//! client's own answer**. `LegacyScoreDecoder` assigns the rank straight out of the file, so what
//! osu! displays for a play is whatever set it: osu!stable's rules for stable-era replays and
//! lazer's for lazer-era ones. Measured on the 100 best scores osu!'s API returned for the account
//! this library belongs to, stable's 300-proportion rule matched **100/100** while the accuracy
//! table matched 99 — and the single disagreement is the proof: a play with 91.2% 300s, no 50s and
//! no misses at 94.12% accuracy is an `S` to stable (and on the site) and an `A` by accuracy alone.
//!
//! ## osu!stable, per ruleset — the four wiki tables
//!
//! | ruleset | rule |
//! |---|---|
//! | osu! | top judgements and misses, with an at-most-1% 50s clause for `S` |
//! | osu!taiko | the same shape on GREATs, without the 50s clause |
//! | osu!catch | accuracy bands, `S` from 98.01% — an `S` is possible with misses |
//! | osu!mania | accuracy bands on `>` (`S` over 95%) — also possible with misses |
//!
//! Only the osu! row is validated against real data (100/100). The other three are the wiki's
//! tables, applied to 56 stable-era replays between them in this library.
//!
//! ## osu!lazer, per ruleset — one shared accuracy table plus a refinement
//!
//! Cutoffs `1 / 0.95 / 0.9 / 0.8 / 0.7`, then:
//!
//! | ruleset | refinement |
//! |---|---|
//! | osu!, osu!taiko | an `S` or `X` with at least one miss becomes `A` |
//! | osu!catch | none — its override is a verbatim copy of the table |
//! | osu!mania | an `S` stays `S` when anything was imperfect, and becomes `X` when nothing was |
//!
//! ## The accuracy must come from rosu-pp, never from here
//!
//! lazer's accuracy is not stable's — it counts slider hits where stable does not. Measured on 302
//! real lazer-era osu! replays carrying a stored letter, deriving that letter from a hand-written
//! classic accuracy matched 237 while stable's own rule matched 151, and 63 of the 65 failures were
//! lazer storing a *better* letter than the classic number allowed. So the caller passes rosu-pp's
//! accuracy (`*HitResults::accuracy()`, behind the same stable/lazer switch the pp path uses) and
//! this module only classifies it.
//!
//! ## Silver, and the one letter that cannot be derived
//!
//! **Silver counts.** osu!'s API returns `rank: "SH"` for a stable-era Hidden play, so the letter
//! stored for a play with HD, FL or FI is `SH`/`XH` rather than `S`/`X`. The caller passes that in,
//! because deciding it is a mod question and this module does not parse mods. Applying it is
//! idempotent: `SH` stays `SH`.
//!
//! **`F` is never derived.** Only a failed score has one, and no field in a `.osr` records failure
//! anywhere — it is only ever *read* from lazer's appended blob
//! ([`crate::osr::Header::stored_rank`]), which is the reason that field exists.

use serde::{Deserialize, Serialize};

/// What a play's letter is. Serializes as `"S"`, `"SH"`, `"X"`, … — the index stores the letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Rank {
    XH,
    X,
    SH,
    S,
    A,
    B,
    C,
    D,
    F,
}

/// The `.osr` ruleset byte, as [`crate::osr::Header::mode`] carries it.
const MODE_OSU: u8 = 0;
const MODE_TAIKO: u8 = 1;
const MODE_CATCH: u8 = 2;
const MODE_MANIA: u8 = 3;

/// `[count300, count100, count50, countGeki, countKatu, countMiss]` — misses are last in every mode.
const MISS: usize = 5;

/// The letter for a play.
///
/// `lazer` is the same switch the pp path derives from the `.osr` version — true for a replay set in
/// lazer (`version >= 30_000_001`), false for one stable set. `silver` is true when the mods include
/// Hidden, Flashlight or Fade In. An unrecognised ruleset byte gets the lazer table without a
/// refinement, or `D` for the stable era, since no stable table is known for it.
pub fn grade(lazer: bool, mode: u8, accuracy: f64, counts: &[u16; 6], silver: bool) -> Rank {
    let rank = if lazer {
        lazer_table(mode, accuracy, counts)
    } else {
        stable_grade(mode, accuracy, counts)
    };
    silvered(silver, rank)
}

/// lazer's letter for a play **whatever era it was set in** — the index's second letter.
///
/// For a lazer-era play this is the same answer [`grade`] gives, which is why the column costs
/// nothing there (measured: +46 bytes across 355 plays, §5). For a stable-era play it is what lazer
/// says about it, and lazer and stable disagree on 28% of this library's stable-era plays — osu!'s
/// site shows whichever one the score was migrated with (§16).
pub fn lazer_grade(mode: u8, accuracy: f64, counts: &[u16; 6], silver: bool) -> Rank {
    silvered(silver, lazer_table(mode, accuracy, counts))
}

/// Silver is the same letter drawn differently for HD, FL or FI, and osu!'s API returns `SH`, so it
/// belongs to the stored letter rather than to the UI (§16). Applying it to a letter that is already
/// silver is a no-op, which is what makes it safe to apply to one read out of a blob.
fn silvered(silver: bool, rank: Rank) -> Rank {
    match (silver, rank) {
        (true, Rank::X) => Rank::XH,
        (true, Rank::S) => Rank::SH,
        _ => rank,
    }
}

/// lazer: `ScoreProcessor.accuracy_cutoff_*`, verbatim, then the ruleset's refinement.
///
/// The comparison is `>=`, so **exactly 95.00% is an `S`** — the wiki says "over 95%", and the code
/// is what the site ends up showing.
fn lazer_table(mode: u8, accuracy: f64, counts: &[u16; 6]) -> Rank {
    let rank = if accuracy == 1.0 {
        Rank::X
    } else if accuracy >= 0.95 {
        Rank::S
    } else if accuracy >= 0.90 {
        Rank::A
    } else if accuracy >= 0.80 {
        Rank::B
    } else if accuracy >= 0.70 {
        Rank::C
    } else {
        Rank::D
    };

    match (mode, rank) {
        // osu! and osu!taiko are byte-identical: `case ScoreRank.S: case ScoreRank.X:` then
        // `if (results.GetValueOrDefault(HitResult.Miss) > 0) rank = ScoreRank.A;`.
        (MODE_OSU | MODE_TAIKO, Rank::S | Rank::X) if counts[MISS] > 0 => Rank::A,
        // osu!mania refines only `S`, promoting it when nothing was imperfect. Reaching this needs
        // an accuracy between 95% and 100% with no imperfect result, which lazer's own weighting
        // makes unusual — implemented because it is what lazer does, not because it fires often.
        (MODE_MANIA, Rank::S) if !any_imperfect(counts) => Rank::X,
        // osu!catch overrides with a copy of the table and adds nothing, so an `S` stands even with
        // misses. An unknown ruleset keeps the table's answer too.
        _ => rank,
    }
}

/// osu!stable: the four rules the wiki documents, which is what the site shows for these plays.
fn stable_grade(mode: u8, accuracy: f64, counts: &[u16; 6]) -> Rank {
    let miss = counts[MISS] as u32;

    // The judgement-proportion modes: stable works from how much of the play was a top judgement,
    // not from accuracy. `geki`/`katu` are excluded because they are subsets of `300`/`100` in
    // osu!standard rather than separate results, and the `50` slot is simply zero in taiko.
    let judged = counts[0] as u32 + counts[1] as u32 + counts[2] as u32 + miss;
    let top = if judged == 0 {
        0.0
    } else {
        counts[0] as f64 / judged as f64
    };
    let fifty = if judged == 0 {
        0.0
    } else {
        counts[2] as f64 / judged as f64
    };

    match mode {
        // The wiki's osu! table, one clause per line, and the only row validated against real data.
        MODE_OSU => {
            if accuracy == 1.0 {
                Rank::X
            } else if top > 0.90 && fifty <= 0.01 && miss == 0 {
                Rank::S
            } else if (top > 0.80 && miss == 0) || top > 0.90 {
                Rank::A
            } else if (top > 0.70 && miss == 0) || top > 0.80 {
                Rank::B
            } else if top > 0.60 {
                Rank::C
            } else {
                Rank::D
            }
        }
        // The same shape on GREATs, without the 50s clause.
        MODE_TAIKO => {
            if accuracy == 1.0 {
                Rank::X
            } else if top > 0.90 && miss == 0 {
                Rank::S
            } else if (top > 0.80 && miss == 0) || top > 0.90 {
                Rank::A
            } else if (top > 0.70 && miss == 0) || top > 0.80 {
                Rank::B
            } else if top > 0.60 {
                Rank::C
            } else {
                Rank::D
            }
        }
        // Accuracy bands, and an S is possible with misses. The wiki states them as ranges
        // (98.01-99.99 for S); `X` has already taken 100%, so the lower bound is the whole test.
        MODE_CATCH => {
            if accuracy == 1.0 {
                Rank::X
            } else if accuracy >= 0.9801 {
                Rank::S
            } else if accuracy >= 0.9401 {
                Rank::A
            } else if accuracy >= 0.9001 {
                Rank::B
            } else if accuracy >= 0.8501 {
                Rank::C
            } else {
                Rank::D
            }
        }
        // Accuracy bands again, but strictly greater, because the wiki says "over 95%".
        MODE_MANIA => {
            if accuracy == 1.0 {
                Rank::X
            } else if accuracy > 0.95 {
                Rank::S
            } else if accuracy > 0.90 {
                Rank::A
            } else if accuracy > 0.80 {
                Rank::B
            } else if accuracy > 0.70 {
                Rank::C
            } else {
                Rank::D
            }
        }
        // No table is known for a ruleset byte this does not recognise, and inventing one would be
        // a guess dressed as a letter.
        _ => Rank::D,
    }
}

/// osu!mania's `anyImperfect`, in terms of the `Good`/`Ok`/`Meh`/`Miss` results.
///
/// The six counts mean different things per mode and mania is where that matters: `countGeki` is
/// `Perfect` (the rainbow MAX), `count300` is `Great`, `countKatu` is `Good`, `count100` is `Ok`
/// and `count50` is `Meh`. So "imperfect" is everything that is neither a `Perfect` nor a `Great` —
/// and since those six results are all mania has, that is simply the four other counts.
fn any_imperfect(counts: &[u16; 6]) -> bool {
    counts[1] as u32 + counts[2] as u32 + counts[4] as u32 + counts[5] as u32 > 0
}

impl Rank {
    /// Parse a letter as osu! writes it — `"SH"`, `"X"`, `"F"`.
    ///
    /// A separate function rather than serde, because the thing being parsed is the *inside* of a
    /// JSON string field rather than a JSON value, and because an unfamiliar letter should be an
    /// absence rather than a failure: lazer keeps adding mods, and one day it may keep adding
    /// letters. Used on lazer's own stored rank, which is authoritative where it exists (§16).
    pub fn from_acronym(letter: &str) -> Option<Self> {
        Some(match letter {
            "XH" => Rank::XH,
            "X" => Rank::X,
            "SH" => Rank::SH,
            "S" => Rank::S,
            "A" => Rank::A,
            "B" => Rank::B,
            "C" => Rank::C,
            "D" => Rank::D,
            "F" => Rank::F,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every count zero except misses — the shape most of these cases care about.
    fn misses(n: u16) -> [u16; 6] {
        [0, 0, 0, 0, 0, n]
    }

    const LAZER: bool = true;

    #[test]
    fn the_lazer_cutoffs_are_inclusive_at_the_boundary() {
        assert_eq!(grade(LAZER, MODE_OSU, 1.0, &misses(0), false), Rank::X);
        assert_eq!(grade(LAZER, MODE_OSU, 0.95, &misses(0), false), Rank::S);
        assert_eq!(grade(LAZER, MODE_OSU, 0.9499, &misses(0), false), Rank::A);
        assert_eq!(grade(LAZER, MODE_OSU, 0.70, &misses(0), false), Rank::C);
        assert_eq!(grade(LAZER, MODE_OSU, 0.6999, &misses(0), false), Rank::D);
    }

    #[test]
    fn the_stable_osu_rule_is_not_the_accuracy_table() {
        // The measured case: 91.2% 300s, no 50s, no misses, 94.12% accuracy. stable and the site
        // call it an S; accuracy alone would call it an A.
        let counts = [912, 88, 0, 0, 0, 0];
        let accuracy: f64 = (912.0 * 300.0 + 88.0 * 100.0) / (1000.0 * 300.0);
        assert!((0.94..0.95).contains(&accuracy), "{accuracy}");
        assert_eq!(grade(false, MODE_OSU, accuracy, &counts, false), Rank::S);
        assert_eq!(grade(LAZER, MODE_OSU, accuracy, &counts, false), Rank::A);
    }

    #[test]
    fn a_letter_read_out_of_a_blob_parses_and_an_unknown_one_is_an_absence() {
        assert_eq!(Rank::from_acronym("SH"), Some(Rank::SH));
        assert_eq!(Rank::from_acronym("F"), Some(Rank::F));
        assert_eq!(Rank::from_acronym("X"), Some(Rank::X));
        assert_eq!(Rank::from_acronym("SHH"), None);
        assert_eq!(Rank::from_acronym(""), None);
    }

    #[test]
    fn the_two_letters_agree_for_a_lazer_play_and_can_differ_for_a_stable_one() {
        // the measured shape: 91.2% 300s, no 50s, no misses at 94.13% accuracy
        let counts = [912, 88, 0, 0, 0, 0];
        let accuracy = 0.941_333_333_333_333_4;
        assert_eq!(grade(false, MODE_OSU, accuracy, &counts, false), Rank::S);
        assert_eq!(lazer_grade(MODE_OSU, accuracy, &counts, false), Rank::A);

        // in the lazer era both come from the same table, so the second letter is redundant there
        let clean = [970, 30, 0, 0, 0, 0];
        assert_eq!(grade(LAZER, MODE_OSU, 0.97, &clean, false), Rank::S);
        assert_eq!(lazer_grade(MODE_OSU, 0.97, &clean, false), Rank::S);
    }

    #[test]
    fn the_stable_osu_rule_needs_a_clean_run_above_ninety() {
        // over 90% 300s but a miss: A, not S
        assert_eq!(
            grade(false, MODE_OSU, 0.99, &[950, 49, 0, 0, 0, 1], false),
            Rank::A
        );
        // 1.5% 50s disqualifies an otherwise perfect S
        assert_eq!(
            grade(false, MODE_OSU, 0.99, &[985, 0, 15, 0, 0, 0], false),
            Rank::A
        );
    }

    #[test]
    fn stable_catch_and_mania_allow_an_s_with_misses() {
        // catch: S from 98.01%, misses and all
        assert_eq!(
            grade(false, MODE_CATCH, 0.99, &[0, 0, 0, 0, 0, 12], false),
            Rank::S
        );
        assert_eq!(grade(false, MODE_CATCH, 0.9401, &misses(0), false), Rank::A);
        // 94.00% is below the 94.01% boundary, so it is still a B
        assert_eq!(grade(false, MODE_CATCH, 0.94, &misses(0), false), Rank::B);
        // mania: strictly over 95%
        assert_eq!(grade(false, MODE_MANIA, 0.9501, &misses(3), false), Rank::S);
        assert_eq!(grade(false, MODE_MANIA, 0.95, &misses(0), false), Rank::A);
    }

    #[test]
    fn silver_wraps_the_two_top_letters_and_nothing_else() {
        // `X` needs accuracy 1 in the stable era, so an all-300 play at 100% is the XH case
        assert_eq!(
            grade(false, MODE_OSU, 1.0, &[1000, 0, 0, 0, 0, 0], true),
            Rank::XH
        );
        assert_eq!(
            grade(false, MODE_OSU, 0.97, &[970, 30, 0, 0, 0, 0], true),
            Rank::SH
        );
        // a B is not silver, and 80% 300s with no miss is an A rather than a B, so use 75%
        assert_eq!(
            grade(false, MODE_OSU, 0.85, &[750, 250, 0, 0, 0, 0], true),
            Rank::B
        );
        // silver is applied on top of whatever the rules said, and is not double-wrapped
        assert_eq!(
            grade(false, MODE_OSU, 1.0, &[1000, 0, 0, 0, 0, 0], true),
            Rank::XH
        );
    }

    #[test]
    fn lazer_mania_promotes_a_clean_s_and_keeps_an_imperfect_one() {
        assert_eq!(
            grade(LAZER, MODE_MANIA, 0.99, &[900, 0, 0, 100, 0, 0], false),
            Rank::X
        );
        assert_eq!(
            grade(LAZER, MODE_MANIA, 0.99, &[899, 0, 0, 100, 1, 0], false),
            Rank::S
        );
        // and unlike osu! and taiko, a miss does not downgrade the S
        assert_eq!(
            grade(LAZER, MODE_MANIA, 0.96, &[899, 0, 0, 100, 0, 1], false),
            Rank::S
        );
    }

    #[test]
    fn lazer_catch_keeps_an_s_with_misses() {
        assert_eq!(grade(LAZER, MODE_CATCH, 0.96, &misses(3), false), Rank::S);
        assert_eq!(grade(LAZER, MODE_CATCH, 0.89, &misses(0), false), Rank::B);
    }

    #[test]
    fn a_letter_this_cannot_know_is_never_produced() {
        for lazer in [true, false] {
            for mode in 0..4u8 {
                for accuracy in [0.0, 0.5, 0.7, 0.9, 0.99, 1.0] {
                    assert_ne!(grade(lazer, mode, accuracy, &misses(4), false), Rank::F);
                }
            }
        }
    }

    #[test]
    fn an_unknown_ruleset_is_not_guessed_at() {
        assert_eq!(grade(LAZER, 9, 0.99, &misses(5), false), Rank::S);
        assert_eq!(grade(false, 9, 0.99, &misses(5), false), Rank::D);
    }
}
