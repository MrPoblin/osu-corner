//! Getting a beatmap that neither game install holds.
//!
//! A play whose beatmap is on no disk still knows its beatmap's MD5 — that is in the replay's own
//! header. So the map is reachable in two hops:
//!
//! ```text
//! MD5         -> beatmap id   a mirror's checksum lookup
//! beatmap id  -> .osu bytes   that mirror's own .osu route, which returns the file, not a ZIP
//! ```
//!
//! **Neither hop is the osu! API's.** The API serves the profile and nothing else, so the checksum
//! hop this used to make — `/api/v2/beatmaps/lookup?checksum=`, which was the only non-profile API
//! call on the production path — moved to a mirror. That is also why **credentials are not needed to
//! fetch a map**: a clone with no osu! application fetches exactly as well as one with.
//!
//! Only some mirrors can make the first hop. Most take integers and nothing else (`catboy.best`'s
//! `/b`, `/d`, `/osu` and `/s` all do), so a mirror *claims* the capability by naming a route for it
//! — see [`Mirror`]. A list where none claims it fetches nothing, which [`Fetcher::configured`]
//! reports rather than disguising as a missing map.
//!
//! **The bytes are verified against the MD5 we asked for before they are kept.** That check is
//! exact rather than hopeful, because a `.osu` is content-addressed: a mirror serving a different
//! version of the map produces different bytes and cannot pass. Without it a mirror would hand
//! back the current version of an outdated map and the replay would be priced against hit objects
//! that never existed on it — wrong, and silent about being wrong.
//!
//! **Three outcomes, and they must not be collapsed.** An earlier version returned `Option`, so a
//! rate limit, a dropped connection and "this map does not exist" all arrived as `None` and were
//! reported to the user as *the map no longer exists anywhere*. That is a lie in two of the three
//! cases, and the lie is invisible because it reads exactly like the true one. So:
//!
//! | outcome | means | next run |
//! |---|---|---|
//! | [`Fetched::Map`] | bytes, verified | nothing to do |
//! | [`Fetched::Unknown`] | a mirror looked, and does not hold it | **do not ask again** |
//! | [`Fetched::Unavailable`] | rate-limited or unreachable | **ask again** |
//!
//! `Unknown` is durable *enough* to stop asking, and [`crate::library`] records it in the ledger
//! for that reason — with thousands of missing maps, re-asking every sync would turn a 7-second
//! run into an hour. It is **not proof the map is gone**: a mirror calls a map it does not hold
//! missing whether or not the map still exists anywhere, so a perfectly playable map can be
//! unnameable from here. The report says so instead of asserting a deletion.
//!
//! **Which is why the first hop asks the batch route about one MD5.** The single-MD5 route answers a
//! bare `404` both for "no such map" and for "could not resolve right now" — the second of which is
//! what happens once the mirror's own upstream budget is spent — and recording the second as the
//! first would stop this project ever asking again. The batch route separates them (`missing` from
//! `unresolved`, with a `reason`), and asking it about one MD5 costs exactly the same request.
//!
//! Credentials come from `.dev.vars` beside the config — the same file the Worker uses, which the
//! README already tells a cloner to create. They gate **the profile and nothing else**; their
//! absence is a supported state rather than a failure.

use osu_core::osu;
use serde::Deserialize;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const TOKEN_URL: &str = "https://osu.ppy.sh/oauth/token";

/// The users endpoint. A real constant rather than the test-only `API` below, because the profile
/// snapshot is fetched by every run that has credentials — that is the whole point of it (§9).
const USERS_URL: &str = "https://osu.ppy.sh/api/v2/users";

/// Only the validation probe uses these. They stay test-only on purpose: §2's promise is that **no
/// osu! API request is made to build the library**, so nothing in a real run may ask osu! for a
/// score.
#[cfg(test)]
const API: &str = "https://osu.ppy.sh/api/v2";

