<p align="center">
  <h1 align="center">cold-sdk</h1>
  <p align="center">LAMCLOD 底层 API 通信协议库</p>
  <p align="center">
    <img src="https://img.shields.io/badge/language-Rust-orange?style=flat-square" alt="Rust">
    <img src="https://img.shields.io/badge/TLS-rustls-blue?style=flat-square" alt="rustls">
    <img src="https://img.shields.io/badge/tests-36_pass-brightgreen?style=flat-square" alt="tests">
    <img src="https://img.shields.io/badge/license-MIT-green?style=flat-square" alt="MIT">
  </p>
</p>

---

## 简介

cold-sdk 是 LAMCLOD 的底层通信层，负责与服务端点之间的高性能数据传输。

采用 OpenAI Chat Completions 兼容协议格式作为传输标准。

## 特性

| | |
|---|---|
| **纯 Rust** | 零 C 依赖，rustls TLS，全平台编译 |
| **高性能** | HTTP/2 + 连接池 + gzip/brotli + BytesMut O(1) buffer |
| **智能重试** | 指数退避 + rate-limit 感知（尊重 Retry-After） |
| **流式支持** | SSE 实时解析，实现 `futures::Stream` trait |
| **可选观测** | `tracing` feature flag，零开销关闭 |

## 安装

```toml
[dependencies]
cold-sdk = "1.0"
```

## 用法

```rust
use cold_sdk::{ColdClient, ChatRequest, ChatMessage};

#[tokio::main]
async fn main() -> cold_sdk::Result<()> {
    let client = ColdClient::new("your-api-key")?;

    let req = ChatRequest::new("model-name", vec![
        ChatMessage::user("Hello"),
    ]);

    // 非流式
    let resp = client.chat(&req).await?;
    println!("{}", resp.text().unwrap_or(""));

    // 流式
    let mut stream = client.chat_stream(&req).await?;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        for choice in &chunk.choices {
            if let Some(text) = &choice.delta.content {
                print!("{text}");
            }
        }
    }

    Ok(())
}
```

## 对接其他端点

```rust
let client = ColdClient::with_endpoint("https://api.example.com", "key")?;
```

传入地址**不带** `/v1`，SDK 自动追加。

## Cold Stack

cold-cli 是 LAMCLOD 的 AI 编码助手 CLI，基于以下 4 个 Rust crate 构建：

```
cold-cli              CLI 入口
  |
cold-agent-sdk        Agent 编排 (loop + sub-agent + hooks + memory)
  |
  +-- cold-context    上下文管理 (压缩 + 安全 + 预算)
  +-- cold-tools      工具框架 + 20 内置工具 + MCP
  |
cold-sdk              API 传输层 (HTTP/2 + SSE + 重试)
```

| Crate | 描述 |
|-------|------|
| [cold-sdk](https://github.com/LamClod/cold-sdk) | API 通信层 |
| [cold-context](https://github.com/LamClod/cold-context) | 上下文窗口管理 |
| [cold-tools](https://github.com/LamClod/cold-tools) | 工具协议框架 |
| [cold-agent-sdk](https://github.com/LamClod/cold-agent-sdk) | Agent 编排 SDK |
| [cold-cli](https://github.com/LamClod/cold-cli) | 命令行界面 |

## License

MIT
