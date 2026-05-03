//! Low-level WebSocket connection: builds the upgrade request with Kalshi's auth
//! headers and connects via `tokio-tungstenite`.

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::StreamExt;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::auth::Credentials;
use crate::error::{KalshiError, Result};

/// Where to connect. `Production` and `Demo` use Kalshi's public hosts;
/// `Custom` is for tests and self-hosted relays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Environment {
    Production,
    Demo,
    /// Custom endpoint. `path` is the value signed for the auth header (typically
    /// the request path of the WS upgrade, e.g. `/trade-api/ws/v2`).
    Custom { url: String, path: String },
}

impl Default for Environment {
    fn default() -> Self {
        Environment::Production
    }
}

impl Environment {
    pub fn ws_url(&self) -> &str {
        match self {
            Environment::Production => "wss://api.elections.kalshi.com/trade-api/ws/v2",
            Environment::Demo => "wss://demo-api.kalshi.co/trade-api/ws/v2",
            Environment::Custom { url, .. } => url,
        }
    }

    pub fn ws_path(&self) -> &str {
        match self {
            Environment::Production | Environment::Demo => "/trade-api/ws/v2",
            Environment::Custom { path, .. } => path,
        }
    }
}

pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub type WsSink = SplitSink<WsStream, Message>;
pub type WsRead = SplitStream<WsStream>;

/// Open a WebSocket connection to the given environment, attaching auth headers
/// when credentials are supplied.
pub async fn connect(env: &Environment, creds: Option<&Credentials>) -> Result<WsStream> {
    let request = build_request(env, creds)?;
    let (ws, response) = connect_async(request).await?;
    if response.status().as_u16() != 101 {
        return Err(KalshiError::Http(format!(
            "unexpected upgrade status: {}",
            response.status()
        )));
    }
    Ok(ws)
}

/// Connect and immediately split into a sink/stream pair.
pub async fn connect_split(
    env: &Environment,
    creds: Option<&Credentials>,
) -> Result<(WsSink, WsRead)> {
    let ws = connect(env, creds).await?;
    Ok(ws.split())
}

fn build_request(env: &Environment, creds: Option<&Credentials>) -> Result<Request> {
    let mut request = env.ws_url().into_client_request()?;
    if let Some(creds) = creds {
        let headers = request.headers_mut();
        for (name, value) in creds.signed_headers("GET", env.ws_path()) {
            let v = HeaderValue::from_str(&value)
                .map_err(|e| KalshiError::Auth(format!("invalid {name} header: {e}")))?;
            headers.insert(name, v);
        }
    }
    Ok(request)
}

impl From<tokio_tungstenite::tungstenite::http::Error> for KalshiError {
    fn from(e: tokio_tungstenite::tungstenite::http::Error) -> Self {
        KalshiError::Http(e.to_string())
    }
}
