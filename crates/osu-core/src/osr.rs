//! Reading `.osr` replay files.
//!
//! The format is osu!'s legacy score encoding, and the layout below was verified against real
//! files rather than taken from documentation: the parse consumes the file's whole length and
//! lands exactly on its trailing online score id.
//!
//! ```text
//!   u8     mode, 0..=3
//!   i32    score format version — stable's YYYYMMDD, or lazer's 3000000x
//!   string beatmap MD5
//!   string player name
//!   string replay MD5
//!   u16x6  count300, count100, count50, countGeki, countKatu, countMiss
//!   i32    total score
//!   u16    max combo
//!   u8     perfect
//!   i32    mods
//!   string hit-error / HP graph  (often empty)
//!   i64    timestamp, .NET ticks since 0001-01-01
//!   i32    replay data length, -1 when absent
//!   bytes  LZMA-compressed replay data
//!   i64    online score id — unreliable from 30000001, see below
//!   i32    length of an appended blob, if present
//!   bytes  that blob: an LZMA-alone container holding JSON
//! ```
//!
//! **Strings are not length-prefixed the obvious way.** A lone `0x00` byte means an empty
//! string; otherwise a `0x0b` marker is followed by a **.NET 7-bit-encoded** length and then
//! UTF-8 bytes. Assuming a fixed-width length is what makes naive parsers of this format fail.
//!
//! **From game version 30000001 the trailing score id field stops being written, and the real one
//! moves into the appended blob.** Measured over this machine's 355 lazer-era replays: the
//! trailing field is `-1` in **all 355**, while the blob holds a real id in **326** of them — the
//! two agree exactly once. So reading the trailing field alone reports 326 submitted plays as
//! never-submitted, and that value is the index's `score_id` **and** the R2 object key that falls
//! back to it (§5). The blob therefore wins whenever it decodes; the trailing field is only the
//! answer when there is no blob. The blob is a standard LZMA-alone stream — `5d 00 00 20 00`, 2 MiB
//! dictionary, then an 8-byte uncompressed size — so it needs no header reconstruction.

use std::fmt;

/// Ticks between 0001-01-01 (.NET's epoch, which the format stores) and 1601-01-01 (the FILETIME
/// epoch, which stable and lazer both use to name stored replays).
///
/// Verified against a real pair: a `.osr` carrying ticks `638206452694499848` is stored by stable
/// as `...-133295220694499848.osr`, and the difference is exactly this constant.
const NET_TO_FILETIME_TICKS: i64 = 504_911_232_000_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Ran off the end of the file.
    Truncated,
    /// A string did not begin with `0x00` or the `0x0b` marker.
    BadStringMarker(u8),
    /// A string's length was implausibly large or self-contradictory.
    BadStringLength,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => write!(f, "file ended mid-header"),
            Self::BadStringMarker(byte) => {
                write!(f, "expected a string marker, found 0x{byte:02x}")
            }
            Self::BadStringLength => write!(f, "implausible string length"),
        }
    }
}

impl std::error::Error for Error {}

