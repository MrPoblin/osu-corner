#![forbid(unsafe_code)]

//! Thin `workers-rs` adapter. Logic belongs in `corner-core`.

use worker::*;

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    Router::new()
        .get_async("/api/health", health)
        .run(req, env)
        .await
}

/// Liveness and the deployed commit, so a deploy can be confirmed from outside.
async fn health(_req: Request, _ctx: RouteContext<()>) -> Result<Response> {
    Response::from_json(&serde_json::json!({
        "ok": true,
        "commit": option_env!("GIT_SHA").unwrap_or("dev"),
    }))
}
