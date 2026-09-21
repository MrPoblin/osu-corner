//! Reading `.osu` beatmap files.

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
