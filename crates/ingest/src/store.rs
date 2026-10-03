//! The store: the index and the replays, published to a bucket over the S3 API.
//!
//! **Why a bucket rather than static assets**: Cloudflare caps static assets at 20,000 files
//! per Worker version and re-uploads all of them on every deploy, so 10,000 replays would consume
//! half that budget permanently and make each deploy enormous. The bucket is free at this size and
//! its egress always is.
//!
//! **Why the index goes too, and not into the repository**: the corner is the public,
//! cloneable repository, so a committed index would ship the owner's plays inside the template a
//! stranger clones. The index carries its own format version, so staleness is detectable by its
//! reader rather than a production-only skew bug — which is what the "commit it so they deploy
//! atomically" argument had going for it.
//!
//! **Why the signature is hand-rolled**: it is one signed `PUT`, and the crates that would do it
//! bring an async runtime into a binary that is deliberately synchronous with a rayon pool. What it
//! costs is `hmac` + `sha2`, both RustCrypto, neither with C — and the alternative reading of
//! "don't hand-roll crypto" does not apply, because HMAC-SHA256 is a construction over a hash and
//! the thing being computed is a *signature*, not a cipher.

use crate::devvars;
use crate::index;
use crate::ledger::{Ledger, Published};
use crate::profile;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The region a bucket is signed in. **`auto` is Cloudflare R2's**, and it is the one value a
/// signer copied from an S3 example gets wrong: a mismatch fails as a bare `403` with no
/// explanation. Every other S3-compatible service wants its own — Backblaze B2 uses the middle
/// segment of its endpoint (`us-west-004` from `s3.us-west-004.backblazeb2.com`) — so this became
/// config (`storage.region`) with `auto` left as the default for a config that predates the field.
const DEFAULT_REGION: &str = "auto";
const SERVICE: &str = "s3";

/// What the ledger records for an index file: the bytes **and** the header they are published
/// with, because the header is part of the object.
///
/// Digging only the bytes would make a metadata fix unshippable — the object is unchanged, so the
/// run skips it, and the old `cache-control` stays on the bucket forever. That is not hypothetical:
/// the four index files were published as `immutable, max-age=31536000` and would still be, so a
/// cache would hold the first index it ever saw. Changing this value is what forces the re-upload.
fn artifact_digest(body: &[u8], cache_control: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    hasher.update(b"\0");
    hasher.update(cache_control.as_bytes());
    osu_core::hex(&hasher.finalize())
}

/// A bucket to upload to, and the credentials to do it with.
pub struct Store {
    pub endpoint: String,
    pub bucket: String,
    /// The SigV4 region: `auto` for R2, the middle segment of the endpoint for B2.
    pub region: String,
    /// The host without its scheme, which is what the signature covers.
    host: String,
    access_key_id: String,
    secret_access_key: String,
    agent: ureq::Agent,
}

/// A replay's object key is content-addressed — map hash plus score timestamp — and a replay is
/// never rewritten under the same name, so a year of `immutable` is honest. It also matters beyond
/// speed: an edge cache HIT never reaches the store, so a repeat or abusive request for an object
/// already cached costs **no Class B operation**, which is the cheapest abuse mitigation here.
///
/// This does *not* retroactively apply to objects uploaded before it, so the zone's cache rule must
/// set its own edge TTL as well.
const REPLAY_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

/// The objects written to the **same key** over and over: `index-<mode>.json` and
/// `profile-<mode>.json`, both replaced by every ingest.
///
/// Marking those `immutable` would strand every cache — edge and browser — on the first copy ever
/// published, so a new play, or a new rank, would never appear. Five minutes fresh with a day of
/// stale-while-revalidate is short enough that a cache cannot hide an ingest.
const REWRITTEN_CACHE_CONTROL: &str = "public, max-age=300, stale-while-revalidate=86400";

