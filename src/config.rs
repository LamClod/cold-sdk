//! 客户端配置。连接参数、重试策略、自定义 header。

use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.lamcold.com/v1";

#[derive(Clone)]
pub struct ClientConfig {
    pub(crate) base_url: String,
    pub(crate) api_key: String,
    pub timeout: Duration,
    pub pool_idle_timeout: Duration,
    pub pool_max_idle_per_host: usize,
    pub retry: RetryConfig,
    pub extra_headers: Vec<(String, String)>,
    /// HTTP proxy URL (e.g. "http://127.0.0.1:7890")
    pub proxy: Option<String>,
}

impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &"[REDACTED]")
            .field("timeout", &self.timeout)
            .field("pool_idle_timeout", &self.pool_idle_timeout)
            .field("pool_max_idle_per_host", &self.pool_max_idle_per_host)
            .field("retry", &self.retry)
            .field(
                "extra_headers",
                &format_args!("[{} header(s)]", self.extra_headers.len()),
            )
            .field("proxy", &self.proxy.as_deref().unwrap_or("none"))
            .finish()
    }
}

impl ClientConfig {
    /// Default endpoint (<https://api.lamcold.com/v1>).
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key: api_key.into(),
            timeout: Duration::from_secs(300),
            pool_idle_timeout: Duration::from_secs(90),
            pool_max_idle_per_host: 10,
            retry: RetryConfig::default(),
            extra_headers: Vec::new(),
            proxy: None,
        }
    }

    /// Custom endpoint. Pass **without** `/v1` — it's appended automatically.
    #[must_use]
    pub fn with_endpoint(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let mut url = base_url.into();
        while url.ends_with('/') {
            url.pop();
        }
        url.push_str("/v1");

        Self {
            base_url: url,
            api_key: api_key.into(),
            timeout: Duration::from_secs(300),
            pool_idle_timeout: Duration::from_secs(90),
            pool_max_idle_per_host: 10,
            retry: RetryConfig::default(),
            extra_headers: Vec::new(),
            proxy: None,
        }
    }

    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub const fn with_retry(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
    }

    #[must_use]
    pub fn with_proxy(mut self, proxy_url: impl Into<String>) -> Self {
        self.proxy = Some(proxy_url.into());
        self
    }

    #[must_use]
    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_headers.push((key.into(), value.into()));
        self
    }

    #[must_use]
    pub const fn with_pool(mut self, max_idle: usize, idle_timeout: Duration) -> Self {
        self.pool_max_idle_per_host = max_idle;
        self.pool_idle_timeout = idle_timeout;
        self
    }
}

#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_retries: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub multiplier: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            multiplier: 2.0,
        }
    }
}

impl RetryConfig {
    #[must_use]
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Default::default()
        }
    }

    #[must_use]
    #[allow(
        clippy::cast_precision_loss,      // u128→f64: acceptable for ms-level backoff
        clippy::cast_possible_truncation, // f64→u64: clamped to max_backoff
        clippy::cast_sign_loss,           // f64→u64: clamped to >= 0.0
    )]
    pub fn backoff_for(&self, attempt: u32) -> Duration {
        if !self.multiplier.is_finite() || self.multiplier <= 0.0 {
            return self.initial_backoff;
        }
        // Clamp exponent to avoid pointless huge powers (63 is more than enough
        // since 2^63 * any base_ms already exceeds max_backoff)
        #[allow(clippy::cast_possible_wrap)] // 63 fits in i32
        let exp = attempt.min(63) as i32;
        let base_ms = self.initial_backoff.as_millis().min(u128::from(u64::MAX)) as f64;
        let max_ms = self.max_backoff.as_millis().min(u128::from(u64::MAX)) as f64;
        let ms = (base_ms * self.multiplier.powi(exp)).min(max_ms).max(0.0);
        Duration::from_millis(ms as u64)
    }
}
