import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";

import { DayPicker } from "./DayPicker.tsx";
import { Tip } from "./Tip.tsx";
import { MESH_OFF, meshTile } from "./Triangles.tsx";
import {
  DEFAULT_QUERY,
  dayCounts,
  describeSettings,
  formatAccuracy,
  formatNumber,
  formatRelative,
  GRADE_FILTERS,
  GRADES,
  modColour,
  modName,
  modsByCategory,
  ppColour,
  SORTS,
  search,
  starColour,
} from "./osu.ts";
import type { GradeKey, Mod, Play, Query } from "./osu.ts";
import type { ScoreMode } from "./config.ts";

/**
 * The list, its chrome, and the behaviour that drives the background.
 *
 * **The list is windowed against the page's own scroll position**, not against the selection. It
 * used to render a fixed band around whichever row was selected, and because selection only changes
 * on hover, scrolling did nothing until the pointer happened to land on a row — then the band jumped
 * and the missing rows appeared all at once. Windowing off `scrollY` makes scrolling the thing that
 * moves the window, which is what the user actually does.
 */

/**
 * One row is 86px tall with a 4px gap under it — a constant pitch, which is what makes a scroll
 * window exact without measuring anything.
 */
const PITCH = 90;

/** Rows kept rendered beyond the viewport, so a flick never shows a hole. */
const OVERSCAN = 6;

/** Artwork loads are delayed so sweeping the list does not fire forty downloads. */
const PREVIEW_DELAY = 90;

/**
 * off → require → forbid → off, the tri-state both filter chips share: a list of what must be there, a list of
 * what must not, and nothing in either meaning "don't care".
 */
function cycle<T>(required: T[], forbidden: T[], value: T): [T[], T[]] {
  if (required.includes(value)) return [required.filter((v) => v !== value), [...forbidden, value]];
  if (forbidden.includes(value)) return [required, forbidden.filter((v) => v !== value)];
  return [[...required, value], forbidden];
}

interface Props {
  mode: string;
  plays: Play[];
  /** The replay being pointed at, so the shell can recolour itself. */
  onPreview: (play: Play | null) => void;
  /** Which of the two totals the score column is in, set from the header's toggle. */
  scoreMode: ScoreMode;
}