impl Store {
    /// Built from the config's `storage.endpoint`, `storage.bucket` and `storage.region` plus the two `STORAGE_*`
    /// values in `.dev.vars`. The `Err` is a human-readable reason, not a failure: a clone that has
    /// created no bucket yet is a **supported state** and the run reports it as such.
    pub fn new(
        endpoint: &str,
        bucket: &str,
        region: &str,
        vars: &HashMap<String, String>,
    ) -> Result<Self, String> {
        if endpoint.is_empty() {
            return Err("no storage.endpoint".to_owned());
        }
        if bucket.is_empty() {
            return Err("no storage.bucket".to_owned());
        }

        // The endpoint is checked before the credentials, so a malformed URL is reported as one
        // rather than as a missing key.
        let Some(host) = endpoint
            .split_once("://")
            .map(|(_, rest)| rest.trim_end_matches('/').to_owned())
        else {
            return Err(format!("storage.endpoint {endpoint:?} is not a URL"));
        };

        let Some((access_key_id, secret_access_key)) =
            devvars::pair(vars, "STORAGE_ACCESS_KEY_ID", "STORAGE_SECRET_ACCESS_KEY")
        else {
            return Err(
                "no STORAGE_ACCESS_KEY_ID / STORAGE_SECRET_ACCESS_KEY in .dev.vars".to_owned(),
            );
        };

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .into();

        Ok(Self {
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            bucket: bucket.to_owned(),
            // An empty region means the config predates the field, which can only be an R2 one.
            region: if region.is_empty() {
                DEFAULT_REGION.to_owned()
            } else {
                region.to_owned()
            },
            host,
            access_key_id,
            secret_access_key,
            agent,
        })
    }

    /// Sign one `PUT` and send it. The body is the object exactly as it will be stored.
    ///
    /// **A transient failure is retried here rather than ending the run.** A full upload is ~8,000
    /// objects over a home connection, so meeting a `500`, a `503` or a reset socket is expected, not
    /// exceptional — and without a retry the first one aborts the run with 7,000 objects still to go.
    /// Only failures the service is *telling* us are temporary are retried: a `403` from a wrong
    /// region or a `404` is a mistake, and repeating it would just spend the same seconds again.
    fn put(&self, object: &str, body: &[u8], cache_control: &str) -> Result<(), String> {
        let url = format!(
            "{}/{}/{}",
            self.endpoint,
            self.bucket,
            uri_encode(object, false)
        );

        let mut attempt = 0;
        loop {
            attempt += 1;

            // Re-signed per attempt rather than reused, because the signature covers the timestamp
            // and a stale one is a `403` that says nothing about why.
            let (date, amz_date) = stamps();
            let signed = sign(
                &self.access_key_id,
                &self.secret_access_key,
                &self.region,
                SERVICE,
                &self.host,
                &self.bucket,
                object,
                body,
                &date,
                &amz_date,
            );

            let sent = self
                .agent
                .put(&url)
                .header("authorization", &signed.authorization)
                .header("x-amz-date", &amz_date)
                .header("x-amz-content-sha256", &signed.payload_hash)
                .header("content-type", "application/octet-stream")
                // Per object, not per bucket: a replay may be cached for a year because it can never
                // change, and the index must not be, because it changes every run.
                .header("cache-control", cache_control)
                .send(body);

            match sent {
                Ok(_) => return Ok(()),
                Err(error) if attempt < ATTEMPTS && transient(&error) => {
                    let wait = backoff(attempt);
                    println!(
                        "  retrying {object} in {}s ({attempt}/{ATTEMPTS}): {error}",
                        wait.as_secs()
                    );
                    std::thread::sleep(wait);
                }
                Err(error) => return Err(format!("uploading {object}: {error}")),
            }
        }
    }
}

/// How many times one object is attempted before the run gives up on it. Five attempts with the
/// backoff below is a little over two minutes of patience per object, which covers a service blip
/// without turning a genuinely broken request into an hour of silence.
const ATTEMPTS: u32 = 5;

/// The wait before attempt `n`+1: 1 s, 2 s, 4 s, 8 s.
fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(1 << (attempt - 1))
}

/// Whether an error is worth trying again.
///
/// `StatusCode` covers what the service says is temporary — `5xx` and the `429`/`503` family. The
/// transport arms cover a socket that died or a timeout, which is the same class of accident. `Error`
/// is `#[non_exhaustive]`, so the final arm is a deliberate "anything else is a mistake": a bad URL,
/// a hostname that does not resolve, too many redirects. Retrying those only delays the report.
fn transient(error: &ureq::Error) -> bool {
    match error {
        ureq::Error::StatusCode(code) => *code >= 500 || *code == 429,
        ureq::Error::Io(_)
        | ureq::Error::Timeout(_)
        | ureq::Error::ConnectionFailed
        | ureq::Error::Protocol(_) => true,
        _ => false,
    }
}

