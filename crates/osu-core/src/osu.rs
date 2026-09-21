//! Reading `.osu` beatmap files.
//!
//! A `.osu` is a text file of `[Section]` headers and `key:value` lines. Only a handful of keys are
//! read here — the ones the index stores — because the rest either duplicates them or is only
//! needed by rosu-pp, which takes the whole file.
//!
//! Two real-world quirks this tolerates, both found in this machine's library and both of which
//! would otherwise drop maps silently:
//!
//! - **620 files begin with a UTF-8 BOM** and one with a stray space. `classify` tolerates both to
//!   recognise the file at all; the reader has to tolerate them again or those maps parse to
//!   nothing.
//! - **`[Metadata]` writes `Key:Value` with no space, `[General]` writes `Key: Value` with one.**
//!   Every value is trimmed, so both work and neither needs special-casing.
//!
//! The MD5 is of the raw bytes and is not affected by any of this — see [`md5`]. Tolerating a quirk
//! must never mean rewriting the file, because the MD5 is what a `.osr` recorded when it was played.

use md5::{Digest, Md5};

/// The MD5 of a beatmap file's own bytes.
///
/// This is the map's identity everywhere in osu!: a `.osr` records the MD5 of the map it was
/// played on, so it is the join key between a replay and a beatmap. It is also what a beatmap
/// mirror's copy is verified against before the viewer renders it.
pub fn md5(bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(bytes);
    let digest = hasher.finalize();

    let mut out = String::with_capacity(32);
    for byte in digest {
        use std::fmt::Write;
        // Writing to a String cannot fail; the Result is for writers that can.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// What the index needs out of a beatmap. Everything else in the file is either duplication or
/// rosu-pp's business.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Beatmap {
    /// `[Metadata] BeatmapID`. **Absent on a map that has never been submitted**, and `-1` is
    /// written by some editors, so both become `None`.
    pub beatmap_id: Option<i64>,
    /// `[Metadata] BeatmapSetID`, same rules. The cover URL is built from this.
    pub beatmap_set_id: Option<i64>,
    /// `[General] Mode`. Absent means 0, which is osu!standard — most files omit it.
    pub mode: u8,
    pub title: String,
    pub artist: String,
    pub creator: String,
    /// The difficulty name, `[Metadata] Version` — not the format version.
    pub version: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// No `osu file format v…` line anywhere. This is not a beatmap.
    NotABeatmap,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotABeatmap => write!(f, "not a beatmap: no 'osu file format' line"),
        }
    }
}

impl std::error::Error for Error {}

/// Read the index's fields out of a `.osu` file.
///
/// Text that is not valid UTF-8 is replaced rather than rejected — a map with one bad byte is
/// still a map, and dropping it would lose a real play. A missing key becomes an empty string or
/// `None` rather than an error, because editors differ about which keys they write; a file with no
/// format line at all is [`Error::NotABeatmap`], which is the one case that is not a map.
pub fn parse(bytes: &[u8]) -> Result<Beatmap, Error> {
    let text = String::from_utf8_lossy(bytes);
    // Trimmed here rather than by the line loop below: `trim()` does not remove a BOM, and 620
    // real files start with one.
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);

    let mut parsed = Beatmap::default();
    let mut section = "";
    let mut saw_format = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }

        if let Some(rest) = line.strip_prefix('[') {
            if let Some(name) = rest.strip_suffix(']') {
                section = name;
            }
            continue;
        }

        if line.starts_with("osu file format v") {
            saw_format = true;
            continue;
        }

        // Split on the *first* colon, so a value may contain one — `Title:Re:Zero` is real.
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();

        match (section, key.trim()) {
            ("General", "Mode") => parsed.mode = value.parse().unwrap_or(0),

            ("Metadata", "Title") => parsed.title = value.to_owned(),
            ("Metadata", "Artist") => parsed.artist = value.to_owned(),
            ("Metadata", "Creator") => parsed.creator = value.to_owned(),
            ("Metadata", "Version") => parsed.version = value.to_owned(),
            ("Metadata", "BeatmapID") => parsed.beatmap_id = id(value),
            ("Metadata", "BeatmapSetID") => parsed.beatmap_set_id = id(value),
            _ => {}
        }
    }

    if saw_format {
        Ok(parsed)
    } else {
        Err(Error::NotABeatmap)
    }
}

