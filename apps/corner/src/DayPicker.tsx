import { useMemo, useState } from "react";

import { dayKey, formatNumber } from "./osu.ts";

/**
 * The calendar. It has **three zoom levels** — days, months, years — and only ever offers days that
 * have plays, so selecting one can never land on an empty list.
 *
 * The design had this as an open question (§13: "date picker versus a year/month/day heatmap"); it is
 * both, because the same count data shades whichever level you are looking at.
 */

const DOW = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

type View = "days" | "months" | "years";

function parseDay(key: string): Date {
  const [year, month, day] = key.split("-").map(Number);
  return new Date(year, month - 1, day);
}

function formatDay(key: string): string {
  return parseDay(key).toLocaleDateString("en-GB", {
    day: "numeric",
    month: "short",
    year: "numeric",
  });
}

interface Props {
  counts: Map<string, number>;
  value: string | null;
  onChange: (day: string | null) => void;
}

export function DayPicker({ counts, value, onChange }: Props) {
  const days = useMemo(() => [...counts.keys()].sort(), [counts]);
  const busiest = useMemo(() => Math.max(1, ...counts.values()), [counts]);

  const [open, setOpen] = useState(false);
  const [view, setView] = useState<View>("days");
  const [cursor, setCursor] = useState(() => (value ? parseDay(value) : new Date()));

  /** Play counts rolled up by year and by month, for the two zoomed-out levels. */
  const { byYear, byMonth } = useMemo(() => {
    const years = new Map<number, number>();
    const months = new Map<string, number>();

    for (const [key, count] of counts) {
      const [year, month] = key.split("-");
      years.set(Number(year), (years.get(Number(year)) ?? 0) + count);
      months.set(`${year}-${month}`, (months.get(`${year}-${month}`) ?? 0) + count);
    }

    return { byYear: years, byMonth: months };
  }, [counts]);

  const previous = useMemo(() => {
    const ordered = [...days].reverse();
    return (value ? ordered.find((day) => day < value) : ordered[0]) ?? null;
  }, [days, value]);

  const next = useMemo(
    () => (value ? days.find((day) => day > value) : days[0]) ?? null,
    [days, value],
  );

  const cells = useMemo(() => {
    const year = cursor.getFullYear();
    const month = cursor.getMonth();
    const lead = (new Date(year, month, 1).getDay() + 6) % 7;
    const length = new Date(year, month + 1, 0).getDate();

    const blanks: null[] = Array.from({ length: lead }, () => null);
    const dates = Array.from({ length }, (_, index) => dayKey(new Date(year, month, index + 1)));

    return [...blanks, ...dates];
  }, [cursor]);

  const years = useMemo(() => [...byYear.keys()].sort((a, b) => b - a), [byYear]);

  /** Shading, as a percentage of the busiest day, so every level reads as a density map. */
  const heat = (count: number): React.CSSProperties =>
    ({ "--heat": `${Math.round(16 + (count / busiest) * 64)}%` }) as React.CSSProperties;

  return (
    <div className="daypicker">
      <button
        type="button"
        className="control"
        aria-label="Previous day with plays"
        disabled={previous === null}
        style={previous === null ? { opacity: 0.4, cursor: "default" } : undefined}
        onClick={() => previous && onChange(previous)}
      >
        <Chevron direction="left" />
      </button>

      <button
        type="button"
        className="control"
        aria-expanded={open}
        onClick={() => {
          setView("days");
          setCursor(value ? parseDay(value) : new Date());
          setOpen((wasOpen) => !wasOpen);
        }}
      >
        <CalendarIcon />
        {value ? formatDay(value) : "Any day"}
      </button>

      <button
        type="button"
        className="control"
        aria-label="Next day with plays"
        disabled={next === null}
        style={next === null ? { opacity: 0.4, cursor: "default" } : undefined}
        onClick={() => next && onChange(next)}
      >
        <Chevron direction="right" />
      </button>

      {open && (
        <div className="daypicker__panel">
          <div className="daypicker__head">
            <button
              type="button"
              className="chip"
              aria-label={view === "days" ? "Previous month" : "Previous year"}
              onClick={() =>
                setCursor(
                  view === "days"
                    ? new Date(cursor.getFullYear(), cursor.getMonth() - 1, 1)
                    : new Date(cursor.getFullYear() - 1, 0, 1),
                )
              }
            >
              <Chevron direction="left" />
            </button>

            {/* The title is the zoom control: days → months → years. */}
            <button
              type="button"
              className="daypicker__title"
              onClick={() =>
                setView(view === "days" ? "months" : view === "months" ? "years" : "days")
              }
              title={
                view === "days"
                  ? "Show months"
                  : view === "months"
                    ? "Show years"
                    : "Back to days"
              }
            >
              {view === "days" &&
                cursor.toLocaleDateString("en-GB", { month: "long", year: "numeric" })}
              {view === "months" && cursor.getFullYear()}
              {view === "years" && "all years"}
            </button>

            <button
              type="button"
              className="chip"
              aria-label={view === "days" ? "Next month" : "Next year"}
              onClick={() =>
                setCursor(
                  view === "days"
                    ? new Date(cursor.getFullYear(), cursor.getMonth() + 1, 1)
                    : new Date(cursor.getFullYear() + 1, 0, 1),
                )
              }
            >
              <Chevron direction="right" />
            </button>
          </div>

          {view === "years" && (
            <div className="daypicker__zoom">
              {years.map((year) => (
                <button
                  key={year}
                  type="button"
                  className="daypicker__tile"
                  data-count={byYear.get(year)}
                  style={heat(byYear.get(year) ?? 0)}
                  onClick={() => {
                    setCursor(new Date(year, 0, 1));
                    setView("months");
                  }}
                >
                  {year}
                  <span className="daypicker__tile-count">{formatNumber(byYear.get(year) ?? 0)}</span>
                </button>
              ))}
            </div>
          )}

          {view === "months" && (
            <div className="daypicker__zoom">
              {Array.from({ length: 12 }, (_, index) => {
                const key = `${cursor.getFullYear()}-${`${index + 1}`.padStart(2, "0")}`;
                const count = byMonth.get(key);

                return (
                  <button
                    key={index}
                    type="button"
                    className="daypicker__tile"
                    disabled={count === undefined}
                    data-count={count}
                    style={count === undefined ? { opacity: 0.3 } : heat(count)}
                    onClick={() => {
                      setCursor(new Date(cursor.getFullYear(), index, 1));
                      setView("days");
                    }}
                  >
                    {new Date(cursor.getFullYear(), index, 1).toLocaleDateString("en-GB", {
                      month: "short",
                    })}
                    <span className="daypicker__tile-count">
                      {count === undefined ? "—" : formatNumber(count)}
                    </span>
                  </button>
                );
              })}
            </div>
          )}

          {view === "days" && (
            <div className="daypicker__grid">
              {DOW.map((label, index) => (
                <span key={index} className="daypicker__dow">
                  {label}
                </span>
              ))}

              {cells.map((key, index) => {
                if (key === null) return <span key={index} />;

                const count = counts.get(key);

                return (
                  <button
                    key={index}
                    type="button"
                    className="daypicker__day"
                    aria-pressed={key === value}
                    disabled={count === undefined}
                    style={count === undefined ? { opacity: 0.3 } : heat(count)}
                    title={count === undefined ? undefined : `${formatNumber(count)} plays`}
                    onClick={() => {
                      onChange(key);
                      setOpen(false);
                    }}
                  >
                    {parseDay(key).getDate()}
                  </button>
                );
              })}
            </div>
          )}

          <div className="mt-2 flex items-center justify-between">
            <span className="count">
              {value ? `${formatNumber(counts.get(value) ?? 0)} plays` : "every day"}
            </span>
            <button
              type="button"
              className="chip"
              aria-pressed={value === null}
              onClick={() => {
                onChange(null);
                setOpen(false);
              }}
            >
              Any day
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function Chevron({ direction }: { direction: "left" | "right" }) {
  return (
    <svg width="8" height="12" viewBox="0 0 8 12" fill="none" aria-hidden="true">
      <path
        d={direction === "left" ? "M6.5 1 1.5 6l5 5" : "M1.5 1l5 5-5 5"}
        stroke="currentColor"
        strokeWidth="1.8"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function CalendarIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <rect x="1.6" y="2.8" width="12.8" height="11.6" rx="3" stroke="currentColor" strokeWidth="1.6" />
      <path
        d="M1.6 6.6h12.8M5.2 1.4v2.6M10.8 1.4v2.6"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinecap="round"
      />
    </svg>
  );
}
