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
//!   i64    online score id
//! ```
//!
//! **Strings are not length-prefixed the obvious way.** A lone `0x00` byte means an empty
//! string; otherwise a `0x0b` marker is followed by a **.NET 7-bit-encoded** length and then
//! UTF-8 bytes. Assuming a fixed-width length is what makes naive parsers of this format fail.

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
    pub score: i32,
    pub max_combo: u16,
    pub mods: i32,
    /// .NET ticks, as stored.
    pub timestamp: i64,
    /// Present only when the file carries one.
    pub online_score_id: Option<i64>,
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

    for _ in 0..6 {
        take_u16(buf, &mut at)?; // count300, count100, count50, geki, katu, miss
    }
    let score = take_i32(buf, &mut at)?;
    let max_combo = take_u16(buf, &mut at)?;
    take_u8(buf, &mut at)?; // perfect
    let mods = take_i32(buf, &mut at)?;
    read_string(buf, &mut at)?; // hit-error / HP graph
    let timestamp = take_i64(buf, &mut at)?;

    let data_length = take_i32(buf, &mut at)?;
    let online_score_id = if data_length >= 0 {
        let after_data = at.saturating_add(data_length as usize);
        if buf.len() >= after_data + 8 {
            at = after_data;
            Some(take_i64(buf, &mut at)?)
        } else {
            None
        }
    } else {
        None
    };

    Ok(Header {
        mode,
        version,
        beatmap_md5,
        player,
        replay_md5,
        score,
        max_combo,
        mods,
        timestamp,
        online_score_id,
    })
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
