//! The profile route's cache policy, and the projection it serves.
//!
//! The projection itself — the allowlist and the code that enforces it — lives in `osu-core`,
//! because `osu-ingest` projects the same response when it publishes a snapshot to the store. One
//! list, one answer to "what may be published".

use corner_core::CachePolicy;

/// The upstream allowlist, re-exported so the route reads `profile::project` either way.
pub use osu_core::profile::{project, with_fetched_at};

/// One hour fresh, then an hour of stale-while-revalidate, kept for a week.
///
/// On demand: a fetch happens when someone asks and the stored copy is older than the hour, so no
/// visitors means no calls. osu! asks for no more than one poll a minute and warns that exceeding
/// the quota can get tokens revoked, which makes this 60× more conservative than the documented ask.
/// Changing the rate later is this one number.
///
/// The week of retention is not freshness — the profile is still refetched after an hour. It is the
/// size of the safety net: osu!'s API rate-limits per IP and Cloudflare Workers egress from shared
/// addresses, so a throttled or unreachable upstream is a normal condition rather than an exotic
/// one, and the last good copy is a far better answer than a 503. Ten minutes of retention would
/// not survive a single osu! hiccup.
pub const PROFILE_POLICY: CachePolicy = CachePolicy::new(3_600, 3_600).retained_for(604_800);

/// What a **browser** may keep, and for how long, for this payload.
///
/// Independent of the upstream policy on purpose. Without it the response carried no `Cache-Control`
/// at all, so a refresh or a revisit paid a round trip to the Worker every time — the visitor
/// re-downloaded the profile on every load even though nothing about it had changed. Five minutes
/// matches the index files, and `stale-while-revalidate` is honoured by browser caches (unlike the
/// Cache API), so a visit after that revalidates behind the paint rather than blocking on it.
pub const BROWSER_CACHE_CONTROL: &str = "public, max-age=300, stale-while-revalidate=3600";
