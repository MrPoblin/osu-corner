//! Exists for one line: tell Cargo that `GIT_SHA` changes the build.
//!
//! `lib.rs` reads it with `option_env!`, and Cargo does **not** track an environment variable it
//! was never told about. CI restores `target/` from a cache keyed on `Cargo.lock`, so a run that
//! changes nothing in this crate — a frontend-only commit, or a `workflow_dispatch` — would reuse
//! the previous wasm and keep reporting the *old* commit from `/api/health`. That endpoint exists
//! precisely to confirm a deploy matches the SHA that triggered it, so a stale value defeats it.
fn main() {
    println!("cargo:rerun-if-env-changed=GIT_SHA");
}
