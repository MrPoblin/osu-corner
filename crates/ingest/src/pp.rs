//! Star rating and pp, via `rosu-pp`.
//!
//! `rosu-pp` is a port of osu!lazer's difficulty and performance calculation. It is a plain
//! crates.io dependency — no fork, no submodule — because there is nothing in it to patch. Its
//! only dependencies are `rosu-map` and `rosu-mods`, and it names the exact lazer commit it
//! tracks, which is what the index's `ppver` records.
//!
//! Three things decide whether a number is right, and all three come out of the `.osr` rather than
//! out of a choice made here:
//!
//! - **the mods**, as legacy bitflags, which the replay carries and `rosu-mods` converts;
//! - **the judgement counts**, passed straight in — so accuracy is derived by `rosu-pp` the way
//!   osu! derives it, and this crate never computes an accuracy of its own;
//! - **`lazer(..)`**, which switches between stable's and lazer's score semantics. It defaults to
//!   `true`, and its own docs say it *"affects internal accuracy calculation because lazer
//!   considers slider heads for accuracy whereas stable does not"*. Most of this library is
//!   stable-era, so the default would be wrong for most of it — the replay's format version says
//!   which side a play belongs to, so it is data, not a preference.
//!
//! **Every calculation goes through the checked path.** `rosu-pp`'s own documentation warns:
//! *"Whereas osu! simply times out on malicious maps, rosu-pp does not. To prevent potential
//! performance/memory issues, it is recommended to check beforehand whether a map is too
//! suspicious for further calculation."* Every `.osu` here either came off a mirror where **only
//! the MD5 was verified and the contents were never vetted** (§10), or came out of a game install
//! that a stranger's clone will point at their own folders. A crafted `.osu` is attacker-controlled
//! input, so `checked_calculate` is used at both entry points rather than `calculate`.

use rosu_mods::serde::GameModsSeed;
use rosu_pp::model::mods::rosu_mods::{GameMode, GameModsIntermode};
use rosu_pp::{Beatmap, Difficulty, GameMods, Performance};
use serde::de::DeserializeSeed;

/// The `rosu-pp` release and the osu!lazer commit it ports. Written into the index as `ppver`, so
/// that a pp rebalance is visible as a change in the index rather than as numbers quietly moving.
///
/// **A rebalance after this commit is a real, measured divergence from live osu!, and it is not a
/// bug.** rosu-pp 4.0.1 ports lazer at `28c846b4` (2025-10-13); osu! shipped a further star and pp
/// update on **2026-07-03** covering osu!, osu!taiko and osu!catch. Comparing 25 real scores
/// against the live API: the inputs agree exactly — osu!'s `accuracy` of `0.9131736526946108` is
/// precisely what our counts produce — while star ratings differ by up to ±0.24 and pp by up to
/// 13.8, **in both directions** — the signature of an algorithm change rather than a plumbing bug,
/// and a plumbing bug would not leave osu!taiko matching to four decimals. Chasing live osu!
/// would mean the index silently changing under a deployed site, which is the opposite of what
/// pinning is for (§8).
///
/// The version half is asserted against `Cargo.lock` by a test below, so bumping the dependency
/// fails the build instead of leaving a stale string behind. The commit half is only documented in
/// `rosu-pp`'s own source and has no public constant to read, so it is hand-maintained —
/// `ponytail:` a dependency bump needs this line edited too, and only the version half is checked.
pub const ROSU_PP: &str = "rosu-pp 4.0.1 @ lazer 28c846b4d9366484792e27f4729cd1afa2cdeb66";

/// The version at which lazer's replay encoder begins, and therefore the boundary between stable
/// and lazer score semantics. Measured: 355 of this library's 8,000 replays are at or above it.
const LAZER_ENCODER: i32 = 30_000_001;

/// What a play on a beatmap came out as.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Attributes {
    pub stars: f64,
    pub pp: f64,
}

#[derive(Debug)]
pub enum Error {
    /// `rosu-map` could not decode the file at all.
    Decode(String),
    /// The map is shaped in a way that would make calculation expensive or unbounded. Refused
    /// before any calculation happens.
    Suspicious(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Decode(message) => write!(f, "cannot decode beatmap: {message}"),
            Error::Suspicious(message) => write!(f, "beatmap refused as too suspicious: {message}"),
        }
    }
}