/// The parts of a replay this project needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub mode: u8,
    pub version: i32,
    /// The MD5 of the `.osu` this replay was set on — the join key to a beatmap.
    pub beatmap_md5: String,
    pub player: String,
    /// Lazer writes `md5("lazer-{user}-{date}")` here, not a hash of the replay data, so this is
    /// **not** a portable identity. See [`Header::key`].
    pub replay_md5: String,
    /// `[count300, count100, count50, countGeki, countKatu, countMiss]`. What judgement names mean
    /// depends on the ruleset (§6). Verified correct even in lazer-era files, which also carry a
    /// newer named `statistics` object in the appended blob: over 355 of them, every one had real
    /// counts here and the numbers agreed, so the index's accuracy derives from these and does not
    /// need the newer encoding.
    pub counts: [u16; 6],
    pub score: i32,
    pub max_combo: u16,
    /// Lazer's "full combo". One byte, so a `u8 != 0`.
    pub perfect: bool,
    pub mods: i32,
    /// .NET ticks, as stored.
    pub timestamp: i64,
    /// Mods lazer records that have **no legacy bitflag** and so appear in neither [`Header::mods`]
    /// nor any dictionary built from it — the `Classic`-style ones. Measured: 181 lazer-era plays
    /// carry mods in the appended blob against 172 with a bitflag, so about nine plays would be
    /// mislabelled as having no mods at all without these. Empty for stable-era replays.
    pub mods_names: Vec<String>,
    /// The appended blob's mods array as **verbatim JSON**, for handing to `rosu-mods`. It carries
    /// the **settings** as well as the acronyms — `speed_change`, `drain_rate`, whatever comes next
    /// — which no bitfield can express and which change the number: measured, 16 of this library's
    /// 355 lazer-era replays carry settings, so pricing them from the bitfield alone uses `DT`'s
    /// default `1.5` instead of the speed actually played. `None` when the file has no blob, which
    /// is every stable-era replay.
    pub mods_json: Option<String>,
    /// Present only when the file carries one. `-1`, which both clients write for "never
    /// submitted", is an absence and becomes `None`.
    pub online_score_id: Option<i64>,
    /// lazer's own letter for this play, out of the appended blob. **The only place a failed score
    /// can be told apart from a passed one** — no field anywhere in the file says so, so a letter
    /// derived from accuracy alone would present a failed replay as though it had passed (§16).
    /// Measured: present in 305 of this library's 355 lazer-era replays, two of them `"F"`, and
    /// `None` for every stable-era replay, which records no rank at all. This is lazer's finished
    /// answer rather than an input, so nothing here should second-guess it — see [`crate::grade`].
    pub stored_rank: Option<String>,
}

impl Header {
    /// The play's identity, as it appears in a stored filename: `<beatmap MD5>-<filetime>`.
    ///
    /// Stable and lazer both name stored replays this way — lazer reuses stable's convention on
    /// import — so this key collapses the same play regardless of which game it came from, of
    /// which export directory it was found in, and of whether the two copies share bytes.
    pub fn key(&self) -> Option<String> {
        let filetime = self.timestamp.checked_sub(NET_TO_FILETIME_TICKS)?;
        if filetime <= 0 {
            return None;
        }
        Some(format!("{}-{}", self.beatmap_md5, filetime))
    }
}

/// The header's shape, checked without fully parsing it. Used to pick `.osr` files out of a
/// content-addressed store that has no filenames to go on.
pub fn looks_like(head: &[u8]) -> bool {
    if head.len() < 40 {
        return false;
    }
    if head[0] > 3 {
        return false;
    }
    let version = i32::from_le_bytes([head[1], head[2], head[3], head[4]]);
    if !(1_000_000..=99_999_999).contains(&version) {
        return false;
    }
    head[5] == 0x0b
        && head[6] == 32
        && head[7..39].iter().all(u8::is_ascii_hexdigit)
        && matches!(head[39], 0x0b | 0x00)
}

/// Parse a whole `.osr` file. The compressed replay data is skipped, not decoded — nothing this
/// project does with a replay needs the input frames.
pub fn parse(buf: &[u8]) -> Result<Header, Error> {
    let mut at = 0usize;

    let mode = take_u8(buf, &mut at)?;
    let version = take_i32(buf, &mut at)?;
    let beatmap_md5 = read_string(buf, &mut at)?;
    let player = read_string(buf, &mut at)?;
    let replay_md5 = read_string(buf, &mut at)?;

    let mut counts = [0u16; 6];
    for slot in &mut counts {
        *slot = take_u16(buf, &mut at)?;
    }
    let score = take_i32(buf, &mut at)?;
    let max_combo = take_u16(buf, &mut at)?;
    let perfect = take_u8(buf, &mut at)? != 0;
    let mods = take_i32(buf, &mut at)?;
    read_string(buf, &mut at)?; // hit-error / HP graph
    let timestamp = take_i64(buf, &mut at)?;

    let data_length = take_i32(buf, &mut at)?;
    let (online_score_id, mods_names, mods_json, stored_rank) = read_tail(buf, data_length, at);

    Ok(Header {
        mode,
        version,
        beatmap_md5,
        player,
        replay_md5,
        counts,
        score,
        max_combo,
        perfect,
        mods,
        timestamp,
        mods_names,
        mods_json,
        online_score_id,
        stored_rank,
    })
}

