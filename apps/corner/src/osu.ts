/**
 * osu!'s own tables, in one place, so the row renders *from* data rather than from conditionals
 * scattered through a component.
 *
 * Sources, and nothing here is chosen by eye:
 *   - grade colours:  lazer `OsuColour.ForRank`
 *   - star ramp:      lazer `OsuColour.STAR_DIFFICULTY_SPECTRUM`
 *   - mod categories: lazer `OsuColour.ForModType`
 *   - mod names/type: lazer's per-mod class declarations
 * See `poblin-osu-corner-design.md` §14.2 and §14.4.
 */

import { CONFIG } from "./config.ts";

/* -------------------------------------------------------------------------------------------------
 * Grades
 * ---------------------------------------------------------------------------------------------- */

/**
 * The index's grade wire order, verbatim: XH X SH S A B C D F. `letter` is what osu! shows — the
 * hidden variants (XH, SH) share a name with their plain counterparts because you cannot see
 * "hidden" in a letter, and the colour already separates the tiers.
 */
export const GRADES = [
  { letter: "SS", colour: "#de31ae", hidden: true, ink: "#ffffff" }, // XH
  { letter: "SS", colour: "#de31ae", hidden: false, ink: "#ffe08f" }, // X
  { letter: "S", colour: "#02b5c3", hidden: true, ink: "#ffffff" }, // SH
  { letter: "S", colour: "#02b5c3", hidden: false, ink: "#ffe08f" }, // S
  { letter: "A", colour: "#88da20", hidden: false, ink: "#26350a" },
  { letter: "B", colour: "#e3b130", hidden: false, ink: "#3a2c05" },
  { letter: "C", colour: "#ff8e5d", hidden: false, ink: "#3a1a08" },
  { letter: "D", colour: "#ff5a5a", hidden: false, ink: "#ffffff" },
  { letter: "F", colour: "#3f3f3f", hidden: false, ink: "#ffffff" },
] as const;

/* -------------------------------------------------------------------------------------------------
 * Star difficulty
 * ---------------------------------------------------------------------------------------------- */

/** lazer's `STAR_DIFFICULTY_SPECTRUM`, exactly as declared, as [stars, colour] stops. */
const STAR_STOPS: [number, string][] = [
  [0.1, "#aaaaaa"],
  [0.1, "#4290fb"],
  [1.25, "#4fc0ff"],
  [2.0, "#4fffd5"],
  [2.5, "#7cff4f"],
  [3.3, "#f6f05c"],
  [4.2, "#ff8068"],
  [4.9, "#ff4e6f"],
  [5.8, "#c645b8"],
  [6.7, "#6563de"],
];

