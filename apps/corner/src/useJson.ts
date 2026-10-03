import { useEffect, useState } from "react";

import { decodeIndex } from "./osu.ts";
import type { Play } from "./osu.ts";

/**
 * The two things the corner loads — the profile and the index files — as a tagged state, so every
 * consumer is forced to handle loading and failure rather than reading a half-built object.
 */
export type Async<T> =
  | { state: "loading" }
  | { state: "error"; error: string }
  | { state: "ready"; data: T };

export function useJson<T>(url: string): Async<T> {
  const [result, setResult] = useState<Async<T>>({ state: "loading" });

  useEffect(() => {
    const controller = new AbortController();
    setResult({ state: "loading" });

    fetch(url, { signal: controller.signal })
      .then(async (response) => {
        if (!response.ok) throw new Error(`${response.status} ${response.statusText}`);
        setResult({ state: "ready", data: (await response.json()) as T });
      })
      .catch((error: unknown) => {
        // An abort is the effect cleaning up, not a failure worth showing.
        if (controller.signal.aborted) return;
        setResult({ state: "error", error: error instanceof Error ? error.message : "failed" });
      });

    return () => controller.abort();
  }, [url]);

  return result;
}

/**
 * All four index files, fetched once, decoded once.
 *
 * All four rather than only the selected one because the mode switcher shows each mode's play count.
 * That sounds wasteful and is not: osu! is 915 KB and the other three are about 4 KB each, so the
 * set is barely larger than the one file the corner cannot avoid.
 *
 * The key is joined rather than passed as an array, because a fresh array literal is a new identity
 * on every render and would refetch forever.
 */
const BASE = import.meta.env.BASE_URL;

/** Injected by `vite.config.ts` from `storage.public_base`; `""` when nothing is configured. */
declare const __INDEX_BASE__: string;

/**
 * Where the index files live: the storage host when one is configured, and this app's own assets
 * when none is.
 *
 * The index is the owner's data, so it is deliberately not in the repository and not in the Worker's
 * asset bundle — a play reaches the site on the next ingest rather than the next deploy. The
 * host is the same `storage.public_base` the CSP is built from, so the policy allows exactly the
 * host this fetches.
 */
const INDEX_BASE = __INDEX_BASE__ || BASE;

export function useIndexes(ids: readonly string[]): Async<Record<string, Play[]>> {
  const [result, setResult] = useState<Async<Record<string, Play[]>>>({ state: "loading" });
  const key = ids.join(",");

  useEffect(() => {
    const controller = new AbortController();
    setResult({ state: "loading" });

    Promise.all(
      key.split(",").map(async (id) => {
        const response = await fetch(`${INDEX_BASE}index-${id}.json`, { signal: controller.signal });
        if (!response.ok) throw new Error(`index-${id}.json — ${response.status}`);
        return [id, decodeIndex(await response.json())] as const;
      }),
    )
      .then((entries) => setResult({ state: "ready", data: Object.fromEntries(entries) }))
      .catch((error: unknown) => {
        if (controller.signal.aborted) return;
        setResult({ state: "error", error: error instanceof Error ? error.message : "failed" });
      });

    return () => controller.abort();
  }, [key]);

  return result;
}

/** The profile the card renders. `osu-core`'s allowlist decides which fields can be here at all. */
export interface Profile {
  /** The account id, so the card can link to the player's own osu! profile. */
  id: number;
  username: string;
  avatar_url: string;
  country_code: string;
  join_date: string;
  rank_history: { data: number[]; mode: string };
  statistics: {
    pp: number;
    global_rank: number | null;
    country_rank: number | null;
    hit_accuracy: number;
    play_count: number;
    play_time: number;
    grade_counts: { ssh: number; ss: number; sh: number; s: number; a: number };
  };
  /**
   * Unix seconds: when this body was last fetched from osu!.
   *
   * Absent on a payload written before the stamp existed, which reads as **unknown** rather than as
   * fresh — a card cannot honestly warn about an age it was never told.
   */
  fetched_at?: number;
}

  /**
   * The date this payload was last fetched, when that is old enough to be worth saying.
   *
   * `null` while the data is current, and `null` when the payload predates the stamp — an unknown
   * age is not a stale one, and guessing would put a false warning on a card that is fine.
   */
export function staleSince(profile: Profile, afterDays: number): Date | null {
  const fetchedAt = profile.fetched_at;
  if (typeof fetchedAt !== "number") return null;

  const ageDays = (Date.now() / 1000 - fetchedAt) / 86_400;
  return ageDays >= afterDays ? new Date(fetchedAt * 1000) : null;
}

/**
 * The profile, from **whichever of its two sources is newer**.
 *
 * **Two sources on purpose.** The Worker's upstream call leaves from Cloudflare's shared egress
 * addresses and osu! rate-limits per IP, so there are windows — measured in hours — in which the
 * Worker cannot reach the API at all and answers `503`, however correct its code is. `osu-ingest`
 * therefore publishes `profile-<mode>.json` beside the index, from a machine whose address osu!
 * does answer, and `.github/workflows/profile.yml` refreshes it hourly.
 *
 * **"Newer", not "whichever answered"**, and that distinction is the whole reason both are fetched.
 * The Worker keeps a copy for a week, so a Worker that has been failing for three days will happily
 * serve a three-day-old body rather than admit it — and preferring it merely because it answered
 * first would throw away the hour-old snapshot that exists precisely for that case. Both payloads
 * carry `fetched_at`, so the comparison is exact. It costs one extra ~1 KB request that the edge
 * caches anyway.
 */
export function useProfile(mode: string): Async<Profile> {
  const [result, setResult] = useState<Async<Profile>>({ state: "loading" });

  useEffect(() => {
    const controller = new AbortController();
    setResult({ state: "loading" });

    const read = async (url: string): Promise<Profile> => {
      const response = await fetch(url, { signal: controller.signal });
      if (!response.ok) throw new Error(`${response.status} ${response.statusText}`);
      return (await response.json()) as Profile;
    };

    /** Unix seconds, or `null` when the payload predates the stamp. */
    const fetchedAt = (profile: Profile) =>
      typeof profile.fetched_at === "number" ? profile.fetched_at : null;

    /** The newer of two, with an unstamped payload losing to a stamped one. */
    const newer = (a: Profile, b: Profile) => {
      const at = fetchedAt(a);
      const bt = fetchedAt(b);
      if (at === null) return b;
      if (bt === null) return a;
      return bt > at ? b : a;
    };

    // `allSettled` rather than `all`: one source failing is the normal case here, not an error.
    Promise.allSettled([
      read(`/api/osu/profile?mode=${mode}`),
      read(`${INDEX_BASE}profile-${mode}.json`),
    ]).then(([api, snapshot]) => {
      // An abort is the effect cleaning up, not a failure worth showing.
      if (controller.signal.aborted) return;

      const profile =
        api.status === "fulfilled" && snapshot.status === "fulfilled"
          ? newer(api.value, snapshot.value)
          : api.status === "fulfilled"
            ? api.value
            : snapshot.status === "fulfilled"
              ? snapshot.value
              : null;

      if (profile !== null) {
        setResult({ state: "ready", data: profile });
        return;
      }

      // Neither answered. Report the live route's failure — that is the one a deploy breaks, and
      // "404" for a snapshot nobody has published yet explains less.
      const reason = api.status === "rejected" ? api.reason : "no profile has been published";
      setResult({
        state: "error",
        error: reason instanceof Error ? reason.message : "failed",
      });
    });

    return () => controller.abort();
  }, [mode]);

  return result;
}