/// One play's inputs, all read from the `.osr`.
#[derive(Debug, Clone)]
pub struct Play {
    /// The ruleset played. Needed because the six counts do not mean the same thing in every
    /// mode — see [`Map::attributes`].
    pub mode: u8,
    /// Legacy mod bitflags, straight from the header.
    pub mods: i32,
    /// Mods lazer records that have **no legacy bitflag**, from the appended blob — `CL`, `AP`,
    /// `4K` and the rest. Real inputs, not decoration: `4K` changes a mania map's key count and
    /// `CL` changes the whole calculation, so dropping them misprices a play silently. Empty for a
    /// stable-era replay, which has none to record.
    pub mods_names: Vec<String>,
    /// The appended blob's mods array, verbatim. It carries each mod's **settings**, which no
    /// bitfield can express and which change the answer: measured, 16 of this library's 355
    /// lazer-era replays carry them, so a `DT` play at `1.3` priced from the bitfield alone comes
    /// out as a `DT` play at `1.5`, and a `DA` play ignores its overrides entirely. `None` for
    /// every stable-era replay, which has no blob.
    pub mods_json: Option<String>,
    /// `[count300, count100, count50, geki, katu, miss]`.
    pub counts: [u16; 6],
    pub max_combo: u16,
    /// The `.osr`'s format version, which decides stable or lazer semantics.
    pub version: i32,
}

/// The ruleset byte of an `.osr` as rosu-mods' enum, so mode-specific acronyms resolve (`4K` is a
/// mania mod) instead of arriving as "unknown".
const fn game_mode(mode: u8) -> GameMode {
    match mode {
        1 => GameMode::Taiko,
        2 => GameMode::Catch,
        3 => GameMode::Mania,
        _ => GameMode::Osu,
    }
}

/// The acronym lazer uses for the Classic mod: stable's rules, played in lazer.
const CLASSIC: &str = "CL";

/// A beatmap, decoded once and reused for every play on it. A map averages 2.65 plays in this
/// library (§16) and decoding is the expensive half, so this is parsed once per map rather than
/// once per play.
pub struct Map {
    inner: Beatmap,
}

impl Map {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let inner = Beatmap::from_bytes(bytes).map_err(|error| Error::Decode(error.to_string()))?;

        // `rosu-map` decodes leniently: a PNG or a truncated file does not error, it yields a
        // beatmap with nothing in it. `classify` cannot catch that either, because it only reads
        // the first 64 bytes. Without this guard a corrupt map would calculate to zero stars and
        // zero pp, which is a plausible-looking row rather than a visible failure.
        if inner.hit_objects.is_empty() {
            return Err(Error::Decode("no hit objects".to_owned()));
        }

