use thiserror::Error;

pub type Result<T> = std::result::Result<T, KalshiError>;

#[derive(Debug, Error)]
pub enum KalshiError {
    #[error("websocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("http error during ws upgrade: {0}")]
    Http(String),

    #[error("invalid url: {0}")]
    Url(#[from] url::ParseError),

    #[error("auth: {0}")]
    Auth(String),

    #[error("server error {code:?}: {msg}")]
    Server { code: ErrorCode, msg: String },

    #[error("clock skew: server rejected timestamp; check system clock")]
    ClockSkew,

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("rsa: {0}")]
    Rsa(String),

    #[error("subscription closed before ack")]
    SubscriptionClosed,

    #[error("subscription request timed out")]
    RequestTimeout,

    #[error("client shut down")]
    Shutdown,

    #[error("disconnected and could not reconnect after {attempts} attempts")]
    Reconnect { attempts: u32 },
}

impl From<rsa::Error> for KalshiError {
    fn from(e: rsa::Error) -> Self {
        KalshiError::Rsa(e.to_string())
    }
}

impl From<rsa::pkcs8::Error> for KalshiError {
    fn from(e: rsa::pkcs8::Error) -> Self {
        KalshiError::Auth(format!("pkcs8: {e}"))
    }
}

impl From<rsa::pkcs1::Error> for KalshiError {
    fn from(e: rsa::pkcs1::Error) -> Self {
        KalshiError::Auth(format!("pkcs1: {e}"))
    }
}

/// Server error codes documented in the Kalshi AsyncAPI spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ErrorCode {
    UnableToProcess = 1,
    ParamsRequired = 2,
    ChannelsRequired = 3,
    SubscriptionIdsRequired = 4,
    UnknownCommand = 5,
    AlreadySubscribed = 6,
    UnknownSubscriptionId = 7,
    UnknownChannel = 8,
    AuthenticationRequired = 9,
    ChannelError = 10,
    InvalidParameter = 11,
    ExactlyOneSubscriptionId = 12,
    UnsupportedAction = 13,
    MarketTickerRequired = 14,
    ActionRequired = 15,
    MarketNotFound = 16,
    InternalError = 17,
    CommandTimeout = 18,
    ShardFactorMustBePositive = 19,
    ShardFactorRequiredWithKey = 20,
    ShardKeyOutOfRange = 21,
    ShardFactorTooLarge = 22,
    Unknown = 0,
}

impl ErrorCode {
    pub fn from_u8(code: u8) -> Self {
        match code {
            1 => ErrorCode::UnableToProcess,
            2 => ErrorCode::ParamsRequired,
            3 => ErrorCode::ChannelsRequired,
            4 => ErrorCode::SubscriptionIdsRequired,
            5 => ErrorCode::UnknownCommand,
            6 => ErrorCode::AlreadySubscribed,
            7 => ErrorCode::UnknownSubscriptionId,
            8 => ErrorCode::UnknownChannel,
            9 => ErrorCode::AuthenticationRequired,
            10 => ErrorCode::ChannelError,
            11 => ErrorCode::InvalidParameter,
            12 => ErrorCode::ExactlyOneSubscriptionId,
            13 => ErrorCode::UnsupportedAction,
            14 => ErrorCode::MarketTickerRequired,
            15 => ErrorCode::ActionRequired,
            16 => ErrorCode::MarketNotFound,
            17 => ErrorCode::InternalError,
            18 => ErrorCode::CommandTimeout,
            19 => ErrorCode::ShardFactorMustBePositive,
            20 => ErrorCode::ShardFactorRequiredWithKey,
            21 => ErrorCode::ShardKeyOutOfRange,
            22 => ErrorCode::ShardFactorTooLarge,
            _ => ErrorCode::Unknown,
        }
    }
}
