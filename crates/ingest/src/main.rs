#![forbid(unsafe_code)]

//! Local ingest: an osu!lazer export in, the library index and R2 objects out. Never
//! deployed — the export is a local directory and the derived index is committed.

use serde::Deserialize;
use std::process::ExitCode;

#[derive(Deserialize)]
struct Config {
    site: Site,
    user: Vec<User>,
    r2: R2,
    mirrors: Mirrors,
}

#[derive(Deserialize)]
struct Site {
    name: String,
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
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "osu-corner.toml".to_owned());

    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) => {
            eprintln!("cannot read {path}: {error}");
            return ExitCode::FAILURE;
        }
    };

    let config: Config = match toml::from_str(&raw) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{path} did not parse: {error}");
            return ExitCode::FAILURE;
        }
    };

    println!("{}", config.site.name);
    for user in &config.user {
        println!("  account {} — names {:?}", user.id, user.names);
    }

    if config.r2.host.is_empty() || config.r2.bucket.is_empty() {
        println!("  R2 not configured, so replays cannot be uploaded");
    } else {
        println!("  R2 {} / {}", config.r2.host, config.r2.bucket);
    }

    println!("  mirrors, in order: {}", config.mirrors.urls.join(", "));
    println!();
    println!("No export is read and no index is written yet.");

    ExitCode::SUCCESS
}
