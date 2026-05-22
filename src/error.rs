use thiserror::Error;

#[derive(Error, Debug)]
pub enum ColdError {
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("api {status}: {body}")]
    Api { status: u16, body: String },

    #[error("rate limited, retry after {retry_after_ms}ms")]
    RateLimited { retry_after_ms: u64 },

    #[error("stream: {0}")]
    Stream(String),

    #[error("timeout after {0}ms")]
    Timeout(u64),

    #[error("config: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, ColdError>;

impl ColdError {
    /// Whether this error is worth retrying.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. } | Self::Timeout(_) => true,
            Self::Transport(e) => e.is_timeout() || e.is_connect(),
            Self::Api { status, .. } => matches!(status, 500 | 502 | 503 | 504),
            _ => false,
        }
    }
}
