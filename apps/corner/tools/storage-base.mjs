/**
 * `storage.public_base`, read once for the two places that need it: the CSP that has to allow the
 * host (`tools/hoist-headers.mjs`) and the URL the index is fetched from (`vite.config.ts`).
 *
 * **One reader rather than two copies of the same regex.** If the policy named a host the frontend
 * did not fetch from, the fetch would fail *silently* and look like a rendering bug — the same
 * failure class the CSP entries are annotated with.
 *
 * Deliberately not a TOML parser: it is one key in files this repository writes itself, and a parser
 * would mean either a new dependency or reimplementing the local-overrides-committed merge, which is
 * how the two sides would drift apart. The local file is checked first, exactly as the merge does.
 *
 * Empty is a supported state, not a failure: a deployment that serves the index as its own static
 * asset has no storage host, and both callers handle that rather than producing a broken entry.
 */
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const appRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

/** The configured base URL, or `""` when nothing is set. Not validated — see the callers. */
export function storageBase() {
  const root = resolve(appRoot, "..", "..");

  for (const name of ["osu-corner.local.toml", "osu-corner.toml"]) {
    const file = join(root, name);
    if (!existsSync(file)) continue;

    const found = readFileSync(file, "utf8").match(/^\s*public_base\s*=\s*"([^"]*)"/m);
    if (found === null || found[1].trim() === "") continue;

    return found[1].trim();
  }

  return "";
}
