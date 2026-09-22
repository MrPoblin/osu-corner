#![forbid(unsafe_code)]

//! Local ingest: the library's game folders in, the working set, index and R2 objects out. Never
//! deployed — it reads game installs and writes only inside `library/`.
//!
//! Today it builds the working set — the maps a play references, every replay, and the ledger that
//! makes a re-sync cheap — and writes the index the site reads. Upload to R2 is not implemented yet.

mod collect;
mod index;
mod ledger;
mod library;
mod mirror;
mod pp;

use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Deserialize)]
struct Config {
    site: Site,
    #[serde(default)]
    source: Vec<Source>,
    #[serde(default)]
    user: Vec<User>,
    r2: R2,
    mirrors: Mirrors,
}

#[derive(Deserialize)]
struct Site {
    name: String,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Lazer,
    Stable,
}

#[derive(Deserialize)]
pub struct Source {
    pub kind: SourceKind,
    pub path: PathBuf,
    /// A disabled source is never opened, so pointing at an uninstalled game is harmless.
    /// Defaults to on: naming a source and leaving the switch out means you meant it.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

#[derive(Deserialize)]
struct User {
    id: i64,
    names: Vec<String>,
}

#[derive(Deserialize)]
struct R2 {
    host: String,
    bucket: String,
}

#[derive(Deserialize)]
struct Mirrors {
    urls: Vec<String>,
}

fn main() -> ExitCode {
    let mut dry_run = false;
    let mut config_path: Option<String> = None;
    // Bounded on purpose. With thousands of missing maps — which is what pruning your library
    // leaves behind — an unbounded first run would sit on the osu! API for an hour before printing
    // anything. The rest are looked for on the next run, and `0` removes the bound.
    let mut fetch_limit: usize = 100;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].clone();
        index += 1;
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--fetch-limit" => {
                let Some(value) = args.get(index) else {
                    eprintln!("--fetch-limit needs a number. Try --help.");
                    return ExitCode::FAILURE;
                };
                index += 1;
                match value.parse::<usize>() {
                    Ok(number) => fetch_limit = number,
                    Err(_) => {
                        eprintln!("--fetch-limit {value:?} is not a number. Try --help.");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "-h" | "--help" => {
                print_help();
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                eprintln!("unknown option {other}. Try --help.");
                return ExitCode::FAILURE;
            }
            other => config_path = Some(other.to_owned()),
        }
    }

    // Zero means no limit; `take` cannot express "all", so it gets the largest value it can.
    let fetch_limit = if fetch_limit == 0 {
        usize::MAX
    } else {
        fetch_limit
    };

    let path = config_path.unwrap_or_else(|| "osu-corner.toml".to_owned());
    let config = match load(Path::new(&path)) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    // The working set sits beside the config, so `--config elsewhere/x.toml` keeps its library
    // with it rather than writing into whatever directory the shell happened to be in.
    let work = Path::new(&path)
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("library");

    // Echo what was understood before doing anything. Every field is read here on purpose: a
    // typo'd section must be visible, not silently ignored.
    println!("{}", config.site.name);
    for user in &config.user {
        println!("  account {} — {:?}", user.id, user.names);
    }
    println!("  mirrors: {}", config.mirrors.urls.join(", "));

    let r2 = if config.r2.host.is_empty() || config.r2.bucket.is_empty() {
        "not configured yet".to_owned()
    } else {
        format!("{} / {}", config.r2.host, config.r2.bucket)
    };
    println!("  R2: {r2}");

    // Whether a beatmap no install holds can be fetched needs the osu! application's credentials.
    // Absence is a supported state, not a failure — it is what a fresh clone looks like.
    println!(
        "  fetching: {}",
        if work
            .parent()
            .is_some_and(|dir| dir.join(".dev.vars").exists())
        {
            "on — a beatmap no install holds is fetched and MD5-verified"
        } else {
            "off — no .dev.vars, so such maps are reported rather than fetched"
        }
    );

    let enabled: Vec<&Source> = config
        .source
        .iter()
        .filter(|source| source.enabled)
        .collect();
    println!(
        "  sources: {} enabled, {} disabled",
        enabled.len(),
        config.source.len() - enabled.len()
    );

    if enabled.is_empty() {
        eprintln!(
            "\nno enabled [[source]] — there is nothing to collect.\n\
             Add them to {} (gitignored), in the shape shown at the bottom of {}.",
            local_sibling(Path::new(&path)).display(),
            path,
        );
        return ExitCode::FAILURE;
    }

