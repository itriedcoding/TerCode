//! Ter Router API client: model discovery, validation, streaming chat with
//! tool calling, retries and precise error reporting.
//!
//! Base URL defaults to `https://api.terrouter.com/v1`. Authentication is a
//! bearer token (`tr_live_…`) that the user stores in the Windows Credential
//! Manager — it is never persisted in settings or logs.

use super::sse;
use super::types::*;
use crate::error::{Result, TerError};
use crate::USER_AGENT;
use bytes::Bytes;
use futures_util::stream::{self, BoxStream, StreamExt};
use std::pin::Pin;
use std::time::{Duration, Instant};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const BASE_BACKOFF_MS: u64 = 400;

pub struct TerRouterClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    request_timeout: Duration,
    max_retries: u32,
}

pub struct ClientOptions {
    pub base_url: String,
    pub api_key: String,
    pub request_timeout_secs: u64,
    pub max_retries: u32,
}

impl TerRouterClient {
    pub fn new(opts: ClientOptions) -> Result<Self> {
        if opts.api_key.trim().is_empty() {
            return Err(TerError::MissingApiKey);
        }
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            // No global timeout: streaming responses legitimately run long. The
            // per-request and per-chunk timeouts below bound every phase.
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .map_err(|e| TerError::Network {
                context: "building HTTP client".into(),
                source: e,
            })?;
        Ok(Self {
            http,
            base_url: opts.base_url.trim_end_matches('/').to_string(),
            api_key: opts.api_key.trim().to_string(),
            request_timeout: Duration::from_secs(opts.request_timeout_secs.clamp(10, 900)),
            max_retries: opts.max_retries.min(8),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    fn authed(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        builder
            .bearer_auth(&self.api_key)
            .header(reqwest::header::ACCEPT, "application/json")
    }

    /// GET /models — the authoritative list of models this key can use.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let request = self.authed(self.http.get(self.url("models")));
        let response = self.send_with_retry(request, "model discovery").await?;
        let status = response.status();
        let body = response.text().await.map_err(|e| TerError::Network {
            context: "reading model list".into(),
            source: e,
        })?;
        if !status.is_success() {
            return Err(map_status(status.as_u16(), &body, None));
        }
        let parsed: ModelList = serde_json::from_str(&body).map_err(|e| {
            TerError::BadResponse(format!(
                "model list was not OpenAI-shaped ({e}); first bytes: {}",
                preview(&body)
            ))
        })?;
        let mut models = parsed.data;
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

    /// Verifies the key and measures round-trip latency.
    pub async fn validate(&self) -> Result<ValidationReport> {
        let started = Instant::now();
        let models = self.list_models().await?;
        Ok(ValidationReport {
            ok: true,
            latency_ms: started.elapsed().as_millis() as u64,
            model_count: models.len(),
            sample_models: models.iter().take(8).map(|m| m.id.clone()).collect(),
            base_url: self.base_url.clone(),
        })
    }

    /// Non-streaming completion; used for quick utility calls and tests.
    pub async fn chat_once(&self, mut req: ChatRequest) -> Result<(ChatResponse, Option<Usage>)> {
        req.stream = false;
        req.stream_options = None;
        let body = serde_json::to_vec(&req).map_err(|e| TerError::json("chat request", e))?;
        let request = self
            .authed(self.http.post(self.url("chat/completions")))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
        let response = self.send_with_retry(request, "chat completion").await?;
        let status = response.status();
        let text = response.text().await.map_err(|e| TerError::Network {
            context: "reading completion".into(),
            source: e,
        })?;
        if !status.is_success() {
            return Err(map_status(status.as_u16(), &text, None));
        }
        let parsed: ChatResponse = serde_json::from_str(&text).map_err(|e| {
            TerError::BadResponse(format!(
                "completion was not OpenAI-shaped ({e}); first bytes: {}",
                preview(&text)
            ))
        })?;
        let usage = parsed.usage.clone();
        Ok((parsed, usage))
    }

    /// Streaming completion. Returns a stream of incremental events; the stream
    /// ends after `Finished` (or on error).
    pub async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream> {
        let body = serde_json::to_vec(&req).map_err(|e| TerError::json("chat request", e))?;
        let request = self
            .authed(self.http.post(self.url("chat/completions")))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .body(body);
        let response = self.send_with_retry(request, "streaming completion").await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(map_status(status.as_u16(), &text, None));
        }

        let idle = self.request_timeout;
        let byte_stream: Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<Bytes>> + Send>> =
            Box::pin(response.bytes_stream());

        let state = StreamState {
            body: byte_stream,
            buf: Vec::new(),
            pending: std::collections::VecDeque::new(),
            accumulator: ToolCallAccumulator::new(),
            finished: false,
            idle,
            saw_any_event: false,
        };

        let stream = stream::unfold(state, |mut st| async move {
            loop {
                if let Some(event) = st.pending.pop_front() {
                    return Some((Ok(event), st));
                }
                if st.finished {
                    return None;
                }
                if let Some(event) = st.take_event() {
                    return Some((Ok(event), st));
                }
                match tokio::time::timeout(st.idle, st.body.next()).await {
                    Err(_) => {
                        st.finished = true;
                        return Some((
                            Err(TerError::Timeout {
                                secs: st.idle.as_secs(),
                            }),
                            st,
                        ));
                    }
                    Ok(None) => {
                        st.finished = true;
                        // Flush a trailing event without a blank line terminator.
                        if !st.buf.is_empty() {
                            let mut tail = std::mem::take(&mut st.buf);
                            for payload in sse::drain_events(&mut tail) {
                                st.handle_payload(&payload);
                            }
                            // Some gateways end without a final blank line.
                            let remainder = String::from_utf8_lossy(&tail).to_string();
                            for line in remainder.split('\n') {
                                if let Some(rest) = line.trim_end().strip_prefix("data:") {
                                    st.handle_payload(rest.trim());
                                }
                            }
                        }
                        if let Some(event) = st.pending.pop_front() {
                            return Some((Ok(event), st));
                        }
                        if !st.saw_any_event {
                            return Some((
                                Err(TerError::BadResponse(
                                    "stream ended without any events".into(),
                                )),
                                st,
                            ));
                        }
                        return None;
                    }
                    Ok(Some(Err(e))) => {
                        st.finished = true;
                        return Some((
                            Err(TerError::Network {
                                context: "streaming completion".into(),
                                source: e,
                            }),
                            st,
                        ));
                    }
                    Ok(Some(Ok(chunk))) => {
                        st.buf.extend_from_slice(&chunk);
                        for payload in sse::drain_events(&mut st.buf) {
                            st.handle_payload(&payload);
                        }
                        if let Some(event) = st.pending.pop_front() {
                            return Some((Ok(event), st));
                        }
                    }
                }
            }
        });

        Ok(ChatStream {
            inner: Box::pin(stream),
        })
    }

    async fn send_with_retry(
        &self,
        request: reqwest::RequestBuilder,
        context: &str,
    ) -> Result<reqwest::Response> {
        let mut attempt = 0u32;
        loop {
            let builder = request
                .try_clone()
                .ok_or_else(|| TerError::Invalid("request body cannot be replayed".into()))?;
            let send = builder.send();
            let outcome = tokio::time::timeout(self.request_timeout, send).await;

            let error = match outcome {
                Err(_) => TerError::Timeout {
                    secs: self.request_timeout.as_secs(),
                },
                Ok(Err(e)) => TerError::Network {
                    context: context.to_string(),
                    source: e,
                },
                Ok(Ok(response)) => {
                    let status = response.status();
                    if status.is_success() {
                        return Ok(response);
                    }
                    let retry_after = retry_after_secs(response.headers());
                    let body = response.text().await.unwrap_or_default();
                    let err = map_status(status.as_u16(), &body, retry_after);
                    if !err.retryable() || attempt >= self.max_retries {
                        return Err(err);
                    }
                    if let TerError::RateLimited { retry_after_secs } = err {
                        let wait = retry_after_secs.min(30);
                        tokio::time::sleep(Duration::from_secs(wait.max(1))).await;
                        attempt += 1;
                        continue;
                    }
                    let backoff = backoff_delay(attempt);
                    tokio::time::sleep(backoff).await;
                    attempt += 1;
                    continue;
                }
            };

            if !error.retryable() || attempt >= self.max_retries {
                return Err(error);
            }
            tokio::time::sleep(backoff_delay(attempt)).await;
            attempt += 1;
        }
    }
}

fn backoff_delay(attempt: u32) -> Duration {
    let factor = 1u64 << attempt.min(5);
    Duration::from_millis(BASE_BACKOFF_MS * factor)
}

fn retry_after_secs(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
}

fn preview(body: &str) -> String {
    let trimmed: String = body.chars().take(180).collect();
    trimmed.replace('\n', " ")
}

/// Maps an HTTP failure to a user-actionable error.
pub fn map_status(status: u16, body: &str, retry_after: Option<u64>) -> TerError {
    let message = serde_json::from_str::<ApiErrorEnvelope>(body)
        .ok()
        .map(|e| {
            if e.error.kind.is_empty() {
                e.error.message
            } else {
                format!("{} ({})", e.error.message, e.error.kind)
            }
        })
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| {
            let p = preview(body);
            if p.trim().is_empty() {
                "no response body".to_string()
            } else {
                p
            }
        });

    match status {
        401 | 403 => TerError::Unauthorized { status },
        429 => TerError::RateLimited {
            retry_after_secs: retry_after.unwrap_or(5),
        },
        _ => TerError::Api { status, message },
    }
}

struct StreamState {
    body: Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<Bytes>> + Send>>,
    buf: Vec<u8>,
    pending: std::collections::VecDeque<StreamEvent>,
    accumulator: ToolCallAccumulator,
    finished: bool,
    idle: Duration,
    saw_any_event: bool,
}

impl StreamState {
    fn take_event(&mut self) -> Option<StreamEvent> {
        self.pending.pop_front()
    }