/// What one run published.
#[derive(Debug, Default)]
pub struct Report {
    pub replays: u64,
    pub replays_skipped: u64,
    pub index: u64,
    pub index_skipped: u64,
    pub profile: u64,
    pub profile_skipped: u64,
    pub bytes: u64,
}

/// Publish the index files and every staged replay that is not already there.
///
/// `objects` is `(play key, object key)` per play in the index — the play key names the staged file,
/// the object key names what it becomes in the bucket. Passed as pairs rather than as `index::Play`
/// so that this module knows nothing about pricing, which is what lets the orchestration below be
/// tested without a bucket.
///
/// `Ok(None)` means no store is configured, which the caller reports rather than treats as failure.
/// The ledger is the skip list for both halves — a replay by play key, an index file by digest —
/// so a rerun uploads only what changed and never asks the bucket what it already holds.
pub fn upload(
    ledger: &mut Ledger,
    work: &Path,
    objects: &[(String, String)],
    bucket: Option<&Store>,
    dry_run: bool,
) -> Result<Option<Report>, String> {
    let Some(bucket) = bucket else {
        return Ok(None);
    };

    let mut report = Report::default();

    // ---------------------------------------------------------------- the index
    //
    // Uploaded first, so that a replay a viewer asks for always has an index row that names it.
    let mut artifacts: Vec<(String, String)> = Vec::new();
    for (_, name) in index::MODES {
        let file = format!("index-{name}.json");
        let body = std::fs::read(work.join(&file))
            .map_err(|error| format!("cannot read {}: {error}", work.join(&file).display()))?;
        let digest = artifact_digest(&body, REWRITTEN_CACHE_CONTROL);

        if ledger.artifact(&file)?.as_deref() == Some(digest.as_str()) {
            report.index_skipped += 1;
            continue;
        }

        if !dry_run {
            bucket.put(&file, &body, REWRITTEN_CACHE_CONTROL)?;
            artifacts.push((file, digest));
        }
        report.index += 1;
        report.bytes += body.len() as u64;
    }

    // Committed as soon as the index is up. A replay upload is minutes long; a run that dies in the
    // middle of it should not cost the four index objects as well.
    if !dry_run {
        commit_artifacts(ledger, &mut artifacts)?;
    }

    // ------------------------------------------------------- the profile snapshot
    //
    // Rewritten every run like the index, so it carries the same short cache rather than the
    // replays' year. A missing file is skipped rather than being an error: a ruleset osu! did not
    // answer for keeps whatever the bucket already holds, and a clone with no osu! credentials
    // never produces one at all.
    let profiles = upload_profiles(ledger, work, bucket, dry_run)?;
    report.profile = profiles.profile;
    report.profile_skipped = profiles.profile_skipped;
    report.bytes += profiles.bytes;
    // ---------------------------------------------------------------- the replays
    let published = ledger.published()?;
    let mut rows: Vec<Published> = Vec::new();

    for (play_key, object) in objects {
        let object = object.as_str();

        // The same play, already there under the same name. A play that *gained* an online id
        // since it was uploaded changed its object key, so it is uploaded again rather than left
        // under a name the index no longer points at.
        if published.get(play_key).map(String::as_str) == Some(object) {
            report.replays_skipped += 1;
            continue;
        }

        let path = work.join("replays").join(format!("{play_key}.osr"));
        let body = std::fs::read(&path).map_err(|error| {
            format!("{play_key} is indexed but its replay is not staged: {error}")
        })?;

        if !dry_run {
            // A failure here records everything uploaded so far, so the next run resumes rather
            // than starting again — which is the whole reason the ledger is written from the
            // successes and not up front.
            if let Err(error) = bucket.put(object, &body, REPLAY_CACHE_CONTROL) {
                commit_rows(ledger, &mut rows)?;
                commit_artifacts(ledger, &mut artifacts)?;
                return Err(error);
            }
            rows.push(Published {
                play: play_key.clone(),
                object: object.to_owned(),
                bytes: body.len() as u64,
            });

            if rows.len() >= COMMIT_EVERY {
                commit_rows(ledger, &mut rows)?;
                println!(
                    "  {:<17}{:>6}/{:<6} replays",
                    "uploading",
                    report.replays,
                    objects.len()
                );
            }
        }
        report.replays += 1;
        report.bytes += body.len() as u64;
    }

    if !dry_run {
        commit_rows(ledger, &mut rows)?;
        commit_artifacts(ledger, &mut artifacts)?;
    }

    Ok(Some(report))
}

