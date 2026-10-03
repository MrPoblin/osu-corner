# osu! corner

A profile page and replay library for [osu!](https://osu.ppy.sh). Clone it, point it at your own
replays, deploy it to your own Cloudflare account.

The library is one small index the browser downloads whole, plus the `.osr` files. Beatmaps and
audio are not stored here — they come from public mirrors, cached by the visitor's own browser.

## Requirements

- Node 24.21.0, pnpm 12.4.2
- Rust 1.98.1 with `wasm32-unknown-unknown` (`rustup` installs both from `rust-toolchain.toml`)
- A Cloudflare Workers account — the free plan is enough
- An S3-compatible bucket: B2, R2, MinIO. 10k replays is roughly 400 MB
- osu!lazer installed, with the replays you want to publish
- An osu! OAuth application

## Layout

    apps/corner/       profile card, replay library, viewer
    crates/worker/     osu-worker   the /api/* Worker
    crates/core/       corner-core  cache logic; no `worker` dep, so it tests under plain cargo
    crates/osu-core/   osu-core     .osr and .osu parsing, the index format, pp
    crates/ingest/     osu-ingest   local binary: replays → index + profile snapshot → bucket

## Setup

1. `pnpm install && pnpm exec wrangler login`

2. Put your values in `osu-corner.local.toml` beside the committed `osu-corner.toml` — gitignored,
   merged over the defaults, and the shape is at the bottom of `osu-corner.toml`. One `[[source]]`
   per game install, one `[[user]]` with your numeric osu! id and every name the account has
   played under.

3. Create a bucket, and put its public URL, bucket name, S3 endpoint and region in the local file.

4. Register an osu! OAuth application at
   [osu.ppy.sh/home/account/edit](https://osu.ppy.sh/home/account/edit) with the client-credentials
   grant, so there is no callback URL. Copy `.dev.vars.example` to `.dev.vars` and fill in the id
   and secret — ingest uses the same pair to look up beatmaps your installs do not hold.

5. `cargo run -p osu-ingest` reads the game folders directly and writes the four
   `index-<mode>.json` files plus one `profile-<mode>.json` per ruleset, uploading them all.

6. Push to `main`. `.github/workflows/deploy.yml` builds the app and the Worker and runs
   `wrangler deploy`. It needs:

   | Name | Kind | Value |
   |---|---|---|
   | `CLOUDFLARE_API_TOKEN` | secret | scoped to the account and zone; Workers Scripts Write + Workers Routes Write |
   | `CLOUDFLARE_ACCOUNT_ID` | secret | the account the Worker lives in |
   | `STORAGE_PUBLIC_BASE` | variable | `storage.public_base` |
   | `PUBLIC_ZONE` | variable | the zone the routes live on |
   | `PUBLIC_ROUTES` | variable | one route per line, e.g. `example.com/osu/*` and `example.com/api/osu/*` |

   Both routes are needed: the page is at `/osu/`, the profile at `/api/osu/profile` on the site
   root. With `PUBLIC_ROUTES` unset it publishes to `osu-corner.<account>.workers.dev` instead.

   `OSU_CLIENT_ID`, `OSU_CLIENT_SECRET` and `OSU_PROFILE_USER` are set once with
   `wrangler secret put` and survive every deploy, so CI never handles them.

7. `.github/workflows/profile.yml` refreshes the published profile snapshot hourly. It needs your
   two gitignored files as secrets:

   ```bash
   gh secret set OSU_CORNER_LOCAL_TOML < osu-corner.local.toml
   gh secret set OSU_CORNER_DEV_VARS   < .dev.vars
   ```

   `--profile-only` reads no game folders, so the runner needs no osu! install and any `[[source]]`
   entries in the copied config are ignored. GitHub disables scheduled workflows after 60 days
   without a push.

## The profile card

Fetches `/api/osu/profile` (live, from the Worker) and `profile-<mode>.json` (the snapshot ingest
publishes), and uses whichever carries the newer `fetched_at`.

Both are needed. The Worker's upstream call leaves from Cloudflare's shared egress addresses and
osu! rate-limits per IP, so it has hour-long windows where it can only answer 503 — and its cache
holds a body for a week, so it will serve a three-day-old one rather than admit it. The snapshot is
the floor under both.

Past `staleProfileAfterDays` (30, in `apps/corner/src/config.ts`) the header shows
`profile updated <date>`.

## Mounting at a path

`base` in `apps/corner/vite.config.ts` must match the route in `wrangler.toml`, and it also decides
the build's output nesting: Workers serve assets only from a directory mirroring the request path,
so `/osu/` builds to `dist/osu/`. Never hardcode a root-absolute path — use
`import.meta.env.BASE_URL`.

## Configuration

| File | Holds | Committed |
|---|---|---|
| `osu-corner.toml` | generic defaults | yes |
| `osu-corner.local.toml` | your installs, account and bucket | **no** |
| `apps/corner/vite.config.ts` | the mount path | yes |
| `wrangler.toml` | Worker name, assets, route | yes |
| `apps/corner/src/config.ts` | what the corner opens on — ruleset, score mode, sort | yes |
| `.dev.vars` | osu! client id and secret | **no** |

## Development

```bash
pnpm check                      # typecheck every workspace package
pnpm build
cargo test -p corner-core -p osu-core
cargo fmt --all --check
```

`pnpm preview` needs the Worker built first: `cd crates/worker && worker-build --release`, then
`pnpm preview` from the root.

## AI Slop Disclosure

AI coding was used while making this project.