import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

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

export default defineConfig({
  base: BASE_PATH,
  plugins: [react(), tailwindcss()],
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
    // Only the API is proxied. The index is served from `public/` under the base, so its URL is
    // identical in dev and in production.
    proxy: { "/api": "http://127.0.0.1:8787" },
  },
});
