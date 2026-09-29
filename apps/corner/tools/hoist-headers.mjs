#!/usr/bin/env node
/**
 * Cloudflare reads `_headers` from the root of the assets directory, but this app builds one
 * level down under its mount path (see vite.config.ts). Without this, every rule in it is
 * silently ignored. `dist/` is gitignored and rebuilt, so it cannot be committed into place.
 *
 * The mount path is not recomputed here on purpose — finding the file that was just built
 * cannot drift out of step with it.
 */
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
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

/*
 * `__STORAGE_ORIGIN__` is the one deployment-specific CSP entry: the hostname the browser fetches
 * the index and the replays from (§9). It is not written into `_headers` because that file is
 * committed and the hostname is per-deployment — a clone must not inherit somebody else's (§14).
 *
 * It is read from the config rather than an environment variable so there is exactly one source of
 * truth: the same `storage.public_base` the ingest writes into the index header. Deliberately not a
 * TOML parser — it is one key in files this repository writes itself, and a parser would mean either
 * a new dependency or reimplementing the local-overrides-committed merge, which is how the two sides
 * would drift apart. The local file is checked first, exactly as the merge does.
 *
 * Empty is a supported state, not a failure: a deployment serving the replays as its own static
 * assets needs no entry at all. It says so either way, because getting this wrong is silent.
 */
function storageOrigin() {
  const root = resolve(appRoot, "..", "..");
  for (const name of ["osu-corner.local.toml", "osu-corner.toml"]) {
    const file = join(root, name);
    if (!existsSync(file)) continue;
    const found = readFileSync(file, "utf8").match(/^\s*public_base\s*=\s*"([^"]*)"/m);
    if (found === null || found[1].trim() === "") continue;
    try {
      return new URL(found[1]).origin; // the CSP wants an origin; a path in it would be ignored
    } catch {
      console.error(`! ${name}: public_base "${found[1]}" is not a URL, so it is not in the CSP`);
      return "";
    }
  }
  return "";
}

const text = readFileSync(source, "utf8");
const origin = storageOrigin();
const hasPlaceholder = text.includes("__STORAGE_ORIGIN__");

// The leading space goes with the token, so an empty origin leaves no double space and no stray
// separator — this file's indentation is the header syntax and must not be normalised away.
writeFileSync(target, text.replaceAll(" __STORAGE_ORIGIN__", origin === "" ? "" : ` ${origin}`));

if (origin !== "") {
  console.log(`+ hoisted ${source.slice(appRoot.length + 1)} -> dist/_headers`);
  if (!hasPlaceholder) {
    console.error("! _headers has no __STORAGE_ORIGIN__, so the CSP omits the storage host");
  } else {
    console.log(`  CSP storage origin: ${origin}`);
  }
} else {
  console.log(`+ hoisted ${source.slice(appRoot.length + 1)} -> dist/_headers`);
  console.log("  CSP storage origin: none (set storage.public_base if the replays are served");
  console.log("  from another hostname; nothing is needed when they are static assets)");
}

/*
 * Always overwrites. `vite build` empties its own outDir (`dist/osu`) but not `dist/`, so a
 * copy from the previous build survives — and skipping the write when the target exists would
 * mean editing _headers, rebuilding, and silently serving the old policy.
 */
