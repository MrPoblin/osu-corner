//! What a profile response is allowed to contain, and the projection that enforces it.
//!
//! An **allowlist**, not a denylist: a denylist publishes anything osu! adds later before anyone
//! has seen it. The list is `poblin-osu-corner-design.md` §2 and nothing else — the fields the card
//! renders. Upstream answers with 9.0 KB; this keeps 1.0 KB of it, and passthrough would also
//! publish `account_history` and `previous_usernames`.
//!
//! **Here rather than in the Worker, because two things now shape a profile.** The Worker projects
//! what it fetches live, and `osu-ingest` projects the snapshot it publishes to the store — so that
//! the card still updates when osu! will not answer a Cloudflare egress IP. A second copy of this
//! list would be a second answer to "what may be published", which is exactly the drift an
//! allowlist exists to prevent.
//!
//! Adding a field is one line here and one in the frontend's field map, which is the whole design.

use serde_json::{Map, Value};

/// The upstream paths served, kept in upstream's own shape.
///
/// `global_rank` and not `rank`: `statistics.rank` is a deprecated `{"country": N}` object, and
/// osu!'s own response-format list contradicts its changelog about which one that is.
///
/// `rank_history` and not `rankHistory`, for the same reason: osu! serves both, and only one can
/// be the 90-point line the card draws.
pub const PROFILE_FIELDS: &[&str] = &[
    // The account id, so the card can link to the player's own osu! profile without a user-agnostic
    // repository having to know whose it is. 11 bytes.
    "id",
    "username",
    "avatar_url",
    "country_code",
    "join_date",
    "statistics.global_rank",
    "statistics.country_rank",
    "statistics.pp",
    "statistics.play_time",
    "statistics.play_count",
    "statistics.ranked_score",
    "statistics.total_score",
    "statistics.hit_accuracy",
    "statistics.maximum_combo",
    "statistics.grade_counts.ssh",
    "statistics.grade_counts.ss",
    "statistics.grade_counts.sh",
    "statistics.grade_counts.s",
    "statistics.grade_counts.a",
    "rank_history",
];

/// Keep only [`PROFILE_FIELDS`], re-nesting the dotted paths so the result is upstream's own shape
/// rather than a shape invented here.
///
/// `None` when the body is not JSON, or when nothing at all was recognised — which is exactly what a
/// change to osu!'s response format looks like. A 502 is a better answer to that than an empty card.
#[must_use]
pub fn project(body: &str) -> Option<String> {
    let source: Value = serde_json::from_str(body).ok()?;
    let mut kept = Map::new();

    for path in PROFILE_FIELDS {
        if let Some(value) = pluck(&source, path) {
            graft(&mut kept, path, value.clone());
        }
    }

    if kept.is_empty() {
        return None;
    }

    serde_json::to_string(&Value::Object(kept)).ok()
}

/// Add the instant this body was fetched, as a field of our own.
///
/// **Deliberately not part of the allowlist above**, because it is not an upstream field: it is the
/// one thing the card cannot derive for itself — how old what it is showing actually is. Both
/// sources stamp it (the Worker from its cache entry, `osu-ingest` from its own clock), so a card
/// whose refresh has silently stopped says so instead of presenting a month-old rank as current.
///
/// Stamped on **every** successful fetch, not only when the numbers move. "Last updated" has to
/// mean "we last managed to ask osu!", or a profile that has not changed in months would look
/// broken while it is in fact perfectly current.
///
/// `None` when the body is not a JSON object — the caller should treat that as a failure to stamp,
/// not as a reason to withhold the body.
#[must_use]
pub fn with_fetched_at(projected: &str, fetched_at: i64) -> Option<String> {
    let mut value: Value = serde_json::from_str(projected).ok()?;
    value
        .as_object_mut()?
        .insert("fetched_at".to_string(), Value::from(fetched_at));

    serde_json::to_string(&value).ok()
}

/// Read a dotted path, e.g. `statistics.grade_counts.ssh`.
fn pluck<'a>(source: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cursor = source;

    for step in path.split('.') {
        cursor = cursor.as_object()?.get(step)?;
    }

    Some(cursor)
}

