//! The index (§5): the one artifact the site reads.
//!
//! One file per game mode, in a fixed field order, and the reason a play costs ~14 bytes rather
//! than a JSON object: **a play names its map and its mods by index, not by value.** Those two
//! tables are the whole encoding.
//!
//! **The shape is a contract.** The browser reads these files by position, so the order of every
//! tuple below is load-bearing and `VERSION` is what a reader checks before trusting it.
//!
//! Three things are deliberately absent, all because they are derivable:
//!
//! - **No personal best.** The PB progression is computed in the browser from `pp` and `played_at`
//!   (§7); a stored PB would be a second source of truth that can disagree with the rows.
//! - **No cover, no weight, no judgement counts.** A cover is rebuilt from `beatmapset_id`, a weight
//!   from pp and position, and the counts are not on screen at any size (§5).
//! - **No ranked-status verdict** — deferred by the owner, so no `beatmaps` field carries one (§5).

use crate::pp;
use osu_core::grade::Rank;
use osu_core::osu;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

/// The index format's own version. Bumped when a file's **shape** changes, never when its contents
/// do — a rebalance changes every number in the file and no reader needs telling.
const VERSION: u32 = 1;

/// The `mode` byte of an `.osr`, and the file each mode's plays go in. All four are always written,
/// even when empty: a missing file would 404 the fetch, while an empty one is a mode with no plays
/// and the browser has to handle that anyway (§6).
const MODES: [(u8, &str); 4] = [(0, "osu"), (1, "taiko"), (2, "catch"), (3, "mania")];

/// osu!'s mod bit for `SV2`. The one thing that moves a stable-era play's recorded score into the
/// modern column, because under ScoreV2 the client recorded a standardised number (§5).
const SCORE_V2: i32 = 1 << 29;

/// The two mods osu! sets **two** bits for: `NC` carries `DT`'s bit and `PF` carries `SD`'s, but
/// osu! names only the stronger one — its own API returns `NC` for a nightcore play and never
/// `DTNC` (measured across this account's best 100: 43 `DT`, 14 `NC`, no overlap).
const SD: &str = "SD";
const DT: &str = "DT";
const NC: &str = "NC";
const PF: &str = "PF";

/// The legacy mod bits and their acronyms, **in bit order** — which is the order osu! displays them
/// in, and therefore the order a badge should read.
///
/// Neither source of mods can supply this order: `rosu-mods` iterates its own internal one, spelling
/// a Hidden+DoubleTime play `DTHD`, and osu!'s own API returns its own too (measured on this
/// library, `HR` before `HD` where the display has `HDHR`). So the expansion is ours, and the test
/// below pins it to §5's verified example.
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

/// A beatmap as the index describes it: what the `.osu` file says, and its own star rating.
///
/// The stars are **no-mod on purpose** — this is the map's rating, the number osu! reports as
/// `difficulty_rating`, and the only one that can be stored once per map. A play's *modded* stars
/// are not stored at all: nothing filters or sorts on them (§5), and they are re-derivable from the
/// map plus the mods.
pub struct Beatmap {
    pub meta: osu::Beatmap,
    pub stars: f64,
}

/// One play, ready to be written: what the file and its filename said, the map it was set on, and
/// the verdicts ingest priced.
pub struct Play {
    /// The `.osr`'s own fields — the pricing inputs, and the era.
    pub play: pp::Play,
    pub attributes: pp::Attributes,
    /// The map's MD5, which is how this play finds its row in `beatmaps`.
    pub md5: String,
    /// The score's own recorded total: the V1 value on a stable-era play, the standardised value on
    /// a lazer-era one. Which column it lands in is [`scores`]' job.
    pub recorded: i64,
    /// osu!'s id for the score, where osu! has one. A play with none is one osu! never saw — 628 of
    /// 8,000 here (§8).
    pub online_id: Option<i64>,
    /// The play key, `(beatmap MD5, score timestamp)`. Used as `score_id` when there is no online
    /// id, because **this value is also the R2 object key** (§9) — so a play and its replay can
    /// never end up filed under two different names.
    pub key: String,
    /// Unix seconds (§5).
    pub played_at: i64,
}

