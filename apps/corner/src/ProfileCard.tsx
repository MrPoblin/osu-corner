import { useEffect, useRef, useState } from "react";

import { formatDate, formatNumber, formatPlayTime, GRADES } from "./osu.ts";
import { OsuLogo } from "./OsuLogo.tsx";
import { Tip } from "./Tip.tsx";
import type { Async, Profile } from "./useJson.ts";

/**
 * The profile card (design §2). One request — the Worker's live route, with the snapshot
 * `osu-ingest` publishes as the fallback (see `useProfile`) — and every field shown comes from it.
 *
 * The picture and the name are the prominent half, side by side; everything else is smaller and
 * pushed to the side of the panel, so the identity leads instead of being huddled into the numbers.
 *
 * A ruleset with no plays is not an error: osu! answers `pp: 0` with `global_rank` and `country_rank`
 * null, so an absent rank is a real **unranked** state rather than a zero.
 */

const BADGES: {
  key: keyof Profile["statistics"]["grade_counts"];
  grade: (typeof GRADES)[number];
}[] = [
  { key: "ssh", grade: GRADES[0] },
  { key: "ss", grade: GRADES[1] },
  { key: "sh", grade: GRADES[2] },
  { key: "s", grade: GRADES[3] },
  { key: "a", grade: GRADES[4] },
];

export function ProfileCard({ profile }: { profile: Async<Profile> }) {
  if (profile.state === "loading") return <Shell />;

  if (profile.state === "error") {
    return (
      <p className="text-sm text-[var(--color-dim)]">
        The profile could not be loaded ({profile.error}). It comes from{" "}
        <code>/api/osu/profile</code>, with a published <code>profile-*.json</code> as the fallback —
        so neither the Worker nor an ingest run has produced one.
      </p>
    );
  }

  const { id, username, avatar_url, country_code, join_date, rank_history, statistics } = profile.data;

  // The card shows days once the number stops being readable; hovering gives the exact figure back.
  const hours = Math.round(statistics.play_time / 3600);

  // The id arrives with the profile. Until the Worker has been rebuilt with it, there is no profile
  // to link to, so the picture is left as a picture rather than a link to `users/undefined`.
  const osuProfile = typeof id === "number" ? `https://osu.ppy.sh/users/${id}` : null;

  /*
   * The guard on the name's size, not the size itself: the size comes from the space the name has (`.username`
   * in the stylesheet), and a card cannot know how long a name will be, so a long one would wrap and push the
   * facts down. Three steps read off its length. `--name-max` is the ceiling on the result.
   */
  const nameScale =
    username.length > 24 ? 0.55 : username.length > 16 ? 0.7 : username.length > 10 ? 0.85 : 1;

  const avatar = (
    <>
      <img src={avatar_url} alt="" width={104} height={104} className="avatar" />
      {osuProfile && (
        <span className="avatar-link__mark">
          <OsuMark />
        </span>
      )}
    </>
  );

  return (
    <div className="profile-row">
      <div className="panel profile">
        {/*
         * The picture, then the name beside it and everything else on the line below.
         *
         * Only the name is the centrepiece; the panel's own contents stay left-aligned beside the
         * picture, which is what the reference layout does.
         */}
        {osuProfile ? (
          <a
            className="avatar-link"
            href={osuProfile}
            target="_blank"
            rel="noreferrer"
            aria-label={`${username} on osu!`}
            title={`${username} on osu!`}
          >
            {avatar}
          </a>
        ) : (
          <span className="avatar-link">{avatar}</span>
        )}

        <div className="profile__name">
          <h2 className="username" style={{ "--name-scale": nameScale } as React.CSSProperties}>{username}</h2>

          {/* A two-letter code is a shortened name, so the card spells it out. */}
          <Tip pop={<b>{countryName(country_code)}</b>}>
            <span className="country">{country_code}</span>
          </Tip>
        </div>

        <div className="profile__details">
          <p>
            joined {formatDate(new Date(join_date))} · {formatNumber(statistics.play_count)} plays ·{" "}
            <Tip pop={<b>{formatNumber(hours)} hours played</b>}>
              <span className="cursor-default">{formatPlayTime(statistics.play_time)} played</span>
            </Tip>
          </p>

          <div className="grade-stats">
              {BADGES.map(({ key, grade }) => (
                <span key={key} className="grade-stat">
                  {/* The pill carries the grade colour; the ink says whether it was visibility-modified,
                      and the count underneath says how many. No card: it repeated all three. */}
                  <span
                    className="grade-pill"
                    style={{ backgroundColor: grade.colour, color: grade.ink }}
                  >
                    {grade.letter}
                  </span>
                  <span className="grade-stat__count">
                    {formatNumber(statistics.grade_counts[key])}
                  </span>
                </span>
              ))}
          </div>
        </div>
      </div>

      <div className="panel profile__stats">
        {/* No utility classes here: `flex-wrap` and the gaps are the stylesheet's, and Tailwind's utilities
            win over it by layer regardless of specificity — which is why the mobile rule that puts the four
            figures on one line had no effect at all. */}
        <dl>
          <Stat label="performance">
            <span className="text-[var(--accent)]">{formatNumber(statistics.pp)}</span>
            <span className="ml-1 text-[0.78rem] text-[var(--color-dim)]">pp</span>
          </Stat>

          <Stat label="global">
            <Rank value={statistics.global_rank} />
          </Stat>

          <Stat label="country">
            <Rank value={statistics.country_rank} />
          </Stat>

          <Stat label="accuracy">{statistics.hit_accuracy.toFixed(2)}%</Stat>
        </dl>

        {/* The label is gone; the graph is the whole thing, and a point reveals its own rank. */}
        <RankGraph values={rank_history.data} />
      </div>
    </div>
  );
}