/// Put `value` at the same dotted path in `into`, creating the objects along the way.
fn graft(into: &mut Map<String, Value>, path: &str, value: Value) {
    let Some((head, rest)) = path.split_once('.') else {
        into.insert(path.to_string(), value);
        return;
    };

    // A leaf that is missing while a sibling is present still needs its parents to exist.
    let branch = into
        .entry(head.to_string())
        .or_insert_with(|| Value::Object(Map::new()));

    if let Some(branch) = branch.as_object_mut() {
        graft(branch, rest, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shape taken from a live response, plus the fields that must **not** survive.
    fn upstream() -> String {
        serde_json::json!({
            "username": "Poblin",
            "avatar_url": "https://a.ppy.sh/31220620",
            "country_code": "LT",
            "join_date": "2022-09-14T11:36:16+00:00",
            "is_supporter": true,
            "previous_usernames": ["PoblinOld"],
            "account_history": [{ "id": 1, "description": "something nobody asked for" }],
            "rankHistory": { "mode": "osu", "data": [1, 2, 3] },
            "rank_history": { "mode": "osu", "data": [79299, 78000, 77123] },
            "statistics": {
                "pp": 5612.34,
                "global_rank": 79299,
                "country_rank": 389,
                "play_time": 1556443,
                "play_count": 41207,
                "ranked_score": 24013520445_u64,
                "total_score": 98614553393_u64,
                "hit_accuracy": 98.14,
                "maximum_combo": 1429,
                "level": { "current": 102, "progress": 43 },
                "rank": { "country": 389 },
                "grade_counts": { "ssh": 9, "ss": 31, "sh": 32, "s": 593, "a": 1429 }
            }
        })
        .to_string()
    }

    fn served() -> Value {
        let kept = project(&upstream()).expect("a live shape must project");
        serde_json::from_str(&kept).expect("the projection must be JSON")
    }

    #[test]
    fn the_allowlist_is_what_comes_out() {
        let served = served();

        assert_eq!(served["username"], "Poblin");
        assert_eq!(served["statistics"]["pp"], 5612.34);
        assert_eq!(served["statistics"]["grade_counts"]["a"], 1429);
        assert_eq!(served["rank_history"]["data"][0], 79299);
    }

    /// The reason this is an allowlist rather than a denylist.
    #[test]
    fn everything_not_named_is_dropped_including_the_deprecated_twins() {
        let served = served();

        for gone in [
            "is_supporter",
            "previous_usernames",
            "account_history",
            "rankHistory",
        ] {
            assert!(
                served.get(gone).is_none(),
                "{gone} must not be served, and was"
            );
        }

        // `statistics.rank` is the deprecated twin of `statistics.global_rank`.
        assert!(served["statistics"].get("rank").is_none());
        assert!(served["statistics"].get("level").is_none());
    }

    /// The projection nests the way upstream does, so the frontend reads upstream's paths.
    #[test]
    fn dotted_paths_come_back_nested() {
        let served = served();

        assert!(served["statistics"].is_object());
        assert!(served["statistics"]["grade_counts"].is_object());
        assert!(served.get("statistics.pp").is_none(), "not a flat key");
    }

    #[test]
    fn a_missing_field_is_simply_absent() {
        let body = serde_json::json!({ "username": "Poblin" }).to_string();
        let served: Value = serde_json::from_str(&project(&body).unwrap()).unwrap();

        assert_eq!(served["username"], "Poblin");
        assert!(served.get("statistics").is_none(), "no invented branch");
    }

    /// A partial `grade_counts` still gets its parent object.
    #[test]
    fn a_partial_branch_still_nests() {
        let body =
            serde_json::json!({ "statistics": { "grade_counts": { "ss": 31 } } }).to_string();
        let served: Value = serde_json::from_str(&project(&body).unwrap()).unwrap();

        assert_eq!(served["statistics"]["grade_counts"]["ss"], 31);
        assert!(served["statistics"]["grade_counts"].get("ssh").is_none());
    }

    /// A shape change, an HTML error page, or an empty object must all be a 502 rather than an
    /// empty card.
    #[test]
    fn an_unrecognisable_body_projects_to_nothing() {
        assert!(project("<html>blocked</html>").is_none());
        assert!(project("{}").is_none());
        assert!(project("[]").is_none());
        assert!(project("").is_none());
    }

    /// The stamp rides beside the allowlisted fields without becoming one of them.
    #[test]
    fn the_fetch_time_is_stamped_beside_the_allowlist() {
        let stamped = with_fetched_at(&project(&upstream()).unwrap(), 1_700_000_000).unwrap();
        let served: Value = serde_json::from_str(&stamped).unwrap();

        assert_eq!(served["fetched_at"], 1_700_000_000);
        assert_eq!(served["username"], "Poblin", "and nothing else moved");
        assert!(
            !PROFILE_FIELDS.contains(&"fetched_at"),
            "it is ours, not osu!'s — it must never enter the allowlist"
        );
    }

    /// A body that cannot be stamped is a caller problem, not a body to throw away silently.
    #[test]
    fn stamping_a_non_object_is_a_none() {
        assert!(with_fetched_at("[]", 1).is_none());
        assert!(with_fetched_at("not json", 1).is_none());
    }
}