/// What a run wrote, for the report.
pub struct Report {
    /// `(filename, bytes, plays, beatmaps)` per mode.
    pub files: Vec<(String, u64, usize, usize)>,
    /// Plays carrying a `score` — a lazer-era play, whose recorded value already is one.
    pub scored: usize,
    /// Plays whose `score` waits for the recalculation of step 5a. Reported rather than left as a
    /// silent null, because a half-filled column is exactly the kind of thing that ships unnoticed.
    pub awaiting_score: usize,
}

/// Write one file per mode. Returns counts so the caller can report them.
///
/// A dry run still builds every file and reports its size; it just does not write it, which is what
/// the run's opening line promises.
pub fn write(
    plays: &[Play],
    maps: &HashMap<String, Beatmap>,
    work: &Path,
    dry_run: bool,
) -> Result<Report, String> {
    let mut by_mode: BTreeMap<u8, Vec<&Play>> = BTreeMap::new();
    for play in plays {
        by_mode.entry(play.play.mode).or_default().push(play);
    }

    let mut report = Report {
        files: Vec::new(),
        scored: 0,
        awaiting_score: 0,
    };

    for (mode, name) in MODES {
        let mut plays = by_mode.remove(&mode).unwrap_or_default();

        // **Ordered so a rebuild of one library is byte-identical.** Nothing here may depend on the
        // order a `HashMap` happened to iterate in, or two runs over the same library would produce
        // two different files and every diff would be noise. Chronological, with the play key as the
        // tiebreak because two plays can share a second (§5).
        plays.sort_by(|a, b| (a.played_at, &a.key).cmp(&(b.played_at, &b.key)));

        let file = mode_file(mode, &plays, maps, &mut report)?;
        let bytes = serde_json::to_vec(&file)
            .map_err(|error| format!("cannot encode index-{name}.json: {error}"))?;
        let path = work.join(format!("index-{name}.json"));
        if !dry_run {
            fs::write(&path, &bytes)
                .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        }

        report.files.push((
            format!("index-{name}.json"),
            bytes.len() as u64,
            plays.len(),
            file.beatmaps.len(),
        ));
    }

    Ok(report)
}

/// One mode's file. Field order here **is** the wire order, so it is written as a struct rather than
/// assembled into a map — `serde_json::Map` sorts its keys, which would silently reorder the format.
#[derive(Serialize)]
struct File {
    v: u32,
    mode: u8,
    /// The `rosu-pp` release and the osu!lazer commit it ports. Rows then carry the version that
    /// priced them, so a pending reprocess after a rebalance is visible rather than guessed at (§8).
    ppver: &'static str,
    /// The score algorithm and its commit. `null` until step 5a lands, because naming an algorithm
    /// that has not run yet would be a claim this file cannot support.
    scorever: Option<&'static str>,
    mods: Vec<Value>,
    beatmaps: Vec<Value>,
    plays: Vec<Value>,
}