    fn handle_payload(&mut self, payload: &str) {
        let payload = payload.trim();
        if payload.is_empty() || sse::is_done(payload) {
            return;
        }
        self.saw_any_event = true;
        match serde_json::from_str::<StreamChunk>(payload) {
            Ok(chunk) => {
                if let Some(err) = chunk.error {
                    self.pending.push_back(StreamEvent::Text(format!(
                        "\n\n[gateway error] {}",
                        err.message
                    )));
                    self.finished = true;
                    return;
                }
                if let Some(usage) = chunk.usage {
                    self.pending.push_back(StreamEvent::Usage(usage));
                }
                for choice in chunk.choices {
                    if let Some(text) = choice.delta.content {
                        if !text.is_empty() {
                            self.pending.push_back(StreamEvent::Text(text));
                        }
                    }
                    if let Some(reasoning) = choice.delta.reasoning_content {
                        if !reasoning.is_empty() {
                            self.pending.push_back(StreamEvent::Reasoning(reasoning));
                        }
                    }
                    if let Some(calls) = choice.delta.tool_calls {
                        for call in calls {
                            self.accumulator.push(&call);
                        }
                    }
                    if let Some(reason) = choice.finish_reason {
                        for call in self.accumulator.drain() {
                            self.pending.push_back(StreamEvent::ToolCall(call));
                        }
                        self.pending.push_back(StreamEvent::Finished { reason });
                        self.finished = true;
                    }
                }
            }
            Err(e) => {
                self.pending.push_back(StreamEvent::Text(format!(
                    "\n\n[unparsable stream chunk: {e}]"
                )));
            }
        }
    }
}

pub struct ChatStream {
    inner: BoxStream<'static, Result<StreamEvent>>,
}

impl ChatStream {
    pub async fn next_event(&mut self) -> Option<Result<StreamEvent>> {
        self.inner.next().await
    }

