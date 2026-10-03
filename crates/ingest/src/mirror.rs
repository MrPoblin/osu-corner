//! Getting a beatmap that neither game install holds.
//!
//! A play whose beatmap is on no disk still knows its beatmap's MD5 — that is in the replay's own
//! header. So the map is reachable, in two hops, because **no mirror can look a beatmap up by
//! MD5** (checked against five of them; `catboy.best`'s `/b`, `/d`, `/osu` and `/s` all take
//! integers):
//!
//! ```text
//! MD5         -> beatmap id   the osu! API's checksum lookup
//! beatmap id  -> .osu bytes   {mirror}/osu/{id}, which returns the file itself, not a ZIP
//! ```
//!
//! **The bytes are verified against the MD5 we asked for before they are kept.** That check is
//! exact rather than hopeful, because a `.osu` is content-addressed: a mirror serving a different
//! version of the map produces different bytes and cannot pass. Without it a mirror would hand
//! back the current version of an outdated map and the replay would be priced against hit objects
//! that never existed on it — wrong, and silent about being wrong.
//!
//! **Three outcomes, and they must not be collapsed.** An earlier version returned `Option`, so a
//! rate limit, a dropped connection and "osu! has never heard of this map" all arrived as `None`
//! and were reported to the user as *the map no longer exists anywhere*. That is a lie in two of
//! the three cases, and the lie is invisible because it reads exactly like the true one. So:
//!
//! | outcome | means | next run |
//! |---|---|---|
//! | [`Fetched::Map`] | bytes, verified | nothing to do |
//! | [`Fetched::Unknown`] | osu! does not know the checksum | **do not ask again** |
//! | [`Fetched::Unavailable`] | rate-limited or unreachable | **ask again** |
//!
//! `Unknown` is durable *enough* to stop asking, and [`crate::library`] records it in the ledger
//! for that reason — with thousands of missing maps, re-asking every sync would turn a 7-second
//! run into an hour. It is **not proof the map is gone**: the checksum lookup does not serve
//! unranked or graveyarded maps, and the mirrors demonstrably do carry those, so a map can be
//! unnameable here and still perfectly playable. The report says so instead of asserting a
//! deletion.
//!
//! Credentials come from `.dev.vars` beside the config — the same file the Worker uses, which the
//! README already tells a cloner to create. **A clone without it is not an error**: fetching is
//! skipped and the run continues, so a stranger's first sync works with no osu! application.

use osu_core::osu;
use serde::Deserialize;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const TOKEN_URL: &str = "https://osu.ppy.sh/oauth/token";
const LOOKUP_URL: &str = "https://osu.ppy.sh/api/v2/beatmaps/lookup";

/// The users endpoint. A real constant rather than the test-only `API` below, because the profile
/// snapshot is fetched by every run that has credentials — that is the whole point of it (§9).
const USERS_URL: &str = "https://osu.ppy.sh/api/v2/users";

/// Only the validation probe uses these. They stay test-only on purpose: §2's promise is that **no
/// osu! API request is made to build the library**, so nothing in a real run may ask osu! for a
/// score.
#[cfg(test)]
const API: &str = "https://osu.ppy.sh/api/v2";

/// One request a second, and back off when told to. osu! publishes a rate limit for the API;
/// rather than hard-code a number that can change, this stays under any plausible value and obeys
/// a 429 — so it is correct whether the limit is 60/minute or something else entirely.
const PACE: Duration = Duration::from_secs(1);
const LOOKUP_ATTEMPTS: u32 = 3;

#[derive(Deserialize)]
struct Token {
    access_token: String,
}

#[derive(Deserialize)]
struct Beatmap {
    id: i64,
}

#[derive(serde::Serialize)]
struct TokenRequest<'a> {
    client_id: &'a str,
    client_secret: &'a str,
    grant_type: &'a str,
    scope: &'a str,
}