/// The online score id and the extra mods, out of whatever follows the compressed replay data.
///
/// Two layouts share this space and the newer one wins:
///
/// - **legacy** — a trailing `i64` score id. Correct for stable and for lazer before 30000001.
/// - **appended** — that same `i64` (still written, but as `-1`), then an `i32` length and an
///   LZMA-alone JSON blob. Read over the whole library: 354 of 355 lazer-era files disagree with
///   the legacy field and 326 hold a real id only here.
///
/// Everything here is best-effort. A file whose blob is truncated, not LZMA, or not JSON falls
/// back to the legacy field rather than failing — losing the extra fields of one replay is not a
/// reason to lose the replay.
fn read_tail(
    buf: &[u8],
    data_length: i32,
    after_header: usize,
) -> (Option<i64>, Vec<String>, Option<String>, Option<String>) {
    if data_length < 0 {
        return (None, Vec::new(), None, None);
    }
    let body_end = after_header.saturating_add(data_length as usize);
    let legacy = id_at(buf, body_end);

    // The appended blob, if there is one, and only if its declared length really is there: a
    // truncated or hostile length must not become a huge allocation.
    let Some(blob_len) = length_at(buf, body_end + 8) else {
        return (legacy, Vec::new(), None, None);
    };
    let end = body_end
        .saturating_add(12)
        .saturating_add(blob_len as usize);
    let Some(blob) = buf.get(body_end + 12..end) else {
        return (legacy, Vec::new(), None, None);
    };

    match decode_appended(blob) {
        // The blob is authoritative when it decodes, even where it says "no online id": it is the
        // newer and only-kept-in-sync copy, and the legacy field is `-1` in every such file.
        Some((id, mods, stored_rank)) => {
            let names = mods
                .iter()
                .filter_map(|entry| entry.get("acronym")?.as_str().map(str::to_owned))
                .collect();
            // Re-serialised unchanged, so nothing the parser does not understand is lost on the
            // way to `rosu-mods`. If it will not serialise it was not JSON to begin with.
            let json = serde_json::to_string(&mods).ok();
            (id.or(legacy), names, json, stored_rank)
        }
        None => (legacy, Vec::new(), None, None),
    }
}

/// The JSON lazer appends: `online_id`, the mods it could not express as a legacy bitflag, and
/// the newer named `statistics`.
#[derive(serde::Deserialize)]
struct Appended {
    online_id: Option<i64>,
    /// lazer's own letter for the play. It has no legacy counterpart, and it is the only failure
    /// flag that exists anywhere in either client's file.
    rank: Option<String>,
    /// The mod objects **as raw JSON**, not typed fields. `rosu-mods` deserializes this exact shape
    /// — its own tests feed it `{"acronym": "DA", "settings": {…}}` alongside bare
    /// `{"acronym": "CS"}` — so keeping it verbatim preserves settings this crate has no business
    /// knowing about, and keeps `osu-core` from depending on `rosu-mods`, which it must not: this
    /// is the crate that has to keep compiling for wasm32 as the Worker's domain crate.
    #[serde(default)]
    mods: Vec<serde_json::Value>,
}

fn decode_appended(blob: &[u8]) -> Option<(Option<i64>, Vec<serde_json::Value>, Option<String>)> {
    let mut raw = Vec::new();
    lzma_rs::lzma_decompress(&mut &blob[..], &mut raw).ok()?;

    let appended: Appended = serde_json::from_slice(&raw).ok()?;
    Some((
        appended.online_id.filter(|id| *id > 0),
        appended.mods,
        appended.rank,
    ))
}

