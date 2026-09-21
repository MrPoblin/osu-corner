#!/usr/bin/env node
/**
 * Cloudflare reads `_headers` from the root of the assets directory, but this app builds one
 * level down under its mount path (see vite.config.ts). Without this, every rule in it is
 * silently ignored. `dist/` is gitignored and rebuilt, so it cannot be committed into place.
 *
 * The mount path is not recomputed here on purpose — finding the file that was just built
 * cannot drift out of step with it.
 */
import { copyFileSync, existsSync, mkdirSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const appRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const dist = join(appRoot, "dist");
const target = join(dist, "_headers");

if (!existsSync(dist)) {
  console.error("! dist/ not found — run `vite build` first.");
  process.exit(1);
}

// Exactly one level of nesting, which is what a single-segment mount path produces.
let source = null;
for (const entry of readdirSync(dist, { withFileTypes: true })) {
  if (!entry.isDirectory()) continue;
  const candidate = join(dist, entry.name, "_headers");
  if (existsSync(candidate)) source = candidate;
}

if (source === null) {
  console.log("- no _headers in this build, nothing to hoist");
  process.exit(0);
}

mkdirSync(dirname(target), { recursive: true });
copyFileSync(source, target);
console.log(`+ hoisted ${source.slice(appRoot.length + 1)} -> dist/_headers`);

/*
 * Always overwrites. `vite build` empties its own outDir (`dist/osu`) but not `dist/`, so a
 * copy from the previous build survives — and skipping the write when the target exists would
 * mean editing _headers, rebuilding, and silently serving the old policy.
 */
