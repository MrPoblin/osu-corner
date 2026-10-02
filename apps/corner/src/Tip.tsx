import { useRef, type ReactNode } from "react";

/**
 * A hover card. Deliberately not `title`: the browser's own tooltip is slow to appear, cannot be
 * styled, and disappears the moment anything moves. This is a real element — it can hold a mod's
 * name *and* its settings, or a date written out in full.
 *
 * CSS-only, so it costs no JS and works on the same transition the rest of the page uses.
 */
export function Tip({
  pop,
  children,
  align = "center",
  className,
  clipOnly = false,
}: {
  pop: ReactNode | undefined;
  children: ReactNode;
  /**
   * `start` and `end` anchor the card's edge to the trigger.
   *
   * `start` is for a trigger that **fills a column** — a row's title — where centring on the trigger
   * puts the card halfway across the empty space instead of under the words.
   */
  align?: "start" | "center" | "end";
  /** Extra class on the wrapper, for a trigger that has to fill the space it sits in. */
  className?: string;
  /**
   * Show the card **only when the trigger is actually cut off**.
   *
   * For a label that truncates with an ellipsis, a card repeating the words already on screen is
   * noise. CSS cannot ask "is this overflowing", so this is the one bit of JS in the component: it
   * stamps the answer on the wrapper and the stylesheet hides the card when the answer is no.
   */
  clipOnly?: boolean;
}) {
  const ref = useRef<HTMLSpanElement>(null);

  /** `scrollWidth` is the text's real width, `clientWidth` the part that fits. */
  function measure() {
    const wrapper = ref.current;
    if (!wrapper) return;
    const label = (wrapper.firstElementChild as HTMLElement | null) ?? wrapper;
    wrapper.dataset.clipped = String(label.scrollWidth > label.clientWidth + 1);
  }

  return (
    <span
      ref={ref}
      className={className ? `tip ${className}` : "tip"}
      data-align={align}
      onMouseEnter={clipOnly ? measure : undefined}
      onFocus={clipOnly ? measure : undefined}
    >
      {children}
      {/* Nothing to say, no card — an empty one would still paint as a small box. */}
      {pop ? (
        <span className="tip__pop" role="tooltip">
          {pop}
        </span>
      ) : null}
    </span>
  );
}