/// Publish the profile snapshots, and nothing else.
///
/// Separate from [`upload`] because a **scheduled** run refreshes only these: it has no game
/// installs to walk and no index to rebuild, so it finishes in seconds — and running it somewhere
/// osu! will answer is the entire point of it (`--profile-only`).
///
/// The ledger is the skip list, keyed by the digest of the bytes **and** their header. Note what
/// the stamp does to that: the bytes change on every successful fetch, so in practice each run
/// re-sends four ~1 KB objects. That is the intent rather than a leak — the timestamp *is* the
/// point, and four kilobytes an hour is nothing.
pub fn upload_profiles(
    ledger: &mut Ledger,
    work: &Path,
    bucket: &Store,
    dry_run: bool,
) -> Result<Report, String> {
    let mut report = Report::default();
    let mut artifacts: Vec<(String, String)> = Vec::new();

    for mode in profile::MODES {
        let file = format!("profile-{mode}.json");
        let Ok(body) = std::fs::read(work.join(&file)) else {
            continue;
        };
        let digest = artifact_digest(&body, REWRITTEN_CACHE_CONTROL);

        if ledger.artifact(&file)?.as_deref() == Some(digest.as_str()) {
            report.profile_skipped += 1;
            continue;
        }

        if !dry_run {
            bucket.put(&file, &body, REWRITTEN_CACHE_CONTROL)?;
            artifacts.push((file, digest));
        }
        report.profile += 1;
        report.bytes += body.len() as u64;
    }

    if !dry_run {
        commit_artifacts(ledger, &mut artifacts)?;
    }

    Ok(report)
}

/// How many uploaded objects are recorded at once.
///
/// Not one transaction per object — that would be 8,000 commits — and **not one at the end either,
/// which is what this started as and what a killed run proved wrong**: the objects were in the
/// bucket and the ledger held nothing, so the next run would have re-sent all 8,030 of them. It is
/// a compromise with a real ceiling rather than a guarantee: a kill loses at most this many objects'
/// worth of bookkeeping, and re-sending them is harmless because a `PUT` of the same bytes to the
/// same key is idempotent.
const COMMIT_EVERY: usize = 200;

/// Record the replays uploaded so far, then forget them — the caller keeps counting separately.
fn commit_rows(ledger: &mut Ledger, rows: &mut Vec<Published>) -> Result<(), String> {
    if rows.is_empty() {
        return Ok(());
    }
    ledger.record_published(rows)?;
    rows.clear();
    Ok(())
}

/// Record the index files uploaded this run, by digest, so the next run can skip them.
fn commit_artifacts(
    ledger: &mut Ledger,
    artifacts: &mut Vec<(String, String)>,
) -> Result<(), String> {
    for (file, digest) in artifacts.iter() {
        ledger.record_artifact(file, digest)?;
    }
    artifacts.clear();
    Ok(())
}

/// What the bucket will be told.
struct Signed {
    authorization: String,
    payload_hash: String,
}

/// The SigV4 signature for one `PUT`.
///
/// Split out from `Store::put` because it is the part worth testing: given the same inputs it must
/// produce the same bytes every time, and its correctness is a *shared secret* — a signer that is
/// subtly wrong is a `403` from the service and nothing else. `region` and `service` are parameters so a
/// test can drive it, not because either ever varies.
#[allow(clippy::too_many_arguments)]
fn sign(
    access_key_id: &str,
    secret_access_key: &str,
    region: &str,
    service: &str,
    host: &str,
    bucket: &str,
    object: &str,
    body: &[u8],
    date: &str,
    amz_date: &str,
) -> Signed {
    let payload_hash = osu_core::hex(&Sha256::digest(body));
    let canonical_uri = format!("/{bucket}/{}", uri_encode(object, false));
    let signed_headers = "host;x-amz-content-sha256;x-amz-date";
    let canonical_headers =
        format!("host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n");

    // Empty query string, and the canonical headers block already ends in a newline — so the blank
    // line below is the separator the format calls for, not a typo.
    let canonical_request =
        format!("PUT\n{canonical_uri}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}");

    let scope = format!("{date}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        osu_core::hex(&Sha256::digest(canonical_request.as_bytes()))
    );

    let signature = osu_core::hex(&hmac(
        &signing_key(secret_access_key, date, region, service),
        string_to_sign.as_bytes(),
    ));

    Signed {
        authorization: format!(
            "AWS4-HMAC-SHA256 Credential={access_key_id}/{scope}, \
             SignedHeaders={signed_headers}, Signature={signature}"
        ),
        payload_hash,
    }
}