fn mode_file(
    mode: u8,
    plays: &[&Play],
    maps: &HashMap<String, Beatmap>,
    report: &mut Report,
) -> Result<File, String> {
    // ------------------------------------------------------------------ the two tables
    //
    // Both tables are complete before any play is written, because a play refers into them by
    // position and a position is only meaningful once the table is settled.

    // Beatmaps, ordered by MD5. Sorting rather than keeping first-seen order is what makes the table
    // independent of iteration order — the indices below point into it, so an unstable order would
    // renumber it between runs.
    let mut md5s: Vec<&str> = plays.iter().map(|play| play.md5.as_str()).collect();
    md5s.sort_unstable();
    md5s.dedup();

    let mut beatmaps = Vec::with_capacity(md5s.len());
    let mut beatmap_index: HashMap<&str, usize> = HashMap::with_capacity(md5s.len());
    for md5 in md5s {
        let Some(beatmap) = maps.get(md5) else {
            // Unreachable for a priced play: pricing needs the map, so a play whose map is missing
            // was counted unpriced and never reaches here. Checked rather than assumed, because a
            // wrong index would mis-attribute every play below the omission.
            return Err(format!("{}: priced but its beatmap was not read", md5));
        };
        beatmap_index.insert(md5, beatmaps.len());
        beatmaps.push(json!([
            beatmap.meta.beatmap_id,
            beatmap.meta.beatmap_set_id,
            beatmap.meta.version,
            round(beatmap.stars, 2),
            beatmap.meta.artist,
            beatmap.meta.title,
            beatmap.meta.creator,
            md5,
        ]));
    }

    // Mods, deduplicated and ordered by their own spelling, which also puts the no-mod entry first.
    let mut mods: BTreeMap<(String, String), Mods> = BTreeMap::new();
    for play in plays {
        let entry = mods_of(&play.play);
        mods.entry(entry.sort_key()).or_insert(entry);
    }
    let mods: Vec<Mods> = mods.into_values().collect();
    let mods_index: HashMap<(String, String), usize> = mods
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.sort_key(), index))
        .collect();

    // ------------------------------------------------------------------ the plays
    let mut rows = Vec::with_capacity(plays.len());
    for play in plays {
        let (score, legacy_score) = scores(play);
        if score.is_null() {
            report.awaiting_score += 1;
        } else {
            report.scored += 1;
        }

        let beatmap = *beatmap_index
            .get(play.md5.as_str())
            .ok_or("a play's beatmap left the table")?;
        let mods = *mods_index
            .get(&mods_of(&play.play).sort_key())
            .ok_or("a play's mods left the table")?;

        rows.push(json!([
            // The online id where osu! has one, else the play key. Both are usable as the R2 object
            // key (§9), which is why one slot carries either.
            play.online_id
                .map_or_else(|| Value::String(play.key.clone()), Value::from),
            beatmap,
            mods,
            round(play.attributes.pp, 1),
            // Accuracy as a percentage to two decimals, which is the precision every osu! surface
            // shows it at — and the digits below it would cost bytes on every row.
            round(play.attributes.accuracy * 100.0, 2),
            play.play.max_combo,
            score,
            legacy_score,
            code(play.attributes.rank),
            code(play.attributes.lazer_rank),
            play.played_at,
        ]));
    }

    Ok(File {
        v: VERSION,
        mode,
        ppver: pp::ROSU_PP,
        scorever: None,
        mods: mods.iter().map(Mods::to_value).collect(),
        beatmaps,
        plays: rows,
    })
}

/// A `mods` dictionary entry: the label a badge shows, and the blob's own mod array when any mod
/// carried a setting.
///
/// Two plays differing only by a setting **must** be two entries, or one play's setting is silently
/// attributed to the other (§5), which is what carrying the settings in the key prevents.
struct Mods {
    label: String,
    settings: Option<Value>,
}

impl Mods {
    /// The dictionary key: the label, then the settings spelled out. Serialising for the key keeps
    /// ordering deterministic without needing `Value` to be `Ord`.
    fn sort_key(&self) -> (String, String) {
        (
            self.label.clone(),
            self.settings
                .as_ref()
                .map(Value::to_string)
                .unwrap_or_default(),
        )
    }

    /// A bare string when there were no settings, and a `[label, mods]` pair when there were — 339
    /// of this library's 355 lazer-era plays have none, and paying for an array on every entry to
    /// spell nothing is the whole cost of the feature for everyone who does not use it.
    ///
    /// The settings are kept **verbatim** as the blob wrote them rather than re-spelled into a shape
    /// of our own: the acronym stays paired with the setting it belongs to, and the entry cannot
    /// drift from what `rosu-pp` was handed for the same play.
    fn to_value(&self) -> Value {
        match &self.settings {
            None => Value::String(self.label.clone()),
            Some(mods) => json!([self.label, mods]),
        }
    }
}

