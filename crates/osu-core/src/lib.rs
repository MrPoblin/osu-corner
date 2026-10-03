#![forbid(unsafe_code)]

//! The osu! domain: `.osr` and `.osu` parsing, the library index format, grades, and what a
//! profile response may contain.
//!
//! (`pp` is not here — it needs `rosu-pp`, which stays in `osu-ingest` so this crate keeps
//! compiling for wasm32 as the Worker's domain crate.)
//!
//! Pure data — no filesystem, no threads, no `worker`. That is what keeps it usable from the
//! Worker and testable under plain `cargo test`, and it is why the file walking lives in
//! `osu-ingest` instead of here.

pub mod grade;
pub mod osr;
pub mod osu;
pub mod profile;

/// Bytes as lowercase hex — how every osu! identity is written down: a beatmap MD5, a `.osr`
/// filename's first half, a store hash. Here rather than beside any one caller because three of
/// them need it: the MD5 below, the ingest ledger's encoding, and the SigV4 signature the ingest
/// signs an upload with.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing into a `String` cannot fail; the `Result` is for writers that can.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Enough bytes to recognise any file type this project cares about; a `.osr` header needs 40 to
/// reach its replay hash. One read of 64 bytes is one page and settles every case.
pub const HEAD_LEN: usize = 64;

/// What a blob turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blob {
    /// A beatmap.
    Osu,
    /// A replay.
    Osr,
    /// Anything else — audio, images, skins, `.osg`, beatmaps still in archives.
    Other,
}

/// A UTF-8 byte-order mark. Some beatmaps carry one — 620 of them in one real library, saved by
/// an editor that wrote a BOM.
const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// Identify one blob from the first bytes it gave us. `head` may be shorter than [`HEAD_LEN`]
/// when the file is — nothing stops a 3-byte shortcut file existing in someone's store.
///
/// Real libraries are messier than the format says. This one holds 620 beatmaps prefixed with a
/// UTF-8 BOM and one prefixed with a stray space, and both are files the game itself stored and
/// played — rejecting them would have silently dropped them from the library. So the check
/// tolerates a BOM and leading whitespace.
///
/// **Neither is stripped from the bytes.** A beatmap's MD5 is of the file exactly as it sits on
/// disk, which is what a `.osr` recorded when it was played, so tolerating them here must not
/// mean rewriting the file.
pub fn classify(head: &[u8]) -> Blob {
    let head = head.strip_prefix(BOM).unwrap_or(head);
    let indent = head
        .iter()
        .take_while(|byte| byte.is_ascii_whitespace())
        .count();
    let head = &head[indent..];

    if head.starts_with(b"osu file format v") {
        return Blob::Osu;
    }
    if osr::looks_like(head) {
        return Blob::Osr;
    }
    Blob::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic `.osr` header, byte for byte in the shape a real one parses as.
    pub(crate) fn osr_head(mode: u8, version: i32) -> Vec<u8> {
        let mut head = vec![mode];
        head.extend_from_slice(&version.to_le_bytes());
        head.push(0x0b);
        head.push(32);
        head.extend_from_slice(b"144e76e9bd39f65370d54689255f31ec");
        head.push(0x0b);
        head.resize(HEAD_LEN, 0);
        head
    }

    #[test]
    fn a_beatmap_is_recognised() {
        assert_eq!(classify(b"osu file format v14\n[General]\n"), Blob::Osu);
    }

    /// A real library had 620 of these. Rejecting them would have quietly dropped a fortieth of
    /// the beatmaps, and nothing downstream would have said so.
    #[test]
    fn a_beatmap_with_a_utf8_bom_is_recognised_too() {
        assert_eq!(
            classify(b"\xEF\xBB\xBFosu file format v14\n[General]\n"),
            Blob::Osu
        );
        // And one that begins with a stray space, which one real file does.
        assert_eq!(classify(b" osu file format v14\r\n"), Blob::Osu);
        assert_eq!(classify(b"\t\r\nosu file format v9\r\n"), Blob::Osu);
    }

    #[test]
    fn a_replay_is_recognised() {
        assert_eq!(classify(&osr_head(0, 20230101)), Blob::Osr);
        assert_eq!(classify(&osr_head(3, 30000019)), Blob::Osr);
        let mut empty_name = osr_head(1, 20260711);
        empty_name[39] = 0x00;
        assert_eq!(classify(&empty_name), Blob::Osr);
    }

    #[test]
    fn the_things_actually_in_a_store_are_not() {
        assert_eq!(classify(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]), Blob::Other); // jpeg
        assert_eq!(classify(b"\x89PNG\r\n\x1a\n\x00\x00\x00\r"), Blob::Other); // png
        assert_eq!(classify(b"ID3\x04\x00\x00\x00\x00\x00\x00"), Blob::Other); // mp3
        assert_eq!(classify(b"PK\x03\x04\x14\x00\x00\x00"), Blob::Other); // zip / .osz

        // A `.osg`: same mode byte and version as its `.osr`, then binary.
        let mut osg = osr_head(0, 20230101);
        for byte in &mut osg[5..] {
            *byte = 0x00;
        }
        assert_eq!(classify(&osg), Blob::Other);
    }

    #[test]
    fn near_misses_do_not_sneak_through() {
        assert_eq!(classify(&[]), Blob::Other);
        assert_eq!(classify(b"osu file format"), Blob::Other);
        assert_eq!(classify(&osr_head(4, 20230101)), Blob::Other);
        assert_eq!(classify(&osr_head(0, 0)), Blob::Other);
        assert_eq!(classify(&osr_head(0, -5)), Blob::Other);

        let mut no_marker = osr_head(0, 20230101);
        no_marker[5] = 0x20;
        assert_eq!(classify(&no_marker), Blob::Other);
        let mut short_hash = osr_head(0, 20230101);
        short_hash[6] = 31;
        assert_eq!(classify(&short_hash), Blob::Other);
        let mut bad_hash = osr_head(0, 20230101);
        bad_hash[7] = b'z';
        assert_eq!(classify(&bad_hash), Blob::Other);
        assert_eq!(classify(&osr_head(0, 20230101)[..39]), Blob::Other);
    }
}
