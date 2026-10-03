//! [`corner_core::Store`] over the Cache API.
//!
//! No binding, no migration, no CI step: the Cache API *is* the product for caching an HTTP
//! response, and both things this Worker stores — the profile body and the OAuth token — are
//! responses.

use corner_core::{CachePolicy, Entry, Store, StoreError};
use worker::*;

/// When this store wrote an entry. `x-` because it is ours, not a header the runtime owns.
///
/// The Cache API does not report when it stored something and [`corner_core::resolve`] needs that
/// instant, so the only way to keep it is to carry it.
const FETCHED_AT: &str = "x-fetched-at";

/// Cache API storage for one kind of entry.
///
/// The lifetime is fixed at construction because `Entry` carries no policy: `Store::write` receives
/// only the value, so the store is the thing that has to know how long what it holds is worth
/// keeping. Its client is `Cache::default()` — the per-Worker `caches.default`, not the HTTP cache.
#[derive(Clone)]
pub struct CacheApiStore {
    policy: CachePolicy,
    origin: String,
}

impl CacheApiStore {
    #[must_use]
    pub fn new(policy: CachePolicy, origin: String) -> Self {
        Self { policy, origin }
    }

    /// The absolute URL an entry is keyed by, resolved against the **incoming request's own
    /// origin**. That is why no hostname appears in this code: the same build keys correctly on
    /// workers.dev, on a preview URL, and on the live route, and a rename cannot orphan the cache.
    fn url(&self, key: &str) -> String {
        format!("{}{key}", self.origin)
    }

    /// Forget an entry.
    ///
    /// Deliberately not part of `Store`: the fetch/cache policy never deletes anything. This is a
    /// repair the caller performs once it has learned the cached value was bad — a 401 says the
    /// token, not the request, was the problem. Best effort, because the next call re-fetches
    /// either way.
    pub async fn evict(&self, key: &str) {
        let _ = Cache::default().delete(self.url(key), false).await;
    }
}

/// Store failures as text. The message is for the Worker's console; it is never sent to a client.
fn fail(error: worker::Error) -> StoreError {
    StoreError(error.to_string())
}

impl Store for CacheApiStore {
    async fn read(&self, key: &str) -> Result<Option<Entry>, StoreError> {
        let Some(mut response) = Cache::default()
            .get(self.url(key), false)
            .await
            .map_err(fail)?
        else {
            return Ok(None);
        };

        let fetched_at = response
            .headers()
            .get(FETCHED_AT)
            .map_err(fail)?
            .and_then(|value| value.parse::<i64>().ok())
            .ok_or_else(|| StoreError(format!("a cached entry has no readable {FETCHED_AT}")))?;

        let etag = response.headers().get("ETag").map_err(fail)?;
        let body = response.text().await.map_err(fail)?;

        Ok(Some(Entry {
            body,
            fetched_at,
            etag,
        }))
    }

    async fn write(&self, key: &str, entry: &Entry) -> Result<(), StoreError> {
        let headers = Headers::new();
        // This is the Cache API's own TTL: it reads the stored response's `max-age`, and it ignores
        // `stale-while-revalidate`, which is why the stored header is the policy's *retention* and
        // not its freshness window. Freshness is decided by `corner_core::resolve` against the
        // `x-fetched-at` below.
        headers
            .set("Cache-Control", &self.policy.storage_cache_control())
            .map_err(fail)?;
        headers
            .set("Content-Type", "application/json")
            .map_err(fail)?;
        headers
            .set(FETCHED_AT, &entry.fetched_at.to_string())
            .map_err(fail)?;
        if let Some(etag) = &entry.etag {
            headers.set("ETag", etag).map_err(fail)?;
        }

        let response = Response::from_body(ResponseBody::Body(entry.body.clone().into_bytes()))
            .map_err(fail)?
            .with_headers(headers);

        Cache::default()
            .put(self.url(key), response)
            .await
            .map_err(fail)
    }
}