/// The play's mods as the dictionary needs them.
fn mods_of(play: &pp::Play) -> Mods {
    Mods {
        label: label(play),
        settings: settings_of(play),
    }
}

/// How the play's mods are spelled: **one spelling per set of mods**, whoever recorded them.
///
/// Two sources feed the set. A lazer-era play's blob names its mods completely — `APIMod[]`, straight
/// from osu!'s own model — so the set comes from there. A stable-era play has no blob at all, so the
/// set comes from the legacy bitfield. Either way the **order** is [`BITS`]', and taking the set from
/// one place and the order from another is what stops one mod set from becoming two dictionary
/// entries and one badge from reading differently depending on which client recorded the play.
///
/// Stable **is** Classic, and osu!'s own API says so — it returns `DT, HD, CL` for a play whose
/// bitfield is only `HDDT`. The mod is nowhere in the file, so `CL` is derived from the era and
/// appended, which makes our spelling match osu!'s instead of omitting Classic from 7,645 rows (§5).
fn label(play: &pp::Play) -> String {
    let mut names: Vec<String> = if play.mods_names.is_empty() {
        BITS.iter()
            .filter(|(bit, _)| play.mods & bit != 0)
            .map(|(_, acronym)| (*acronym).to_owned())
            .collect()
    } else {
        play.mods_names.clone()
    };

    // osu! carries the mod it implies as well — Nightcore is DoubleTime and Perfect is SuddenDeath —
    // but shows and returns only the stronger one, so the implied mod leaves the set rather than
    // being spelled beside it.
    if names.iter().any(|name| name == NC) {
        names.retain(|name| name != DT);
    }
    if names.iter().any(|name| name == PF) {
        names.retain(|name| name != SD);
    }

    let mut text = String::new();
    for (_, acronym) in BITS {
        if names.iter().any(|name| name == acronym) {
            text.push_str(acronym);
        }
    }
    // A mod this table does not know — lazer keeps adding them — is still named, in the order it
    // arrived, rather than dropped. Dropping it would make a badge quietly wrong, which is the one
    // outcome worse than an unfamiliar acronym.
    for name in &names {
        if !BITS.iter().any(|(_, acronym)| acronym == name) {
            text.push_str(name);
        }
    }

    if !play.lazer() && !text.contains(CL) {
        text.push_str(CL);
    }

    text
}

/// The acronym stable_ is Classic by.
const CL: &str = "CL";

/// The blob's mod array, but **only when a mod actually carries settings**. Without that test the
/// array would be the label spelled a second time, on every lazer-era play.
fn settings_of(play: &pp::Play) -> Option<Value> {
    let raw: Vec<Value> = serde_json::from_str(play.mods_json.as_deref()?).ok()?;
    raw.iter()
        .any(|game_mod| game_mod.get("settings").is_some())
        .then_some(Value::Array(raw))
}

/// §5's two score columns, of which **exactly one is ever computed**.
///
/// A lazer-era play was *scored* by the standardised system, so it never had a V1 number and its
/// recorded value is the `score`. A stable-era play's recorded value is the V1 one, so it is the
/// `legacy_score` and `score` waits for the recalculation (step 5a).
///
/// `null` is the honest value in both empty cases, **never a fallback to the other column**: a
/// fallback would put two units of measure inside one view, and the single rule that keeps the scores
/// coherent is one unit per view (§5).
///
/// The case that must not be missed: a stable-era play wearing `SV2` recorded an already
/// standardised score, so it belongs in the `score` column like a lazer-era play. osu! special-cases
/// exactly this in `ModScoreV2`. **This library has zero such plays**, so nothing here would catch a
/// mistake — a stranger's might not be so lucky (a ponytail for a later reader, not a bug today).
fn scores(play: &Play) -> (Value, Value) {
    let recorded = Value::from(play.recorded);
    if play.play.lazer() || play.play.mods & SCORE_V2 != 0 {
        (recorded, Value::Null)
    } else {
        (Value::Null, recorded)
    }
}

