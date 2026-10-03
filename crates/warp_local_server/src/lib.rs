use std::fmt;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response, Sse};
use axum::routing::{get, post};
use tokio::sync::Semaphore;

mod chat;
mod protocol;
mod stream;
mod tools;

/// A fixed OpenAI-compatible provider. Incoming requests cannot redirect credentials elsewhere.
#[derive(Clone)]
pub struct Config {
    pub base_url: String,
    pub model: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Clone)]
pub(crate) struct Server {
    config: Config,
    client: reqwest::Client,
    requests: Arc<Semaphore>,
}

#[derive(Debug)]
pub enum Error {
    Invalid(&'static str),
    Provider(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) | Self::Provider(message) => formatter.write_str(message),
        }
    }
}
impl std::error::Error for Error {}

/// Creates the local protocol adapter; no Warp authentication or cloud calls are made.
pub fn router(config: Config) -> Result<Router, Error> {
    let url = reqwest::Url::parse(&config.base_url)
        .map_err(|_| Error::Invalid("Invalid provider base URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Invalid(
            "Provider URL must be an HTTP(S) base URL without credentials, query, or fragment",
        ));
    }
    let server = Server {
        config,
        client: stream::http_client()?,
        requests: Arc::new(Semaphore::new(4)),
    };
    Ok(Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ai/multi-agent", post(generate))
        .layer(DefaultBodyLimit::max(16 * 1024 * 1024))
        .with_state(server))
}

async fn generate(State(server): State<Server>, body: Bytes) -> Response {
    let permit = match server.requests.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Too many active local agent requests",
            )
                .into_response();
        }
    };
    let turn = match protocol::decode_request(&body)
        .and_then(|request| chat::prepare(request, &server.config))
    {
        Ok(turn) => turn,
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };
    // The upstream future lives inside the response body; dropping it cancels inference.
    Sse::new(stream::response(server, turn, permit))
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response()
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
