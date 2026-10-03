//! The osu! OAuth client-credentials exchange, and the token it yields.
//!
//! Client-credentials tokens belong to a guest user with no resource owner: there is no
//! authorization screen, nothing ever displays the application's name, and the only thing the
//! exchange needs is the id and the secret.

use corner_core::{CachePolicy, Entry, Store};
use worker::*;

use crate::store::CacheApiStore;
use crate::{Failure, header, variable};

/// How long the Cache API keeps the token.
///
/// This is the token's own lifetime rather than a serving window: the stored response never leaves
/// the Worker — only [`bearer`] reads it — so it is stored to outlive its usefulness, and a token
/// that expires early costs one extra exchange, not a broken response. The real freshness decision
/// is made against `expires_in` from the body.
pub const TOKEN_POLICY: CachePolicy = CachePolicy::new(86_400, 0);

/// Resolved against the incoming origin by the store, so no hostname is written here.
const TOKEN_KEY: &str = "/__cache/osu:token";

const TOKEN_URL: &str = "https://osu.ppy.sh/oauth/token";

/// Treat a token as spent this long before osu! says it is, so it cannot expire mid-flight.
const SKEW_SECS: i64 = 60;

/// A bearer token: from the cache while it is still good, and from osu! when it is not.
pub async fn bearer(env: &Env, store: &CacheApiStore, now: i64) -> Result<String, Failure> {
    let cached = store.read(TOKEN_KEY).await.map_err(cache_failure)?;

    // The lifetime osu! stated, measured from when the exchange happened.
    if let Some(entry) = &cached
        && let Some((token, expires_in)) = access_token(&entry.body)
        && entry.fetched_at + expires_in - SKEW_SECS > now
    {
        return Ok(token);
    }

    let body = exchange(env).await?;
    let (token, _) = access_token(&body).ok_or(Failure::Shape)?;

    store
        .write(
            TOKEN_KEY,
            &Entry {
                body,
                fetched_at: now,
                etag: None,
            },
        )
        .await
        .map_err(cache_failure)?;

    Ok(token)
}

/// Forget the token, so the next call exchanges a fresh one. The repair for a 401.
pub async fn evict(store: &CacheApiStore) {
    store.evict(TOKEN_KEY).await;
}

/// The token and the lifetime osu! stated for it.
///
/// The body is kept exactly as osu! sent it, so this reads a field rather than a shape invented
/// here — and anything osu! adds to the exchange later is already in the cache.
fn access_token(body: &str) -> Option<(String, i64)> {
    #[derive(serde::Deserialize)]
    struct Exchange {
        access_token: String,
        expires_in: i64,
    }

    let exchange: Exchange = serde_json::from_str(body).ok()?;
    Some((exchange.access_token, exchange.expires_in))
}

/// Ask osu! for a client-credentials token.
async fn exchange(env: &Env) -> Result<String, Failure> {
    let client_id = variable(env, "OSU_CLIENT_ID")?;
    let client_secret = variable(env, "OSU_CLIENT_SECRET")?;

    // Percent-encoded with the `url` crate `worker` already re-exports, rather than assembled by
    // hand: a secret containing `&` or `+` would otherwise corrupt the form body, silently.
    let form = worker::url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &client_id)
        .append_pair("client_secret", &client_secret)
        .append_pair("grant_type", "client_credentials")
        .append_pair("scope", "public")
        .finish();

    let headers = Headers::new();
    header(&headers, "Accept", "application/json")?;
    header(
        &headers,
        "Content-Type",
        "application/x-www-form-urlencoded",
    )?;

    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(wasm_bindgen::JsValue::from_str(&form)));

    let request = Request::new_with_init(TOKEN_URL, &init).map_err(|_| Failure::Upstream)?;
    let mut response = Fetch::Request(request)
        .send()
        .await
        .map_err(|_| Failure::Upstream)?;

    match response.status_code() {
        200 => response.text().await.map_err(|_| Failure::Upstream),
        // The credentials themselves are wrong. That never fixes itself, so it is not retried and
        // is reported as a configuration failure rather than an outage.
        401 => Err(Failure::Credentials),
        429 => {
            console_error!("osu! rate-limited the token exchange");
            Err(Failure::RateLimited)
        }
        _ => Err(Failure::Upstream),
    }
}

fn cache_failure(error: corner_core::StoreError) -> Failure {
    console_error!("cache: {}", error.0);
    Failure::Cache
}