/// One request a second, and back off when told to.
///
/// This is no longer a documented limit being obeyed — the mirror's JSON lane is unlimited and its
/// byte lane allows a thousand requests a minute per IP. It is being a good citizen to a service
/// one person pays for out of pocket and earns nothing from, and it stays under every number a
/// mirror might publish. A `429`, or the `503` a saturated byte lane returns, still backs off.
const PACE: Duration = Duration::from_secs(1);
const LOOKUP_ATTEMPTS: u32 = 3;

/// One configured mirror.
///
/// A bare string is the common case: it serves `{base}/osu/{id}` and cannot look a beatmap up by
/// checksum. The table form exists because **mirrors do not agree on their routes** — the one that
/// aggregates the others serves a raw `.osu` at `/api/osu/{id}` and answers `404` at the `/osu/{id}`
/// nearly every other mirror uses — and because resolving a checksum is a capability, not a given.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Mirror {
    /// `{base}/osu/{id}`, and no checksum lookup.
    Bare(String),
    /// Named routes, and possibly a checksum lookup.
    Routed(Routed),
}

/// A mirror whose routes are not the common shape.
#[derive(Debug, Clone, Deserialize)]
pub struct Routed {
    /// The base URL. A trailing slash is harmless.
    url: String,
    /// Where the raw `.osu` for a beatmap id lives; `{id}` is substituted.
    #[serde(default = "common_osu")]
    osu: String,
    /// Where a checksum becomes a beatmap id; `{md5}` is substituted. The answer must be the batch
    /// shape — see [`Batch`] — because that is the one that separates "not held" from "not
    /// answered".
    ///
    /// **Absent means this mirror cannot resolve**, which is the ordinary case: most mirrors take
    /// integers only.
    #[serde(default)]
    md5: Option<String>,
}

fn common_osu() -> String {
    "/osu/{id}".to_owned()
}

impl Mirror {
    /// The base URL, for reporting.
    pub fn url(&self) -> &str {
        match self {
            Mirror::Bare(url) => url,
            Mirror::Routed(routed) => &routed.url,
        }
    }

    /// Whether this mirror can turn a checksum into a beatmap id.
    ///
    /// Without one of these in the list a map no install holds is reported rather than fetched, so
    /// this is what [`Fetcher::configured`] answers with.
    pub fn can_resolve(&self) -> bool {
        matches!(self, Mirror::Routed(routed) if routed.md5.is_some())
    }

    /// Where the raw `.osu` for this beatmap id lives on this mirror.
    pub fn osu(&self, id: i64) -> String {
        let path = match self {
            Mirror::Bare(_) => common_osu(),
            Mirror::Routed(routed) => routed.osu.clone(),
        };
        format!(
            "{}{}",
            self.url().trim_end_matches('/'),
            path.replace("{id}", &id.to_string())
        )
    }

    /// Where this mirror turns a checksum into a beatmap id, if it can.
    pub fn resolve(&self, md5: &str) -> Option<String> {
        let Mirror::Routed(routed) = self else {
            return None;
        };
        let path = routed.md5.as_deref()?;
        Some(format!(
            "{}{}",
            routed.url.trim_end_matches('/'),
            path.replace("{md5}", md5)
        ))
    }
}

#[derive(Deserialize)]
struct Token {
    access_token: String,
}

#[derive(Deserialize)]
struct Beatmap {
    id: i64,
}

/// The checksum lookup's answer.
///
/// Parsed tolerantly, and read **only** through [`Batch::id`] and [`Batch::says_missing`], because
/// the difference between those two is the entire reason this route is used instead of the
/// single-MD5 one.
///
/// The response also carries an `unresolved` list, with a `reason`, and it is deliberately **not** a
/// field here: an answer that is neither a result nor a `missing` entry already means nothing was
/// learned, which is precisely what those two methods express between them. Reading the reason
/// would change no outcome — it would only name why the transient case was transient.
#[derive(Deserialize)]
struct Batch {
    #[serde(default)]
    results: Vec<Beatmap>,
    #[serde(default)]
    missing: Option<Bucket>,
}