    match library::build(&enabled, &work, &config.mirrors.urls, fetch_limit, dry_run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("\n{error}");
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!(
        "osu-ingest — build the local working set from your osu! installs\n\n\
         USAGE:\n    osu-ingest [OPTIONS] [CONFIG]\n\n\
         ARGS:\n    CONFIG      path to the committed defaults (default: osu-corner.toml).\n                \
         A sibling <name>.local.toml is merged over it if present.\n\n\
         OPTIONS:\n    --dry-run   report what would be staged without writing anything\n    \
         --fetch-limit N\n                \
         how many beatmaps no install holds to look for this run\n                \
         (default 100; 0 means no limit). Each takes about a second,\n                \
         paced to the osu! API, and the rest wait until the next run.\n    \
         -h, --help  show this\n\n\
         Reads the game folders. Writes only inside library/ beside the config."
    );
}

// ---------------------------------------------------------------- configuration

/// Read the committed defaults, then layer the caller's own file over them.
///
/// Two files rather than one because the committed file has to stay generic: a stranger's clone
/// must build and deploy untouched, and a path from somebody's machine is wrong for everyone
/// else. Keeping personal values out of the repository is a separate benefit, not the reason.
///
/// The merge is key-by-key for tables and **whole-array for arrays**. A `[[source]]` list in the
/// local file replaces the defaults' list rather than extending it, which is the only reading
/// that makes sense for a list of game installs.
fn load(path: &Path) -> Result<Config, String> {
    let defaults = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut table: toml::Table = toml::from_str(&defaults)
        .map_err(|error| format!("{} did not parse: {error}", path.display()))?;

    let local = local_sibling(path);
    if local.exists() {
        let raw = fs::read_to_string(&local)
            .map_err(|error| format!("cannot read {}: {error}", local.display()))?;
        let overrides: toml::Table = toml::from_str(&raw)
            .map_err(|error| format!("{} did not parse: {error}", local.display()))?;
        overlay(&mut table, overrides);
        println!("  merged {}", local.display());
    }

    toml::Value::Table(table)
        .try_into()
        .map_err(|error| format!("{path:?} is missing something ingest needs: {error}"))
}

/// `osu-corner.toml` → `osu-corner.local.toml`, beside it.
fn local_sibling(config: &Path) -> PathBuf {
    let stem = config
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("osu-corner");
    config.with_file_name(format!("{stem}.local.toml"))
}

fn overlay(base: &mut toml::Table, overrides: toml::Table) {
    for (key, value) in overrides {
        let merges = value.is_table() && base.get(&key).is_some_and(toml::Value::is_table);

        if merges {
            // Both sides were just checked to be tables.
            if let (Some(toml::Value::Table(base_table)), toml::Value::Table(over_table)) =
                (base.get_mut(&key), value)
            {
                overlay(base_table, over_table);
            }
        } else {
            // Scalars and whole arrays replace. Replacing is what stops a local `[[source]]`
            // list from being appended to the defaults' one.
            base.insert(key, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> toml::Table {
        toml::from_str(text).expect("test fixture must parse")
    }

    /// The committed file is the generic case. That is the entire point of it: a clone builds
    /// and deploys untouched. A source path or an account id in here is the mistake this shape
    /// exists to prevent, so the test asserts their absence rather than their presence.
    #[test]
    fn the_committed_config_is_generic() {
        let config: Config = toml::from_str(include_str!("../../../osu-corner.toml"))
            .expect("osu-corner.toml must parse");

        assert!(!config.site.name.is_empty());
        assert!(!config.mirrors.urls.is_empty());
        assert!(
            config.source.is_empty(),
            "install paths belong in osu-corner.local.toml, not in the repository"
        );
        assert!(
            config.user.is_empty(),
            "account ids and usernames belong in osu-corner.local.toml"
        );
    }

    #[test]
    fn tables_merge_and_arrays_replace() {
        let mut base = parse(
            "[site]\nname = 'default'\n\n[r2]\nhost = 'h'\nbucket = 'b'\n\n\
             [mirrors]\nurls = ['https://default']\n\n\
             [[source]]\nkind = 'lazer'\npath = '/placeholder'\n",
        );
        let overrides = parse(
            "[site]\nname = 'mine'\n\n\
             [[source]]\nkind = 'stable'\npath = '/real'\n",
        );

        overlay(&mut base, overrides);
        let config: Config = toml::Value::Table(base)
            .try_into()
            .expect("merged table must deserialize");

        assert_eq!(config.site.name, "mine", "a scalar is replaced");
        assert_eq!(config.r2.host, "h", "an untouched sibling table survives");
        assert_eq!(config.r2.bucket, "b");
        assert_eq!(
            config.mirrors.urls,
            vec!["https://default"],
            "an untouched array survives"
        );
        assert_eq!(config.source.len(), 1, "arrays replace, they do not append");
        assert_eq!(config.source[0].path, PathBuf::from("/real"));
        assert_eq!(config.source[0].kind, SourceKind::Stable);
    }

    /// `enabled` is the one field with a default, and it has to default to on: naming a source
    /// and leaving the switch out means you meant it.
    #[test]
    fn a_source_without_the_switch_is_enabled() {
        let config: Config = toml::from_str(
            "[site]\nname = 'x'\n[r2]\nhost = ''\nbucket = ''\n[mirrors]\nurls = []\n\
             [[source]]\nkind = 'lazer'\npath = '/tmp/x'\n",
        )
        .expect("minimal config must parse");

        assert!(config.source[0].enabled);
        assert_eq!(config.source[0].kind, SourceKind::Lazer);
    }

    #[test]
    fn the_local_config_is_a_sibling() {
        assert_eq!(
            local_sibling(Path::new("/repo/osu-corner.toml")),
            PathBuf::from("/repo/osu-corner.local.toml")
        );
        assert_eq!(
            local_sibling(Path::new("osu-corner.toml")),
            PathBuf::from("osu-corner.local.toml")
        );
    }
}