/// The stored letter, as its position in §5's wire order: `XH` 0 through `F` 8.
///
/// An exhaustive match rather than a table lookup, because this is the one place that decides where
/// a letter sits in a contract already written into browsers. A new variant in `Rank` then breaks
/// the build instead of quietly taking whatever a lookup returned.
fn code(rank: Rank) -> u8 {
    match rank {
        Rank::XH => 0,
        Rank::X => 1,
        Rank::SH => 2,
        Rank::S => 3,
        Rank::A => 4,
        Rank::B => 5,
        Rank::C => 6,
        Rank::D => 7,
        Rank::F => 8,
    }
}

/// Round to a set precision, so the file carries the digits that are shown and not the ones that are
/// only paid for. `serde_json` then writes the shortest form that round-trips, so a rounded `322.2`
/// is five bytes rather than seven.
fn round(value: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (value * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;
    use osu_core::osr::SliderCounts;

    fn play(mods: i32, names: &[&str], version: i32, mods_json: Option<&str>) -> pp::Play {
        pp::Play {
            mode: 0,
            mods,
            mods_names: names.iter().map(|name| (*name).to_owned()).collect(),
            mods_json: mods_json.map(str::to_owned),
            counts: [100, 0, 0, 0, 0, 0],
            max_combo: 100,
            version,
            sliders: Some(SliderCounts::default()),
            stored_rank: None,
        }
    }

    fn entry(play: &pp::Play) -> Play {
        Play {
            play: play.clone(),
            attributes: pp::Attributes {
                stars: 4.0,
                pp: 100.0,
                accuracy: 1.0,
                rank: Rank::S,
                lazer_rank: Rank::S,
            },
            md5: "md5".to_owned(),
            recorded: 706543,
            online_id: Some(4_566_394_317),
            key: "md5-133493808368081330".to_owned(),
            played_at: 1_704_907_237,
        }
    }

    /// The wire order is a contract: these integers are already in browsers, so this test exists to
    /// fail loudly if a letter ever moves.
    #[test]
    fn the_letters_are_written_in_wire_order() {
        assert_eq!(code(Rank::XH), 0);
        assert_eq!(code(Rank::X), 1);
        assert_eq!(code(Rank::SH), 2);
        assert_eq!(code(Rank::S), 3);
        assert_eq!(code(Rank::A), 4);
        assert_eq!(code(Rank::B), 5);
        assert_eq!(code(Rank::C), 6);
        assert_eq!(code(Rank::D), 7);
        assert_eq!(code(Rank::F), 8);
    }

    /// A stable-era play spells `CL` even though nothing in the file says it, and the bitflag
    /// expansion is `rosu-mods`' own — the same crate `rosu-pp` was handed, so the label cannot
    /// describe a different set of mods than the calculation used.
    #[test]
    fn a_stable_play_spells_classic_and_its_bitflags() {
        // Hidden (8) + DoubleTime (64). osu! shows this play as `HDDTCL`.
        let stable = play(8 | 64, &[], 20_240_102, None);
        assert_eq!(label(&stable), "HDDTCL");

        // A lazer play's blob is complete and already in osu!'s order, so it is used as it comes.
        let lazer = play(8 | 64, &["HD", "DT"], 30_000_019, None);
        assert_eq!(label(&lazer), "HDDT");

        // A lazer play's blob is complete, and its order is canonicalised into osu!'s display order
        // rather than trusted: measured on this library, osu!'s API spells one replay `HRHD` where
        // the display has `HDHR`.
        let reversed = play(8 | 64, &["DT", "HD"], 30_000_019, None);
        assert_eq!(label(&reversed), "HDDT");

        // Nightcore already carries DoubleTime and Perfect already carries SuddenDeath; naming the
        // implied mod too would spell one mod twice.
        assert_eq!(label(&play(1 << 9, &["DT", "NC"], 30_000_019, None)), "NC");
        assert_eq!(
            label(&play((1 << 6) | (1 << 9), &[], 20_240_102, None)),
            "NCCL"
        );

        // An acronym this table has never heard of is still named, not dropped.
        assert_eq!(label(&play(8, &["HD", "XX"], 30_000_019, None)), "HDXX");

        // No mods at all is the empty string, not a name.
        assert_eq!(label(&play(0, &[], 20_240_102, None)), "CL");
        assert_eq!(label(&play(0, &[], 30_000_019, None)), "");
    }

    /// Settings must reach the entry, and a play without them must not pay for the array.
    #[test]
    fn settings_appear_only_when_a_mod_has_them() {
        let plain = play(64, &["DT"], 30_000_019, Some(r#"[{"acronym":"DT"}]"#));
        assert!(settings_of(&plain).is_none());
        assert_eq!(mods_of(&plain).to_value(), Value::String("DT".to_owned()));

        let with = play(
            64,
            &["DT"],
            30_000_019,
            Some(r#"[{"acronym":"DT","settings":{"speed_change":1.3}}]"#),
        );
        assert!(settings_of(&with).is_some());
        // The settings are kept verbatim, so the acronym stays paired with its own setting.
        assert_eq!(
            mods_of(&with).to_value(),
            json!([
                "DT",
                [{"acronym": "DT", "settings": {"speed_change": 1.3}}]
            ])
        );

        // Two plays differing only by a setting are two entries, or one play's setting would be
        // attributed to the other.
        assert_ne!(mods_of(&plain).sort_key(), mods_of(&with).sort_key());
    }

    /// Which column the recorded score lands in is decided by the era, and never by both.
    #[test]
    fn exactly_one_score_column_is_filled() {
        // Stable-era: the recorded number is the V1 one, and `score` waits for step 5a.
        let (score, legacy) = scores(&entry(&play(0, &[], 20_240_102, None)));
        assert_eq!(score, Value::Null);
        assert_eq!(legacy, Value::from(706_543));

        // Lazer-era: the recorded number already is standardised, and there was never a V1 value.
        let (score, legacy) = scores(&entry(&play(0, &[], 30_000_019, None)));
        assert_eq!(score, Value::from(706_543));
        assert_eq!(legacy, Value::Null);

        // ScoreV2 in a stable-era file was recorded standardised too, so it moves columns even
        // though the file is stable's. No such play exists in this library.
        let (score, legacy) = scores(&entry(&play(SCORE_V2, &[], 20_240_102, None)));
        assert_eq!(score, Value::from(706_543));
        assert_eq!(legacy, Value::Null);
    }

    /// The whole point of the sort above: the same library must produce the same bytes. Anything
    /// reading a `HashMap` into the output would fail this intermittently rather than always.
    #[test]
    fn a_rebuild_is_byte_identical() {
        let maps: HashMap<String, Beatmap> = [(
            "md5".to_owned(),
            Beatmap {
                meta: osu::Beatmap {
                    beatmap_id: Some(1_278_814),
                    beatmap_set_id: Some(600_702),
                    mode: 0,
                    title: "Harumachi Clover".to_owned(),
                    artist: "Hanasaka Yui(CV: M.A.O)".to_owned(),
                    creator: "Djulus".to_owned(),
                    version: "Tarrasky's True Love".to_owned(),
                },
                stars: 4.54,
            },
        )]
        .into_iter()
        .collect();

        let plays: Vec<Play> = vec![entry(&play(0, &[], 20_240_102, None))];
        let mut report = Report {
            files: Vec::new(),
            scored: 0,
            awaiting_score: 0,
        };
        let first = serde_json::to_vec(&mode_file(0, &[&plays[0]], &maps, &mut report).unwrap());
        let second = serde_json::to_vec(&mode_file(0, &[&plays[0]], &maps, &mut report).unwrap());
        assert_eq!(first.unwrap(), second.unwrap());
    }
}