/// A beatmap id, where `-1` and an empty value both mean "there isn't one". Editors write both.
fn id(value: &str) -> Option<i64> {
    value.parse::<i64>().ok().filter(|id| *id > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "osu file format v14\n\
                          \n\
                          [General]\n\
                          AudioFilename: audio.mp3\n\
                          Mode: 3\n\
                          \n\
                          [Metadata]\n\
                          Title:Re:Zero kara Hajimeru\n\
                          TitleUnicode:Re:Zero\n\
                          Artist:Some Artist\n\
                          Creator:Some Mapper\n\
                          Version:Insane\n\
                          BeatmapID:175250\n\
                          BeatmapSetID:56940\n";

    #[test]
    fn md5_matches_the_known_vector() {
        // The canonical RFC 1321 test vector, as lowercase hex.
        assert_eq!(md5(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn md5_is_always_32_hex() {
        let digest = md5(b"osu file format v14\n");
        assert_eq!(digest.len(), 32);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn the_index_fields_are_read() {
        let beatmap = parse(SAMPLE.as_bytes()).expect("must parse");
        assert_eq!(beatmap.beatmap_id, Some(175_250));
        assert_eq!(beatmap.beatmap_set_id, Some(56_940));
        assert_eq!(beatmap.mode, 3);
        assert_eq!(beatmap.artist, "Some Artist");
        assert_eq!(beatmap.creator, "Some Mapper");
        assert_eq!(beatmap.version, "Insane");
        // The colon in the value survives, which is why the split takes the first one only.
        assert_eq!(beatmap.title, "Re:Zero kara Hajimeru");
    }

    /// Most files omit `Mode`, and it means standard.
    #[test]
    fn an_absent_mode_is_standard() {
        let text = "osu file format v14\n[Metadata]\nTitle:x\n";
        assert_eq!(parse(text.as_bytes()).expect("must parse").mode, 0);
    }

    /// `[General]` writes `Key: Value` and `[Metadata]` writes `Key:Value`. Both must work, and a
    /// test that only uses the tidy one would pass while every real file failed.
    #[test]
    fn spacing_after_the_colon_does_not_matter() {
        let tight = parse(b"osu file format v14\n[Metadata]\nTitle:NoSpace\n").expect("parse");
        let loose = parse(b"osu file format v14\n[Metadata]\nTitle: Some Space\n").expect("parse");
        assert_eq!(tight.title, "NoSpace");
        assert_eq!(loose.title, "Some Space");
    }

    /// The 620 files that carry a BOM, and the one that carries a leading space. `classify`
    /// tolerates them to recognise the file; if the reader did not, those maps would parse to
    /// empty strings with no error at all.
    #[test]
    fn a_bom_and_a_leading_space_are_tolerated_again_here() {
        let with_bom = [b"\xEF\xBB\xBF".as_slice(), SAMPLE.as_bytes()].concat();
        let beatmap = parse(&with_bom).expect("must parse");
        assert_eq!(beatmap.title, "Re:Zero kara Hajimeru");
        assert_eq!(beatmap.beatmap_id, Some(175_250));

        let with_space = [b" ", SAMPLE.as_bytes()].concat();
        assert_eq!(parse(&with_space).expect("parse").mode, 3);
    }

    /// Both spellings of "no id": editors write `-1`, and some leave the value empty.
    #[test]
    fn no_beatmap_id_is_none() {
        let minus_one = parse(b"osu file format v14\n[Metadata]\nBeatmapID:-1\n").expect("parse");
        assert_eq!(minus_one.beatmap_id, None);

        let empty = parse(b"osu file format v14\n[Metadata]\nBeatmapID:\n").expect("parse");
        assert_eq!(empty.beatmap_id, None);

        let junk = parse(b"osu file format v14\n[Metadata]\nBeatmapID:abc\n").expect("parse");
        assert_eq!(junk.beatmap_id, None);
    }

    /// Keys are only read in their own section — a `Title` in `[Events]` must not become the title.
    #[test]
    fn keys_are_read_only_from_their_own_section() {
        let text = "osu file format v14\n[Events]\nTitle:wrong\n[Metadata]\nTitle:right\n";
        assert_eq!(parse(text.as_bytes()).expect("parse").title, "right");
    }

    #[test]
    fn something_that_is_not_a_beatmap_is_rejected() {
        assert_eq!(parse(b"\x89PNG\r\n\x1a\n").unwrap_err(), Error::NotABeatmap);
        assert_eq!(parse(b"").unwrap_err(), Error::NotABeatmap);
    }

    /// The MD5 covers the bytes as they are, BOM included, because that is what a `.osr` recorded
    /// when the play happened. Stripping the BOM for parsing must not change the hash.
    #[test]
    fn the_bom_is_parsed_around_but_still_hashed() {
        let plain = b"osu file format v14\n[Metadata]\nTitle:x\n".to_vec();
        let with_bom = [b"\xEF\xBB\xBF".as_slice(), &plain[..]].concat();

        assert_eq!(parse(&with_bom).expect("parse").title, "x");
        assert_ne!(md5(&with_bom), md5(&plain));
    }
}