        Ok(Self { inner })
    }

    /// Stars and pp for one play. The star rating does not depend on the score, only on the map and
    /// the mods, but every play asks for both because the index stores both.
    pub fn attributes(&self, play: &Play) -> Result<Attributes, Error> {
        // **Prefer the appended blob, because it carries settings.** Lazer writes the score's
        // complete mod list there — `APIMod[]`, including the mods that *do* have a bitflag, which
        // is why the measured example is `[{"acronym":"NF"},{"acronym":"DT","settings":{…}}]` — so
        // when it decodes it is both the fuller and the more accurate source and the bitfield adds
        // nothing. `GameModsSeed` is rosu-mods' own entry point for exactly this shape, and it
        // resolves mode-specific acronyms (`4K` is mania's) from the mode we pass.
        //
        // `deny_unknown_fields: false` on purpose: a setting this version does not know about must
        // not sink the whole deserialization. Losing a play's mods is worse than carrying one field
        // we cannot name.
        let from_blob = play.mods_json.as_deref().and_then(|json| {
            // An empty list must fall through to the bitfield, not win: a blob that names no mods
            // while the bitfield names some would otherwise silently erase them.
            let raw: Vec<serde_json::Value> = serde_json::from_str(json).ok()?;
            if raw.is_empty() {
                return None;
            }

            let mut de = serde_json::Deserializer::from_str(json);
            GameModsSeed::Mode {
                mode: game_mode(play.mode),
                deny_unknown_fields: false,
            }
            .deserialize(&mut de)
            .ok()
            // The seed yields `rosu_mods::GameMods`, which is a different type from the
            // `rosu_pp::GameMods` that `Performance` takes. rosu-pp has a `From` for exactly this.
            .map(GameMods::from)
        });

        // Otherwise start from the legacy bitflags, then add the mods that have no bitflag at all.
        // Losing the second group misprices a play without saying so — `4K` alone changes a mania
        // map's key count, and `CL` changes everything. The acronyms are joined and parsed once,
        // because `rosu-mods` already knows every mod's spelling and reimplementing that table here
        // would be a second place to be wrong.
        let mods = from_blob.unwrap_or_else(|| {
            let mut all = GameModsIntermode::from_bits(play.mods.max(0) as u32);
            if !play.mods_names.is_empty() {
                // This parse cannot fail: an acronym `rosu-mods` does not recognise becomes an
                // "unknown mod" carrying its text rather than an error. That is the behaviour we
                // want — a mod we cannot name is still a mod the score was set with, and dropping
                // it would misprice the play to buy nothing.
                let Ok(extra) = play.mods_names.concat().parse::<GameModsIntermode>();
                all.extend(extra);
            }
            GameMods::from(all)
        });

        // **The 4th and 5th counts are not judgements in osu!standard.** osu! reports them as
        // `null` for a std score while `stable` writes real numbers into those slots — its slider
        // tick counts. They are genuinely used in mania (`geki` = MAX, `katu` = 200), catch
        // (`katu` = missed droplets) and taiko, so only osu!standard zeroes them.
        //
        // Measured against osu!'s own values on 25 real std scores, **this made no difference at
        // all** — `rosu-pp` ignores those fields for osu!standard, so it is input hygiene rather
        // than a fix. It is kept because feeding a slider-tick count into a judgement field is
        // wrong on its face and would go unnoticed if a later version started reading it. The pp
        // gap it was first blamed for is the upstream rebalance documented in `ROSU_PP`.
        let (n_geki, n_katu) = match play.mode {
            0 => (0, 0),
            _ => (u32::from(play.counts[3]), u32::from(play.counts[4])),
        };

        // **Stable or lazer semantics, and Classic is the second half of that answer.** The format
        // version identifies the client that *wrote* the replay, which covers every play set in
        // stable — lazer byte-copies those files on import rather than re-encoding them, verified
        // byte-identical across 400 duplicated plays. What the version cannot express is a play set
        // in **lazer with Classic enabled**: the file is lazer's, so the version says lazer, while
        // the rules behind the score are stable's. The mod list is the only place that says so.
        // This library has no such play — none of its 355 lazer-era replays names `CL` — so nothing
        // here would have caught it; a stranger's clone will.
        let classic =
            play.version < LAZER_ENCODER || play.mods_names.iter().any(|name| name == CLASSIC);

        let difficulty = Difficulty::new()
            .mods(mods.clone())
            .checked_calculate(&self.inner)
            .map_err(|error| Error::Suspicious(format!("{error:?}")))?;

        let stars = difficulty.stars();

        let pp = Performance::new(difficulty)
            .mods(mods)
            .combo(u32::from(play.max_combo))
            .n300(u32::from(play.counts[0]))
            .n100(u32::from(play.counts[1]))
            .n50(u32::from(play.counts[2]))
            .n_geki(n_geki)
            .n_katu(n_katu)
            .misses(u32::from(play.counts[5]))
            .lazer(!classic)
            .checked_calculate()
            .map_err(|error| Error::Suspicious(format!("{error:?}")))?
            .pp();

        Ok(Attributes { stars, pp })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but valid osu!standard beatmap: one circle, so a calculation has something to do.
    /// Built by hand rather than committed as a fixture, so the tests stay text and readable.
    fn tiny_map() -> Vec<u8> {
        let mut text = String::from(
            "osu file format v14\n\
             \n\
             [General]\n\
             Mode: 0\n\
             \n\
             [Metadata]\n\
             Title:test\n\
             Artist:test\n\
             Creator:test\n\
             Version:test\n\
             BeatmapID:1\n\
             BeatmapSetID:1\n\
             \n\
             [Difficulty]\n\
             HPDrainRate:5\n\
             CircleSize:4\n\
             OverallDifficulty:8\n\
             ApproachRate:9\n\
             SliderMultiplier:1.4\n\
             SliderTickRate:1\n\
             \n\
             [TimingPoints]\n\
             0,500,4,2,1,100,1,0\n\
             \n\
             [HitObjects]\n",
        );
        for i in 0..64 {
            let x = 64 + (i % 8) * 48;
            let y = 64 + ((i / 8) % 8) * 48;
            let time = 500 + i * 250;
            if i % 4 == 3 {
                // A slider. Slider heads count for accuracy under lazer and not under stable, so
                // a map of nothing but circles cannot show that switch having any effect.
                text.push_str(&format!(
                    "{x},{y},{time},2,0,L|{}:{y},1,140,0:0:0:0:\n",
                    x + 96
                ));
            } else {
                text.push_str(&format!("{x},{y},{time},1,0,0:0:0:0:\n"));
            }
        }
        text.into_bytes()
    }

    fn play(mods: i32, version: i32) -> Play {
        Play {
            mode: 0,
            mods,
            mods_names: Vec::new(),
            mods_json: None,
            counts: [64, 0, 0, 0, 0, 0],
            max_combo: 64,
            version,
        }
    }

    #[test]
    fn a_real_map_produces_a_star_rating_and_pp() {
        let map = Map::parse(&tiny_map()).expect("must decode");
        let attributes = map.attributes(&play(0, 20230101)).expect("must calculate");

        assert!(attributes.stars > 0.0, "stars: {}", attributes.stars);
        assert!(attributes.pp > 0.0, "pp: {}", attributes.pp);
        assert!(
            attributes.pp < 1_000.0,
            "an implausible number: {}",
            attributes.pp
        );
    }

    /// Difficulty rises with the mods that make a map harder, which is the cheapest check that the
    /// mods actually reach the calculation rather than being dropped on the way.
    #[test]
    fn mods_reach_the_calculation() {
        let map = Map::parse(&tiny_map()).expect("must decode");

        let nomod = map.attributes(&play(0, 20230101)).expect("nomod");
        let hardrock = map.attributes(&play(16, 20230101)).expect("HR");
        let doubletime = map.attributes(&play(64, 20230101)).expect("DT");

        assert!(hardrock.stars > nomod.stars, "HR should raise stars");
        assert!(doubletime.stars > nomod.stars, "DT should raise stars");
        assert!(hardrock.pp > nomod.pp, "HR should raise pp");
    }

    /// A mod that exists only in lazer's appended blob — no legacy bitflag carries it. Losing
    /// these misprices a play with nothing to show for it, which is why this is worth a test.
    #[test]
    fn a_mod_with_no_bitflag_still_reaches_the_calculation() {
        let map = Map::parse(&tiny_map()).expect("must decode");

        let plain = play(0, 30_000_019);
        let with_extra = Play {
            mods_names: vec!["AP".to_owned()],
            ..play(0, 30_000_019)
        };

        // Autopilot changes how the score is judged, so the two must not come out identical.
        assert_ne!(
            map.attributes(&plain).expect("plain"),
            map.attributes(&with_extra).expect("with AP")
        );
    }

    /// **The gate for 4a-i.** A settings-bearing mod must price differently from the same mod at
    /// its default, or the settings are not reaching `rosu-mods` and the fix is cosmetic — which is
    /// exactly the failure the old code had, where the field was dropped and nothing said so.
    #[test]
    fn a_mod_setting_changes_the_result_and_only_the_blob_carries_it() {
        let map = Map::parse(&tiny_map()).expect("must decode");

        // The same bitfield both times (DT = 64): the difference has to come from the blob, since
        // a bitflag cannot express a speed at all.
        let bitfield_only = play(64, 30_000_019);
        let at_1_3 = Play {
            mods_json: Some(r#"[{"acronym":"DT","settings":{"speed_change":1.3}}]"#.to_owned()),
            ..play(64, 30_000_019)
        };

        let default_speed = map.attributes(&bitfield_only).expect("bitfield");
        let slower = map.attributes(&at_1_3).expect("with settings");

        assert_ne!(
            default_speed, slower,
            "1.3x must not price as the default 1.5x"
        );
        assert!(
            slower.stars < default_speed.stars,
            "a slower clock is a lower star rating: {} vs {}",
            slower.stars,
            default_speed.stars
        );
    }

    /// **The half of the question the version cannot answer.** A play set in *lazer with Classic
    /// enabled* carries a lazer-era version but stable's rules. No replay in this library does
    /// that, so nothing here would have caught it — a stranger's clone would.
    #[test]
    fn classic_calculates_like_stable_even_on_a_lazer_era_replay() {
        let map = Map::parse(&tiny_map()).expect("must decode");

        let mixed = Play {
            mode: 0,
            mods: 0,
            mods_names: Vec::new(),
            mods_json: None,
            counts: [40, 20, 4, 0, 0, 0],
            max_combo: 64,
            version: 30_000_019,
        };
        let stable_era = Play {
            version: 20230101,
            ..mixed.clone()
        };
        let lazer_with_classic = Play {
            mods_names: vec![CLASSIC.to_owned()],
            ..mixed.clone()
        };

        assert_eq!(
            map.attributes(&stable_era).expect("stable"),
            map.attributes(&lazer_with_classic).expect("CL"),
            "Classic must calculate exactly as a stable-era replay does"
        );
        assert_ne!(
            map.attributes(&mixed).expect("lazer"),
            map.attributes(&stable_era).expect("stable"),
            "and without it, lazer-era must differ"
        );
    }

    /// A full combo is worth more than the same score with misses. If the counts were being
    /// dropped, these would be equal.
    #[test]
    fn the_judgement_counts_reach_the_calculation() {
        let map = Map::parse(&tiny_map()).expect("must decode");

        let clean = Play {
            counts: [64, 0, 0, 0, 0, 0],
            ..play(0, 20230101)
        };
        let sloppy = Play {
            counts: [54, 8, 0, 0, 0, 2],
            ..play(0, 20230101)
        };

        assert!(
            map.attributes(&clean).expect("clean").pp > map.attributes(&sloppy).expect("sloppy").pp
        );
    }

    /// The stable/lazer switch has to be wired to the replay's version, not left at `rosu-pp`'s
    /// default of `true` — most of this library is stable-era, where that default is wrong.
    #[test]
    fn stable_and_lazer_semantics_are_chosen_by_the_replay_version() {
        let map = Map::parse(&tiny_map()).expect("must decode");

        // Slider heads count for accuracy under lazer and not under stable, so the two disagree
        // whenever a score has non-300 judgements on a map with sliders.
        let mixed = Play {
            mode: 0,
            counts: [40, 20, 4, 0, 0, 0],
            max_combo: 64,
            mods: 0,
            mods_names: Vec::new(),
            mods_json: None,
            version: 0,
        };
        let stable = map
            .attributes(&Play {
                version: 20230101,
                ..mixed.clone()
            })
            .expect("stable");
        let lazer = map
            .attributes(&Play {
                version: 30_000_019,
                ..mixed
            })
            .expect("lazer");

        assert_ne!(stable, lazer, "the version must change the result");
    }

    /// `rosu-map` decodes leniently — it does not reject a PNG, it returns a beatmap with nothing
    /// in it — and `classify` only reads the first 64 bytes, so a truncated `.osu` passes that too.
    /// The guard is what stops a corrupt file becoming a plausible zero-star row.
    #[test]
    fn a_map_with_nothing_in_it_is_refused() {
        assert!(matches!(
            Map::parse(b"\x89PNG\r\n\x1a\n"),
            Err(Error::Decode(_))
        ));
        assert!(matches!(Map::parse(b""), Err(Error::Decode(_))));

        // A well-formed header with no `[HitObjects]` at all: the shape a truncated file has.
        let header_only = b"osu file format v14\n[General]\nMode: 0\n[Metadata]\nTitle:x\n";
        assert!(matches!(Map::parse(header_only), Err(Error::Decode(_))));
    }

    /// The version half of [`ROSU_PP`] is checked against the lockfile, so bumping the dependency
    /// cannot leave a stale string heading for the index. This is the one part of `ppver` that can
    /// be automated; the lazer commit has no public constant to read.
    #[test]
    fn the_recorded_rosu_pp_version_matches_the_lockfile() {
        let lock = include_str!("../../../Cargo.lock");
        let version = lock
            .split("[[package]]")
            .find(|block| block.contains("name = \"rosu-pp\""))
            .and_then(|block| block.lines().find(|line| line.starts_with("version = ")))
            .map(|line| {
                line.trim_start_matches("version = ")
                    .trim()
                    .trim_matches('"')
            })
            .expect("rosu-pp must appear in Cargo.lock");

        assert!(
            ROSU_PP.contains(version),
            "ROSU_PP says {ROSU_PP:?} but Cargo.lock has rosu-pp {version} — update the constant"
        );
    }
}
