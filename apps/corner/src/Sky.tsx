import { useEffect, useState } from "react";

import { GroundTriangles, trianglesOn } from "./Triangles.tsx";

/**
 * The background, which is the whole point of the redesign.
 *
 * osu!lazer does not paint a gradient behind its lists — it blurs **the beatmap's own cover art**
 * and lays a faint triangle mesh over it. So this does the same: `cover` is the artwork of whatever
 * replay is being pointed at, and moving down the list crossfades the whole page from one map's
 * colour to the next. The colour is then real and always different, instead of one invented wash.
 *
 * Two layers, both always mounted, alternating: a crossfade cannot flash, and a transition retargets
 * cleanly when the pointer moves faster than the 620ms fade.
 */
export function Sky({ cover }: { cover: string | null }) {
  const [layers, setLayers] = useState<[string | null, string | null]>([cover, null]);
  const [active, setActive] = useState<0 | 1>(0);

  useEffect(() => {
    if (cover === layers[active]) return;

    const next: 0 | 1 = active === 0 ? 1 : 0;
    setLayers((previous) => (next === 0 ? [cover, previous[1]] : [previous[0], cover]));
    setActive(next);
    // The effect re-runs and returns immediately, because the cover it wants is now the active one.
  }, [cover, layers, active]);

  return (
    <div className="sky" aria-hidden="true">
      {layers.map((url, index) => (
        <div
          key={index}
          className="sky__art"
          data-active={index === active}
          style={url ? { backgroundImage: `url("${url}")` } : undefined}
        />
      ))}

      <div className="sky__scrim" />
      {trianglesOn() && <GroundTriangles />}
    </div>
  );
}
