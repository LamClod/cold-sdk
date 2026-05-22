//! SSE 流式响应解析器。BytesMut O(1) buffer 管理，支持所有行尾格式。

use bytes::{Bytes, BytesMut};
use futures_core::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::error::{ColdError, Result};
use crate::types::ChatStreamChunk;

const MAX_TOOL_CALL_INDEX: usize = 128;

/// A streaming response from the chat completions API.
///
/// Implements both `Stream` trait and an async `next()` method.
/// SSE `id:` and `retry:` fields are intentionally not tracked.
pub struct ChatStream {
    inner: Pin<Box<dyn Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send>>,
    buffer: BytesMut,
    done: bool,
    bom_stripped: bool,
}

// Compile-time assertion: ChatStream must be Unpin for get_mut() in poll_next.
const _: fn() = || {
    const fn assert_unpin<T: Unpin>() {}
    assert_unpin::<ChatStream>();
};

impl ChatStream {
    pub fn new(
        byte_stream: impl Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send + 'static,
    ) -> Self {
        Self {
            inner: Box::pin(byte_stream),
            buffer: BytesMut::with_capacity(4096),
            done: false,
            bom_stripped: false,
        }
    }

    /// Pull the next chunk. Returns `None` when the stream is complete.
    pub async fn next(&mut self) -> Option<Result<ChatStreamChunk>> {
        use tokio_stream::StreamExt;

        if self.done {
            return None;
        }

        loop {
            if let Some(result) = self.try_parse_event() {
                return match result {
                    ParseResult::Event(chunk) => Some(Ok(chunk)),
                    ParseResult::Done => {
                        self.done = true;
                        None
                    }
                    ParseResult::Error(e) => Some(Err(e)),
                };
            }

            let mut pinned = self.inner.as_mut();
            match pinned.next().await {
                Some(Ok(bytes)) => self.push_bytes(&bytes),
                Some(Err(e)) => return Some(Err(ColdError::Transport(e))),
                None => {
                    self.done = true;
                    // Flush: try to parse any remaining buffered event at EOF
                    return self.try_parse_event_eof().map(|r| match r {
                        ParseResult::Event(chunk) => Ok(chunk),
                        ParseResult::Done => {
                            unreachable!()
                        }
                        ParseResult::Error(e) => Err(e),
                    });
                }
            }
        }
    }

    /// Collect all text content from the stream.
    ///
    /// # Errors
    ///
    /// Returns `ColdError::Transport` on mid-stream network failure, or
    /// `ColdError::Json` on malformed SSE data.
    pub async fn collect_text(&mut self) -> Result<String> {
        let mut text = String::new();
        while let Some(chunk) = self.next().await {
            let chunk = chunk?;
            for choice in &chunk.choices {
                if let Some(content) = &choice.delta.content {
                    text.push_str(content);
                }
            }
        }
        Ok(text)
    }

    /// Collect all tool call fragments and return assembled tool calls.
    ///
    /// # Errors
    ///
    /// Same as [`collect_text`](Self::collect_text).
    pub async fn collect_tool_calls(&mut self) -> Result<(String, Vec<crate::types::ToolCall>)> {
        let mut text = String::new();
        let mut tool_calls: Vec<ToolCallAccumulator> = Vec::new();

        while let Some(chunk) = self.next().await {
            let chunk = chunk?;
            for choice in &chunk.choices {
                if let Some(content) = &choice.delta.content {
                    text.push_str(content);
                }
                if let Some(tc_deltas) = &choice.delta.tool_calls {
                    for tc_delta in tc_deltas {
                        let idx = tc_delta.index as usize;
                        if idx > MAX_TOOL_CALL_INDEX {
                            return Err(ColdError::Stream(format!(
                                "tool_call index {idx} exceeds maximum {MAX_TOOL_CALL_INDEX}"
                            )));
                        }
                        while tool_calls.len() <= idx {
                            tool_calls.push(ToolCallAccumulator::default());
                        }
                        let acc = &mut tool_calls[idx];
                        if let Some(id) = &tc_delta.id {
                            acc.id.clone_from(id);
                        }
                        if let Some(f) = &tc_delta.function {
                            if let Some(name) = &f.name {
                                acc.name.clone_from(name);
                            }
                            if let Some(args) = &f.arguments {
                                acc.arguments.push_str(args);
                            }
                        }
                    }
                }
            }
        }

        let assembled = tool_calls
            .into_iter()
            .map(|acc| crate::types::ToolCall {
                id: acc.id,
                call_type: "function".to_string(),
                function: crate::types::FunctionCall {
                    name: acc.name,
                    arguments: acc.arguments,
                },
            })
            .collect();

        Ok((text, assembled))
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        if self.bom_stripped {
            self.buffer.extend_from_slice(bytes);
        } else {
            self.bom_stripped = true;
            // Strip UTF-8 BOM if present at stream start
            let data = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
                &bytes[3..]
            } else {
                bytes
            };
            self.buffer.extend_from_slice(data);
        }
    }

    /// At EOF, try to parse whatever is left in the buffer as a final event.
    /// Strips trailing `\r` and `\n` and attempts to parse as a data event.
    fn try_parse_event_eof(&mut self) -> Option<ParseResult> {
        if self.buffer.is_empty() {
            return None;
        }
        // Take all remaining bytes
        let raw = self.buffer.split_off(0);
        let data = collect_data_fields(&raw);
        let data = data?;
        if data == b"[DONE]" {
            return Some(ParseResult::Done);
        }
        match serde_json::from_slice::<ChatStreamChunk>(&data) {
            Ok(chunk) => Some(ParseResult::Event(chunk)),
            Err(e) => Some(ParseResult::Error(ColdError::Json(e))),
        }
    }

    fn try_parse_event(&mut self) -> Option<ParseResult> {
        loop {
            let (event_end, terminator_len) = find_event_boundary(&self.buffer)?;

            let raw_bytes = self.buffer.split_to(event_end + terminator_len);
            let raw = &raw_bytes[..event_end];

            let data = collect_data_fields(raw);

            let Some(data) = data else {
                continue;
            };

            if data == b"[DONE]" {
                return Some(ParseResult::Done);
            }

            match serde_json::from_slice::<ChatStreamChunk>(&data) {
                Ok(chunk) => {
                    #[cfg(feature = "tracing")]
                    tracing::trace!(id = %chunk.id, choices = chunk.choices.len(), "stream event parsed");
                    return Some(ParseResult::Event(chunk));
                }
                Err(e) => return Some(ParseResult::Error(ColdError::Json(e))),
            }
        }
    }
}

