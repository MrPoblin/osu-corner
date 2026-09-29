//! `.dev.vars`, the file secrets live in and nothing else does (§14).
//!
//! Two parts of the ingest read it — the beatmap fetcher for its osu! client credentials, and the
//! store uploader for its access key pair — so the parsing lives here rather than in either of them.
//!
//! Deliberately quiet: a missing, empty or half-filled file yields nothing rather than an error,
//! because the alternative is that a stranger's first run dies on a feature they have not set up.
//! The values are never logged, and never stored anywhere but this file.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// The key/value pairs in the `.dev.vars` beside the working set. Comments and blanks are skipped,
/// and surrounding quotes are stripped, because a `.env` file is written by hand.
pub(crate) fn read(work: &Path) -> HashMap<String, String> {
    let Some(text) = work
        .parent()
        .and_then(|dir| fs::read_to_string(dir.join(".dev.vars")).ok())
    else {
        return HashMap::new();
    };

    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'').to_owned();
        if value.is_empty() {
            continue;
        }
        out.insert(key.trim().to_owned(), value);
    }
    out
}

/// Two values, or `None` when either is missing — half a credential is not a credential.
pub(crate) fn pair(
    vars: &HashMap<String, String>,
    first: &str,
    second: &str,
) -> Option<(String, String)> {
    Some((vars.get(first)?.clone(), vars.get(second)?.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_quotes_and_blanks_are_handled() {
        let dir = std::env::temp_dir().join(format!("osu-ingest-devvars-{}", std::process::id()));
        let work = dir.join("library");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(
            dir.join(".dev.vars"),
            "# a comment\n\nA=one\nB = \"two\"\nC='three'\nD=\nnot-a-pair\n",
        )
        .unwrap();

        let vars = read(&work);
        assert_eq!(vars.get("A").map(String::as_str), Some("one"));
        assert_eq!(vars.get("B").map(String::as_str), Some("two"));
        assert_eq!(vars.get("C").map(String::as_str), Some("three"));
        assert!(!vars.contains_key("D"), "an empty value is not a value");
        assert_eq!(vars.len(), 3);

        assert_eq!(
            pair(&vars, "A", "B"),
            Some(("one".to_owned(), "two".to_owned()))
        );
        assert_eq!(pair(&vars, "A", "D"), None, "half a pair is not a pair");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let dir =
            std::env::temp_dir().join(format!("osu-ingest-devvars-none-{}", std::process::id()));
        assert!(read(&dir.join("library")).is_empty());
    }
}
