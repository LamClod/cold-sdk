//! Unit tests for `cold_sdk::config` and `cold_sdk::error`.

use std::time::Duration;

use cold_sdk::{ClientConfig, ColdError, RetryConfig};

// ═══════════════════════════════════════════════════════════════════════════════
// ClientConfig tests
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_client_config_new_default_url() {
    let config = ClientConfig::new("sk-test-key");
    // base_url is pub(crate), so we verify via Debug output
    let debug = format!("{:?}", config);
    assert!(
        debug.contains("https://api.lamcold.com/v1"),
        "Default base URL should be https://api.lamcold.com/v1, got: {debug}"
    );
}

#[test]
fn test_client_config_with_endpoint_appends_v1() {
    let config = ClientConfig::with_endpoint("https://custom.api.com", "sk-key");
    let debug = format!("{:?}", config);
    assert!(
        debug.contains("https://custom.api.com/v1"),
        "with_endpoint should append /v1, got: {debug}"
    );
}

#[test]
fn test_client_config_with_endpoint_strips_trailing_slashes() {
    let config = ClientConfig::with_endpoint("https://custom.api.com///", "sk-key");
    let debug = format!("{:?}", config);
    assert!(
        debug.contains("https://custom.api.com/v1"),
        "Trailing slashes should be stripped before appending /v1, got: {debug}"
    );
    assert!(
        !debug.contains("https://custom.api.com///"),
        "Should not contain trailing slashes"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// RetryConfig::backoff_for tests
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_backoff_for_normal_values() {
    let retry = RetryConfig {
        max_retries: 5,
        initial_backoff: Duration::from_millis(500),
        max_backoff: Duration::from_secs(30),
        multiplier: 2.0,
    };

    // attempt 0: 500ms * 2^0 = 500ms
    assert_eq!(retry.backoff_for(0), Duration::from_millis(500));
    // attempt 1: 500ms * 2^1 = 1000ms
    assert_eq!(retry.backoff_for(1), Duration::from_millis(1000));
    // attempt 2: 500ms * 2^2 = 2000ms
    assert_eq!(retry.backoff_for(2), Duration::from_millis(2000));
    // attempt 3: 500ms * 2^3 = 4000ms
    assert_eq!(retry.backoff_for(3), Duration::from_millis(4000));
}

#[test]
fn test_backoff_for_clamps_at_max() {
    let retry = RetryConfig {
        max_retries: 10,
        initial_backoff: Duration::from_millis(500),
        max_backoff: Duration::from_secs(30),
        multiplier: 2.0,
    };

    // attempt 10: 500 * 2^10 = 512_000ms > 30_000ms, should clamp
    assert_eq!(retry.backoff_for(10), Duration::from_secs(30));
}

#[test]
fn test_backoff_for_attempt_63_clamp() {
    // Exponent clamped to 63 to prevent overflow
    let retry = RetryConfig {
        max_retries: 100,
        initial_backoff: Duration::from_millis(100),
        max_backoff: Duration::from_secs(60),
        multiplier: 2.0,
    };

    // With attempt=63 (clamped), 100 * 2^63 is astronomically large,
    // should be clamped to max_backoff
    let result = retry.backoff_for(63);
    assert_eq!(result, Duration::from_secs(60));

    // attempt=200 should also clamp exponent to 63, then clamp result to max_backoff
    let result = retry.backoff_for(200);
    assert_eq!(result, Duration::from_secs(60));
}

#[test]
fn test_backoff_for_nan_multiplier() {
    let retry = RetryConfig {
        max_retries: 3,
        initial_backoff: Duration::from_millis(500),
        max_backoff: Duration::from_secs(30),
        multiplier: f64::NAN,
    };

    // NaN multiplier: should fall back to initial_backoff
    assert_eq!(retry.backoff_for(0), Duration::from_millis(500));
    assert_eq!(retry.backoff_for(5), Duration::from_millis(500));
}

#[test]
fn test_backoff_for_infinite_multiplier() {
    let retry = RetryConfig {
        max_retries: 3,
        initial_backoff: Duration::from_millis(500),
        max_backoff: Duration::from_secs(30),
        multiplier: f64::INFINITY,
    };

    // Infinite multiplier: should fall back to initial_backoff
    assert_eq!(retry.backoff_for(0), Duration::from_millis(500));
    assert_eq!(retry.backoff_for(3), Duration::from_millis(500));
}

#[test]
fn test_backoff_for_negative_infinity_multiplier() {
    let retry = RetryConfig {
        max_retries: 3,
        initial_backoff: Duration::from_millis(500),
        max_backoff: Duration::from_secs(30),
        multiplier: f64::NEG_INFINITY,
    };

    // Negative infinity is not finite and <= 0, so fall back to initial_backoff
    assert_eq!(retry.backoff_for(0), Duration::from_millis(500));
}

// ═══════════════════════════════════════════════════════════════════════════════
// ColdError::is_retryable tests
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_is_retryable_rate_limited() {
    let err = ColdError::RateLimited {
        retry_after_ms: 1000,
    };
    assert!(err.is_retryable());
}

#[test]
fn test_is_retryable_timeout() {
    let err = ColdError::Timeout(5000);
    assert!(err.is_retryable());
}

#[test]
fn test_is_retryable_api_500() {
    let err = ColdError::Api {
        status: 500,
        body: "internal".into(),
    };
    assert!(err.is_retryable());
}

#[test]
fn test_is_retryable_api_502() {
    let err = ColdError::Api {
        status: 502,
        body: "bad gateway".into(),
    };
    assert!(err.is_retryable());
}

#[test]
fn test_is_retryable_api_503() {
    let err = ColdError::Api {
        status: 503,
        body: "unavailable".into(),
    };
    assert!(err.is_retryable());
}

#[test]
fn test_is_retryable_api_504() {
    let err = ColdError::Api {
        status: 504,
        body: "gateway timeout".into(),
    };
    assert!(err.is_retryable());
}

#[test]
fn test_not_retryable_api_400() {
    let err = ColdError::Api {
        status: 400,
        body: "bad request".into(),
    };
    assert!(!err.is_retryable());
}

#[test]
fn test_not_retryable_api_401() {
    let err = ColdError::Api {
        status: 401,
        body: "unauthorized".into(),
    };
    assert!(!err.is_retryable());
}

#[test]
fn test_not_retryable_api_403() {
    let err = ColdError::Api {
        status: 403,
        body: "forbidden".into(),
    };
    assert!(!err.is_retryable());
}

#[test]
fn test_not_retryable_api_404() {
    let err = ColdError::Api {
        status: 404,
        body: "not found".into(),
    };
    assert!(!err.is_retryable());
}

#[test]
fn test_not_retryable_json_error() {
    let err: ColdError = serde_json::from_str::<serde_json::Value>("invalid")
        .unwrap_err()
        .into();
    assert!(!err.is_retryable());
}

#[test]
fn test_not_retryable_stream_error() {
    let err = ColdError::Stream("some stream error".into());
    assert!(!err.is_retryable());
}

#[test]
fn test_not_retryable_config_error() {
    let err = ColdError::Config("bad config".into());
    assert!(!err.is_retryable());
}