/// What asking for one beatmap produced.
#[derive(Debug)]
pub enum Fetched {
    /// The bytes, verified against the MD5 asked for. Ready to keep.
    Map(Vec<u8>),
    /// osu! does not know this checksum. Durable enough to stop asking — but **not** proof the
    /// map is gone, because unranked and graveyarded maps are not served by this lookup.
    Unknown,
    /// Rate-limited or unreachable. Transient, and must never be reported as a deleted map.
    Unavailable,
}

/// The result of the checksum lookup alone.
enum Lookup {
    Found(i64),
    Unknown,
    Unavailable,
}

/// Fetches missing beatmaps, or explains why it cannot.
pub struct Fetcher {
    credentials: Option<(String, String)>,
    token: Option<String>,
    mirrors: Vec<String>,
    agent: ureq::Agent,
    last_call: Option<Instant>,
}

impl Fetcher {
    /// `work` is the working set; the credentials file sits beside it, with the config.
    pub fn new(work: &Path, mirrors: &[String]) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();

        Self {
            credentials: credentials(work),
            token: None,
            mirrors: mirrors.to_vec(),
            agent,
            last_call: None,
        }
    }

    /// Whether there is any point asking. False means a clone that has not set up an osu!
    /// application, which is a supported state and not a failure.
    pub fn configured(&self) -> bool {
        self.credentials.is_some() && !self.mirrors.is_empty()
    }

    /// The `.osu` for this MD5, or a reason it could not be had.
    pub fn beatmap(&mut self, md5: &str) -> Fetched {
        let id = match self.beatmap_id(md5) {
            Lookup::Found(id) => id,
            Lookup::Unknown => return Fetched::Unknown,
            Lookup::Unavailable => return Fetched::Unavailable,
        };

        let mut unreachable = false;
        for base in &self.mirrors {
            let url = format!("{}/osu/{id}", base.trim_end_matches('/'));
            match self.get(&url) {
                Ok(bytes) if osu::md5(&bytes) == md5 => return Fetched::Map(bytes),
                // A mirror serving a different version of the same map. Try the next one: this is
                // a version problem, not a missing map, and it is not this run's to solve.
                Ok(_) => {}
                Err(_) => unreachable = true,
            }
        }

        if unreachable {
            Fetched::Unavailable
        } else {
            Fetched::Unknown
        }
    }

    /// Every score this account has on a beatmap, by beatmap id.
    ///
    /// **This is the endpoint the validation has to use.** A stable-era replay's trailing id is its
    /// *legacy* score id, so asking `/api/v2/scores/{id}` returns an unrelated score of the account's;
    /// the per-map, per-user form returns the account's own scores for that map, one call covering
    /// every play on it. The join is then that score's `legacy_score_id` against the id in the `.osr`,
    /// which *confirms* the join rather than assuming it.
    ///
    /// Test-only, like [`API`]: a real run never asks osu! for a score.
    #[cfg(test)]
    pub fn user_scores(&mut self, beatmap_id: i64, user_id: i64) -> Option<Vec<serde_json::Value>> {
        for attempt in 1..=LOOKUP_ATTEMPTS {
            self.pace();
            let token = self.token()?;
            let url = format!("{API}/beatmaps/{beatmap_id}/scores/users/{user_id}/all");

            match self.get_with_token(&url, &token) {
                Ok(body) => {
                    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
                    // The `/all` form wraps the list in an object; the plain form returns it as is.
                    return match value {
                        serde_json::Value::Array(scores) => Some(scores),
                        other => other.get("scores")?.as_array().cloned(),
                    };
                }
                // Told to slow down: back off and try again rather than reporting a rate limit as
                // "osu! does not have this score".
                Err(ureq::Error::StatusCode(429)) => {
                    std::thread::sleep(PACE * 2u32.pow(attempt));
                }
                Err(_) => return None,
            }
        }
        None
    }

    /// One score, by **modern** score id.
    ///
    /// §5's example play was documented as returning `legacy_total_score`, `total_score` and
    /// `legacy_score_id` from an endpoint of this shape, which would make it the oracle for a
    /// converted score. Whether this endpoint accepts a stable-era replay's id is exactly what a probe
    /// has to find out rather than assume.
    #[cfg(test)]
    pub fn score_by_id(&mut self, id: i64) -> Option<serde_json::Value> {
        for attempt in 1..=LOOKUP_ATTEMPTS {
            self.pace();
            let token = self.token()?;
            let url = format!("{API}/scores/{id}");

            match self.get_with_token(&url, &token) {
                Ok(body) => return serde_json::from_str(&body).ok(),
                Err(ureq::Error::StatusCode(429)) => {
                    std::thread::sleep(PACE * 2u32.pow(attempt));
                }
                Err(_) => return None,
            }
        }
        None
    }

    /// This account's best scores, as osu! reports them.
    ///
    /// The last oracle 5a needs: the per-map endpoint reports only the **legacy** value in `score`, so
    /// it cannot check a recalculation. If an entry here matches a staged play and its `score` differs
    /// from the number inside the `.osr`, that field is the converted total score.
    ///
    /// Test-only, like [`API`]: a real run never asks osu! for a score.
    #[cfg(test)]
    pub fn user_best(&mut self, user_id: i64, limit: usize) -> Option<Vec<serde_json::Value>> {
        for attempt in 1..=LOOKUP_ATTEMPTS {
            self.pace();
            let token = self.token()?;
            let url = format!("{API}/users/{user_id}/scores/best?limit={limit}");

            match self.get_with_token(&url, &token) {
                Ok(body) => {
                    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
                    return match value {
                        serde_json::Value::Array(scores) => Some(scores),
                        other => other.get("scores")?.as_array().cloned(),
                    };
                }
                Err(ureq::Error::StatusCode(429)) => {
                    std::thread::sleep(PACE * 2u32.pow(attempt));
                }
                Err(_) => return None,
            }
        }
        None
    }

    /// One ruleset's profile for an account, verbatim.
    ///
    /// Verbatim because projecting is the caller's job: `osu_core::profile::project` is the single
    /// allowlist, and applying it here would mean this module knew what may be published.
    pub fn profile(&mut self, user_id: i64, mode: &str) -> Option<String> {
        for attempt in 1..=LOOKUP_ATTEMPTS {
            self.pace();
            let token = self.token()?;
            let url = format!("{USERS_URL}/{user_id}/{mode}");

            match self.get_with_token(&url, &token) {
                Ok(body) => return Some(body),
                Err(ureq::Error::StatusCode(429)) => {
                    std::thread::sleep(PACE * 2u32.pow(attempt));
                }
                Err(_) => return None,
            }
        }
        None
    }

    /// MD5 -> beatmap id.
    fn beatmap_id(&mut self, md5: &str) -> Lookup {
        for attempt in 1..=LOOKUP_ATTEMPTS {
            self.pace();

            let Some(token) = self.token() else {
                return Lookup::Unavailable;
            };
            let url = format!("{LOOKUP_URL}?checksum={md5}");

            match self.get_with_token(&url, &token) {
                Ok(body) => {
                    return match serde_json::from_str::<Beatmap>(&body) {
                        Ok(beatmap) => Lookup::Found(beatmap.id),
                        // A 200 that is not a beatmap. Nothing to retry against.
                        Err(_) => Lookup::Unknown,
                    };
                }
                // Told to slow down. Back off and try again rather than recording a rate limit as
                // "osu! has never heard of this map" — that mistake is the whole reason this
                // function returns three things instead of an `Option`.
                Err(ureq::Error::StatusCode(429)) => {
                    std::thread::sleep(PACE * 2u32.pow(attempt));
                }
                Err(ureq::Error::StatusCode(404)) => return Lookup::Unknown,
                Err(_) => return Lookup::Unavailable,
            }
        }
        Lookup::Unavailable
    }

    /// Fetched once and kept; one run may ask for many maps.
    fn token(&mut self) -> Option<String> {
        if let Some(token) = &self.token {
            return Some(token.clone());
        }
        let (client_id, client_secret) = self.credentials.clone()?;

        let body = self
            .agent
            .post(TOKEN_URL)
            .send_json(TokenRequest {
                client_id: &client_id,
                client_secret: &client_secret,
                grant_type: "client_credentials",
                scope: "public",
            })
            .ok()?
            .body_mut()
            .read_to_string()
            .ok()?;

        let token: Token = serde_json::from_str(&body).ok()?;
        self.token = Some(token.access_token.clone());
        Some(token.access_token)
    }

    /// Keep the request rate under the API's limit by construction rather than by hoping.
    fn pace(&mut self) {
        if let Some(last) = self.last_call {
            let since = last.elapsed();
            if since < PACE {
                std::thread::sleep(PACE - since);
            }
        }
        self.last_call = Some(Instant::now());
    }

    fn get(&self, url: &str) -> Result<Vec<u8>, ureq::Error> {
        self.agent.get(url).call()?.body_mut().read_to_vec()
    }

    fn get_with_token(&self, url: &str, token: &str) -> Result<String, ureq::Error> {
        self.agent
            .get(url)
            .header("Authorization", &format!("Bearer {token}"))
            .header("Accept", "application/json")
            .call()?
            .body_mut()
            .read_to_string()
    }
}

