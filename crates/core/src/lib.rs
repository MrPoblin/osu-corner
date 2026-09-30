#![forbid(unsafe_code)]

//! Runtime-agnostic cache logic.
//!
//! Must never depend on `worker` — that is what lets the fetch/cache decision be tested with
//! `cargo test` rather than against a deployed Worker.
//!
//! Nothing here performs I/O, names a runtime, or mentions Cloudflare. `now` and every client are
//! passed in, so each boundary is testable without sleeping, and the same code runs unchanged
//! behind the `workers-rs` adapter or any future one.
//!
//! Why the cache exists at all: the osu! API allows 60 requests per minute for the *whole
//! application* and its terms ask for caching and against polling the same user more than once a
//! minute. So this is a condition of having API access, not an optimisation. Getting the freshness
//! arithmetic right, and proving it with tests, is far cheaper than discovering it wrong against a
//! live quota.
//!
//! Provenance: the cache policy came over from `poblin-site`'s `poblin-core` when the corner
//! became its own repository. The storage and HTTP traits were deliberately absent until now —
//! a trait with no implementor is dead weight — and arrived with the first route that needed them.

/// How long a cached value is fresh, and how long past that it may still be served while a refresh
/// happens in the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachePolicy {
    pub ttl_secs: i64,
    pub stale_while_revalidate_secs: i64,
}

impl CachePolicy {
    #[must_use]
    pub const fn new(ttl_secs: i64, stale_while_revalidate_secs: i64) -> Self {
        Self {
            ttl_secs,
            stale_while_revalidate_secs,
        }
    }

    /// Past this age the value is gone and the caller must fetch before answering.
    #[must_use]
    pub const fn hard_expiry_secs(&self) -> i64 {
        self.ttl_secs + self.stale_while_revalidate_secs
    }

    /// The `Cache-Control` value a response served under this policy should carry.
    ///
    /// This is also how long the Cache API holds the entry: its TTL comes from the stored
    /// response's own `Cache-Control`, so the policy and the storage agree by construction rather
    /// than by two numbers that have to be kept in step.
    #[must_use]
    pub fn cache_control(&self) -> String {
        format!(
            "public, max-age={}, stale-while-revalidate={}",
            self.ttl_secs, self.stale_while_revalidate_secs
        )
    }
}

/// A stored upstream response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub body: String,
    /// Unix seconds.
    pub fetched_at: i64,
    pub etag: Option<String>,
}

/// What the cache can answer for a request at a given instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Serve it. Nothing to do in the background.
    Fresh,
    /// Serve it, and kick off a refresh.
    Stale,
    /// Nothing usable. The caller must fetch before it can answer.
    Miss,
}

/// Decide how to answer from a cached value.
///
/// Pure: no clock, no storage, no I/O. `now` is passed in, so every boundary is testable without
/// sleeping.
#[must_use]
pub fn resolve(entry: Option<&Entry>, policy: CachePolicy, now: i64) -> Resolution {
    let Some(entry) = entry else {
        return Resolution::Miss;
    };

    let age = now - entry.fetched_at;
    if age < 0 {
        // Clock skew: a row written by a machine running ahead of us. Refetching until the clocks
        // agree would turn one skew into a request flood, so treat it as fresh and let it age out
        // normally.
        return Resolution::Fresh;
    }

    if age < policy.ttl_secs {
        Resolution::Fresh
    } else if age < policy.hard_expiry_secs() {
        Resolution::Stale
    } else {
        Resolution::Miss
    }
}

/// A completed upstream request, whatever it answered.
///
/// A 4xx or a 5xx is **not** a [`TransportError`]: the call worked and the upstream replied. The
/// status is data the caller acts on — [`get_or_fetch`] serves a cached body rather than failing
/// the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    pub status: u16,
    pub body: String,
    pub etag: Option<String>,
}

impl Upstream {
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The request never completed: DNS, TLS, connection, or a timeout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError(pub String);

/// The cache could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError(pub String);

/// Why a request could not be answered.
///
/// Kept distinct per layer so a transport implementation cannot fabricate a storage failure, and
/// so the adapter has one place to map a failure onto a response code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Transport(TransportError),
    Store(StoreError),
    /// The upstream answered with a status this code cannot use, **and** nothing cached could
    /// stand in. The status is carried so the adapter can map it.
    Upstream(u16),
}