export function ReplayList({ mode, plays, onPreview, scoreMode }: Props) {
  const [query, setQuery] = useState<Query>(DEFAULT_QUERY);
  const [selected, setSelected] = useState(0);
  const [range, setRange] = useState({ start: 0, count: 24 });
  const [modsOpen, setModsOpen] = useState(false);
  const listRef = useRef<HTMLDivElement | null>(null);
  const previewTimer = useRef<number | undefined>(undefined);
  /**
   * Scrolling the page is the keyboard's job only.
   *
   * Doing it on hover made the page lurch under the pointer: every row you crossed called
   * `scrollIntoView`, so moving the mouse up and down the list scrolled it uncontrollably.
   */
  const scrollWanted = useRef(false);

  /**
   * **One unit of measure per view.** A play carries two totals — the V1 number it was recorded with, and
   * osu!'s standardised conversion of it — and the stable view puts the V1 one in the score column while keeping
   * only the plays that have one. `legacyScore` is null exactly when there was no V1 number (a lazer-era play, or
   * a stable-era play wearing SV2), so the filter is the `null` test and nothing else.
   *
   * Filtering here rather than in `search` means the day shading and the mod counts describe the set on screen.
   */
  const pool = useMemo(() => {
    if (scoreMode !== "stable") return plays;

    return plays.filter((play) => play.legacyScore !== null).map((play) => ({
      ...play,
      score: play.legacyScore ?? play.score,
      /*
       * The Classic marker goes with it. `CL` is not a mod anyone played — osu!'s own API spells a stable-era
       * score that way and the ingest appends it to match — and in this view every row carries it, so the badge
       * says nothing. In the lazer view it stays: there it is what tells the stable-scored rows apart.
       */
      mods: play.mods.filter((mod) => mod.acronym !== "CL"),
    }));
  }, [plays, scoreMode]);

  const results = useMemo(() => search(pool, query), [pool, query]);
  const modGroups = useMemo(() => modsByCategory(pool), [pool]);
  const days = useMemo(() => dayCounts(pool), [pool]);
  /** The scale the pp ramp runs against: this mode's own best play. */
  /* The pp colour ramp keeps the whole library's ceiling in both views: the scale is a property of the data,
     and a ramp that moved when the filter did would repaint every row for nothing. */
  const ppMax = useMemo(() => plays.reduce((top, play) => Math.max(top, play.pp), 1), [plays]);

  /**
   * Switching modes resets the filters, the selection **and the window**.
   *
   * The window is measured against a scroll position that meant something in the mode being left, so
   * keeping it while the play count changes from 8,125 to 23 renders a page of empty space, then a
   * scramble of rows. Landing on the top of the list makes the new mode start where it should.
   */
  const lastMode = useRef(mode);
  useEffect(() => {
    setQuery(DEFAULT_QUERY);
    setSelected(0);
    setRange({ start: 0, count: 24 });

    // Only a real change of mode repositions; the first render (and StrictMode's second pass) does not.
    if (lastMode.current === mode) return;
    lastMode.current = mode;

    // And only when the reader is already down in the list — switching modes from the top of the page
    // should leave the page where it is.
    const list = listRef.current;
    if (!list) return;
    const top = list.getBoundingClientRect().top + window.scrollY - 12;
    if (window.scrollY > top) window.scrollTo({ top });
  }, [mode]);

  useEffect(() => setSelected(0), [query]);

  // The window follows the page's scroll, throttled to one layout read per frame.
  useEffect(() => {
    let frame = 0;

    const measure = () => {
      frame = 0;
      const list = listRef.current;
      if (!list) return;

      const top = list.getBoundingClientRect().top + window.scrollY;
      const relative = window.scrollY - top;
      const first = Math.max(0, Math.floor(relative / PITCH) - OVERSCAN);
      const last = Math.ceil((relative + window.innerHeight) / PITCH) + OVERSCAN;
      const next = { start: first, count: Math.max(1, last - first) };

      /*
       * Only when it actually moved. Setting the same range every frame re-rendered the whole list on
       * every scroll frame, which is what made scrolling crawl on a throttled CPU.
       */
      setRange((current) => (current.start === next.start && current.count === next.count ? current : next));
    };

    const onScroll = () => {
      if (!frame) frame = requestAnimationFrame(measure);
    };

    measure();
    window.addEventListener("scroll", onScroll, { passive: true });
    window.addEventListener("resize", onScroll);

    return () => {
      if (frame) cancelAnimationFrame(frame);
      window.removeEventListener("scroll", onScroll);
      window.removeEventListener("resize", onScroll);
    };
  }, [results.length]);

  /*
   * One cover on load — the best play's — and one more on each hover. Never a cover per rendered
   * row: `Sky` holds a single pair of layers and swaps between them. A filter change deliberately
   * does not fire a download per keystroke.
   */
  useEffect(() => {
    onPreview(results[0] ?? null);
  }, [plays, onPreview]);

  useEffect(() => () => window.clearTimeout(previewTimer.current), []);

  function preview(play: Play) {
    window.clearTimeout(previewTimer.current);
    previewTimer.current = window.setTimeout(() => onPreview(play), PREVIEW_DELAY);
  }

  /**
   * Keyboard movement scrolls the page itself, because the target row may not be rendered yet —
   * asking a missing node to scroll itself into view is what left the old list on the wrong rows.
   */
  function reveal(index: number) {
    const list = listRef.current;
    if (!list) return;

    const top = list.getBoundingClientRect().top + window.scrollY + index * PITCH;
    const bottom = top + (PITCH - 4);

    if (top < window.scrollY) window.scrollTo({ top: top - 8, behavior: "smooth" });
    else if (bottom > window.scrollY + window.innerHeight) {
      window.scrollTo({ top: bottom - window.innerHeight + 8, behavior: "smooth" });
    }
  }

  function move(next: number) {
    const clamped = Math.max(0, Math.min(results.length - 1, next));
    setSelected(clamped);
    if (scrollWanted.current) {
      scrollWanted.current = false;
      reveal(clamped);
    }
    const play = results[clamped];
    if (play) preview(play);
  }

  /*
   * Stable identities, so `Row`'s memo actually holds. With an inline arrow per row, every row
   * re-rendered whenever the selection moved — 23 rows of Tips and mod badges on every hover.
   */
  const enterRow = useCallback((index: number) => move(index), [results]);
  const focusRow = useCallback((index: number) => setSelected(index), []);

  /** off → require → forbid → off, so a chip can ask for and against the same thing. */
  function cycleMod(acronym: string) {
    setQuery((q) => {
      const [mods, excluded] = cycle(q.mods, q.excluded, acronym);
      return { ...q, mods, excluded };
    });
  }

  /** The same cycle for a grade letter, so both filters read the same way. */
  function cycleGrade(key: GradeKey) {
    setQuery((q) => {
      const [grades, excludedGrades] = cycle(q.grades, q.excludedGrades, key);
      return { ...q, grades, excludedGrades };
    });
  }

  // The measured window, clamped to what still exists after a filter narrows the list.
  const start = Math.min(range.start, Math.max(0, results.length - 1));
  const end = Math.min(results.length, start + range.count);

  const visible = results.slice(start, end);

  return (
    <>
      <div className="toolbar">
        <div className="toolbar__row">
          <label className="control search">
            <SearchIcon />
            <input
              type="search"
              value={query.text}
              placeholder="Search plays"
              onChange={(event) => setQuery((q) => ({ ...q, text: event.target.value }))}
            />
          </label>

          <DayPicker counts={days} value={query.day} onChange={(day) => setQuery((q) => ({ ...q, day }))} />

          {/*
           * A phone shows this instead of 18 chips; CSS hides it everywhere else. It sits on the day picker's
           * line, which has room, rather than at the end of the control row where it wrapped onto a line of its
           * own for one chip.
           */}
          <button
            type="button"
            className="chip mods-toggle"
            aria-expanded={modsOpen}
            aria-controls="mods-filter"
            onClick={() => setModsOpen((open) => !open)}
          >
            Mods
            {query.mods.length + query.excluded.length > 0 && (
              <span className="chip__count">{query.mods.length + query.excluded.length}</span>
            )}
            <span aria-hidden="true">{modsOpen ? "▴" : "▾"}</span>
          </button>

          {/*
           * The count belongs to the search box's line, where `margin-left: auto` parks it at the right end
           * of whatever line it lands on — the day picker's, on a phone. As a column of the toolbar it
           * reserved its own width beside every control above it, which is what squeezed the search box.
           */}
          <span className="count">
            {formatNumber(results.length)} of {formatNumber(plays.length)} plays
          </span>

        <div className="flex flex-wrap items-center gap-1" role="group" aria-label="Sort by">
          {SORTS.map((sort) => {
            const current = query.sort === sort.key;
            return (
              <button
                key={sort.key}
                type="button"
                className="control"
                aria-pressed={current}
                onClick={() =>
                  setQuery((q) => ({
                    ...q,
                    sort: sort.key,
                    descending: q.sort === sort.key ? !q.descending : true,
                  }))
                }
              >
                {sort.label}
                {current && <span aria-hidden="true">{query.descending ? "↓" : "↑"}</span>}
              </button>
            );
          })}
        </div>

        <div className="flex flex-wrap items-center gap-1" role="group" aria-label="Grade">
          {/*
           * The day picker's form: one pill, seven sections, hairlines between — a wall of seven separate chips
           * would be seven outlines to read instead of one control. A section toggles off → must → must not, the
           * same as a mod chip, and lights up in its own grade's colour (the row's colour, from `GRADES`).
           */}
          <div className="grade-filter">
            {GRADE_FILTERS.map((entry) => {
              const state = query.grades.includes(entry.key)
                ? "in"
                : query.excludedGrades.includes(entry.key)
                  ? "out"
                  : "off";
              const grade = GRADES[entry.indices[0]];

              return (
                <button
                  key={entry.key}
                  type="button"
                  className="grade-filter__button"
                  data-grade-state={state}
                  aria-pressed={state !== "off"}
                  /* Spelled out, because `aria-pressed` alone cannot say must from must-not. */
                  aria-label={
                    state === "off" ? entry.letter : `${entry.letter} ${state === "in" ? "only" : "excluded"}`
                  }
                  style={state === "in" ? { backgroundColor: grade.colour, color: grade.ink } : undefined}
                  onClick={() => cycleGrade(entry.key)}
                >
                  {entry.letter}
                </button>
              );
            })}
          </div>

          {/* The progression: only the plays that raised the best pp at the time. */}
          <button
            type="button"
            className="chip"
            aria-pressed={query.pbOnly}
            title="Only plays that were a new best pp when they were set"
            style={query.pbOnly ? { backgroundColor: "var(--accent)" } : undefined}
            onClick={() => setQuery((q) => ({ ...q, pbOnly: !q.pbOnly }))}
          >
            pb progression
          </button>

          {/* A difficulty's pp is its best score, so this is the set the profile figure is made of. */}
          <button
            type="button"
            className="chip"
            aria-pressed={query.bestPerDiff}
            title="Only your best score on each beatmap difficulty"
            style={query.bestPerDiff ? { backgroundColor: "var(--accent)" } : undefined}
            onClick={() => setQuery((q) => ({ ...q, bestPerDiff: !q.bestPerDiff }))}
          >
            best per diff
          </button>
        </div>
        </div>

        <div
          className="mods-filter"
          id="mods-filter"
          data-open={modsOpen ? "true" : undefined}
          role="group"
          aria-label="Mods"
        >
          {modGroups.map((group) => (
            <div key={group.key} className="mod-group">
              <span className="mod-group__label" style={{ color: group.colour }}>
                {group.label}
              </span>

              {group.mods.map(({ acronym, count }) => {
                const state = query.mods.includes(acronym)
                  ? "in"
                  : query.excluded.includes(acronym)
                    ? "out"
                    : "off";

                return (
                  /* The chip can only hold two letters, so the card behind it carries the mod's name.
                     Nothing else: the count is already on the chip. */
                  <Tip
                    key={acronym}
                    pop={<b>{modName(acronym)}</b>}
                  >
                    <button
                      type="button"
                      className="chip"
                      data-mod-state={state}
                      aria-pressed={state !== "off"}
                      style={state === "in" ? { backgroundColor: modColour(acronym) } : undefined}
                      onClick={() => cycleMod(acronym)}
                    >
                      {acronym}
                      <span className="chip__count">{count}</span>
                    </button>
                  </Tip>
                );
              })}
            </div>
          ))}
        </div>
      </div>

      {results.length === 0 ? (
        <div className="panel mt-3 grid place-items-center gap-2 px-6 py-12 text-center">
          <p className="text-sm text-[var(--color-dim)]">Nothing here matches that combination.</p>
          <button type="button" className="control" onClick={() => setQuery(DEFAULT_QUERY)}>
            Clear filters
          </button>
        </div>
      ) : (
        <div
          ref={listRef}
          className="list mt-3"
          role="listbox"
          aria-label="Plays"
          tabIndex={0}
          onKeyDown={onKeyDown}
        >
          {/* Spacers stand in for the rows that are not rendered, so the scrollbar stays honest. */}
          {start > 0 && <div style={{ height: start * PITCH }} aria-hidden="true" />}

          {visible.map((play, offset) => {
            const index = start + offset;
            const active = index === selected;

            return (
              <Row
                key={play.id}
                play={play}
                index={index}
                active={active}
                ppMax={ppMax}
                onEnter={enterRow}
                onFocus={focusRow}
              />
            );
          })}

          {end < results.length && (
            <div style={{ height: (results.length - end) * PITCH }} aria-hidden="true" />
          )}
        </div>
      )}
    </>
  );

  function onKeyDown(event: React.KeyboardEvent) {
    scrollWanted.current = true;
    if (event.key === "ArrowDown") move(selected + 1);
    else if (event.key === "ArrowUp") move(selected - 1);
    else if (event.key === "PageDown") move(selected + 10);
    else if (event.key === "PageUp") move(selected - 10);
    else {
      scrollWanted.current = false;
      return;
    }

    event.preventDefault();
  }
}

