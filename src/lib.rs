//! # cold-sdk
//!
//! LAMCLOD 底层 API 通信协议库。纯 Rust 实现，零 C 依赖。
//! 采用 `OpenAI` Chat Completions 兼容协议格式。

pub mod client;
pub mod config;
pub mod error;
pub mod stream;
pub mod types;

pub use client::ColdClient;
pub use config::{ClientConfig, RetryConfig};
pub use error::{ColdError, Result};
pub use stream::ChatStream;
pub use types::*;
