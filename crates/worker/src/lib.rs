#![forbid(unsafe_code)]

//! Thin `workers-rs` adapter. Logic belongs in `corner-core`.
//!
//! Two routes. `/api/health` exists so a deploy can be confirmed from outside against the SHA that
//! triggered it. `/api/osu/profile` is the corner's profile payload, cached so osu! is called about once an
//! hour rather than once per visitor — which is not an optimisation but the condition of having API
//! access at all.
//!
//! **Every osu route is `/api/osu/<thing>`.** One rule rather than a judgement per route: the next
//! osu-derived route takes a leaf here rather than a new top-level name, and nothing that is not
//! about osu belongs under the prefix.

use corner_core::Store;
use worker::*;

mod profile;
mod store;
mod token;
mod transport;

use store::CacheApiStore;

/// The profile entry's cache key prefix. The store resolves it against the incoming request's own
/// origin, so no hostname is written into this code.
const PROFILE_KEY: &str = "/__cache/osu:profile";

/// The cache key for one ruleset's profile.
///
/// **The ruleset has to be part of the key.** Without it the first mode fetched answers for every
/// other mode until the entry expires — which is exactly what this did before the ruleset was added:
/// `?mode=taiko` served the osu! profile for an hour, and nothing about the response said so.
fn profile_key(mode: Option<&str>) -> String {
    match mode {
        Some(mode) => format!("{PROFILE_KEY}:{mode}"),
        // Not `:osu`. An unspecified mode means the account's own ruleset, which is a different
        // request from asking for osu! explicitly, and would answer differently for a taiko main.
        None => format!("{PROFILE_KEY}:default"),
    }
}

/// The base every profile request is built from. Not configurable: osu! has one API version, and a
/// second one would be a different route rather than a setting.
const OSU_API: &str = "https://osu.ppy.sh/api/v2";

/// The rulesets osu! answers for. An unrecognised `mode` is refused here rather than forwarded, so
/// this route can never become an open query forwarder onto osu!'s API.
const MODES: &[&str] = &["osu", "taiko", "fruits", "mania"];

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    // The `Context` rides in the router's data because it is where `wait_until` lives, and a
    // background refresh is the only way to serve a stale entry without also waiting on its
    // replacement. A path that matches no route falls through to the router's own 404.
    Router::with_data(ctx)
        .get_async("/api/health", health)
        .get_async("/api/osu/profile", osu)
        .run(req, env)
        .await
}

/// Liveness and the deployed commit, so a deploy can be confirmed from outside.
async fn health(_req: Request, _ctx: RouteContext<Context>) -> Result<Response> {
    Response::from_json(&serde_json::json!({
        "ok": true,
        "commit": option_env!("GIT_SHA").unwrap_or("dev"),
    }))
}

/// The profile payload, projected to [`profile::PROFILE_FIELDS`].
async fn osu(req: Request, ctx: RouteContext<Context>) -> Result<Response> {
    match serve_profile(&req, &ctx).await {
        Ok(response) => Ok(response),
        Err(failure) => failure.response(),
    }
}

