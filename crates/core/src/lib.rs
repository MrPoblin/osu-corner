#![forbid(unsafe_code)]

//! Runtime-agnostic cache logic.
//!
//! Must never depend on `worker` — that is what lets the fetch/cache decision be tested with
//! `cargo test` rather than against a deployed Worker.
