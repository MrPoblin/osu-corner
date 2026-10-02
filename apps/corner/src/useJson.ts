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
 * set is barely larger than the one file the corner cannot avoid — and they are same-origin static
 * assets, free at the edge.
 *
 * The key is joined rather than passed as an array, because a fresh array literal is a new identity
 * on every render and would refetch forever.
 */
const BASE = import.meta.env.BASE_URL;

export function useIndexes(ids: readonly string[]): Async<Record<string, Play[]>> {
  const [result, setResult] = useState<Async<Record<string, Play[]>>>({ state: "loading" });
  const key = ids.join(",");

  useEffect(() => {
    const controller = new AbortController();
    setResult({ state: "loading" });

    Promise.all(
      key.split(",").map(async (id) => {
        const response = await fetch(`${BASE}index-${id}.json`, { signal: controller.signal });
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
