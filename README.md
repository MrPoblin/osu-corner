# osu! corner

A profile page and a replay library for [osu!](https://osu.ppy.sh) — clone it, point it at
your own replays, deploy it to your own Cloudflare account.

The library is one small static index that the browser downloads whole, plus the `.osr`
replay files themselves. Beatmap files and audio are **not** stored here: they come from
public beatmap mirrors, fetched and cached by the visitor's own browser.

> **Status: scaffolding.** The workspace, configuration and deploy path exist and build.
> There is no ingest, no API route and no interface yet — see [What exists](#what-exists).

## Requirements

| | |
|---|---|
| Node 24.21.0 | pinned in `.nvmrc` |
| pnpm 12.4.2 | `packageManager` in `package.json` |
| Rust 1.98.1 + `wasm32-unknown-unknown` | `rust-toolchain.toml`; `rustup` installs both on first use |
| A Cloudflare Workers account | the free plan is enough |
| An R2 bucket | for the replay files. 10,000 replays is roughly 400 MB, inside the free 10 GB |
| osu!lazer, installed | with the replays you want to publish |
| An osu! OAuth application | client credentials — see below |

## What exists

```
apps/corner/       the corner itself: profile card, replay library, later the viewer
crates/worker/     osu-worker   the /api/* Worker
crates/core/       corner-core  cache logic, runtime-agnostic so it tests without wasm
crates/osu-core/   osu-core     .osr and .osu parsers, the index format, pp
crates/ingest/     osu-ingest   the local binary: replays → index → R2
```

Only `apps/corner` and `crates/worker` do anything today — the Worker serves `/api/health`
and nothing else. The other three are the decided crate layout with their boundaries
recorded, and are filled in later.

## Setting it up

1. **Install and authenticate.**

   ```bash
   pnpm install
   pnpm exec wrangler login
   ```

2. **Point it at your installs.** `osu-corner.toml` is committed and holds only generic
   defaults. Your own values belong in `osu-corner.local.toml` beside it, which is gitignored and
   merged over the defaults — the shape to copy is at the bottom of `osu-corner.toml`. Add a
   `[[source]]` for each game install you want read.

3. **Set your account** in that same local file — `[[user]] id` is your numeric osu! id, and
   `names` must list every username the account has played under. A rename means a new
   alias; ingest reports replays that matched no configured account, so a missing one is
   visible rather than silent.

4. **Create an R2 bucket** in the Cloudflare dashboard, then put its public hostname and
   the bucket name into the local file.

5. **Register an osu! OAuth application** at
   [osu.ppy.sh/home/account/edit](https://osu.ppy.sh/home/account/edit). Use the
   client-credentials grant, so the application callback URL stays empty, then copy
   `.dev.vars.example` to `.dev.vars` and fill in the id and secret. Production uses
   `wrangler secret put` instead, so the values never enter the repository. Ingest uses the
   same two values: a beatmap that **neither** of your installs holds can still be fetched,
   because its MD5 is enough for the osu! API to name it and a mirror then serves the file.
   **Without them the run still succeeds** and simply reports those maps as not looked for.

6. **Collect, then build the index** — `cargo run -p osu-ingest`. It reads the game folders
   directly, so there is no export step and no third-party tool. *Index writing not
   implemented yet.*

7. **Deploy.**

   ```bash
   pnpm build
   pnpm exec wrangler deploy
   ```

   With no route configured this publishes to `osu-corner.<account>.workers.dev`, and the
   page is at `/osu/` there rather than at the root — see the next section.

## Mounting it at a path

The corner is built to be mounted at a **path**, so it can live at `example.com/osu/`
alongside another site instead of taking a whole domain.

That path appears in two places and they must agree:

| Where | What |
|---|---|
| `base` in `apps/corner/vite.config.ts` | what URLs the build emits |
| the route in `wrangler.toml` | what the Worker answers for |

**`vite.config.ts` also derives the build's output nesting from it, and that part is not a
preference.** Workers serve static assets only from a directory structure that mirrors the
requested path, so a corner answering at `/osu/` must physically build to `dist/osu/`. A
corner serving from a domain root uses `/` and builds to `dist/`.

**Nothing in the app may hardcode a root-absolute path.** Use `import.meta.env.BASE_URL`.
Vite rewrites the bundle and `index.html` for you; anything hand-written — a fetch, an image
`src`, a link — has to read the same value, or it breaks in exactly the places nobody tests.

## Configuration

| File | Holds | Committed |
|---|---|---|
| `osu-corner.toml` | generic defaults — the site name, the mirror list, and the shape of everything else | yes |
| `osu-corner.local.toml` | **your** values — install paths, your account, the R2 bucket. Merged over the defaults; arrays replace rather than append | **no** — gitignored |
| `apps/corner/vite.config.ts` | the mount path | yes |
| `wrangler.toml` | the Worker name, assets, and later the route | yes |
| `.dev.vars` | the osu! client id and secret — read by **both** the Worker and `osu-ingest`, the latter to fetch a beatmap no install holds | **no** — gitignored |

## Layout notes

`crates/core` (`corner-core`) is deliberately separate from `crates/osu-core`. It must not
depend on `worker`, which is what lets its logic be tested with `cargo test` — no wasm, no
network, no runtime. `osu-core` holds the osu! domain and is the piece worth lifting out on
its own.

## Development

```bash
pnpm check                      # typecheck every workspace package
pnpm build                      # build the app
cargo test -p corner-core -p osu-core
cargo fmt --all --check
```

`pnpm preview` needs the Worker built first:

```bash
cd crates/worker && worker-build --release && cd ../..
pnpm preview
```