async fn serve_profile(req: &Request, ctx: &RouteContext<Context>) -> Result<Response, Failure> {
    let now = Date::now().as_millis() as i64 / 1000;

    let request_url = req.url().map_err(|_| Failure::Shape)?;
    let origin = request_url.origin().ascii_serialization();

    // Never hardcoded: whose profile this route serves is configuration, not code. `[[user]] id` in
    // `osu-corner.local.toml` is the same value, but the Worker has no TOML reader — its config
    // arrives as variables — so this reads `OSU_PROFILE_USER`, which is set from that same config.
    let user = variable(&ctx.env, "OSU_PROFILE_USER")?;
    let mode = mode(&request_url)?;

    let endpoint = match &mode {
        Some(mode) => format!("{OSU_API}/users/{user}/{mode}"),
        None => format!("{OSU_API}/users/{user}"),
    };
    let key = profile_key(mode.as_deref());

    let transport = transport::HttpTransport;
    let profiles = CacheApiStore::new(profile::PROFILE_POLICY, origin.clone());
    let tokens = CacheApiStore::new(token::TOKEN_POLICY, origin);

    // osu! retires a client-credentials token before it says it will, and answers 401 when it has.
    // One retry with a fresh token is the difference between a broken card and a working one; a
    // second 401 means the credentials are wrong, which retrying cannot fix.
    for attempt in 0..2 {
        // A token this Worker cannot obtain must not fail a request the store can already answer.
        // osu! throttling the token endpoint is exactly when the last good copy is worth serving,
        // and exchanging a token would not have changed the body anyway.
        let bearer = match token::bearer(&ctx.env, &tokens, now).await {
            Ok(bearer) => bearer,
            Err(problem) => return cached(&profiles, &key, problem).await,
        };

        let fetched = corner_core::get_or_fetch(
            &transport,
            &profiles,
            &key,
            &endpoint,
            &bearer,
            profile::PROFILE_POLICY,
            now,
        )
        .await;

        match fetched {
            Ok(outcome) => {
                if outcome.refresh {
                    // Stale: answer with what is stored and replace it after the response has gone.
                    // `wait_until` rather than a bare spawned future, because the Worker may be
                    // frozen the moment the response is returned.
                    let profiles = profiles.clone();
                    let endpoint = endpoint.clone();
                    // Cloned rather than moved: the retry below still needs the outer binding, and a
                    // move here would be a move out of a loop.
                    let key = key.clone();
                    ctx.data.wait_until(async move {
                        let _ =
                            corner_core::refresh(transport, profiles, key, endpoint, bearer, now)
                                .await;
                    });
                }

                // The **upstream** body is what got cached, not this projection, so the allowlist
                // can change without flushing anything. The age is stamped on the way out, from
                // the entry the body came from.
                return project(&outcome.body, outcome.fetched_at);
            }
            Err(corner_core::Error::Upstream(401)) if attempt == 0 => token::evict(&tokens).await,
            Err(error) => return Err(error.into()),
        }
    }

    Err(Failure::Upstream)
}

/// Answer with the allowlisted subset, so nothing osu! adds later is published before anyone has
/// seen it, stamped with the instant the body was fetched.
fn project(body: &str, fetched_at: i64) -> Result<Response, Failure> {
    let kept = profile::project(body).ok_or(Failure::Shape)?;
    // A cache entry lives for a week, so a body can be served long after it was fetched. The card
    // shows the age rather than presenting a week-old rank as current — which is also the only way
    // a silent stop in the refresh ever becomes visible.
    let kept = profile::with_fetched_at(&kept, fetched_at).ok_or(Failure::Shape)?;

    let response =
        Response::from_body(ResponseBody::Body(kept.into_bytes())).map_err(|_| Failure::Shape)?;
    let headers = response.headers();
    header(headers, "Content-Type", "application/json")?;
    // The browser's copy, not the store's: without this a refresh or a revisit re-requests the
    // profile every time. `_headers` cannot help here — it applies to static assets, not to a
    // Worker's own responses.
    header(headers, "Cache-Control", profile::BROWSER_CACHE_CONTROL)?;
    header(headers, "X-Content-Type-Options", "nosniff")?;

    Ok(response)
}

/// The stored body for `key`, or `problem` when there is nothing stored to stand in.
///
/// Used where a failure happens **before** the store was consulted — a token that could not be
/// minted, or credentials osu! rejected — so that a cached profile is still an answer.
async fn cached(store: &CacheApiStore, key: &str, problem: Failure) -> Result<Response, Failure> {
    match store.read(key).await {
        Ok(Some(entry)) => project(&entry.body, entry.fetched_at),
        _ => Err(problem),
    }
}

/// The `mode` query parameter, if the caller asked for one.
fn mode(url: &Url) -> Result<Option<String>, Failure> {
    let mut requested = None;

    for (key, value) in url.query_pairs() {
        if key == "mode" {
            let value = value.into_owned();

            if !MODES.contains(&value.as_str()) {
                return Err(Failure::BadMode);
            }

            requested = Some(value);
        }
    }

    Ok(requested)
}

/// A variable, with an unset or blank one treated as unconfigured: a blank `.dev.vars` entry should
/// say so rather than send an empty username upstream.
pub(crate) fn variable(env: &Env, name: &str) -> Result<String, Failure> {
    let Ok(value) = env.var(name) else {
        return Err(Failure::Unconfigured(name.to_string()));
    };

    let value = value.to_string();
    if value.trim().is_empty() {
        return Err(Failure::Unconfigured(name.to_string()));
    }

    Ok(value)
}