function hexToRgb(hex: string): [number, number, number] {
  const n = Number.parseInt(hex.slice(1), 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

/**
 * The ramp sampled at a star value, as channel values.
 *
 * lazer rounds the star value to two decimals and interpolates, so a 4.90-star play is the identical
 * colour here as in the game.
 */
export function starRgb(stars: number): [number, number, number] {
  const value = Math.round(stars * 100) / 100;

  let lower = STAR_STOPS[0];
  let upper = STAR_STOPS[STAR_STOPS.length - 1];

  for (let i = 0; i < STAR_STOPS.length; i += 1) {
    if (STAR_STOPS[i][0] <= value) lower = STAR_STOPS[i];
    if (STAR_STOPS[i][0] >= value) {
      upper = STAR_STOPS[i];
      break;
    }
  }

  const span = upper[0] - lower[0];
  const t = span <= 0 ? 0 : (value - lower[0]) / span;

  const a = hexToRgb(lower[1]);
  const b = hexToRgb(upper[1]);

  return [0, 1, 2].map((channel) => Math.round(a[channel] + (b[channel] - a[channel]) * t)) as [
    number,
    number,
    number,
  ];
}

export function starColour(stars: number): string {
  return `rgb(${starRgb(stars).join(" ")})`;
}

/**
 * pp onto the **same colour progression** the star ramp uses.
 *
 * The user's rule: a pp figure is coloured by its own value, not by the play's star rating — but
 * the progression must be the one stars use. So pp is mapped across the ramp's own domain
 * (`0` at the bottom stop, `ceiling` at the top) and sampled there. `ceiling` is the library's own
 * maximum, so the scale is content-derived rather than invented.
 */
export function ppColour(pp: number, ceiling: number): string {
  const top = STAR_STOPS[STAR_STOPS.length - 1][0];
  const bottom = STAR_STOPS[0][0];
  const t = Math.min(1, Math.max(0, pp / Math.max(ceiling, 1)));
  return starColour(bottom + t * (top - bottom));
}

/* -------------------------------------------------------------------------------------------------
 * Mods
 * ---------------------------------------------------------------------------------------------- */

/** lazer's `OsuColour.ForModType`. */
export const MOD_CATEGORY_COLOURS = {
  DifficultyIncrease: "#ff6666",
  DifficultyReduction: "#b2ff66",
  Conversion: "#8c66ff",
  Automation: "#66ccff",
  Fun: "#ff66ab",
  System: "#ffcc22",
} as const;

type ModCategory = keyof typeof MOD_CATEGORY_COLOURS;

/** lazer's `ModType`, in osu!'s own presentation order — the order the mod select lays them out. */
export const MOD_CATEGORY_ORDER = [
  "DifficultyIncrease",
  "DifficultyReduction",
  "Conversion",
  "Automation",
  "Fun",
  "System",
] as const satisfies readonly ModCategory[];

/** What each group is called on screen. osu! lays the groups out without words; a chip row needs them. */
export const MOD_CATEGORY_LABELS: Record<ModCategory, string> = {
  DifficultyIncrease: "harder",
  DifficultyReduction: "easier",
  Conversion: "convert",
  Automation: "auto",
  Fun: "fun",
  System: "system",
};

/**
 * Every mod lazer declares, with its own name and category — the whole select, not only the mods this
 * library happens to hold.
 *
 * The filter shows all of them because a filter is a question you can ask, not a summary of the data:
 * "what if I had played DT here" still needs a DT chip. Mods with no plays are dimmed and count zero.
 *
 * osu!'s real icon art is CC-BY-NC (lazer) or AGPL (osu-web), so the badge is a hexagon we draw
 * containing the community-standard acronym — the user's condition for accepting that was that the
 * acronyms be exactly the ones everyone uses. An acronym absent from this table still renders; it
 * just has no name to show on hover.
 */
export const MOD_CATALOG: Record<string, { name: string; category: ModCategory }> = {
  "10K": { name: "Ten Keys", category: "Conversion" },
  "1K": { name: "One Key", category: "Conversion" },
  "2K": { name: "Two Keys", category: "Conversion" },
  "3K": { name: "Three Keys", category: "Conversion" },
  "4K": { name: "Four Keys", category: "Conversion" },
  "5K": { name: "Five Keys", category: "Conversion" },
  "6K": { name: "Six Keys", category: "Conversion" },
  "7K": { name: "Seven Keys", category: "Conversion" },
  "8K": { name: "Eight Keys", category: "Conversion" },
  "9K": { name: "Nine Keys", category: "Conversion" },
  "AC": { name: "Accuracy Challenge", category: "DifficultyIncrease" },
  "AD": { name: "Approach Different", category: "Fun" },
  "AP": { name: "Autopilot", category: "Automation" },
  "AS": { name: "Adaptive Speed", category: "Fun" },
  "AT": { name: "Autoplay", category: "Automation" },
  "BL": { name: "Blinds", category: "DifficultyIncrease" },
  "BM": { name: "Bloom", category: "Fun" },
  "BR": { name: "Barrel Roll", category: "Fun" },
  "BU": { name: "Bubbles", category: "Fun" },
  "CL": { name: "Classic", category: "Conversion" },
  "CM1": { name: "Customisable Mod 1", category: "Conversion" },
  "CM2": { name: "Customisable Mod 2", category: "Conversion" },
  "CN": { name: "Cinema", category: "Automation" },
  "CO": { name: "Cover", category: "DifficultyIncrease" },
  "CS": { name: "Constant Speed", category: "Conversion" },
  "DA": { name: "Difficulty Adjust", category: "Conversion" },
  "DC": { name: "Daycore", category: "DifficultyReduction" },
  "DF": { name: "Deflate", category: "Fun" },
  "DP": { name: "Depth", category: "Fun" },
  "DS": { name: "Dual Stages", category: "Conversion" },
  "DT": { name: "Double Time", category: "DifficultyIncrease" },
  "EZ": { name: "Easy", category: "DifficultyReduction" },
  "FF": { name: "Floating Fruits", category: "Fun" },
  "FI": { name: "Fade In", category: "DifficultyIncrease" },
  "FL": { name: "Flashlight", category: "DifficultyIncrease" },
  "FR": { name: "Freeze Frame", category: "Fun" },
  "GR": { name: "Grow", category: "Fun" },
  "HD": { name: "Hidden", category: "DifficultyIncrease" },
  "HO": { name: "Hold Off", category: "Conversion" },
  "HR": { name: "Hard Rock", category: "DifficultyIncrease" },
  "HT": { name: "Half Time", category: "DifficultyReduction" },
  "IN": { name: "Invert", category: "Conversion" },
  "MF": { name: "Moving Fast", category: "Fun" },
  "MG": { name: "Magnetised", category: "Fun" },
  "MR": { name: "Mirror", category: "Conversion" },
  "MU": { name: "Muted", category: "Fun" },
  "NC": { name: "Nightcore", category: "DifficultyIncrease" },
  "NF": { name: "No Fail", category: "DifficultyReduction" },
  "NM": { name: "No Mod", category: "System" },
  "NR": { name: "No Release", category: "DifficultyReduction" },
  "NS": { name: "No Scope", category: "Fun" },
  "PF": { name: "Perfect", category: "DifficultyIncrease" },
  "RD": { name: "Random", category: "Conversion" },
  "RP": { name: "Repel", category: "Fun" },
  "RX": { name: "Relax", category: "Automation" },
  "SD": { name: "Sudden Death", category: "DifficultyIncrease" },
  "SI": { name: "Spin In", category: "Fun" },
  "SO": { name: "Spun Out", category: "Automation" },
  "SR": { name: "Simplified Rhythm", category: "DifficultyReduction" },
  "SW": { name: "Swap", category: "Conversion" },
  "SY": { name: "Synesthesia", category: "Fun" },
  "TC": { name: "Traceable", category: "DifficultyIncrease" },
  "TD": { name: "Touch Device", category: "System" },
  "TP": { name: "Target Practice", category: "Conversion" },
  "TR": { name: "Transform", category: "Fun" },
  "WD": { name: "Wind Down", category: "Fun" },
  "WG": { name: "Wiggle", category: "Fun" },
  "WU": { name: "Wind Up", category: "Fun" },
};

/** One mod as the index stores it: an acronym, plus any settings that differ from the default. */
export interface Mod {
  acronym: string;
  name: string | null;
  colour: string;
  settings: Record<string, number | boolean> | null;
}

/**
 * A mods entry is either a bare acronym string — `""`, `"HDDT"` — or a `[acronyms, settings]` pair
 * for a play that customised something, e.g. `["DT", [{acronym: "DT", settings: {speed_change: 1.3}}]]`.
 *
 * Every osu! mod acronym is two characters, so a bare string chunks into pairs. Anything that does
 * not chunk cleanly is kept whole rather than mangled.
 */
export function decodeMods(entry: unknown): Mod[] {
  const [acronyms, settings] = Array.isArray(entry) ? entry : [entry, null];
  const settingsByAcronym = new Map<string, Record<string, number | boolean>>();
  if (Array.isArray(settings)) {
    for (const item of settings) {
      if (item && typeof item === "object" && "acronym" in item) {
        const { acronym, settings: values } = item as {
          acronym: string;
          settings?: Record<string, number | boolean>;
        };
        if (values) settingsByAcronym.set(acronym, values);
      }
    }
  }

  if (typeof acronyms !== "string" || acronyms.length === 0) return [];

  const pairs = acronyms.length % 2 === 0 ? (acronyms.match(/../g) ?? []) : [acronyms];

  return pairs.map((acronym) => {
    const known = MOD_CATALOG[acronym];
    return {
      acronym,
      name: known?.name ?? null,
      colour: modColour(acronym),
      settings: settingsByAcronym.get(acronym) ?? null,
    };
  });
}

/** lazer's category colour for an acronym, or a neutral for anything not in the table. */
export function modColour(acronym: string): string {
  const known = MOD_CATALOG[acronym];
  return known ? MOD_CATEGORY_COLOURS[known.category] : "hsl(240 8% 46%)";
}

/** The mod's own name — `HD` reads as `Hidden`. */
export function modName(acronym: string): string {
  return MOD_CATALOG[acronym]?.name ?? acronym;
}

/** Human-readable settings for the hover card: `{speed_change: 1.3}` becomes `1.3x speed`. */
export function describeSettings(settings: Record<string, number | boolean>): string[] {
  return Object.entries(settings).map(([key, value]) => {
    const label = key.replace(/_/g, " ");
    if (typeof value === "boolean") return value ? label : `no ${label}`;
    if (key.includes("speed")) return `${value}x ${label.replace("speed change", "speed")}`;
    return `${label} ${value}`;
  });
}

/* -------------------------------------------------------------------------------------------------
 * The index
 * ---------------------------------------------------------------------------------------------- */

export interface Beatmap {
  id: number;
  set: number;
  difficulty: string;
  stars: number;
  artist: string;
  title: string;
  creator: string;
  md5: string;
}

export interface Play {
  id: number;
  beatmap: Beatmap;
  mods: Mod[];
  pp: number;
  accuracy: number;
  combo: number;
  score: number;
  legacyScore: number | null;
  grade: (typeof GRADES)[number];
  /** Position in `GRADES`, where lower is better: XH X SH S A B C D F. */
  gradeIndex: number;
  /**
   * The play's **mod-adjusted** star rating — 2.41 becomes 3.38 under DT.
   *
   * `beatmap.stars` is the map's own rating and is deliberately no-mod, so on a modded play the two
   * are far apart. The row shows this one and reveals the map's rating on hover.
   */
  stars: number;
  playedAt: Date;
}

interface RawIndex {
  mods: unknown[];
  beatmaps: unknown[][];
  plays: unknown[][];
}

/**
 * The index ships as tuples rather than objects — every key spelled 8,000 times is 8,000 keys of
 * nothing — so this is a real decode rather than a `JSON.parse` and a read. `poblin-osu-library-plan.md`
 * §7 has the wire format.
 */
export function decodeIndex(raw: RawIndex): Play[] {
  const beatmaps: Beatmap[] = raw.beatmaps.map(
    ([id, set, difficulty, stars, artist, title, creator, md5]) => ({
      id: id as number,
      set: set as number,
      difficulty: difficulty as string,
      stars: stars as number,
      artist: artist as string,
      title: title as string,
      creator: creator as string,
      md5: md5 as string,
    }),
  );

  const mods = raw.mods.map(decodeMods);

  const plays: Play[] = raw.plays.map(
    ([
      id,
      beatmapIndex,
      modIndex,
      pp,
      accuracy,
      combo,
      score,
      legacyScore,
      rank,
      ,
      playedAt,
      stars,
    ]) => ({
      id: id as number,
      beatmap: beatmaps[beatmapIndex as number],
      mods: mods[modIndex as number] ?? [],
      pp: pp as number,
      accuracy: accuracy as number,
      combo: combo as number,
      score: score as number,
      legacyScore: legacyScore as number | null,
      grade: GRADES[rank as number] ?? GRADES[8],
      gradeIndex: rank as number,
      // Absent on a version-1 file, where the play's own rating was never stored.
      stars: (stars as number | undefined) ?? beatmaps[beatmapIndex as number].stars,
      playedAt: new Date((playedAt as number) * 1000),
    }),
  );

  /*
   * The index ships **oldest first, not in pp order**. That was assumed the other way round until it
   * was checked against the shipped file: `plays[0]` is a 1.8pp play from 2022-09-19 and the file is
   * strictly ascending by date. So the default view does need a sort, and the pp weighting below is
   * only correct *after* this has run — which is why it happens here, once, rather than at each
   * consumer.
   */
  return plays.sort((a, b) => b.pp - a.pp);
}

/**
 * Cover art is not stored: it is derived from the beatmapset id, and the browser fetches it straight
 * from osu!'s CDN — it never touches the Worker's request budget.
 *
 * `card@2x` is the listing size (800x280, ~50 KB). It is blurred to 56px for the background, so the
 * extra resolution of a bigger variant would be discarded anyway.
 */
export function coverUrl(beatmapsetId: number): string {
  return `https://assets.ppy.sh/beatmaps/${beatmapsetId}/covers/card@2x.jpg`;
}

/* -------------------------------------------------------------------------------------------------
 * Formatting
 * ---------------------------------------------------------------------------------------------- */

const NUMBER = new Intl.NumberFormat("en-US");

export function formatNumber(value: number): string {
  return NUMBER.format(Math.round(value));
}

export function formatAccuracy(value: number): string {
  return `${value.toFixed(2)}%`;
}

/** osu! writes long play times in days once they stop being readable as hours. */
export function formatPlayTime(seconds: number): string {
  const hours = seconds / 3600;
  if (hours < 48) return `${Math.round(hours)}h`;
  return `${NUMBER.format(Math.round(hours / 24))}d`;
}

export function formatDate(date: Date): string {
  return date.toLocaleDateString("en-GB", { day: "numeric", month: "short", year: "numeric" });
}

/** "3 years ago", the way a score row reads it. */
export function formatRelative(date: Date, now = new Date()): string {
  const days = Math.floor((now.getTime() - date.getTime()) / 86_400_000);
  if (days < 1) return "today";
  if (days < 30) return `${days}d ago`;
  if (days < 365) return `${Math.floor(days / 30)}mo ago`;
  const years = Math.floor(days / 365);
  return `${years} year${years === 1 ? "" : "s"} ago`;
}

/** The API calls catch "fruits"; every other surface calls it "catch". */
export function apiMode(mode: string): string {
  return mode === "catch" ? "fruits" : mode;
}

/**
 * The accent for a play, derived from its star rating.
 *
 * The brief asked for an accent drawn from the background artwork. That is **not possible from
 * osu!'s CDN**: `assets.ppy.sh` returns no `Access-Control-Allow-Origin` (verified against the live
 * response headers, with and without an `Origin`), so drawing a cover to a canvas taints it and
 * `getImageData` throws.
 *
 * **The wall became bypassable on 2026-10-03, and this accent predates that.**
 * `GET /v3/osu/beatmaps/proxy-image?url=…` on the mirror returns the *identical bytes* — measured,
 * same length, same JPEG — with `Access-Control-Allow-Origin: *`, so the artwork's colour is one
 * fetch away rather than impossible. **Sampled colour is still not what ships, and that is now a
 * choice rather than a constraint**: it would make the page's hue depend on whatever a mapper put in
 * their background, where the star ramp is content-derived, comes from osu!'s own palette, and
 * changes as you move between difficulties. Left as it is deliberately; the route exists if it is
 * ever wanted.
 *
 * Adjusted rather than used raw: the bottom of the ramp is near-grey and the top is a pale mint or
 * lemon, and neither reads as an accent on a dark panel. Saturation is floored and lightness pulled
 * into a range that stays legible — a deliberate correction, not an average, since averaging is what
 * turns artwork into mud.
 */
export function accentFor(stars: number): string {
  const [r, g, b] = starRgb(stars);

  const max = Math.max(r, g, b) / 255;
  const min = Math.min(r, g, b) / 255;
  const lightness = (max + min) / 2;
  const delta = max - min;

  let hue = 0;
  if (delta > 0) {
    const [rn, gn, bn] = [r / 255, g / 255, b / 255];
    if (max === rn) hue = ((gn - bn) / delta) % 6;
    else if (max === gn) hue = (bn - rn) / delta + 2;
    else hue = (rn - gn) / delta + 4;
    hue = (hue * 60 + 360) % 360;
  }

  let saturation = delta === 0 ? 0 : delta / (1 - Math.abs(2 * lightness - 1));
  saturation = Math.max(saturation, 0.62);

  return `hsl(${hue.toFixed(0)} ${(saturation * 100).toFixed(0)}% 68%)`;
}

/* -------------------------------------------------------------------------------------------------
 * Sorting and filtering
 * ---------------------------------------------------------------------------------------------- */

export const SORTS = [
  { key: "pp", label: "pp" },
  { key: "date", label: "date" },
  { key: "accuracy", label: "accuracy" },
  { key: "stars", label: "stars" },
] as const;

export type SortKey = (typeof SORTS)[number]["key"];

/** Lower is better, so `max` is the worst grade the filter still admits. */
export const GRADE_TIERS = [
  { key: "all", label: "any grade", max: 8 },
  { key: "a", label: "A and up", max: 4 },
  { key: "s", label: "S and up", max: 3 },
] as const;

export type GradeTierKey = (typeof GRADE_TIERS)[number]["key"];

export interface Query {
  text: string;
  grade: GradeTierKey;
  /** Acronyms every result must carry. */
  mods: string[];
  /** Acronyms no result may carry. */
  excluded: string[];
  /** Keep only the plays that were a new best pp when they were set. */
  pbOnly: boolean;
  /** Keep only the best-pp play on each beatmap difficulty. */
  bestPerDiff: boolean;
  /** `YYYY-MM-DD` in local time, or null for every day. */
  day: string | null;
  sort: SortKey;
  descending: boolean;
}

export const DEFAULT_QUERY: Query = {
  text: "",
  grade: "all",
  mods: [],
  excluded: [],
  pbOnly: false,
  bestPerDiff: false,
  day: null,
  sort: CONFIG.defaultSort,
  descending: true,
};

/**
 * A play's day, in local time, as `YYYY-MM-DD`.
 *
 * Built from the local parts rather than `toISOString`, because the latter converts to UTC — which
 * would file a 01:00 play under the previous day for anyone east of Greenwich.
 */
export function dayKey(date: Date): string {
  const month = `${date.getMonth() + 1}`.padStart(2, "0");
  const day = `${date.getDate()}`.padStart(2, "0");
  return `${date.getFullYear()}-${month}-${day}`;
}

/** How many plays fall on each day, which is what the calendar shades itself with. */
export function dayCounts(plays: Play[]): Map<string, number> {
  const counts = new Map<string, number>();

  for (const play of plays) {
    const key = dayKey(play.playedAt);
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }

  return counts;
}

function valueOf(play: Play, key: SortKey): number {
  switch (key) {
    case "pp":
      return play.pp;
    case "accuracy":
      return play.accuracy;
    case "stars":
      // Mod-adjusted, because "sort by stars" on a list of plays means the difficulty they were
      // actually played at: a 4.55★ map under DT is a 6.49★ play and belongs above a 6.0★ one.
      return play.stars;
    case "date":
      return play.playedAt.getTime();
  }
}

/**
 * One pass over the whole list. At 8,107 rows this is about a millisecond, so there is no index,
 * no memoised facet table and nothing cleverer than a filter and a sort.
 */
export function search(plays: Play[], query: Query): Play[] {
  const text = query.text.trim().toLowerCase();
  const tier = GRADE_TIERS.find((entry) => entry.key === query.grade) ?? GRADE_TIERS[0];
  const bests = query.pbOnly ? personalBests(plays) : null;
  const perDiff = query.bestPerDiff ? bestPerDifficulty(plays) : null;

  const matched = plays.filter((play) => {
    if (play.gradeIndex > tier.max) return false;

    if (bests && !bests.has(play.id)) return false;
    if (perDiff && !perDiff.has(play.id)) return false;

    if (query.day !== null && dayKey(play.playedAt) !== query.day) return false;

    if (query.mods.length > 0 || query.excluded.length > 0) {
      const present = play.mods.map((mod) => mod.acronym);

      /*
       * `NM` is not an acronym any play carries — it is the absence of one, so it is handled as
       * "has no mods at all" rather than looked for in the play's list. It composes with the rest:
       * requiring NM alongside another mod can never match, which is the honest answer.
       */
      const carries = (acronym: string) => (acronym === "NM" ? present.length === 0 : present.includes(acronym));

      if (!query.mods.every(carries)) return false;
      if (query.excluded.some(carries)) return false;
    }

    if (text.length === 0) return true;

    const { title, artist, difficulty, creator } = play.beatmap;
    return (
      title.toLowerCase().includes(text) ||
      artist.toLowerCase().includes(text) ||
      difficulty.toLowerCase().includes(text) ||
      creator.toLowerCase().includes(text)
    );
  });

  /*
   * Sorting is by value alone, and the sign lives here. A comparator returning negative sorts `a`
   * first, so descending multiplies by -1 to invert the difference — the other way round, which is
   * what this did first, put the *worst* plays at the top of a "pp ↓" sort.
   */
  const direction = query.descending ? -1 : 1;
  return matched.sort((a, b) => direction * (valueOf(a, query.sort) - valueOf(b, query.sort)));
}

/** The mods that actually occur, grouped the way osu! groups them — by category, not alphabetically. */
export interface ModGroup {
  key: string;
  label: string;
  colour: string;
  mods: { acronym: string; count: number }[];
}

/**
 * The mods this mode actually has plays for, grouped by category in lazer's own order.
 *
 * Categories with nothing in them are not returned at all — a filter offering 68 chips to a library
 * that holds 18 mods is a wall, not a control.
 *
 * `NM` — No Mod — is the one entry that is not an acronym on any play: a play with no mods has an empty
 * mod list, so its count is the number of such plays and `search` treats it specially.
 *
 * `CL` is **not offered as a chip.** It is not a mod anyone played: it is how osu! itself spells a
 * stable-era score (`index.rs` appends it to match the API), and it is true of exactly the plays the header's
 * stable switch selects — 7,578 of 7,578 here, no exceptions either way. As a filter it is that switch with
 * less information, since the switch also puts the original score in the score column.
 */
export function modsByCategory(plays: Play[]): ModGroup[] {
  const counts = new Map<string, number>();
  let noMods = 0;

  for (const play of plays) {
    if (play.mods.length === 0) noMods += 1;
    for (const mod of play.mods) {
      counts.set(mod.acronym, (counts.get(mod.acronym) ?? 0) + 1);
    }
  }

  const groups = new Map<string, ModGroup>();

  for (const [acronym, { category }] of Object.entries(MOD_CATALOG)) {
    if (acronym === "CL") continue;

    const count = acronym === "NM" ? noMods : (counts.get(acronym) ?? 0);
    if (count === 0) continue;

    let group = groups.get(category);
    if (!group) {
      group = {
        key: category,
        label: MOD_CATEGORY_LABELS[category],
        colour: MOD_CATEGORY_COLOURS[category],
        mods: [],
      };
      groups.set(category, group);
    }

    group.mods.push({ acronym, count });
  }

  return [...groups.values()]
    .sort((a, b) => MOD_CATEGORY_ORDER.indexOf(a.key as ModCategory) - MOD_CATEGORY_ORDER.indexOf(b.key as ModCategory))
    .map((group) => ({
      ...group,
      mods: group.mods.sort((a, b) => a.acronym.localeCompare(b.acronym)),
    }));
}

/**
 * The best-pp play on each beatmap difficulty — a difficulty's pp *is* its best score, so this is the
 * set a profile's pp figure is actually made of.
 *
 * Computed over the whole mode rather than over what is currently on screen, for the same reason as
 * `personalBests`: "your best on this difficulty" is a fact about the library, not about the filter.
 */
export function bestPerDifficulty(plays: Play[]): Set<number> {
  const best = new Map<number, Play>();

  for (const play of plays) {
    const current = best.get(play.beatmap.id);
    if (!current || play.pp > current.pp) best.set(play.beatmap.id, play);
  }

  return new Set([...best.values()].map((play) => play.id));
}

/**
 * The plays that were a new personal best in pp **at the time they were set** — the player's own pp
 * progression, derived rather than stored.
 *
 * Strictly greater, so a repeat that merely ties the best is not a new best; the very first play always
 * is one. Computed over the whole mode, not over the current filter, because "was this my best then"
 * cannot depend on what is on screen.
 */
export function personalBests(plays: Play[]): Set<number> {
  const byDate = [...plays].sort((a, b) => a.playedAt.getTime() - b.playedAt.getTime());
  const ids = new Set<number>();
  let best = Number.NEGATIVE_INFINITY;

  for (const play of byDate) {
    if (play.pp > best) {
      ids.add(play.id);
      best = play.pp;
    }
  }

  return ids;
}