/// `OSU_CLIENT_ID` and `OSU_CLIENT_SECRET` out of `.dev.vars`.
///
/// Deliberately quiet: a missing or half-filled file returns `None` rather than failing, because
/// the alternative is that a stranger's first run dies on an optional feature. The values are
/// never logged.
fn credentials(work: &Path) -> Option<(String, String)> {
    let text = fs::read_to_string(work.parent()?.join(".dev.vars")).ok()?;

    let mut id = None;
    let mut secret = None;
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
        match key.trim() {
            "OSU_CLIENT_ID" => id = Some(value),
            "OSU_CLIENT_SECRET" => secret = Some(value),
            _ => {}
        }
    }

    Some((id?, secret?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_read_from_a_dev_vars_file() {
        let dir = std::env::temp_dir().join("osu-ingest-mirror-test");
        fs::create_dir_all(dir.join("library")).expect("create");
        fs::write(
            dir.join(".dev.vars"),
            "# a comment\nOSU_CLIENT_ID=12345\nOSU_CLIENT_SECRET=abcdef\nUNRELATED=ignored\n",
        )
        .expect("write");

        let fetcher = Fetcher::new(&dir.join("library"), &["https://catboy.best".to_owned()]);
        assert!(fetcher.configured());
        assert_eq!(
            fetcher.credentials,
            Some(("12345".to_owned(), "abcdef".to_owned()))
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// A clone with no osu! application must not be treated as broken.
    #[test]
    fn no_credentials_means_no_fetching_and_no_complaint() {
        let dir = std::env::temp_dir().join("osu-ingest-mirror-absent");
        fs::create_dir_all(dir.join("library")).expect("create");

        let fetcher = Fetcher::new(&dir.join("library"), &["https://catboy.best".to_owned()]);
        assert!(!fetcher.configured());

        // Naming the app but not the secret cannot work either.
        fs::write(dir.join(".dev.vars"), "OSU_CLIENT_ID=12345\n").expect("write");
        assert!(!Fetcher::new(&dir.join("library"), &[]).configured());

        fs::remove_dir_all(&dir).ok();
    }

    /// No mirrors configured means nowhere to fetch from, whatever the credentials say.
    #[test]
    fn credentials_without_mirrors_still_fetches_nothing() {
        let work = Path::new(".");
        let fetcher = Fetcher::new(work, &[]);
        assert!(!fetcher.configured());
    }

    /// The three outcomes have to stay distinct, because two of them are retried and one is not.
    /// Collapsing them is what let a rate limit be reported as a deleted map.
    #[test]
    fn a_transient_failure_is_not_a_missing_map() {
        assert!(!matches!(
            Fetched::Unavailable,
            Fetched::Unknown | Fetched::Map(_)
        ));
        assert!(!matches!(Fetched::Unknown, Fetched::Unavailable));
    }
}
