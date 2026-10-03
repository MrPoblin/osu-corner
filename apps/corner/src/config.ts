/**
 * **The corner's defaults, in one place, for whoever deploys it.**
 *
 * This repo is meant to be cloned by someone else and pointed at their own replays, so nothing in the app may
 * decide for them what they see first — and none of these values may name a person or a library. They are the
 * things a stranger changes after cloning; everything else is derived from the index they generate.
 *
 * The rest of the configuration is elsewhere, because it is not the browser's to decide: which accounts to
 * ingest lives in `osu-corner.local.toml`, the mount path in `apps/corner/vite.config.ts`, and the osu! client
 * credentials in `.dev.vars`. `README.md`'s Configuration table lists them all.
 */

export type ScoreMode = "lazer" | "stable";
export type ModeId = "osu" | "taiko" | "catch" | "mania";
export type SortKey = "pp" | "date" | "accuracy" | "stars";

export const CONFIG = {
  /** The ruleset the corner opens on. */
  defaultMode: "osu" as ModeId,

  /**
   * Which score the list is in, which is the pair the corner's own toggle switches between.
   *
   *   `"lazer"`  — osu!'s standardised totals. Every play has one, including stable-era plays converted.
   *   `"stable"` — the original V1 totals, and only the plays that were recorded with one.
   */
  defaultScore: "lazer" as ScoreMode,

  /** How the list is ordered to begin with. */
  defaultSort: "pp" as SortKey,

  /**
   * How old the profile may be before the card says when it was last fetched.
   *
   * **Nothing about the card needs a human**, which is exactly why a silent stop matters: the Worker
   * refreshes it whenever osu! answers, and `.github/workflows/profile.yml` refreshes the published
   * snapshot hourly — but GitHub disables scheduled workflows after 60 days without a push, and a
   * revoked credential looks identical from here. Past this age the card states the date instead of
   * presenting a month-old rank as if it were current. Raise it and the warning is rarer and less
   * useful; lower it and a quiet week starts crying wolf.
   */
  staleProfileAfterDays: 30,

  /**
   * Start with the drifting triangle field behind the page.
   *
   * The `?triangles` query flag turns it on regardless, so this is the default rather than the only say.
   */
  backgroundTriangles: false,
};
