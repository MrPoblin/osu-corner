import { useCallback, useState } from "react";
import type { CSSProperties } from "react";

import { ProfileCard } from "./ProfileCard.tsx";
import { ReplayList } from "./ReplayList.tsx";
import { Sky } from "./Sky.tsx";
import { MESH_STILL, Triangles, trianglesOn, useEntryFinished } from "./Triangles.tsx";
import { accentFor, coverUrl, formatNumber } from "./osu.ts";
import type { Play } from "./osu.ts";
import { CONFIG } from "./config.ts";
import type { ScoreMode } from "./config.ts";
import { useIndexes } from "./useJson.ts";

/**
 * The corner.
 *
 * The background is the cover art of whichever replay is being pointed at, and the page's **accent
 * is derived from that play's star rating** — see `accentFor` for why it is not sampled from the
 * artwork itself (osu!'s CDN sends no CORS header, so the browser cannot read those pixels).
 *
 * API paths are site-wide at `/api/osu/...` so a clone mounted at any base gets the same route
 * (`poblin-osu-api-plan.md` §1); everything else goes through `import.meta.env.BASE_URL`.
 */

const MODES = [
  { id: "osu", label: "osu!" },
  { id: "taiko", label: "taiko" },
  { id: "catch", label: "catch" },
  { id: "mania", label: "mania" },
] as const;

type ModeId = (typeof MODES)[number]["id"];

/**
 * The two units of measure the score column can be in, in the order the toggle shows them.
 *
 * `stable` is the original V1 total — the number the play actually recorded — and `lazer` is osu!'s standardised
 * conversion of it. They are not two clients: they are the two things a score can mean.
 */
const SCORE_MODES: { id: ScoreMode; label: string; title: string }[] = [
  { id: "stable", label: "stable", title: "The original V1 totals, for the plays that have one" },
  { id: "lazer", label: "lazer", title: "osu!'s standardised totals, which every play has" },
];
const MODE_IDS = MODES.map((entry) => entry.id);

export default function App() {
  const [mode, setMode] = useState<ModeId>(CONFIG.defaultMode);
  const [preview, setPreview] = useState<Play | null>(null);
  /**
   * Which of the two totals the score column is in.
   *
   * A play carries both — the V1 number it was recorded with, and osu!'s standardised conversion of it — and
   * only one unit of measure is on screen at a time (§5). `stable` keeps only the plays that *have* a V1 number
   * (`legacyScore` is null for a lazer-era play, or a stable-era play wearing SV2) and puts that number in the
   * column; `lazer` is every play, in the standardised totals. What it starts as is a deployment's, in
   * `config.ts`.
   */
  const [scoreMode, setScoreMode] = useState<ScoreMode>(CONFIG.defaultScore);
  const indexes = useIndexes(MODE_IDS);

  // Stable, because `ReplayList` reports previews from an effect.
  const onPreview = useCallback((play: Play | null) => setPreview(play), []);

  // The arrival sweep is a one-off; leaving its elements mounted paints nothing forever.
  const entered = useEntryFinished();
  // The reveal's clip is released when it lands, for Firefox; see `.reveal[data-revealed]`.
  const revealed = useEntryFinished(700);

  const accent = preview ? accentFor(preview.stars) : "#ff66aa";

  return (
    <div
      className="relative min-h-[100dvh]"
      style={{ "--accent": accent } as CSSProperties}
      data-mesh={MESH_STILL ? "still" : undefined}
    >
      <Sky cover={preview ? coverUrl(preview.beatmap.set) : null} />

      {trianglesOn() && !entered && (
        <div className="entry" aria-hidden="true">
          <Triangles />
        </div>
      )}

      <div className="reveal" data-revealed={revealed ? "" : undefined}>
        <header className="mx-auto flex w-full max-w-[1240px] flex-wrap items-center justify-between gap-3 px-3 pt-5 sm:px-6 sm:pt-7">
          <div className="flex flex-wrap items-center gap-3">
            {/* A plain circle: "back to site" is a way out, not a destination worth a full button, and
                the wordmark should be the first thing on the page. */}
            <a className="icon-control" href="/" aria-label="Back to the site" title="Back to the site">
              <Chevron />
            </a>
            <h1 className="brand text-[1.05rem]">osu! corner</h1>

            {/*
             * **The score, not the client.** A pair rather than a switch: there are exactly two units of measure
             * and the page is always in one. Beside the wordmark it costs no line of its own on a phone — as the
             * last item of the mode row it read as one more mode, and on a narrow header it wrapped onto a line
             * by itself.
             */}
            <div className="score-mode" role="group" aria-label="Score">
              {SCORE_MODES.map((entry) => (
                <button
                  key={entry.id}
                  type="button"
                  className="score-mode__option"
                  aria-pressed={entry.id === scoreMode}
                  title={entry.title}
                  onClick={() => setScoreMode(entry.id)}
                >
                  {entry.label}
                </button>
              ))}
            </div>
          </div>

          <nav className="flex flex-wrap items-center gap-1.5" aria-label="Game mode">
              {MODES.map((entry) => {
                const count = indexes.state === "ready" ? indexes.data[entry.id]?.length : undefined;

                return (
                  <button
                    key={entry.id}
                    type="button"
                    className="control"
                    aria-current={entry.id === mode ? "page" : undefined}
                    onClick={() => setMode(entry.id)}
                  >
                    {entry.label}
                    {count !== undefined && (
                      <span className="count" style={{ color: "inherit", opacity: 0.65 }}>
                        {formatNumber(count)}
                      </span>
                    )}
                  </button>
                );
              })}
            </nav>
        </header>

        <main className="mx-auto w-full max-w-[1240px] px-3 sm:px-6">
          <section aria-label="Profile" className="mt-3">
            <ProfileCard mode={mode} />
          </section>

          <section aria-label="Library" className="pt-3 pb-24">
            {indexes.state === "loading" && <ListSkeleton />}

            {indexes.state === "error" && (
              <div className="panel px-6 py-6">
                <p className="text-sm text-[var(--color-dim)]">
                  The index could not be loaded ({indexes.error}). The generated{" "}
                  <code>index-*.json</code> files are copied from <code>library/</code> into{" "}
                  <code>apps/corner/public/</code> by the ingest step.
                </p>
              </div>
            )}

            {indexes.state === "ready" && (
              <ReplayList
                mode={mode}
                plays={indexes.data[mode] ?? []}
                onPreview={onPreview}
                scoreMode={scoreMode}
              />
            )}
          </section>
        </main>
      </div>
    </div>
  );
}

function Chevron() {
  return (
    <svg width="7" height="11" viewBox="0 0 8 12" fill="none" aria-hidden="true">
      <path
        d="M6.5 1 1.5 6l5 5"
        stroke="currentColor"
        strokeWidth="1.8"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

/** The list's shape while the index is in flight: the same geometry, no invented numbers. */
function ListSkeleton() {
  return (
    <div className="list">
      {Array.from({ length: 9 }, (_, index) => (
        <div key={index} className="row" style={{ opacity: 1 - index * 0.07 }} aria-hidden="true" />
      ))}
    </div>
  );
}