    pub fn into_inner(self) -> BoxStream<'static, Result<StreamEvent>> {
        self.inner
    }
}

impl futures_util::Stream for ChatStream {
    type Item = Result<StreamEvent>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationReport {
    pub ok: bool,
    pub latency_ms: u64,
    pub model_count: usize,
    pub sample_models: Vec<String>,
    pub base_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Minimal HTTP/1.1 test server: each request gets the next scripted reply.
    struct MockServer {
        addr: std::net::SocketAddr,
        hits: Arc<AtomicUsize>,
    }

    impl MockServer {
        async fn start(replies: Vec<(u16, Vec<(&'static str, &'static str)>, String)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let hits = Arc::new(AtomicUsize::new(0));
            let counter = hits.clone();
            tokio::spawn(async move {
                let mut replies = replies.into_iter();
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        return;
                    };
                    let reply = replies.next();
                    let counter = counter.clone();
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 65536];
                        let mut read = 0;
                        // Read headers (and any body that arrived with them).
                        loop {
                            let Ok(n) = socket.read(&mut buf[read..]).await else {
                                return;
                            };
                            if n == 0 {
                                break;
                            }
                            read += n;
                            let text = String::from_utf8_lossy(&buf[..read]).to_string();
                            if let Some(idx) = text.find("\r\n\r\n") {
                                let headers = &text[..idx];
                                let len = headers
                                    .lines()
                                    .find_map(|l| {
                                        l.to_ascii_lowercase()
                                            .strip_prefix("content-length:")
                                            .and_then(|v| v.trim().parse::<usize>().ok())
                                    })
                                    .unwrap_or(0);
                                if read >= idx + 4 + len {
                                    break;
                                }
                            }
                            if read == buf.len() {
                                break;
                            }
                        }
                        counter.fetch_add(1, Ordering::SeqCst);
                        match reply {
                            Some((status, headers, body)) => {
                                let mut response = format!("HTTP/1.1 {status} OK\r\n");
                                for (k, v) in headers {
                                    response.push_str(&format!("{k}: {v}\r\n"));
                                }
                                response.push_str(&format!("content-length: {}\r\n", body.len()));
                                response.push_str("connection: close\r\n\r\n");
                                response.push_str(&body);
                                let _ = socket.write_all(response.as_bytes()).await;
                            }
                            None => {
                                let body = "{\"error\":{\"message\":\"no scripted reply\"}}";
                                let response = format!(
                                    "HTTP/1.1 500 Internal Server Error\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                                    body.len()
                                );
                                let _ = socket.write_all(response.as_bytes()).await;
                            }
                        }
                        let _ = socket.flush().await;
                    });
                }
            });
            Self { addr, hits }
        }

        fn url(&self) -> String {
            format!("http://{}/v1", self.addr)
        }

        fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }
    }

    fn client(server: &MockServer, retries: u32) -> TerRouterClient {
        TerRouterClient::new(ClientOptions {
            base_url: server.url(),
            api_key: "tr_live_test".into(),
            request_timeout_secs: 15,
            max_retries: retries,
        })
        .unwrap()
    }

    #[test]
    fn rejects_empty_key() {
        let err = TerRouterClient::new(ClientOptions {
            base_url: "https://api.terrouter.com/v1".into(),
            api_key: "  ".into(),
            request_timeout_secs: 30,
            max_retries: 1,
        });
        assert!(matches!(err, Err(TerError::MissingApiKey)));
    }

    #[tokio::test]
    async fn lists_models() {
        let body = r#"{"object":"list","data":[
            {"id":"openai/gpt-5","object":"model","owned_by":"openai","context_length":400000},
            {"id":"anthropic/claude-sonnet-4.5","object":"model","owned_by":"anthropic"}
        ]}"#;
        let server = MockServer::start(vec![(
            200,
            vec![("content-type", "application/json")],
            body.to_string(),
        )])
        .await;
        let models = client(&server, 0).list_models().await.unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "anthropic/claude-sonnet-4.5");
        assert_eq!(models[1].context_length, Some(400000));
    }

    #[tokio::test]
    async fn maps_unauthorized() {
        let body = r#"{"error":{"message":"Invalid API key","type":"authentication_error"}}"#;
        let server = MockServer::start(vec![(
            401,
            vec![("content-type", "application/json")],
            body.to_string(),
        )])
        .await;
        let err = client(&server, 3).list_models().await.unwrap_err();
        assert!(matches!(err, TerError::Unauthorized { status: 401 }));
        assert_eq!(server.hits(), 1, "401 must not be retried");
    }

    #[tokio::test]
    async fn retries_server_errors_then_succeeds() {
        let server = MockServer::start(vec![
            (503, vec![], r#"{"error":{"message":"upstream busy"}}"#.to_string()),
            (200, vec![], r#"{"data":[{"id":"a/b"}]}"#.to_string()),
        ])
        .await;
        let models = client(&server, 2).list_models().await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(server.hits(), 2);
    }

    #[tokio::test]
    async fn streams_text_and_usage() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\" Roblox\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\n\n",
            "data: [DONE]\n\n"
        );
        let server = MockServer::start(vec![(
            200,
            vec![("content-type", "text/event-stream")],
            sse.to_string(),
        )])
        .await;
        let mut stream = client(&server, 0)
            .chat_stream(ChatRequest::new("m", vec![ChatMessage::user("hi")]))
            .await
            .unwrap();
        let mut text = String::new();
        let mut usage = None;
        let mut finished = false;
        while let Some(event) = stream.next_event().await {
            match event.unwrap() {
                StreamEvent::Text(t) => text.push_str(&t),
                StreamEvent::Usage(u) => usage = Some(u),
                StreamEvent::Finished { reason } => {
                    finished = true;
                    assert_eq!(reason, "stop");
                }
                _ => {}
            }
        }
        assert_eq!(text, "Hello Roblox");
        assert!(finished);
        assert_eq!(usage.unwrap().total_tokens, 7);
    }

    #[tokio::test]
    async fn streams_tool_calls() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"script_read\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"game.Workspace.Part\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n"
        );
        let server = MockServer::start(vec![(
            200,
            vec![("content-type", "text/event-stream")],
            sse.to_string(),
        )])
        .await;
        let mut stream = client(&server, 0)
            .chat_stream(ChatRequest::new("m", vec![ChatMessage::user("read")]))
            .await
            .unwrap();
        let mut calls = Vec::new();
        while let Some(event) = stream.next_event().await {
            if let StreamEvent::ToolCall(call) = event.unwrap() {
                calls.push(call);
            }
        }
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "script_read");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "game.Workspace.Part");
    }

    #[tokio::test]
    async fn non_streaming_completion_returns_usage() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#;
        let server = MockServer::start(vec![(200, vec![], body.to_string())]).await;
        let (resp, usage) = client(&server, 0)
            .chat_once(ChatRequest::new("m", vec![ChatMessage::user("hi")]))
            .await
            .unwrap();
        assert_eq!(
            resp.choices[0].message.as_ref().unwrap().content.as_deref(),
            Some("ok")
        );
        assert_eq!(usage.unwrap().total_tokens, 4);
    }

    #[test]
    fn status_mapping_is_specific() {
        assert!(matches!(
            map_status(401, "", None),
            TerError::Unauthorized { .. }
        ));
        assert!(matches!(
            map_status(429, "", Some(9)),
            TerError::RateLimited {
                retry_after_secs: 9
            }
        ));
        match map_status(400, r#"{"error":{"message":"bad model","type":"invalid_request"}}"#, None) {
            TerError::Api { status, message } => {
                assert_eq!(status, 400);
                assert!(message.contains("bad model"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn backoff_grows_but_is_bounded() {
        assert_eq!(backoff_delay(0).as_millis(), 400);
        assert_eq!(backoff_delay(1).as_millis(), 800);
        assert_eq!(backoff_delay(20).as_millis(), 12_800);
    }
}