/// A score id at an offset, `None` where the file is too short, and never a non-positive value —
/// `-1` is what both clients write for "never submitted", which is an absence, not an id.
fn id_at(buf: &[u8], at: usize) -> Option<i64> {
    let bytes = buf.get(at..at.checked_add(8)?)?;
    let value = i64::from_le_bytes(bytes.try_into().ok()?);
    (value > 0).then_some(value)
}

fn length_at(buf: &[u8], at: usize) -> Option<i32> {
    let bytes = buf.get(at..at.checked_add(4)?)?;
    let value = i32::from_le_bytes(bytes.try_into().ok()?);
    (value > 0).then_some(value)
}

fn take_u8(buf: &[u8], at: &mut usize) -> Result<u8, Error> {
    let byte = *buf.get(*at).ok_or(Error::Truncated)?;
    *at += 1;
    Ok(byte)
}

fn take_u16(buf: &[u8], at: &mut usize) -> Result<u16, Error> {
    let bytes = buf.get(*at..*at + 2).ok_or(Error::Truncated)?;
    *at += 2;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn take_i32(buf: &[u8], at: &mut usize) -> Result<i32, Error> {
    let bytes = buf.get(*at..*at + 4).ok_or(Error::Truncated)?;
    *at += 4;
    Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn take_i64(buf: &[u8], at: &mut usize) -> Result<i64, Error> {
    let bytes = buf.get(*at..*at + 8).ok_or(Error::Truncated)?;
    *at += 8;
    Ok(i64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

/// `0x00` alone is the empty string; anything else is a `0x0b` marker, a .NET 7-bit-encoded
/// length, and then the bytes.
fn read_string(buf: &[u8], at: &mut usize) -> Result<String, Error> {
    let marker = take_u8(buf, at)?;
    if marker == 0x00 {
        return Ok(String::new());
    }
    if marker != 0x0b {
        return Err(Error::BadStringMarker(marker));
    }

    let mut length = 0usize;
    let mut shift = 0u32;
    loop {
        let byte = take_u8(buf, at)?;
        length |= ((byte & 0x7f) as usize) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 28 {
            return Err(Error::BadStringLength);
        }
    }

    let end = at.checked_add(length).ok_or(Error::BadStringLength)?;
    let bytes = buf.get(*at..end).ok_or(Error::Truncated)?;
    *at = end;
    // A player name is arbitrary user text, so lossy is the right call: refusing the whole file
    // over one odd byte would lose a replay for nothing.
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(test)]
mod tests {
    /// lazer's own letter is the only place a failed score can be recognised, so it has to survive
    /// the parse. `F` is the case that matters: accuracy alone would call that play something it
    /// is not. A stable-era file has no blob and so no letter at all.
    #[test]
    fn the_appended_blob_carries_lazers_own_letter() {
        let json = r#"{"online_id":7,"rank":"F","mods":[]}"#;
        let header =
            parse(&build_lazer([10, 0, 0, 0, 0, 3], false, -1, Some(json))).expect("parses");
        assert_eq!(header.stored_rank.as_deref(), Some("F"));

        let plain = parse(&build_lazer([10, 0, 0, 0, 0, 0], true, 456, None)).expect("parses");
        assert_eq!(plain.stored_rank, None);
    }
    use super::*;

    /// Build a real-shaped `.osr` by hand, so the tests do not depend on a fixture file.
    fn build(beatmap_md5: &str, player: &str, timestamp: i64, data: &[u8], online: i64) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(0u8); // mode
        out.extend_from_slice(&20230101i32.to_le_bytes()); // version
        write_string(&mut out, beatmap_md5);
        write_string(&mut out, player);
        write_string(&mut out, "abcdef0123456789abcdef0123456789"); // replay md5
        for _ in 0..6 {
            out.extend_from_slice(&0u16.to_le_bytes()); // counts
        }
        out.extend_from_slice(&439_370i32.to_le_bytes()); // score
        out.extend_from_slice(&245u16.to_le_bytes()); // combo
        out.push(0); // perfect
        out.extend_from_slice(&0i32.to_le_bytes()); // mods
        write_string(&mut out, ""); // hp graph, empty
        out.extend_from_slice(&timestamp.to_le_bytes());
        out.extend_from_slice(&(data.len() as i32).to_le_bytes());
        out.extend_from_slice(data);
        out.extend_from_slice(&online.to_le_bytes());
        out
    }

    fn write_string(out: &mut Vec<u8>, text: &str) {
        if text.is_empty() {
            out.push(0);
            return;
        }
        out.push(0x0b);
        let mut length = text.len();
        loop {
            let mut byte = (length & 0x7f) as u8;
            length >>= 7;
            if length > 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if length == 0 {
                break;
            }
        }
        out.extend_from_slice(text.as_bytes());
    }

    /// A lazer-era replay: real judgement counts, and the appended blob that carries the online
    /// score id and any mods with no legacy bitflag.
    fn build_lazer(counts: [u16; 6], perfect: bool, legacy: i64, json: Option<&str>) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(0u8); // mode 0
        out.extend_from_slice(&30000019i32.to_le_bytes()); // a real lazer encoder version
        write_string(&mut out, MAP);
        write_string(&mut out, "Poblin");
        write_string(&mut out, "abcdef0123456789abcdef0123456789");
        for count in counts {
            out.extend_from_slice(&count.to_le_bytes());
        }
        out.extend_from_slice(&439_370i32.to_le_bytes());
        out.extend_from_slice(&245u16.to_le_bytes());
        out.push(u8::from(perfect));
        out.extend_from_slice(&64i32.to_le_bytes()); // mods, DT
        write_string(&mut out, ""); // hp graph
        out.extend_from_slice(&TICKS.to_le_bytes());
        out.extend_from_slice(&4i32.to_le_bytes()); // replay data length
        out.extend_from_slice(b"body");
        out.extend_from_slice(&legacy.to_le_bytes());

        if let Some(json) = json {
            let mut blob = Vec::new();
            lzma_rs::lzma_compress(&mut json.as_bytes(), &mut blob).expect("compress");
            out.extend_from_slice(&(blob.len() as i32).to_le_bytes());
            out.extend_from_slice(&blob);
        }
        out
    }

    /// The judgements reach `Header` instead of being stepped over to find `mods`. The index
    /// derives accuracy from them, so silently dropping them would have produced a library of
    /// plays with no accuracy and no error.
    #[test]
    fn the_judgements_and_the_perfect_flag_are_kept() {
        let file = build_lazer([110, 2, 0, 0, 0, 0], true, 1, None);
        let header = parse(&file).expect("must parse");

        assert_eq!(header.counts, [110, 2, 0, 0, 0, 0]);
        assert!(header.perfect);
        assert_eq!(header.score, 439_370);
        assert_eq!(header.max_combo, 245);
    }

    /// **The bug this replaced.** Lazer-era files write `-1` in the trailing id field and the real
    /// id only in the appended blob. Measured over 355 real files: 354 disagree and 326 hold a
    /// real id here. Reading the trailing field alone reports 326 submitted plays as
    /// never-submitted — and that value is the index's `score_id` *and* the R2 object key.
    #[test]
    fn the_appended_blob_supplies_the_online_id_the_legacy_field_lost() {
        let json = r#"{"online_id":7403726280,"mods":[]}"#;
        let file = build_lazer([110, 2, 0, 0, 0, 0], false, -1, Some(json));
        let header = parse(&file).expect("must parse");

        assert_eq!(header.online_score_id, Some(7_403_726_280));
    }

    /// The blob wins even when the legacy field carries something, because it is the newer copy
    /// and the legacy one is `-1` in every real file that has a blob at all.
    #[test]
    fn the_appended_blob_wins_over_a_populated_legacy_field() {
        let json = r#"{"online_id":7403726280,"mods":[]}"#;
        let file = build_lazer([1, 0, 0, 0, 0, 0], false, 999, Some(json));
        assert_eq!(parse(&file).unwrap().online_score_id, Some(7_403_726_280));
    }

    /// `-1` is what both clients write for "never submitted", so it is an absence rather than an
    /// id — including when it comes from the blob.
    #[test]
    fn a_negative_online_id_in_the_blob_is_an_absence() {
        let json = r#"{"online_id":-1,"mods":[]}"#;
        let file = build_lazer([1, 0, 0, 0, 0, 0], false, -1, Some(json));
        assert_eq!(parse(&file).unwrap().online_score_id, None);
    }

    /// Mods with no legacy bitflag live only in the blob. Measured: 181 lazer-era plays carry mods
    /// there against 172 with a bitflag, so about nine plays would be mislabelled as having no
    /// mods at all without this.
    #[test]
    fn the_appended_blob_carries_the_mods_that_have_no_bitflag() {
        let json = r#"{"online_id":1,"mods":[{"acronym":"CL"},{"acronym":"SV2"}]}"#;
        let file = build_lazer([1, 0, 0, 0, 0, 0], false, 1, Some(json));
        let header = parse(&file).expect("must parse");

        assert_eq!(header.mods_names, ["CL", "SV2"]);
        // The bitflag field is untouched by any of this and still says DT.
        assert_eq!(header.mods, 64);
    }

    /// **The defect this closes.** A mod's *settings* change the number and live only in the blob —
    /// `speed_change`, `drain_rate` — and the first parser kept nothing but the acronym, so a `DT`
    /// play at `1.3` was priced at the default `1.5` and a `DA` play ignored its overrides
    /// entirely. Measured: 16 of this library's 355 lazer-era replays carry settings.
    #[test]
    fn the_appended_blob_keeps_mod_settings_verbatim() {
        let json = r#"{"online_id":1,"mods":[{"acronym":"NF"},{"acronym":"DT","settings":{"speed_change":1.3}}]}"#;
        let file = build_lazer([1, 0, 0, 0, 0, 0], false, 1, Some(json));
        let header = parse(&file).expect("must parse");

        assert_eq!(header.mods_names, ["NF", "DT"]);
        let kept = header.mods_json.as_deref().expect("settings must survive");
        assert!(kept.contains("speed_change"), "got {kept}");
        assert!(kept.contains("1.3"), "got {kept}");
        // Kept as the original objects, not re-serialised from typed fields: a `{"acronym":"NF"}`
        // entry must not gain a `"settings": null`, because `rosu-mods` reads this back and that is
        // not a shape it accepts.
        assert!(!kept.contains("null"), "got {kept}");
    }

    /// Losing the extra fields of one replay is not a reason to lose the replay. A blob that is
    /// not LZMA, or not JSON, or truncated, falls back to the legacy field.
    #[test]
    fn an_unreadable_appended_blob_falls_back_to_the_legacy_id() {
        for json in ["not lzma at all", "", "{ unclosed"] {
            let file = build_lazer([1, 0, 0, 0, 0, 0], false, 4_360_173_832, Some(json));
            let header = parse(&file).expect("must parse");
            assert_eq!(header.online_score_id, Some(4_360_173_832));
            assert!(header.mods_names.is_empty());
        }
    }

    /// A declared blob length reaching past the end of the file must be refused rather than
    /// allocated: the length is attacker-controlled bytes.
    #[test]
    fn an_overlong_blob_length_is_ignored() {
        let mut file = build_lazer([1, 0, 0, 0, 0, 0], false, 4_360_173_832, None);
        file.extend_from_slice(&i32::MAX.to_le_bytes());
        file.extend_from_slice(b"tiny");

        let header = parse(&file).expect("must parse");
        assert_eq!(header.online_score_id, Some(4_360_173_832));
    }

    /// A stable-era replay has no blob at all, so nothing changes about how it is read.
    #[test]
    fn a_legacy_replay_has_no_blob_and_no_extra_mods() {
        let file = build_lazer([1, 0, 0, 0, 0, 0], false, 4_360_173_832, None);
        let header = parse(&file).expect("must parse");
        assert_eq!(header.online_score_id, Some(4_360_173_832));
        assert!(header.mods_names.is_empty());
    }

    const MAP: &str = "144e76e9bd39f65370d54689255f31ec";
    // The ticks a real `.osr` carries, and the FILETIME stable names that same file with.
    const TICKS: i64 = 638_206_452_694_499_848;
    const FILETIME: i64 = 133_295_220_694_499_848;

    #[test]
    fn a_real_shaped_replay_parses() {
        let file = build(MAP, "Poblin", TICKS, b"compressed junk", 4_360_173_832);
        let header = parse(&file).expect("must parse");

        assert_eq!(header.mode, 0);
        assert_eq!(header.version, 20230101);
        assert_eq!(header.beatmap_md5, MAP);
        assert_eq!(header.player, "Poblin");
        assert_eq!(header.score, 439_370);
        assert_eq!(header.max_combo, 245);
        assert_eq!(header.timestamp, TICKS);
        assert_eq!(header.online_score_id, Some(4_360_173_832));
    }

    /// The whole point of the key: it has to equal the name stable and lazer already give the
    /// file, which is the FILETIME form — not the ticks the header stores.
    #[test]
    fn the_key_is_the_stored_filename() {
        let file = build(MAP, "Poblin", TICKS, b"x", 1);
        let header = parse(&file).expect("must parse");
        assert_eq!(
            header.key().as_deref(),
            Some("144e76e9bd39f65370d54689255f31ec-133295220694499848")
        );
    }

    /// The two epochs are 504,911,232,000,000,000 ticks apart. Using the Unix epoch instead gives
    /// keys that are consistently wrong — dedupe still works and nothing complains, while every
    /// key silently stops matching the filename it is supposed to reproduce.
    #[test]
    fn the_epoch_offset_matches_a_real_file() {
        assert_eq!(TICKS - NET_TO_FILETIME_TICKS, FILETIME);
        assert_eq!(FILETIME, 133_295_220_694_499_848);
    }

    #[test]
    fn an_absent_replay_body_and_online_id_are_tolerated() {
        // Stable writes replayLength = -1 for a score whose replay it never stored.
        let mut file = build(MAP, "Poblin", TICKS, b"", -1);
        let length_at = file.len() - 12;
        file[length_at..length_at + 4].copy_from_slice(&(-1i32).to_le_bytes());
        file.truncate(length_at + 4); // no data, no trailing id

        let header = parse(&file).expect("must parse");
        assert_eq!(header.online_score_id, None);
        assert_eq!(header.beatmap_md5, MAP);
    }

    #[test]
    fn a_long_string_uses_the_multibyte_length() {
        // A HP graph longer than 127 bytes is where a fixed-width length assumption breaks.
        let graph = "1234|1,".repeat(40); // 280 bytes
        let mut out = Vec::new();
        out.push(0u8);
        out.extend_from_slice(&20230101i32.to_le_bytes());
        write_string(&mut out, MAP);
        write_string(&mut out, "Poblin");
        write_string(&mut out, "abcdef0123456789abcdef0123456789");
        for _ in 0..6 {
            out.extend_from_slice(&0u16.to_le_bytes());
        }
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.push(0);
        out.extend_from_slice(&0i32.to_le_bytes());
        write_string(&mut out, &graph);
        out.extend_from_slice(&TICKS.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&7i64.to_le_bytes());

        let header = parse(&out).expect("must parse");
        assert_eq!(header.timestamp, TICKS);
        assert_eq!(header.online_score_id, Some(7));
    }

    #[test]
    fn rubbish_is_rejected_rather_than_guessed_at() {
        assert_eq!(parse(&[]), Err(Error::Truncated));
        // An `.osu` fed to a `.osr` reader gets as far as the first string, then stops.
        assert!(parse(b"osu file format v14").is_err());

        let mut wrong_marker = build(MAP, "Poblin", TICKS, b"x", 1);
        wrong_marker[5] = 0x99; // where the first string's marker should be
        assert_eq!(parse(&wrong_marker), Err(Error::BadStringMarker(0x99)));
    }

    #[test]
    fn a_timestamp_before_the_filetime_epoch_has_no_key() {
        let file = build(MAP, "Poblin", 0, b"x", 1);
        let header = parse(&file).expect("must parse");
        assert_eq!(header.key(), None);
    }
}