interface RowProps {
  play: Play;
  index: number;
  active: boolean;
  ppMax: number;
  onEnter: (index: number) => void;
  onFocus: (index: number) => void;
}

/**
 * One row, laid out the way osu-web lays out its own score rows: the grade on the left as a
 * full-height block, three lines of identity, then the mods, the stars, accuracy and a single pp
 * figure at the far right in its own arrow-ended block.
 *
 * The mapper is deliberately absent — it was asked for twice and then explicitly removed.
 */
const Row = memo(function Row({ play, index, active, ppMax, onEnter, onFocus }: RowProps) {
  const { beatmap, grade } = play;

  // The play's own rating, which is what matters on a modded score, and the map's unmodified one
  // underneath it on hover.
  const modded = play.stars;
  const base = beatmap.stars;
  const differs = Math.abs(modded - base) >= 0.01;

  return (
    <div
      role="option"
      aria-selected={active}
      data-selected={active}
      className="row"
      onMouseEnter={() => onEnter(index)}
      onFocus={() => onFocus(index)}
      tabIndex={-1}
      style={
        {
          "--star": starColour(modded),
          "--pp": ppColour(play.pp, ppMax),
          /* Which row this is: its own mesh. */
          "--row-i": index,
          "--tri-mesh": meshTile(starColour(modded), index),
        } as CSSProperties
      }
    >
      {/*
       * This row's own mesh, in this row's own star colour, drifting.
       *
       * Per row, not one field shared down the list: the game gives every score its own, which is why
       * two rows never show the same arrangement. `aria-hidden` because it is decoration.
       */}
      {!MESH_OFF && (
        <span className="row__mesh" aria-hidden="true">
          <span className="row__mesh__tile" />
        </span>
      )}
      {/* The ink is the distinction: yellow for a plain grade, white for a visibility-modified one. */}
      {/* `--grade` because the flare's gradient cannot read `background-color`; the stylesheet paints both. */}
      <span
        className="row__grade"
        style={{ "--grade": grade.colour, color: grade.ink } as React.CSSProperties}
      >
        {grade.letter}
      </span>

      <div className="row__identity">
        {/* The row shows the map's own name; the full `artist — title` is a hover away, since the
            artist has no line of its own on osu-web's three-line layout. */}
        <Tip
          className="tip--fill"
          align="start"
          pop={
            <>
              <b>
                {beatmap.artist} — {beatmap.title}
              </b>
              <span>{beatmap.difficulty}</span>
            </>
          }
        >
          <p className="row__title">{beatmap.title}</p>
        </Tip>

        <p className="row__score">
          {formatNumber(play.score)} <span className="row__combo">{formatNumber(play.combo)}x</span>
        </p>

        <p className="row__meta">
          {/* The full difficulty is in the card, because a long one cannot expand in place — and the
              card only appears when the name is actually cut off, since a card repeating the words
              already on screen is noise. */}
          <Tip className="tip--fill" align="start" clipOnly pop={<b>{beatmap.difficulty}</b>}>
            <span className="row__difficulty">{beatmap.difficulty}</span>
          </Tip>

          <Tip pop={<b>{play.playedAt.toLocaleString()}</b>}>
            <span className="row__date">{formatRelative(play.playedAt)}</span>
          </Tip>
        </p>
      </div>

      <ul className="row__mods">
        {play.mods.map((mod) => (
          <li key={mod.acronym} className="row__mod">
            <ModBadge mod={mod} />
          </li>
        ))}
      </ul>

      {/* Only when the mods moved the rating, and then it carries the one thing the row cannot say:
          what the map rates with no mods at all. Same number, no card. */}
      <Tip
        className="tip--stars"
        align="center"
        pop={differs ? <b>{base.toFixed(2)}★</b> : undefined}
      >
        <p className="row__stars">{modded.toFixed(2)}★</p>
      </Tip>

      <p className="row__accuracy">{formatAccuracy(play.accuracy)}</p>

      {/* One raw pp value — not the weighted one, which was never asked for — in the block osu!'s
          own score rows use, with the left edge cut into a chevron. The row rounds pp to the whole
          number osu! displays; the exact value the index stored is on the card. */}
      {/* `center`, not `end`: the block is mostly empty space to the right of the number, so anchoring
          to its edge put the card over the "pp" unit instead of on the value. The block centres its
          own text, so the card centred on the block lands on the number. */}
      <Tip
        className="tip--stretch"
        align="center"
        pop={<b>{play.pp.toFixed(1)} pp</b>}
      >
        <span className="row__pp">
          {formatNumber(play.pp)}
          <span className="row__pp-unit">pp</span>
        </span>
      </Tip>
    </div>
  );
});

/**
 * osu!'s flat hexagon, holding the community-standard acronym in its category colour, with a real
 * hover card carrying the mod's full name and any setting it was played with.
 */
function ModBadge({ mod }: { mod: Mod }) {
  const settings = mod.settings ? describeSettings(mod.settings) : [];

  return (
    <Tip
      pop={
        <>
          <b>{mod.name ?? mod.acronym}</b>
          {settings.map((line) => (
            <span key={line}>{line}</span>
          ))}
        </>
      }
    >
      <span className="mod" style={{ backgroundColor: mod.colour }}>
        {mod.acronym}
      </span>
    </Tip>
  );
}

function SearchIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <circle cx="6.6" cy="6.6" r="4.6" stroke="currentColor" strokeWidth="1.8" />
      <path d="M10.2 10.2 14 14" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
    </svg>
  );
}
