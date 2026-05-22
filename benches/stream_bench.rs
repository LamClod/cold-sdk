use bytes::Bytes;
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use futures_core::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::runtime::Runtime;

use cold_sdk::ChatStream;

// ─── Mock stream ─────────────────────────────────────────────

struct MockByteStream {
    chunks: Vec<Bytes>,
    index: usize,
}

impl MockByteStream {
    fn new(chunks: Vec<Vec<u8>>) -> Self {
        Self {
            chunks: chunks.into_iter().map(Bytes::from).collect(),
            index: 0,
        }
    }
}

impl Stream for MockByteStream {
    type Item = Result<Bytes, reqwest::Error>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.index < self.chunks.len() {
            let chunk = self.chunks[self.index].clone();
            self.index += 1;
            Poll::Ready(Some(Ok(chunk)))
        } else {
            Poll::Ready(None)
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────

fn make_small_event(i: usize) -> String {
    format!(
        "data: {{\"id\":\"chatcmpl-{i}\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"gpt-4o\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"word{i}\"}},\"finish_reason\":null}}]}}\n\n"
    )
}

fn make_large_event(i: usize) -> String {
    let padding = "x".repeat(4000);
    format!(
        "data: {{\"id\":\"chatcmpl-{i}\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"gpt-4o\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{padding}\"}},\"finish_reason\":null}}]}}\n\n"
    )
}

fn make_tool_call_events(num_tools: usize) -> String {
    let mut out = String::new();
    // Initial event with tool call starts
    for t in 0..num_tools {
        out.push_str(&format!(
            "data: {{\"id\":\"chatcmpl-tc\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"gpt-4o\",\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":{t},\"id\":\"call_{t}\",\"type\":\"function\",\"function\":{{\"name\":\"tool_{t}\",\"arguments\":\"\"}}}}]}},\"finish_reason\":null}}]}}\n\n"
        ));
    }
    // Argument fragments
    for _ in 0..5 {
        for t in 0..num_tools {
            out.push_str(&format!(
                "data: {{\"id\":\"chatcmpl-tc\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"gpt-4o\",\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":{t},\"function\":{{\"arguments\":\"{{\\\"key\\\":\\\"val\\\"}}\"}}}}]}},\"finish_reason\":null}}]}}\n\n"
            ));
        }
    }
    out.push_str("data: [DONE]\n\n");
    out
}

// ─── Benchmarks ──────────────────────────────────────────────

fn bench_parse_1000_small_events(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut payload = String::new();
    for i in 0..1000 {
        payload.push_str(&make_small_event(i));
    }
    payload.push_str("data: [DONE]\n\n");
    let data = payload.into_bytes();

    c.bench_function("parse_1000_small_sse_events", |b| {
        b.iter(|| {
            rt.block_on(async {
                let stream = MockByteStream::new(vec![data.clone()]);
                let mut chat_stream = ChatStream::new(stream);
                let text = chat_stream.collect_text().await.unwrap();
                black_box(text);
            });
        });
    });
}

fn bench_parse_100_large_events(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut payload = String::new();
    for i in 0..100 {
        payload.push_str(&make_large_event(i));
    }
    payload.push_str("data: [DONE]\n\n");
    let data = payload.into_bytes();

    c.bench_function("parse_100_large_sse_events_4kb", |b| {
        b.iter(|| {
            rt.block_on(async {
                let stream = MockByteStream::new(vec![data.clone()]);
                let mut chat_stream = ChatStream::new(stream);
                let text = chat_stream.collect_text().await.unwrap();
                black_box(text);
            });
        });
    });
}

fn bench_tool_call_accumulation(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let payload = make_tool_call_events(10);
    let data = payload.into_bytes();

    c.bench_function("tool_call_accumulation_10_parallel", |b| {
        b.iter(|| {
            rt.block_on(async {
                let stream = MockByteStream::new(vec![data.clone()]);
                let mut chat_stream = ChatStream::new(stream);
                let result = chat_stream.collect_tool_calls().await.unwrap();
                black_box(result);
            });
        });
    });
}

fn bench_chunked_delivery(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    // Simulate realistic chunked delivery: each event arrives in its own chunk
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    for i in 0..500 {
        chunks.push(make_small_event(i).into_bytes());
    }
    chunks.push(b"data: [DONE]\n\n".to_vec());

    c.bench_function("parse_500_events_individual_chunks", |b| {
        b.iter(|| {
            rt.block_on(async {
                let stream = MockByteStream::new(chunks.clone());
                let mut chat_stream = ChatStream::new(stream);
                let text = chat_stream.collect_text().await.unwrap();
                black_box(text);
            });
        });
    });
}

fn bench_partial_chunks(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    // Split events at arbitrary byte boundaries (simulates TCP fragmentation)
    let mut full_payload = String::new();
    for i in 0..200 {
        full_payload.push_str(&make_small_event(i));
    }
    full_payload.push_str("data: [DONE]\n\n");
    let bytes = full_payload.into_bytes();

    // Split into 64-byte chunks
    let chunks: Vec<Vec<u8>> = bytes.chunks(64).map(|c| c.to_vec()).collect();

    c.bench_function("parse_200_events_64byte_fragments", |b| {
        b.iter(|| {
            rt.block_on(async {
                let stream = MockByteStream::new(chunks.clone());
                let mut chat_stream = ChatStream::new(stream);
                let text = chat_stream.collect_text().await.unwrap();
                black_box(text);
            });
        });
    });
}

criterion_group!(
    benches,
    bench_parse_1000_small_events,
    bench_parse_100_large_events,
    bench_tool_call_accumulation,
    bench_chunked_delivery,
    bench_partial_chunks,
);
criterion_main!(benches);