/// The checksums the batch answer positively reported it does not hold.
#[derive(Deserialize)]
struct Bucket {
    #[serde(default)]
    md5: Vec<String>,
}

impl Batch {
    /// The beatmap id this answer names, if it names one.
    fn id(&self) -> Option<i64> {
        self.results.first().map(|beatmap| beatmap.id)
    }

    /// Whether the mirror positively said it does not hold this checksum.
    ///
    /// **Not** the same as "this answer does not contain it". An `unresolved` answer says nothing
    /// was learned, and treating it as a durable "no such map" would record a wrong negative that
    /// this project never revisits.
    fn says_missing(&self, md5: &str) -> bool {
        self.missing
            .as_ref()
            .is_some_and(|bucket| bucket.md5.iter().any(|hash| hash == md5))
    }
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
    /// A mirror looked, and does not hold this checksum. Durable enough to stop asking — but
    /// **not** proof the map is gone: a mirror reports a map it does not hold as missing whether or
    /// not the map still exists.
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
    mirrors: Vec<Mirror>,
    agent: ureq::Agent,
    last_call: Option<Instant>,
}

impl Fetcher {
    /// `work` is the working set; the credentials file sits beside it, with the config.
    pub fn new(work: &Path, mirrors: &[Mirror]) -> Self {
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

    /// Whether there is any point asking for a beatmap no install holds.
    ///
    /// **Not a question about credentials.** They gate the profile and nothing else now, so an osu!
    /// application is irrelevant here — what fetching a map needs is a mirror that can turn a
    /// checksum into a beatmap id, and most mirrors cannot. False means such maps are reported
    /// rather than fetched, which is a supported state and not a failure.
    pub fn configured(&self) -> bool {
        self.mirrors.iter().any(Mirror::can_resolve)
    }

    /// The `.osu` for this MD5, or a reason it could not be had.
    pub fn beatmap(&mut self, md5: &str) -> Fetched {
        let id = match self.resolve(md5) {
            Lookup::Found(id) => id,
            Lookup::Unknown => return Fetched::Unknown,
            Lookup::Unavailable => return Fetched::Unavailable,
        };

        // Collected before the loop, because pacing needs `&mut self` and the loop would otherwise
        // be holding a borrow of the mirror list at the same time.
        let urls: Vec<String> = self.mirrors.iter().map(|mirror| mirror.osu(id)).collect();

        let mut unreachable = false;
        for url in &urls {
            // Paced here, where the real bandwidth leaves the mirror's servers, and not only in
            // `resolve` — the byte route is the one worth being gentle with.
            self.pace();

            match self.get(url) {
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

    /// MD5 -> beatmap id, through the first mirror that can do it.
    ///
    /// **Not the osu! API.** The API serves the profile and nothing else (§2), so the checksum hop
    /// that used to live there is a mirror's now — which is also why this needs no token, and why a
    /// clone with no osu! application can still fetch a map it does not hold.
    ///
    /// **The batch route, for one MD5, on purpose.** It costs the same request as the single-MD5
    /// route and it is the only one that distinguishes a mirror saying *I do not hold this* (durable
    /// — recorded, never asked again) from *I could not answer right now* (transient — asked again
    /// next run, nothing recorded). The single-MD5 route answers a bare `404` for both, and
    /// collapsing them would record a wrong negative that this project never revisits.
    ///
    /// Only a mirror that names an `md5` route is asked, and a `404` on that route is treated as a
    /// *broken configuration* rather than as a missing map: the batch route answers `200` even for a
    /// checksum it does not know, so a `404` means the configured route is wrong, and reporting
    /// every map as permanently gone because of a typo would be the worst possible reading.
    fn resolve(&mut self, md5: &str) -> Lookup {
        // One URL per mirror that claims the capability, collected up front for the same reason as
        // in `beatmap`: pacing needs `&mut self`. Empty means nobody can look a checksum up, which
        // is a config state — see `configured` — and not an answer about this map.
        let urls: Vec<String> = self
            .mirrors
            .iter()
            .filter_map(|mirror| mirror.resolve(md5))
            .collect();

        for url in &urls {
            for attempt in 1..=LOOKUP_ATTEMPTS {
                self.pace();

                match self.get(url) {
                    Ok(body) => {
                        let Ok(batch) = serde_json::from_slice::<Batch>(&body) else {
                            // A 200 that is not this shape. Nothing to retry against, and nothing
                            // was learned — so it must not become a durable "no such map".
                            return Lookup::Unavailable;
                        };
                        if let Some(id) = batch.id() {
                            return Lookup::Found(id);
                        }
                        if batch.says_missing(md5) {
                            return Lookup::Unknown;
                        }
                        // `unresolved`, or an answer that names this MD5 nowhere. Transient by
                        // construction: the mirror never said it does not hold it.
                        return Lookup::Unavailable;
                    }
                    // Told to slow down. Back off and try again rather than recording a rate limit
                    // as "no such map" — that mistake is the whole reason this returns three
                    // things instead of an `Option`.
                    Err(ureq::Error::StatusCode(429 | 503)) => {
                        std::thread::sleep(PACE * 2u32.pow(attempt));
                    }
                    // This mirror could not answer, so try the next one and learn nothing. **A
                    // `404` belongs here rather than in the `missing` bucket**: the batch route
                    // answers `200` even for a checksum it does not know, so a `404` means the
                    // configured route is wrong, and reporting every map as permanently gone
                    // because of a typo would be the worst possible reading of it.
                    Err(_) => break,
                }
            }
        }

        // Either no mirror can resolve and none was asked, or none that was asked answered. Nothing
        // is known — and `Unknown` is the one outcome that records something, so it needs a real
        // answer saying so. It is never reached from here.
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

    /// Keep the request rate down by construction rather than by hoping.
    ///
    /// Applied to both hops, because both are requests to somebody else's server, and the mirror
    /// this project now resolves through is one person's out-of-pocket hobby rather than an
    /// infrastructure company.
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
/// these gate **the profile and nothing else** — a map is fetched without them — and a stranger's
/// first run must not die on an optional feature. The values are never logged.
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

    /// A mirror of the ordinary kind: one route, no way to turn a checksum into an id.
    fn bare(url: &str) -> Mirror {
        Mirror::Bare(url.to_owned())
    }

    /// The aggregator this project resolves through: a raw `.osu` where no other mirror serves
    /// one, and the batch route that turns a checksum into an id.
    fn routed() -> Mirror {
        Mirror::Routed(Routed {
            url: "https://mirror.hinamizawa.ai/".to_owned(),
            osu: "/api/osu/{id}".to_owned(),
            md5: Some("/v3/osu/beatmaps/batch?md5={md5}".to_owned()),
        })
    }

    #[test]
    fn credentials_are_read_from_a_dev_vars_file() {
        let dir = std::env::temp_dir().join("osu-ingest-mirror-test");
        fs::create_dir_all(dir.join("library")).expect("create");
        fs::write(
            dir.join(".dev.vars"),
            "# a comment\nOSU_CLIENT_ID=12345\nOSU_CLIENT_SECRET=abcdef\nUNRELATED=ignored\n",
        )
        .expect("write");

        let fetcher = Fetcher::new(&dir.join("library"), &[bare("https://catboy.best")]);
        assert_eq!(
            fetcher.credentials,
            Some(("12345".to_owned(), "abcdef".to_owned()))
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// A bare mirror is the common shape, and it cannot resolve — which is a property of the
    /// mirror rather than a failure, so it is reported rather than worked around.
    #[test]
    fn a_bare_mirror_serves_the_common_shape_and_cannot_resolve() {
        let mirror = bare("https://catboy.best");
        assert_eq!(mirror.osu(175250), "https://catboy.best/osu/175250");
        assert_eq!(mirror.resolve("abc"), None);
        assert!(!mirror.can_resolve());
        assert_eq!(mirror.url(), "https://catboy.best");
    }

    /// A trailing slash on the base must not produce a doubled one: the committed default has none,
    /// but a hand-written mirror list is exactly where one appears.
    #[test]
    fn a_routed_mirror_uses_its_own_paths_and_does_not_double_its_slash() {
        let mirror = routed();
        assert_eq!(
            mirror.osu(175250),
            "https://mirror.hinamizawa.ai/api/osu/175250"
        );
        assert_eq!(
            mirror.resolve("abc"),
            Some("https://mirror.hinamizawa.ai/v3/osu/beatmaps/batch?md5=abc".to_owned())
        );
        assert!(mirror.can_resolve());
    }

    /// The distinction the first hop exists for. A mirror saying *I do not hold it* is durable; a
    /// mirror saying *I could not answer* is not, and collapsing them records a map as gone for
    /// good. Shapes taken from a real response, `upstream_cap` included.
    #[test]
    fn a_map_a_mirror_lacks_and_a_map_it_could_not_answer_for_are_different() {
        let held: Batch = serde_json::from_str(
            r#"{"results":[{"id":175250}],"missing":{"md5":[]},"unresolved":{"md5":[],"reason":null}}"#,
        )
        .expect("parses");
        assert_eq!(held.id(), Some(175250));
        assert!(!held.says_missing("aaa"));

        let missing: Batch = serde_json::from_str(
            r#"{"results":[],"missing":{"md5":["aaa"]},"unresolved":{"md5":[],"reason":null}}"#,
        )
        .expect("parses");
        assert!(missing.says_missing("aaa"));
        assert_eq!(missing.id(), None);

        let unresolved: Batch = serde_json::from_str(
            r#"{"results":[],"missing":{"md5":[]},"unresolved":{"md5":["aaa"],"reason":"upstream_cap"}}"#,
        )
        .expect("parses");
        assert!(
            !unresolved.says_missing("aaa"),
            "unresolved must not read as missing, or a wrong negative is recorded forever"
        );
        assert_eq!(unresolved.id(), None);
    }

    /// An answer with none of the expected fields must claim nothing at all.
    #[test]
    fn an_unrecognised_answer_claims_nothing() {
        let odd: Batch = serde_json::from_str("{}").expect("parses");
        assert_eq!(odd.id(), None);
        assert!(!odd.says_missing("aaa"));
    }

    /// The whole point of moving the checksum hop onto a mirror: a clone with no osu! application
    /// fetches a map exactly as well as one with. Credentials gate the profile and nothing else.
    #[test]
    fn fetching_a_map_needs_a_resolver_not_an_osu_application() {
        let dir = std::env::temp_dir().join("osu-ingest-mirror-no-creds");
        fs::create_dir_all(dir.join("library")).expect("create");

        let fetcher = Fetcher::new(&dir.join("library"), &[routed()]);
        assert_eq!(fetcher.credentials, None, "no .dev.vars here");
        assert!(
            fetcher.configured(),
            "nothing to authenticate against, and maps are still fetchable"
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// Mirrors that cannot resolve leave a map no install holds unreachable, and saying so quietly
    /// is the whole job of `configured`.
    #[test]
    fn mirrors_that_cannot_resolve_leave_nothing_to_ask() {
        let work = Path::new(".");
        assert!(!Fetcher::new(work, &[]).configured());
        assert!(!Fetcher::new(work, &[bare("https://catboy.best")]).configured());
        assert!(Fetcher::new(work, &[bare("https://osu.direct"), routed()]).configured());
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