pub(crate) fn header(headers: &Headers, name: &str, value: &str) -> Result<(), Failure> {
    headers.set(name, value).map_err(|_| Failure::Shape)
}

/// Everything that can go wrong, and the only place in this Worker a status code is chosen.
///
/// The upstream's own text is never forwarded: an osu! error body echoes the request, and the token
/// must never appear in a response. What the client gets is a code and a short reason.
enum Failure {
    /// A variable this route needs is unset or blank. Naming it is safe, and is what makes a
    /// half-configured clone diagnosable rather than mysterious.
    Unconfigured(String),
    /// osu! refused the client credentials, so the exchange will never succeed.
    Credentials,
    /// osu! answered with something unusable, or did not answer at all.
    Upstream,
    /// osu! is rate limiting us and nothing was cached to stand in.
    RateLimited,
    /// The cache itself failed.
    Cache,
    /// osu! answered, but not in a shape this code recognises.
    Shape,
    /// The caller asked for a ruleset that does not exist.
    BadMode,
}

impl Failure {
    fn response(self) -> Result<Response> {
        let (status, body) = match self {
            Self::Unconfigured(variable) => {
                console_error!("unconfigured: {variable} is not set");
                (
                    500,
                    serde_json::json!({ "error": "unconfigured", "variable": variable }),
                )
            }
            Self::Credentials => {
                console_error!("osu! rejected the client credentials");
                (500, serde_json::json!({ "error": "credentials_rejected" }))
            }
            Self::Upstream => (502, serde_json::json!({ "error": "upstream_unavailable" })),
            Self::RateLimited => {
                // Logged, unlike the other arms, because this is the one failure that leaves no
                // other trace: it is a normal HTTP answer with no exception and no upstream error
                // body, so a `wrangler tail` of a rate-limited route used to be silent — which is
                // exactly the state that made this hard to diagnose from outside.
                console_error!("osu! rate-limited us and nothing was cached to stand in");
                (503, serde_json::json!({ "error": "upstream_rate_limited" }))
            }
            Self::Cache => (500, serde_json::json!({ "error": "cache_unavailable" })),
            Self::Shape => (
                502,
                serde_json::json!({ "error": "unexpected_upstream_shape" }),
            ),
            Self::BadMode => (400, serde_json::json!({ "error": "unknown_mode" })),
        };

        let response = Response::from_json(&body).map(|response| response.with_status(status))?;
        // Never stored: a transient 503 is not an answer, and a client that cached it would keep
        // showing a broken card after the upstream came back.
        let _ = header(response.headers(), "Cache-Control", "no-store");
        let _ = header(response.headers(), "X-Content-Type-Options", "nosniff");

        Ok(response)
    }
}

impl From<corner_core::Error> for Failure {
    fn from(error: corner_core::Error) -> Self {
        match error {
            // The one upstream status worth its own answer: it means "come back later", not "broken".
            corner_core::Error::Upstream(429) => Self::RateLimited,
            corner_core::Error::Upstream(status) => {
                console_error!("osu! answered {status}");
                Self::Upstream
            }
            corner_core::Error::Transport(error) => {
                console_error!("transport: {}", error.0);
                Self::Upstream
            }
            corner_core::Error::Store(error) => {
                console_error!("cache: {}", error.0);
                Self::Cache
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::profile_key;

    /// The regression this exists for: one shared key meant `?mode=taiko` was answered with the osu!
    /// profile, for an hour, silently.
    #[test]
    fn each_ruleset_gets_its_own_cache_entry() {
        assert_ne!(profile_key(Some("osu")), profile_key(Some("taiko")));
        assert_ne!(profile_key(Some("taiko")), profile_key(Some("mania")));
        assert_ne!(profile_key(Some("fruits")), profile_key(Some("osu")));

        // An unstated mode is the account's own ruleset, not osu! — they can disagree.
        assert_ne!(profile_key(None), profile_key(Some("osu")));
    }

    #[test]
    fn the_same_ruleset_is_the_same_entry() {
        assert_eq!(profile_key(Some("taiko")), profile_key(Some("taiko")));
        assert_eq!(profile_key(None), profile_key(None));
    }
}
