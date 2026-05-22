//! LAMCLOD API 客户端。
//!
//! 连接池复用、rate-limit 感知重试、SSE 流式响应。

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use std::time::Duration;
#[cfg(feature = "tracing")]
use std::time::Instant;
use tokio::time::sleep;

use crate::config::ClientConfig;
use crate::error::{ColdError, Result};
use crate::stream::ChatStream;
use crate::types::{ChatRequest, ChatResponse};

/// Thread-safe client for `OpenAI`-compatible Chat Completions API. Clone is cheap.
#[derive(Clone)]
pub struct ColdClient {
    http: reqwest::Client,
    config: ClientConfig,
    endpoint: String,
}

const _: fn() = || {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ColdClient>();
};

impl ColdClient {
    /// Default endpoint (<https://api.lamcold.com/v1>).
    ///
    /// # Errors
    ///
    /// Returns `ColdError::Config` if the API key contains invalid header characters.
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        Self::from_config(ClientConfig::new(api_key))
    }

    /// Custom endpoint. Pass **without** `/v1`.
    ///
    /// # Errors
    ///
    /// Returns `ColdError::Config` if the API key or custom headers are invalid.
    pub fn with_endpoint(base_url: impl Into<String>, api_key: impl Into<String>) -> Result<Self> {
        Self::from_config(ClientConfig::with_endpoint(base_url, api_key))
    }

    /// Build from full configuration.
    ///
    /// # Errors
    ///
    /// Returns `ColdError::Config` if headers are invalid, or `ColdError::Transport`
    /// if the HTTP client cannot be constructed.
    pub fn from_config(config: ClientConfig) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", config.api_key))
                .map_err(|_| ColdError::Config("invalid api key characters".into()))?,
        );
        for (key, value) in &config.extra_headers {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(key.as_bytes())
                    .map_err(|_| ColdError::Config(format!("invalid header name: {key}")))?,
                HeaderValue::from_str(value)
                    .map_err(|_| ColdError::Config(format!("invalid header value for {key}")))?,
            );
        }

        let mut builder = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(config.timeout)
            .pool_idle_timeout(config.pool_idle_timeout)
            .pool_max_idle_per_host(config.pool_max_idle_per_host)
            .tcp_nodelay(true)
            .tcp_keepalive(Duration::from_secs(60))
            .http2_adaptive_window(true);

        if let Some(ref proxy_url) = config.proxy {
            let proxy = reqwest::Proxy::all(proxy_url)
                .map_err(|e| ColdError::Config(format!("invalid proxy URL: {e}")))?;
            builder = builder.proxy(proxy);
        }

        let http = builder.build().map_err(ColdError::Transport)?;

        let endpoint = format!("{}/chat/completions", config.base_url);

        Ok(Self {
            http,
            config,
            endpoint,
        })
    }

    /// Non-streaming request with automatic retry.
    ///
    /// # Errors
    ///
    /// Returns `ColdError::Api` on 4xx/5xx, `ColdError::RateLimited` on 429 (after
    /// exhausting retries), or `ColdError::Transport` on network failure.
    pub async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse> {
        let max = self.config.retry.max_retries;
        let mut last_err: ColdError;

        match self.send_and_parse(request).await {
            Ok(v) => return Ok(v),
            Err(e) => last_err = e,
        }

        for attempt in 1..=max {
            if !last_err.is_retryable() {
                break;
            }
            self.wait_backoff(&last_err, attempt - 1).await;
            match self.send_and_parse(request).await {
                Ok(v) => return Ok(v),
                Err(e) => last_err = e,
            }
        }

        Err(last_err)
    }

    /// Streaming request with automatic retry on the initial connection.
    ///
    /// # Errors
    ///
    /// Returns `ColdError::Api` on 4xx/5xx, `ColdError::RateLimited` on 429 (after
    /// exhausting retries), or `ColdError::Transport` on network failure.
    /// Mid-stream errors surface from the returned [`ChatStream`] iterator.
    pub async fn chat_stream(&self, request: &ChatRequest) -> Result<ChatStream> {
        let mut req = request.clone();
        req.stream = Some(true);

        let max = self.config.retry.max_retries;
        let mut last_err: ColdError;

        match self.send_request(&req).await {
            Ok(resp) => return Ok(ChatStream::new(resp.bytes_stream())),
            Err(e) => last_err = e,
        }

        for attempt in 1..=max {
            if !last_err.is_retryable() {
                break;
            }
            self.wait_backoff(&last_err, attempt - 1).await;
            match self.send_request(&req).await {
                Ok(resp) => return Ok(ChatStream::new(resp.bytes_stream())),
                Err(e) => last_err = e,
            }
        }

        Err(last_err)
    }

    // ─── Internal ────────────────────────────────────────────

    async fn wait_backoff(&self, err: &ColdError, attempt: u32) {
        let backoff = match err {
            ColdError::RateLimited { retry_after_ms } => {
                #[cfg(feature = "tracing")]
                tracing::trace!(retry_after_ms, "rate limited");
                Duration::from_millis(*retry_after_ms)
            }
            _ => self.config.retry.backoff_for(attempt),
        };
        #[cfg(feature = "tracing")]
        tracing::trace!(attempt, backoff_ms = backoff.as_millis() as u64, error = %err, "retry attempt");
        sleep(backoff).await;
    }

    async fn send_and_parse(&self, request: &ChatRequest) -> Result<ChatResponse> {
        let resp = self.send_request(request).await?;
        resp.json().await.map_err(ColdError::Transport)
    }

    async fn send_request(&self, request: &ChatRequest) -> Result<reqwest::Response> {
        #[cfg(feature = "tracing")]
        let start = Instant::now();
        #[cfg(feature = "tracing")]
        tracing::trace!(
            model = %request.model,
            endpoint = %self.endpoint,
            stream = request.stream.unwrap_or(false),
            "request sent"
        );

        let resp = self
            .http
            .post(&self.endpoint)
            .json(request)
            .send()
            .await
            .map_err(ColdError::Transport)?;
        let status = resp.status().as_u16();

        #[cfg(feature = "tracing")]
        tracing::trace!(
            status,
            latency_ms = start.elapsed().as_millis() as u64,
            "response received"
        );

        if status == 429 {
            let retry_after_ms = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| {
                    // Try seconds (integer or fractional, e.g. "1", "0.5", "2.0")
                    if let Ok(secs) = s.parse::<f64>() {
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        return Some((secs.max(0.0) * 1000.0) as u64);
                    }
                    // Try HTTP-date (e.g. "Thu, 22 May 2025 12:00:00 GMT")
                    parse_http_date_delta(s)
                })
                .unwrap_or(1000);
            return Err(ColdError::RateLimited { retry_after_ms });
        }

        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(ColdError::Api { status, body });
        }

        Ok(resp)
    }
}

/// Parse an HTTP-date `Retry-After` value and return milliseconds until that time.
/// Returns `None` if the string is not a recognized HTTP-date or the date is in the past.
fn parse_http_date_delta(s: &str) -> Option<u64> {
    let target = httpdate::parse_http_date(s).ok()?;
    let now = std::time::SystemTime::now();
    let delta = target.duration_since(now).ok()?;
    #[allow(clippy::cast_possible_truncation)]
    Some(delta.as_millis().min(u128::from(u64::MAX)) as u64)
}
