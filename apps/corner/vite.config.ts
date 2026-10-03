import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

import { storageBase } from "./tools/storage-base.mjs";

/** Must match the route in `../../wrangler.toml`. `/osu/` mounts under a site, `/` takes a domain. */
const BASE_PATH = "/osu/";

/**
 * Workers serve static assets only from a directory mirroring the requested path, so a corner
 * answering at `/osu/` has to build to `dist/osu/`. `base` rewrites URLs, not the layout.
 *
 * Serving assets *through* the Worker instead would let a flat layout work, but every asset
 * request would then invoke the script and count against the daily request budget.
 */
const outDir = `dist${BASE_PATH.replace(/\/$/, "")}`;

/**
 * Where the browser fetches `index-*.json` from, injected as `__INDEX_BASE__`.
 *
 * **The storage host in a build.** The index is the owner's data, so it is not in the repository
 * and not in the Worker's asset bundle (§9) — which also means a play reaches the site on the next
 * ingest rather than the next deploy. The host comes from the same `storage.public_base` that
 * `tools/hoist-headers.mjs` writes into the CSP, so the policy and the fetch cannot disagree.
 *
 * **This app's own assets in dev**, where the index sits in `public/` — copied out of `library/` by
 * whoever ran the ingest. That keeps a developer's real library in front of them with no round trip,
 * and without the bucket needing `localhost` in its CORS rule.
 *
 * Empty means "serve it as our own asset", which is the supported state for a deployment that keeps
 * the index in `public/`. The trailing slash is added because the caller appends `index-<mode>.json`.
 */
function indexBase(command: "build" | "serve"): string {
  if (command !== "build") return "";

  const base = storageBase();
  return base === "" || base.endsWith("/") ? base : `${base}/`;
}

export default defineConfig(({ command }) => ({
  base: BASE_PATH,
  plugins: [react(), tailwindcss()],
  define: { __INDEX_BASE__: JSON.stringify(indexBase(command)) },
  // No SPA fallback, so an unmatched path 404s in dev instead of the corner answering every URL.
  // Production gets the same from `not_found_handling = "404-page"`.
  appType: "mpa",
  build: {
    outDir,
    target: "es2022",
    sourcemap: false,
    // Native at this target, and the polyfill is an inline script that `script-src 'self'` blocks.
    modulePreload: { polyfill: false },
    cssCodeSplit: true,
    reportCompressedSize: false,
  },
  server: {
    host: true,
    port: 5179,
    open: false,
    // The profile route, proxied to a locally running `wrangler dev` rather than to any deployed
    // host. A hostname here would be one contributor's URL baked into a repository other people
    // clone; `127.0.0.1` is nobody's. `wrangler dev` reads `.dev.vars`, so the data is real — a
    // live osu! response through the same Worker and the same Cache API — with no deploy.
    //
    // Only the API is proxied: the index comes from `public/` in dev, not from the bucket
    // (`indexBase` above), so there is nothing cross-origin to proxy.
    proxy: { "/api": "http://127.0.0.1:8787" },
  },
}));