/// The four-step key derivation, which is what makes a leaked signature useless for another date,
/// region or service.
fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> [u8; 32] {
    let date_key = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let region_key = hmac(&date_key, region.as_bytes());
    let service_key = hmac(&region_key, service.as_bytes());
    hmac(&service_key, b"aws4_request")
}

fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    // HMAC accepts a key of any length, so this cannot fail.
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// Percent-encoding as the signature requires it: everything outside the unreserved set is escaped,
/// and `/` is escaped only when it is data rather than a separator.
fn uri_encode(value: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        let unreserved = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~');
        if unreserved || (byte == b'/' && !encode_slash) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Now, as the two strings SigV4 wants: `YYYYMMDD` and `YYYYMMDDTHHMMSSZ`, always UTC.
fn stamps() -> (String, String) {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0);

    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let second_of_day = seconds.rem_euclid(86_400);

    (
        format!("{year:04}{month:02}{day:02}"),
        format!(
            "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
            second_of_day / 3600,
            second_of_day % 3600 / 60,
            second_of_day % 60
        ),
    )
}

/// Days since 1970-01-01 to a civil date. Howard Hinnant's `civil_from_days`, which is the standard
/// way to do this without a date crate — and it is here rather than reaching for one because the
/// only thing needed is one UTC timestamp in one fixed format.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;

    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_encoding_follows_rfc_3986() {
        assert_eq!(
            uri_encode("a-b_c.d~e", false),
            "a-b_c.d~e",
            "unreserved stays"
        );
        assert_eq!(uri_encode("a b", false), "a%20b", "space is %20, never +");
        assert_eq!(uri_encode("a+b", false), "a%2Bb");
        assert_eq!(uri_encode("a/b", false), "a/b", "a separator is kept");
        assert_eq!(uri_encode("a/b", true), "a%2Fb", "but not when it is data");
        assert_eq!(
            uri_encode("133295220694499848", false),
            "133295220694499848",
            "which is the whole point: every real object key is untouched"
        );
        assert_eq!(
            uri_encode("477990bba544108ad74438ac77f937ea-133295220694499848", false),
            "477990bba544108ad74438ac77f937ea-133295220694499848"
        );
        assert_eq!(
            uri_encode("é", false),
            "%C3%A9",
            "UTF-8 is escaped per byte"
        );
    }

    /// The one computation whose failure the service reports as a bare `403`.
    ///
    /// The expected value was produced by an **independent implementation** — Python's `hmac` and
    /// `hashlib` over the same canonical request — so this is a cross-check rather than a
    /// transcription of six fixture values agreed with my own code.
    #[test]
    fn a_signature_matches_an_independent_computation() {
        let signed = sign(
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "auto",
            SERVICE,
            "examplebucket.example.r2.cloudflarestorage.com",
            "examplebucket",
            "test.txt",
            b"Welcome to Amazon S3.",
            "20130524",
            "20130524T000000Z",
        );

        assert_eq!(
            signed.payload_hash, "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072",
            "the payload hash is independently checkable, and it is AWS's own documented example"
        );
        assert_eq!(
            signed.authorization,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20130524/auto/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=b68d1295e10d93ed3a4330057ce74107994c6eac3523c995a3734dc2fc58c947",
            "the whole Authorization header, against the independent computation"
        );
    }

    #[test]
    fn a_timestamp_is_utc_and_leap_years_are_right() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29), "a leap day exists");
        assert_eq!(civil_from_days(19_783), (2024, 3, 1));
        assert_eq!(
            civil_from_days(11_016),
            (2000, 2, 29),
            "2000 is a leap year"
        );
        assert_eq!(civil_from_days(20_726), (2026, 9, 30));
        assert_eq!(civil_from_days(-1), (1969, 12, 31), "and before the epoch");
    }

    /// One word wrong here is silent, year-long staleness on every new ingest, so it gets a guard.
    #[test]
    fn the_index_is_never_uploaded_immutable() {
        assert!(!REWRITTEN_CACHE_CONTROL.contains("immutable"));
        assert!(REPLAY_CACHE_CONTROL.contains("immutable"));
    }

    /// The skip list has to notice a metadata-only change, or the header above never reaches a
    /// bucket that already holds the object.
    #[test]
    fn an_index_digest_changes_with_its_header_alone() {
        let body = b"[]";
        assert_ne!(
            artifact_digest(body, REWRITTEN_CACHE_CONTROL),
            artifact_digest(body, REPLAY_CACHE_CONTROL)
        );
        assert_eq!(
            artifact_digest(body, REWRITTEN_CACHE_CONTROL),
            artifact_digest(body, REWRITTEN_CACHE_CONTROL),
            "and is otherwise stable"
        );
    }

    /// The first run of an upload: what it plans, and what a second run then skips.
    ///
    /// A dry run never touches the network, which is exactly why this can be a test: it reads the
    /// four index files and one staged replay, decides what would go where, and reports. It is the
    /// half of the uploader that is this project's own logic — the other half is one signed `PUT`,
    /// whose signature is checked above and whose behaviour needs a bucket to check at all.
    #[test]
    fn a_first_run_plans_everything_and_a_second_plans_nothing() {
        let dir = std::env::temp_dir().join(format!("osu-ingest-r2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let work = dir.join("library");
        std::fs::create_dir_all(work.join("replays")).unwrap();

        for (_, name) in index::MODES {
            std::fs::write(work.join(format!("index-{name}.json")), b"[]").unwrap();
        }

        // One profile snapshot, so the second rewritten-object path is planned and skipped too.
        std::fs::write(work.join("profile-osu.json"), b"{}").unwrap();

        // One play osu! has an id for, and one it does not — the two shapes of object key.
        let keyed = "477990bba544108ad74438ac77f937ea-133295220694499848";
        let bare = "477990bba544108ad74438ac77f937ea-133295220694499999";
        for key in [keyed, bare] {
            std::fs::write(
                work.join("replays").join(format!("{key}.osr")),
                vec![0u8; 40],
            )
            .unwrap();
        }

        let mut vars = HashMap::new();
        vars.insert("STORAGE_ACCESS_KEY_ID".to_owned(), "id".to_owned());
        vars.insert("STORAGE_SECRET_ACCESS_KEY".to_owned(), "secret".to_owned());
        let bucket = Store::new("https://api.example", "a-bucket", "auto", &vars).unwrap();

        let objects = vec![
            (keyed.to_owned(), "13329522".to_owned()),
            (bare.to_owned(), bare.to_owned()),
        ];

        let mut ledger = Ledger::open(&work.join("state.db")).unwrap();
        let first = upload(&mut ledger, &work, &objects, Some(&bucket), true)
            .unwrap()
            .expect("a configured bucket reports");

        assert_eq!(first.index, 4, "all four index files, all new");
        assert_eq!(first.profile, 1, "the one profile snapshot, new");
        assert_eq!(first.replays, 2, "both replays, both new");
        assert_eq!(first.replays_skipped, 0);
        assert_eq!(first.index_skipped, 0);
        assert_eq!(first.profile_skipped, 0);
        assert_eq!(first.bytes, 4 * 2 + 2 + 2 * 40, "every byte it would send");

        // A dry run records nothing, so a second dry run plans exactly the same thing — which is
        // the property that makes `--dry-run` worth trusting.
        let again = upload(&mut ledger, &work, &objects, Some(&bucket), true)
            .unwrap()
            .unwrap();
        assert_eq!(
            again.replays, 2,
            "still nothing published, so still not skipped"
        );

        // Publishing by hand is what a real run would have done; then the skip list works.
        ledger
            .record_published(&[
                Published {
                    play: keyed.to_owned(),
                    object: "13329522".to_owned(),
                    bytes: 40,
                },
                Published {
                    play: bare.to_owned(),
                    object: bare.to_owned(),
                    bytes: 40,
                },
            ])
            .unwrap();
        for (_, name) in index::MODES {
            let file = format!("index-{name}.json");
            let body = std::fs::read(work.join(&file)).unwrap();
            ledger
                .record_artifact(&file, &artifact_digest(&body, REWRITTEN_CACHE_CONTROL))
                .unwrap();
        }
        let profile = std::fs::read(work.join("profile-osu.json")).unwrap();
        ledger
            .record_artifact(
                "profile-osu.json",
                &artifact_digest(&profile, REWRITTEN_CACHE_CONTROL),
            )
            .unwrap();

        let settled = upload(&mut ledger, &work, &objects, Some(&bucket), true)
            .unwrap()
            .unwrap();
        assert_eq!(settled.replays, 0, "a published replay is not sent again");
        assert_eq!(settled.replays_skipped, 2);
        assert_eq!(settled.index, 0, "an unchanged index is not sent again");
        assert_eq!(settled.index_skipped, 4);
        assert_eq!(
            settled.profile, 0,
            "an unchanged snapshot is not sent again"
        );
        assert_eq!(settled.profile_skipped, 1);
        assert_eq!(settled.bytes, 0);

        // And with no bucket configured, nothing is planned and nothing is an error.
        assert!(
            upload(&mut ledger, &work, &objects, None, false)
                .unwrap()
                .is_none()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A retry that fires on a mistake is worse than having none: it delays the real error by the
    /// whole backoff, and across 8,000 objects that is hours. So the classification is pinned here
    /// rather than left to be read off the match arms.
    #[test]
    fn only_failures_the_service_calls_temporary_are_retried() {
        for code in [500, 502, 503, 504, 429] {
            assert!(
                transient(&ureq::Error::StatusCode(code)),
                "{code} is temporary"
            );
        }
        for code in [400, 403, 404, 409] {
            assert!(
                !transient(&ureq::Error::StatusCode(code)),
                "{code} is a mistake"
            );
        }

        assert!(transient(&ureq::Error::Io(std::io::Error::other(
            "connection reset"
        ))));
        assert!(transient(&ureq::Error::ConnectionFailed));
        assert!(!transient(&ureq::Error::HostNotFound));

        assert_eq!(
            backoff(1).as_secs(),
            1,
            "1, 2, 4, 8 — and ATTEMPTS covers all four"
        );
        assert_eq!(backoff(4).as_secs(), 8);
    }

    /// A replay the index names but the working set does not hold is a real inconsistency, and it
    /// must not look like success.
    #[test]
    fn an_indexed_replay_with_no_file_is_an_error() {
        let dir =
            std::env::temp_dir().join(format!("osu-ingest-r2-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let work = dir.join("library");
        std::fs::create_dir_all(work.join("replays")).unwrap();
        for (_, name) in index::MODES {
            std::fs::write(work.join(format!("index-{name}.json")), b"[]").unwrap();
        }

        let mut vars = HashMap::new();
        vars.insert("STORAGE_ACCESS_KEY_ID".to_owned(), "id".to_owned());
        vars.insert("STORAGE_SECRET_ACCESS_KEY".to_owned(), "secret".to_owned());
        let bucket = Store::new("https://api.example", "a-bucket", "auto", &vars).unwrap();

        let mut ledger = Ledger::open(&work.join("state.db")).unwrap();
        let error = upload(
            &mut ledger,
            &work,
            &[("absent".to_owned(), "absent".to_owned())],
            Some(&bucket),
            true,
        )
        .expect_err("a missing replay must not be reported as uploaded");
        assert!(error.contains("not staged"), "{error}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `Store` holds a live HTTP agent, so it is not `Debug` and `unwrap_err` cannot be used on
    /// it — which is a good thing to be told rather than a nuisance.
    fn reason(result: Result<Store, String>) -> String {
        match result {
            Ok(_) => panic!("expected a reason, not a bucket"),
            Err(reason) => reason,
        }
    }

    #[test]
    fn a_bucket_without_credentials_says_which_piece_is_missing() {
        let vars = HashMap::new();
        assert!(reason(Store::new("", "b", "", &vars)).contains("storage.endpoint"));
        assert!(
            reason(Store::new("https://api.example", "", "", &vars)).contains("storage.bucket")
        );
        assert!(reason(Store::new("https://api.example", "b", "", &vars)).contains(".dev.vars"));
        assert!(reason(Store::new("api.example", "b", "", &vars)).contains("not a URL"));

        let mut vars = HashMap::new();
        vars.insert("STORAGE_ACCESS_KEY_ID".to_owned(), "id".to_owned());
        assert!(
            Store::new("https://api.example", "b", "", &vars).is_err(),
            "half a pair is not a pair"
        );
        vars.insert("STORAGE_SECRET_ACCESS_KEY".to_owned(), "secret".to_owned());
        let bucket = Store::new("https://api.example/", "b", "", &vars).expect("complete");
        assert_eq!(bucket.host, "api.example");
        assert_eq!(
            bucket.endpoint, "https://api.example",
            "trailing slash trimmed"
        );
    }
}
