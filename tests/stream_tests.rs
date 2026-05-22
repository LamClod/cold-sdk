//! Unit tests for the SSE stream parser in `cold_sdk::stream`.

use bytes::Bytes;
use cold_sdk::{ChatStream, ColdError};
use futures_core::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

// ─── Mock byte stream helper ─────────────────────────────────────────────────

/// A mock byte stream that yields pre-defined chunks one at a time.
struct MockByteStream {
    chunks: Vec<Vec<u8>>,
    index: usize,
}

impl MockByteStream {
    fn new(chunks: Vec<Vec<u8>>) -> Self {
        Self { chunks, index: 0 }
    }
}

impl Stream for MockByteStream {
    type Item = Result<Bytes, reqwest::Error>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.index < self.chunks.len() {
            let chunk = self.chunks[self.index].clone();
            self.index += 1;
            Poll::Ready(Some(Ok(Bytes::from(chunk))))
        } else {
            Poll::Ready(None)
        }
    }
}

/// Helper to create a ChatStream from raw byte chunks.
fn stream_from_chunks(chunks: Vec<Vec<u8>>) -> ChatStream {
    ChatStream::new(MockByteStream::new(chunks))
}

// ─── Helpers for building SSE payloads ───────────────────────────────────────

fn make_chunk_json(content: &str) -> String {
    format!(
        r#"{{"id":"1","object":"chat.completion.chunk","created":0,"model":"gpt-4","choices":[{{"index":0,"delta":{{"content":"{content}"}},"finish_reason":null}}]}}"#
    )
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_normal_sse_event() {
    let data = format!("data: {}\n\n", make_chunk_json("hello"));
    let mut stream = stream_from_chunks(vec![data.into_bytes()]);

    let chunk = stream.next().await.unwrap().unwrap();
    assert_eq!(chunk.id, "1");
    assert_eq!(chunk.model, "gpt-4");
    assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("hello"));

    // Stream should end
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn test_multiple_events_in_one_chunk() {
    let data = format!(
        "data: {}\n\ndata: {}\n\n",
        make_chunk_json("hello"),
        make_chunk_json(" world")
    );
    let mut stream = stream_from_chunks(vec![data.into_bytes()]);

    let c1 = stream.next().await.unwrap().unwrap();
    assert_eq!(c1.choices[0].delta.content.as_deref(), Some("hello"));

    let c2 = stream.next().await.unwrap().unwrap();
    assert_eq!(c2.choices[0].delta.content.as_deref(), Some(" world"));

    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn test_partial_event_split_across_chunks() {
    let full = format!("data: {}\n\n", make_chunk_json("split"));
    let bytes = full.into_bytes();
    // Split in the middle of the JSON
    let mid = bytes.len() / 2;
    let chunk1 = bytes[..mid].to_vec();
    let chunk2 = bytes[mid..].to_vec();

    let mut stream = stream_from_chunks(vec![chunk1, chunk2]);

    let c = stream.next().await.unwrap().unwrap();
    assert_eq!(c.choices[0].delta.content.as_deref(), Some("split"));

    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn test_done_sentinel() {
    let data = format!("data: {}\n\ndata: [DONE]\n\n", make_chunk_json("fin"));
    let mut stream = stream_from_chunks(vec![data.into_bytes()]);

    let c = stream.next().await.unwrap().unwrap();
    assert_eq!(c.choices[0].delta.content.as_deref(), Some("fin"));

    // [DONE] means stream is complete
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn test_crlf_line_endings() {
    let data = format!("data: {}\r\n\r\n", make_chunk_json("crlf"));
    let mut stream = stream_from_chunks(vec![data.into_bytes()]);

    let c = stream.next().await.unwrap().unwrap();
    assert_eq!(c.choices[0].delta.content.as_deref(), Some("crlf"));

    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn test_bare_cr_line_endings() {
    // Bare \r\r as event boundary. The parser needs at least one byte after
    // the second \r to confirm it's not \r\n, so we append a space.
    let data = format!("data: {}\r\r ", make_chunk_json("cr"));
    let mut stream = stream_from_chunks(vec![data.into_bytes()]);

    let c = stream.next().await.unwrap().unwrap();
    assert_eq!(c.choices[0].delta.content.as_deref(), Some("cr"));
}

#[tokio::test]
async fn test_mixed_line_endings() {
    // First event uses \r\n + \n, second uses \n\n
    let data = format!(
        "data: {}\r\n\ndata: {}\n\n",
        make_chunk_json("mixed1"),
        make_chunk_json("mixed2")
    );
    let mut stream = stream_from_chunks(vec![data.into_bytes()]);

    let c1 = stream.next().await.unwrap().unwrap();
    assert_eq!(c1.choices[0].delta.content.as_deref(), Some("mixed1"));

    let c2 = stream.next().await.unwrap().unwrap();
    assert_eq!(c2.choices[0].delta.content.as_deref(), Some("mixed2"));

    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn test_utf8_bom_at_stream_start() {
    let json = make_chunk_json("bom");
    let mut bytes = vec![0xEF, 0xBB, 0xBF]; // UTF-8 BOM
    bytes.extend_from_slice(format!("data: {}\n\n", json).as_bytes());

    let mut stream = stream_from_chunks(vec![bytes]);

    let c = stream.next().await.unwrap().unwrap();
    assert_eq!(c.choices[0].delta.content.as_deref(), Some("bom"));

    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn test_comment_lines_skipped() {
    // SSE comment lines start with ':' and should be ignored
    let data = format!(
        ": this is a comment\ndata: {}\n\n",
        make_chunk_json("after_comment")
    );
    let mut stream = stream_from_chunks(vec![data.into_bytes()]);

    let c = stream.next().await.unwrap().unwrap();
    assert_eq!(c.choices[0].delta.content.as_deref(), Some("after_comment"));

    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn test_multiline_data_fields_joined() {
    // Per SSE spec, multiple `data:` lines in one event are joined by \n.
    // Since \n is valid JSON whitespace between tokens, we can split valid JSON
    // across two data: lines and it should parse successfully.
    let data = "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\ndata: \"created\":0,\"model\":\"gpt-4\",\"choices\":[]}\n\n";
    let mut stream = stream_from_chunks(vec![data.as_bytes().to_vec()]);

    // The two data fields are joined with \n — the result is valid JSON because
    // \n is whitespace between the comma and the next key.
    let chunk = stream.next().await.unwrap().unwrap();
    assert_eq!(chunk.id, "1");
    assert_eq!(chunk.model, "gpt-4");
    assert!(chunk.choices.is_empty());
}

#[tokio::test]
async fn test_id_and_retry_fields_ignored() {
    // id: and retry: fields in SSE should be ignored by the parser
    let data = format!(
        "id: msg-123\nretry: 5000\ndata: {}\n\n",
        make_chunk_json("ignore_fields")
    );
    let mut stream = stream_from_chunks(vec![data.into_bytes()]);

    let c = stream.next().await.unwrap().unwrap();
    assert_eq!(c.choices[0].delta.content.as_deref(), Some("ignore_fields"));

    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn test_empty_data_field() {
    // `data:\n\n` has an empty data field — should try to parse empty bytes as JSON
    // and fail because empty string is not valid JSON.
    let data = b"data:\n\n".to_vec();
    let mut stream = stream_from_chunks(vec![data]);

    let result = stream.next().await.unwrap();
    assert!(
        result.is_err(),
        "Empty data field should produce a JSON parse error"
    );
}

#[tokio::test]
async fn test_tool_call_streaming_accumulation() {
    // Simulate incremental tool call streaming.
    // Use serde_json to build proper JSON strings to avoid escaping issues.
    let ev1 = serde_json::json!({
        "id": "1", "object": "chat.completion.chunk", "created": 0, "model": "gpt-4",
        "choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "id": "call_1", "type": "function", "function": {"name": "get_weather", "arguments": "{"}}
        ]}, "finish_reason": null}]
    });
    let ev2 = serde_json::json!({
        "id": "1", "object": "chat.completion.chunk", "created": 0, "model": "gpt-4",
        "choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "function": {"arguments": "\"city\""}}
        ]}, "finish_reason": null}]
    });
    let ev3 = serde_json::json!({
        "id": "1", "object": "chat.completion.chunk", "created": 0, "model": "gpt-4",
        "choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "function": {"arguments": ":\"Paris\"}"}}
        ]}, "finish_reason": null}]
    });

    let events = vec![
        format!("data: {}\n\n", ev1),
        format!("data: {}\n\n", ev2),
        format!("data: {}\n\n", ev3),
        "data: [DONE]\n\n".to_string(),
    ];

    let chunks: Vec<Vec<u8>> = events.into_iter().map(|e| e.into_bytes()).collect();
    let mut stream = stream_from_chunks(chunks);

    let (text, tool_calls) = stream.collect_tool_calls().await.unwrap();
    assert!(text.is_empty());
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "call_1");
    assert_eq!(tool_calls[0].function.name, "get_weather");
    assert_eq!(tool_calls[0].function.arguments, r#"{"city":"Paris"}"#);
}

#[tokio::test]
async fn test_tool_call_index_over_128_returns_error() {
    // tool_call index > 128 should return an error
    let ev = serde_json::json!({
        "id": "1", "object": "chat.completion.chunk", "created": 0, "model": "gpt-4",
        "choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 129, "id": "call_x", "type": "function", "function": {"name": "fn", "arguments": "{}"}}
        ]}, "finish_reason": null}]
    });
    let event = format!("data: {}\n\n", ev);
    let mut stream = stream_from_chunks(vec![event.into_bytes()]);

    let result = stream.collect_tool_calls().await;
    assert!(result.is_err());
    match result.unwrap_err() {
        ColdError::Stream(msg) => {
            assert!(msg.contains("129"));
            assert!(msg.contains("128"));
        }
        other => panic!("Expected ColdError::Stream, got: {other:?}"),
    }
}
