use thiserror::Error;

pub type Result<T> = std::result::Result<T, RestError>;

#[derive(Debug, Error)]
pub enum RestError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    #[error("server returned {status}: {body}")]
    Server { status: u16, body: String },

    #[error("auth: {0}")]
    Auth(String),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("invalid header value: {0}")]
    Header(#[from] reqwest::header::InvalidHeaderValue),

    #[error("retries exhausted after {attempts} attempts; last status: {last_status:?}")]
    RetriesExhausted {
        attempts: u32,
        last_status: Option<u16>,
    },

    /// Caller asked Mode::Paper to refuse a destructive call. Surfaces as a
    /// distinct error so test harnesses can assert on it.
    #[error("paper mode: refusing to {action}")]
    PaperRefused { action: &'static str },
}

impl RestError {
    /// True for 429 and 5xx; the retry layer uses this to decide whether to
    /// back off and try again.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            RestError::Server { status, .. } if *status == 429 || (500..600).contains(status)
        )
    }
}
