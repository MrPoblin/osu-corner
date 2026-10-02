import { useEffect, useState } from "react";
import type { CSSProperties } from "react";

/**
 * osu!'s triangle field.
 *
 * The motif is uniform: every triangle is **upright and unrotated**, with a **hairline outline**.
 * An earlier pass scattered them at random angles, which is not the motif — that is confetti.
 *
 * ## Why it is a tiled texture and not elements
 *
 * Researched rather than assumed. MDN's animation guide and the CSS/SVG/Canvas/WebGL comparisons agree
 * on the shape of the answer: `transform` and `opacity` are the only properties that can run in the
 * compositor without style, layout or paint, while `canvas` and WebGL are worth their cost when
 * **thousands of objects change independently every frame** or the whole scene is redrawn each frame.
 *
 * Neither applies here. This is ~200 triangles that do not move relative to each other at all — the
 * whole field drifts as one — so the cheapest possible frame is not 200 draws or 200 nodes, it is
 * **one texture moved by the compositor**. That is the same trick a game uses for a scrolling
 * background: a repeating texture whose UVs slide.
 *
 * So each depth is one element whose `background-image` is a single data-URI tile, repeated, moved by
 * exactly one tile height on a linear loop. The browser rasterises the tile once. Three elements
 * replace what was three hundred, and there is nothing to keep in the DOM.
 *
 * ## Two traps, both hit
 *
 * - **A custom property read inside `@keyframes` does not animate.** `var()` is resolved against the
 *   element, but a custom property referenced from a keyframe becomes *animation-tainted* and the
 *   `transform` never resolves — the drift silently does nothing. The travel distances are therefore
 *   **literal per-layer keyframes** in the stylesheet, and each layer's tile height must match its
 *   keyframe. `TRAVEL` below and the `@keyframes tri-drift-*` rules are one pair of numbers, and the
 *   comment on each says so.
 * - **Speed is travel ÷ period, in pixels per second.** A short tile over a long period is a
 *   statue: 168px over 96s is 1.7px/s and reads as stopped. The periods below are chosen to keep the
 *   front layer near 10px/s, which is what the first version looked like.
 */

/**
 * Depth by scale: small and dense at the front, large and sparse at the back.
 *
 * 150 + 60 + 14 ≈ 224 triangles on a 1560×1000 screen.
 */
interface Tile {
  width: number;
  height: number;
  count: number;
  minSize: number;
  maxSize: number;
  alpha: [number, number];
}

/** The two still depths, drawn together. */
const STILL: Tile[] = [
  { width: 320, height: 400, count: 4, minSize: 54, maxSize: 104, alpha: [0.05, 0.13] },
  { width: 560, height: 700, count: 3, minSize: 110, maxSize: 165, alpha: [0.04, 0.09] },
];

/**
 * The one depth that moves.
 *
 * **Why one and not three.** A background that animates forever has to be nearly free, and the cost of
 * an animation is per animated compositor layer, not per triangle: three drifting depths meant three
 * full-screen textures composited sixty times a second. The brief was "a bunch of triangles and **some
 * of them moving**", so exactly one depth drifts and the other two are painted once into the base
 * layer, where they cost nothing at all.
 *
 * Its tile height is also its travel distance, which the matching keyframe must equal.
 */
const DRIFTING: Tile = { width: 260, height: 340, count: 7, minSize: 22, maxSize: 64, alpha: [0.06, 0.16] };

/**
 * A small linear congruential generator: the same field every time, without a seed library.
 * Deterministic matters — the tile is baked into a data URI, so it has to be reproducible.
 */
