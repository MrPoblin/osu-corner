//! [`corner_core::Transport`] over the runtime's `fetch`.

use corner_core::{Transport, TransportError, Upstream};
use worker::*;

/// How osu! is told who is calling.
///
/// osu! asks clients to identify themselves and rate-limits anonymous traffic harder, so this is
/// not decoration.
const USER_AGENT: &str = concat!("osu-corner/", env!("CARGO_PKG_VERSION"));

/// The Worker's outbound HTTP client.
///
/// A unit struct because `Fetch` is a global — there is nothing to hold, and cloning it costs
/// nothing, which is what lets `corner_core::refresh` take it by value for `wait_until`.
#[derive(Clone, Copy)]
pub struct HttpTransport;

impl Transport for HttpTransport {
    async fn get(&self, url: &str, bearer: &str) -> Result<Upstream, TransportError> {
        let headers = Headers::new();
        headers
            .set("Authorization", &format!("Bearer {bearer}"))
            .map_err(fail)?;
        headers.set("Accept", "application/json").map_err(fail)?;
        headers.set("User-Agent", USER_AGENT).map_err(fail)?;

        let mut init = RequestInit::new();
        init.with_method(Method::Get).with_headers(headers);

        let request = Request::new_with_init(url, &init).map_err(fail)?;
        let mut response = Fetch::Request(request).send().await.map_err(fail)?;

        let status = response.status_code();
        let etag = response.headers().get("ETag").map_err(fail)?;
        let body = response.text().await.map_err(fail)?;

        Ok(Upstream { status, body, etag })
    }
}

/// Failures as text. `worker::Error`'s own message never quotes request headers, so the bearer
/// cannot reach a log through here, and the text is only ever written to the Worker's console —
/// [`crate::Failure`] turns it into a status and nothing more.
fn fail(error: worker::Error) -> TransportError {
    TransportError(error.to_string())
}