/** `LT` becomes `Lithuania` — the browser already carries the region table. */
function countryName(code: string): string {
  try {
    return new Intl.DisplayNames(["en"], { type: "region" }).of(code) ?? code;
  } catch {
    return code;
  }
}

/**
 * The osu! mark, drawn from osu-web's own logo (§14.4).
 *
 * The logo is white artwork, so it sits on osu!'s pink the way the game's own icon does.
 */
function OsuMark() {
  return (
    <span className="osu-mark">
      <span className="osu-mark__logo">
        <OsuLogo />
      </span>
    </span>
  );
}

function Stat({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="stat">
      <dt className="stat__label">{label}</dt>
      <dd className="stat__value">{children}</dd>
    </div>
  );
}

/** A ruleset nobody has played has no rank, and the honest thing to say is so. */
function Rank({ value }: { value: number | null }) {
  if (value === null) return <span className="text-[var(--color-dim)]">unranked</span>;
  return <>#{formatNumber(value)}</>;
}

/**
 * The 90-day rank line, with the exact rank under the pointer.
 *
 * A *lower* rank is better, so the y axis is inverted — and because every point is a day, moving
 * across the graph is enough to pick one; no invisible hit targets are needed.
 */
function RankGraph({ values }: { values: number[] }) {
  const [hover, setHover] = useState<number | null>(null);
  /**
   * The graph is as wide as the panel, not a fixed 420px sitting in it with a gap at the end.
   *
   * Measured rather than stretched: a `preserveAspectRatio="none"` viewBox would fill the width but
   * squash the stroke and turn the hover dot into an ellipse.
   */
  const [width, setWidth] = useState(420);
  const box = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const element = box.current;
    if (!element) return;

    const observer = new ResizeObserver(([entry]) => {
      setWidth(Math.max(160, Math.round(entry.contentRect.width)));
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  if (values.length < 2) return <div className="rankgraph" ref={box} />;

  const min = Math.min(...values);
  const max = Math.max(...values);
  const span = max - min || 1;
  const height = 44;

  const points = values.map((value, index) => ({
    x: (index / (values.length - 1)) * width,
    y: ((value - min) / span) * (height - 5) + 2.5,
  }));

  const current = hover === null ? null : points[hover];

  return (
    <div className="rankgraph" ref={box}>
      <svg
        width={width}
        height={height}
        viewBox={`0 0 ${width} ${height}`}
        role="img"
        aria-label={`Rank across the last ${values.length} days`}
        onMouseMove={(event) => {
          const rect = event.currentTarget.getBoundingClientRect();
          const x = ((event.clientX - rect.left) / rect.width) * width;
          const index = Math.round((x / width) * (values.length - 1));
          setHover(Math.max(0, Math.min(values.length - 1, index)));
        }}
        onMouseLeave={() => setHover(null)}
      >
        <polyline
          points={points.map((point) => `${point.x.toFixed(1)},${point.y.toFixed(1)}`).join(" ")}
          fill="none"
          stroke="var(--accent)"
          strokeWidth="1.75"
          strokeLinejoin="round"
          strokeLinecap="round"
        />

        {current && (
          <>
            <line
              x1={current.x}
              y1="0"
              x2={current.x}
              y2={height}
              stroke="var(--color-line)"
              strokeWidth="1"
            />
            <circle cx={current.x} cy={current.y} r="3.5" fill="var(--accent)" />
          </>
        )}
      </svg>

      {hover !== null && current && (
        <span className="rankgraph__tip" style={{ left: `${(current.x / width) * 100}%` }}>
          #{formatNumber(values[hover])}
        </span>
      )}
    </div>
  );
}

function Shell() {
  return (
    <div className="flex flex-wrap items-start gap-7">
      <div className="size-[104px] rounded-[var(--radius-md)] bg-white/10" />
      <div className="grid gap-2">
        <div className="h-[34px] w-[220px] rounded-[var(--radius-sm)] bg-white/10" />
        <div className="h-[12px] w-[150px] rounded-[var(--radius-sm)] bg-white/10" />
        <div className="h-[12px] w-[200px] rounded-[var(--radius-sm)] bg-white/10" />
      </div>
    </div>
  );
}