function tile(tile: Tile, seed: number): string {
  let state = seed;
  const next = () => {
    state = (state * 1664525 + 1013904223) >>> 0;
    return state / 0x100000000;
  };
  const between = (a: number, b: number) => a + next() * (b - a);

  const triangles = Array.from({ length: tile.count }, () => {
    const size = between(tile.minSize, tile.maxSize);
    const x = between(0, tile.width - size);
    const y = between(0, tile.height - size);
    const alpha = between(tile.alpha[0], tile.alpha[1]);

    // Upright, unrotated, hairline. Inset so the stroke is never clipped by the tile edge.
    const inset = size * 0.06;
    const half = size / 2 - inset;
    const height = size - inset * 2;
    const left = x + inset;
    const top = y + inset;

    const points = [
      `${(left + half).toFixed(1)},${top.toFixed(1)}`,
      `${(left + half * 2).toFixed(1)},${(top + height).toFixed(1)}`,
      `${left.toFixed(1)},${(top + height).toFixed(1)}`,
    ].join(" ");

    return `<polygon points='${points}' fill='none' stroke='#fff' stroke-opacity='${alpha.toFixed(3)}' stroke-width='0.6'/>`;
  }).join("");

  const svg = `<svg xmlns='http://www.w3.org/2000/svg' width='${tile.width}' height='${tile.height}'>${triangles}</svg>`;

  return `url("data:image/svg+xml,${encodeURIComponent(svg)}")`;
}

/** The drifting background field. */
export function GroundTriangles() {
  return (
    <div className="tri-field" aria-hidden="true">
      {/* Two depths, painted once. No animation, so no compositor layer of their own. */}
      <div
        className="tri-still"
        style={{
          backgroundImage: STILL.map((depth, index) => tile(depth, 0x2545f491 + index * 0x9e3779b9)).join(", "),
          backgroundSize: STILL.map((depth) => `${depth.width}px ${depth.height}px`).join(", "),
          backgroundRepeat: "repeat, repeat",
        }}
      />

      {/* The one depth that moves. Its height keeps it covering the viewport for the whole pass. */}
      <div
        className="tri-layer"
        data-drift="1"
        style={{
          backgroundImage: tile(DRIFTING, 0x51ed270b),
          backgroundSize: `${DRIFTING.width}px ${DRIFTING.height}px`,
          height: `calc(100% + ${DRIFTING.height * 2}px)`,
          animationDuration: "26s",
        } as CSSProperties}
      />
    </div>
  );
}

/* -------------------------------------------------------------------------------------------------
 * The arrival: the same motif, rising once, then gone
 * ---------------------------------------------------------------------------------------------- */

interface Rising {
  x: number;
  y: number;
  size: number;
  alpha: number;
  delay: number;
}

/** The entry sweep: a one-off, so its 26 elements are unmounted when it ends. */
const RISING: Rising[] = (() => {
  let seed = 0x9e3779b9;
  const next = () => {
    seed = (seed * 1664525 + 1013904223) >>> 0;
    return seed / 0x100000000;
  };
  const between = (a: number, b: number) => a + next() * (b - a);

  return Array.from({ length: 26 }, () => {
    const y = between(20, 100);

    return {
      x: between(0, 100),
      y,
      size: between(18, 70),
      alpha: between(0.3, 0.6),
      // `y` grows downward, so whatever is lowest on screen starts first.
      delay: Math.round((1 - y / 100) * 260),
    };
  });
})();

export function Triangles() {
  return (
    <>
      {RISING.map((triangle, index) => (
        <span
          key={index}
          className="tri"
          style={
            {
              left: `${triangle.x}%`,
              top: `${triangle.y}vh`,
              width: `${triangle.size}px`,
              height: `${triangle.size}px`,
              "--tri-alpha": triangle.alpha,
              "--tri-delay": `${triangle.delay}ms`,
            } as CSSProperties
          }
        >
          <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
            <polygon points="12,3.4 21.2,19.6 2.8,19.6" stroke="currentColor" strokeWidth="0.55" />
          </svg>
        </span>
      ))}
    </>
  );
}

/** True once the arrival sweep has finished, so its elements can leave the document. */
export function useEntryFinished(ms = 900): boolean {
  const [finished, setFinished] = useState(false);

  useEffect(() => {
    const timer = window.setTimeout(() => setFinished(true), ms);
    return () => window.clearTimeout(timer);
  }, [ms]);

  return finished;
}