impl From<TransportError> for Error {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

impl From<StoreError> for Error {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

/// One HTTP GET, reduced to what the fetch/cache policy needs.
///
/// `bearer` is the token only, never a full header value, and nothing in this crate logs it.
// `async fn` in a trait is stable and these are generic, never `dyn` — the Send-bound ambiguity
// the lint warns about cannot arise here, and `async_trait` would be a dependency bought for
// nothing.
#[allow(async_fn_in_trait)]
pub trait Transport {
    async fn get(&self, url: &str, bearer: &str) -> Result<Upstream, TransportError>;
}

/// A cache keyed by an opaque string.
#[allow(async_fn_in_trait)]
pub trait Store {
    async fn read(&self, key: &str) -> Result<Option<Entry>, StoreError>;
    async fn write(&self, key: &str, entry: &Entry) -> Result<(), StoreError>;
}

/// What to answer with, and whether the entry behind it wants refreshing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub body: String,
    /// The body came from a stale entry: serve it now and refresh in the background. The Worker
    /// hands [`refresh`] to `ctx.wait_until`; a test just awaits it.
    ///
    /// Set **only** on the stale-while-revalidate path. A failed fetch of a hard-expired entry is
    /// not flagged, so a rate-limited upstream is not retried once per request on top of failing.
    pub refresh: bool,
}

/// Answer from the cache when it can, and from upstream when it cannot.
///
/// Fresh serves the entry. Stale serves the entry and asks the caller to refresh. A miss fetches:
/// on success it stores and serves the new body, and on any failure it still prefers a
/// hard-expired copy to an error page — cached-but-old beats broken, and with nothing cached there
/// is nothing to stand in. `now` is Unix seconds; the caller owns the clock.
pub async fn get_or_fetch<T: Transport, S: Store>(
    transport: &T,
    store: &S,
    key: &str,
    url: &str,
    token: &str,
    policy: CachePolicy,
    now: i64,
) -> Result<Outcome, Error> {
    let cached = store.read(key).await?;

    match (resolve(cached.as_ref(), policy, now), cached) {
        (Resolution::Fresh, Some(entry)) => Ok(Outcome {
            body: entry.body,
            refresh: false,
        }),
        (Resolution::Stale, Some(entry)) => Ok(Outcome {
            body: entry.body,
            refresh: true,
        }),
        // A miss, or a resolver that disagreed with the entry it was handed. Either way: fetch.
        (_, fallback) => match transport.get(url, token).await {
            Ok(upstream) if upstream.is_success() => {
                store
                    .write(
                        key,
                        &Entry {
                            body: upstream.body.clone(),
                            fetched_at: now,
                            etag: upstream.etag,
                        },
                    )
                    .await?;
                Ok(Outcome {
                    body: upstream.body,
                    refresh: false,
                })
            }
            // Upstream answered, and the answer is unusable.
            Ok(upstream) => match fallback {
                Some(entry) => Ok(Outcome {
                    body: entry.body,
                    refresh: false,
                }),
                None => Err(Error::Upstream(upstream.status)),
            },
            // Never got an answer at all.
            Err(error) => match fallback {
                Some(entry) => Ok(Outcome {
                    body: entry.body,
                    refresh: false,
                }),
                None => Err(Error::Transport(error)),
            },
        },
    }
}

/// Fetch and store, ignoring what is cached. The background half of stale-while-revalidate.
///
/// Every argument is owned so the future is `'static`, which is what `ctx.wait_until` requires. A
/// non-success status is not an error: nothing is written and there is nothing left to do, because
/// the stale copy is already being served.
pub async fn refresh<T: Transport, S: Store>(
    transport: T,
    store: S,
    key: String,
    url: String,
    token: String,
    now: i64,
) -> Result<(), Error> {
    let upstream = transport.get(&url, &token).await?;

    if upstream.is_success() {
        store
            .write(
                &key,
                &Entry {
                    body: upstream.body,
                    fetched_at: now,
                    etag: upstream.etag,
                },
            )
            .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::rc::Rc;

    /// 1 hour fresh, then 1 hour of stale-while-revalidate. 2 hours hard expiry.
    const POLICY: CachePolicy = CachePolicy::new(3_600, 3_600);
    const START: i64 = 1_700_000_000;
    const KEY: &str = "/__cache/osu:profile";
    const URL: &str = "https://osu.ppy.sh/api/v2/users/@poblin/osu";
    const TOKEN: &str = "not-a-real-token";

    /// Counts calls, so "exactly one upstream call" is an assertion rather than a claim.
    ///
    /// `Rc` because [`refresh`] takes its clients by value so the future can outlive the request;
    /// a clone shares the count rather than starting a new one.
    #[derive(Clone)]
    struct CountingTransport {
        calls: Rc<Cell<u32>>,
        status: u16,
        body: &'static str,
    }

    impl CountingTransport {
        fn new(body: &'static str) -> Self {
            Self {
                calls: Rc::new(Cell::new(0)),
                status: 200,
                body,
            }
        }

        fn failing(status: u16) -> Self {
            Self {
                calls: Rc::new(Cell::new(0)),
                status,
                body: "{\"error\":\"check your token\"}",
            }
        }

        fn calls(&self) -> u32 {
            self.calls.get()
        }
    }

    impl Transport for CountingTransport {
        async fn get(&self, url: &str, bearer: &str) -> Result<Upstream, TransportError> {
            assert_eq!(url, URL);
            assert_eq!(bearer, TOKEN);
            self.calls.set(self.calls.get() + 1);
            Ok(Upstream {
                status: self.status,
                body: self.body.to_string(),
                etag: None,
            })
        }
    }

    #[derive(Clone, Default)]
    struct MemoryStore {
        entries: Rc<RefCell<HashMap<String, Entry>>>,
    }

    impl Store for MemoryStore {
        async fn read(&self, key: &str) -> Result<Option<Entry>, StoreError> {
            Ok(self.entries.borrow().get(key).cloned())
        }

        async fn write(&self, key: &str, entry: &Entry) -> Result<(), StoreError> {
            self.entries
                .borrow_mut()
                .insert(key.to_string(), entry.clone());
            Ok(())
        }
    }

    fn seeded(body: &'static str, fetched_at: i64) -> MemoryStore {
        let store = MemoryStore::default();
        store.entries.borrow_mut().insert(
            KEY.to_string(),
            Entry {
                body: body.to_string(),
                fetched_at,
                etag: None,
            },
        );
        store
    }

    /// The pass condition for this phase: the second read is answered from the cache, so upstream
    /// sees one call rather than two.
    #[test]
    fn two_reads_inside_the_ttl_make_exactly_one_upstream_call() {
        let transport = CountingTransport::new("{\"username\":\"poblin\"}");
        let store = MemoryStore::default();

        let first = pollster::block_on(get_or_fetch(
            &transport, &store, KEY, URL, TOKEN, POLICY, START,
        ));
        let second = pollster::block_on(get_or_fetch(
            &transport,
            &store,
            KEY,
            URL,
            TOKEN,
            POLICY,
            START + 3_599,
        ));

        assert_eq!(first.unwrap().body, "{\"username\":\"poblin\"}");
        assert_eq!(second.unwrap().body, "{\"username\":\"poblin\"}");
        assert_eq!(transport.calls(), 1, "the second read must come from cache");
    }

    #[test]
    fn a_stale_hit_serves_the_cached_body_and_asks_for_a_refresh() {
        let transport = CountingTransport::new("{}");
        let store = MemoryStore::default();

        pollster::block_on(get_or_fetch(
            &transport, &store, KEY, URL, TOKEN, POLICY, START,
        ))
        .unwrap();

        let stale = pollster::block_on(get_or_fetch(
            &transport,
            &store,
            KEY,
            URL,
            TOKEN,
            POLICY,
            START + POLICY.ttl_secs,
        ))
        .unwrap();

        assert_eq!(stale.body, "{}");
        assert!(stale.refresh, "a stale hit is the one case that refreshes");
        assert_eq!(transport.calls(), 1, "serving stale is not itself a fetch");

        let later = START + POLICY.ttl_secs + 400;
        pollster::block_on(refresh(
            transport.clone(),
            store.clone(),
            KEY.to_string(),
            URL.to_string(),
            TOKEN.to_string(),
            later,
        ))
        .unwrap();

        assert_eq!(transport.calls(), 2, "the refresh is the second call");
        let entry = store.entries.borrow().get(KEY).cloned().unwrap();
        assert_eq!(entry.fetched_at, later, "the refresh re-dates the entry");
    }

    #[test]
    fn past_hard_expiry_it_fetches_again() {
        let transport = CountingTransport::new("{}");
        let store = MemoryStore::default();

        pollster::block_on(get_or_fetch(
            &transport, &store, KEY, URL, TOKEN, POLICY, START,
        ))
        .unwrap();
        let after = pollster::block_on(get_or_fetch(
            &transport,
            &store,
            KEY,
            URL,
            TOKEN,
            POLICY,
            START + POLICY.hard_expiry_secs(),
        ))
        .unwrap();

        assert!(
            !after.refresh,
            "a hard miss fetches, it does not background"
        );
        assert_eq!(transport.calls(), 2);
    }

    /// osu! answers 429 when the rate limit is hit. That must not reach the client as an error
    /// while a cached copy exists — even one that has passed its hard expiry.
    #[test]
    fn a_refused_upstream_falls_back_to_the_cached_body() {
        let store = seeded("{\"cached\":true}", START);
        let transport = CountingTransport::failing(429);

        let answered = pollster::block_on(get_or_fetch(
            &transport,
            &store,
            KEY,
            URL,
            TOKEN,
            POLICY,
            START + POLICY.hard_expiry_secs(),
        ))
        .unwrap();

        assert_eq!(answered.body, "{\"cached\":true}");
        assert!(
            !answered.refresh,
            "a failed fetch must not schedule a retry loop"
        );
        assert_eq!(transport.calls(), 1);
    }

    #[test]
    fn with_nothing_cached_a_failure_is_an_error_carrying_the_status() {
        let transport = CountingTransport::failing(503);
        let store = MemoryStore::default();

        let failed = pollster::block_on(get_or_fetch(
            &transport, &store, KEY, URL, TOKEN, POLICY, START,
        ));

        assert_eq!(failed, Err(Error::Upstream(503)));
    }

    // ---- the cache policy, as ported ----

    /// 2 min fresh, then 10 min of stale-while-revalidate. 12 min hard expiry.
    const SHORT: CachePolicy = CachePolicy::new(120, 600);

    fn entry_at(fetched_at: i64) -> Entry {
        Entry {
            body: "{}".to_string(),
            fetched_at,
            etag: None,
        }
    }

    #[test]
    fn no_entry_is_a_miss() {
        assert_eq!(resolve(None, SHORT, 1_000), Resolution::Miss);
    }

    #[test]
    fn every_boundary_lands_on_the_right_side() {
        let e = entry_at(1_000);

        assert_eq!(resolve(Some(&e), SHORT, 1_000), Resolution::Fresh, "age 0");
        assert_eq!(
            resolve(Some(&e), SHORT, 1_119),
            Resolution::Fresh,
            "age 119"
        );

        // The ttl boundary is the important one: it is where a refresh starts.
        assert_eq!(
            resolve(Some(&e), SHORT, 1_120),
            Resolution::Stale,
            "age 120 == ttl"
        );
        assert_eq!(
            resolve(Some(&e), SHORT, 1_719),
            Resolution::Stale,
            "age 719"
        );

        assert_eq!(
            resolve(Some(&e), SHORT, 1_720),
            Resolution::Miss,
            "age 720 == hard expiry"
        );
        assert_eq!(
            resolve(Some(&e), SHORT, 99_999),
            Resolution::Miss,
            "long gone"
        );
    }

    #[test]
    fn clock_skew_reads_as_fresh_not_as_a_refetch_loop() {
        let future = entry_at(2_000);
        assert_eq!(resolve(Some(&future), SHORT, 1_000), Resolution::Fresh);
    }

    #[test]
    fn a_zero_ttl_policy_expires_immediately() {
        let policy = CachePolicy::new(0, 0);
        let e = entry_at(1_000);
        assert_eq!(resolve(Some(&e), policy, 1_000), Resolution::Miss);
        assert_eq!(policy.hard_expiry_secs(), 0);
    }

    #[test]
    fn cache_control_matches_the_policy() {
        assert_eq!(
            SHORT.cache_control(),
            "public, max-age=120, stale-while-revalidate=600"
        );
    }
}