impl Stream for ChatStream {
    type Item = Result<ChatStreamChunk>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        if this.done {
            return Poll::Ready(None);
        }

        // Loop until we produce a result or the inner stream is pending.
        loop {
            // Try to parse a complete event from buffer
            if let Some(result) = this.try_parse_event() {
                return match result {
                    ParseResult::Event(chunk) => Poll::Ready(Some(Ok(chunk))),
                    ParseResult::Done => {
                        this.done = true;
                        Poll::Ready(None)
                    }
                    ParseResult::Error(e) => Poll::Ready(Some(Err(e))),
                };
            }

            // Poll inner stream — this registers the waker if Pending
            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    this.push_bytes(&bytes);
                    // Loop back to try parsing again
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(ColdError::Transport(e))));
                }
                Poll::Ready(None) => {
                    this.done = true;
                    // Flush remaining buffered event at EOF
                    return match this.try_parse_event_eof() {
                        Some(ParseResult::Event(chunk)) => Poll::Ready(Some(Ok(chunk))),
                        Some(ParseResult::Error(e)) => Poll::Ready(Some(Err(e))),
                        _ => Poll::Ready(None),
                    };
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

// ─── Internal ────────────────────────────────────────────────

#[derive(Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
}

enum ParseResult {
    Event(ChatStreamChunk),
    Done,
    Error(ColdError),
}

/// Find the boundary of a complete SSE event.
/// Returns `(event_end, terminator_len)` where terminator is the blank line separator.
/// Handles `\n\n`, `\r\n\r\n`, `\r\r`, and mixed combinations.
fn find_event_boundary(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < buf.len() {
        let (is_eol, eol_len) = match buf[i] {
            b'\n' => (true, 1),
            b'\r' => {
                if i + 1 < buf.len() && buf[i + 1] == b'\n' {
                    (true, 2) // \r\n
                } else if i + 1 < buf.len() {
                    (true, 1) // bare \r (next byte is not \n)
                } else {
                    // \r at end of buffer — ambiguous, need more data
                    return None;
                }
            }
            _ => (false, 0),
        };

        if !is_eol {
            i += 1;
            continue;
        }

        let next = i + eol_len;
        if next >= buf.len() {
            return None; // need more data to confirm second EOL
        }

        let (is_second_eol, second_eol_len) = match buf[next] {
            b'\n' => (true, 1),
            b'\r' => {
                if next + 1 < buf.len() && buf[next + 1] == b'\n' {
                    (true, 2) // \r\n
                } else if next + 1 < buf.len() {
                    (true, 1) // bare \r
                } else {
                    // Ambiguous — need more data
                    return None;
                }
            }
            _ => (false, 0),
        };

        if is_second_eol {
            return Some((i, eol_len + second_eol_len));
        }

        i = next;
    }
    None
}

/// Collect all `data:` fields from a raw SSE event, joined by `\n` per spec.
fn collect_data_fields(raw: &[u8]) -> Option<Vec<u8>> {
    let mut result: Option<Vec<u8>> = None;

    for line in split_lines(raw) {
        let trimmed = strip_cr(line);
        let value = trimmed
            .strip_prefix(b"data: ")
            .or_else(|| trimmed.strip_prefix(b"data:"));

        if let Some(value) = value {
            match &mut result {
                None => result = Some(value.to_vec()),
                Some(buf) => {
                    buf.push(b'\n');
                    buf.extend_from_slice(value);
                }
            }
        }
    }

    result
}

fn split_lines(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    data.split(|&b| b == b'\n')
}

fn strip_cr(line: &[u8]) -> &[u8] {
    if line.last() == Some(&b'\r') {
        &line[..line.len() - 1]
    } else {
        line
    }
}
