//! The AI development agent: project-aware prompting, streaming answers, MCP
//! tool calling, confirmations and usage accounting.

use crate::ai::types::*;
use crate::ai::TerRouterClient;
use crate::error::{Result, TerError};
use crate::logging::{ActivityLog, LogSource};
use crate::luau::Diagnostic;
use crate::mcp::{CallToolResult, McpTool};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Anything that can list and call Studio tools. `McpManager` implements this;
/// tests use a deterministic fake.
#[async_trait]
pub trait ToolProvider: Send + Sync {
    async fn list_tools(&self) -> Vec<McpTool>;
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<CallToolResult>;
    /// Short human description, e.g. "connected to Roblox Studio (29 tools)".
    async fn studio_status(&self) -> String;
}

#[async_trait]
impl ToolProvider for crate::mcp::McpManager {
    async fn list_tools(&self) -> Vec<McpTool> {
        crate::mcp::McpManager::tools(self).await
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<CallToolResult> {
        crate::mcp::McpManager::call_tool(self, name, arguments).await
    }

    async fn studio_status(&self) -> String {
        let status = crate::mcp::McpManager::status(self).await;
        match &status {
            crate::mcp::McpStatus::Connected {
                server,
                tool_count,
                studio_id,
                ..
            } => format!(
                "connected to {server} ({tool_count} tools{})",
                studio_id
                    .as_ref()
                    .map(|id| format!(", studio {id}"))
                    .unwrap_or_default()
            ),
            other => other.label().to_lowercase(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveFileContext {
    pub path: String,
    pub contents: String,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProjectContext {
    pub project_name: Option<String>,
    pub project_path: Option<String>,
    /// Relative file paths (already trimmed by the caller).
    pub files: Vec<String>,
    pub active_file: Option<ActiveFileContext>,
    pub studio_status: String,
    pub templates: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmationRequest {
    pub id: String,
    pub tool: String,
    pub arguments: Value,
    pub reason: String,
}

#[async_trait]
pub trait ConfirmationGate: Send + Sync {
    async fn confirm(&self, request: &ConfirmationRequest) -> bool;
}

pub struct AlwaysApprove;
pub struct AlwaysDeny;

#[async_trait]
impl ConfirmationGate for AlwaysApprove {
    async fn confirm(&self, _request: &ConfirmationRequest) -> bool {
        true
    }
}

#[async_trait]
impl ConfirmationGate for AlwaysDeny {
    async fn confirm(&self, _request: &ConfirmationRequest) -> bool {
        false
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AgentEvent {
    Delta {
        text: String,
    },
    Reasoning {
        text: String,
    },
    Round {
        round: u32,
    },
    ToolStart {
        id: String,
        name: String,
        arguments: Value,
        significant: bool,
    },
    ToolEnd {
        id: String,
        name: String,
        ok: bool,
        summary: String,
        #[serde(rename = "durationMs")]
        duration_ms: u64,
    },
    Confirmation {
        request: ConfirmationRequest,
    },
    Usage {
        prompt: u64,
        completion: u64,
        total: u64,
    },
    Done {
        text: String,
        #[serde(rename = "toolCalls")]
        tool_calls: u32,
        rounds: u32,
    },
    Failed {
        code: &'static str,
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: String,
    pub temperature: f32,
    pub max_tokens: u32,
    pub max_tool_rounds: u32,
    pub require_confirmation: bool,
    pub custom_instructions: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            temperature: 0.3,
            max_tokens: 8192,
            max_tool_rounds: 12,
            require_confirmation: true,
            custom_instructions: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentOutcome {
    pub text: String,
    pub usage: Usage,
    pub tool_calls: u32,
    pub rounds: u32,
    /// Tools that were proposed but declined by the user.
    pub denied: Vec<String>,
}

pub struct Agent {
    client: Arc<TerRouterClient>,
    tools: Arc<dyn ToolProvider>,
    log: ActivityLog,
    config: AgentConfig,
    events: broadcast::Sender<AgentEvent>,
}

impl Agent {
    pub fn new(
        client: Arc<TerRouterClient>,
        tools: Arc<dyn ToolProvider>,
        log: ActivityLog,
        config: AgentConfig,
    ) -> Self {
        let (events, _) = broadcast::channel(512);
        Self {
            client,
            tools,
            log,
            config,
            events,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Runs one user turn. `history` is mutated so the conversation continues
    /// across turns; the system prompt is rebuilt each turn from live context.
    pub async fn run(
        &self,
        history: &mut Vec<ChatMessage>,
        user_text: &str,
        context: &ProjectContext,
        gate: Arc<dyn ConfirmationGate>,
    ) -> Result<AgentOutcome> {
        if self.config.model.trim().is_empty() {
            return Err(TerError::NoModelSelected);
        }
        let tools = self.tools.list_tools().await;
        let tool_map: HashMap<String, String> = tools
            .iter()
            .map(|t| (t.model_name(), t.name.clone()))
            .collect();

        let mut messages: Vec<ChatMessage> = Vec::with_capacity(history.len() + 4);
        messages.push(ChatMessage::system(self.system_prompt(context, &tools)));
        messages.extend(history.iter().cloned());
        messages.push(ChatMessage::user(user_text));
        history.push(ChatMessage::user(user_text));

        let tool_defs = if tools.is_empty() {
            None
        } else {
            Some(
                tools
                    .iter()
                    .map(|t| {
                        ToolDefinition::function(
                            t.model_name(),
                            tool_description(t),
                            normalize_schema(&t.input_schema),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        };

        let mut outcome = AgentOutcome::default();
        let mut total_usage = Usage::default();

        for round in 1..=self.config.max_tool_rounds {
            outcome.rounds = round;
            let _ = self.events.send(AgentEvent::Round { round });

            let mut request = ChatRequest::new(self.config.model.clone(), messages.clone());
            request.stream = true;
            request.temperature = self.config.temperature;
            request.max_tokens = Some(self.config.max_tokens);
            request.tools = tool_defs.clone();
            if tool_defs.is_some() {
                request.tool_choice = Some("auto".into());
            }

            self.log.debug(
                LogSource::Ai,
                format!(
                    "round {round}: {} messages, {} tools",
                    messages.len(),
                    tool_defs.as_ref().map(|t| t.len()).unwrap_or(0)
                ),
            );

            let mut stream = self.client.chat_stream(request).await?;
            let mut text = String::new();
            let mut calls: Vec<ToolCall> = Vec::new();
            let mut finish_reason = String::new();

            while let Some(event) = stream.next_event().await {
                match event? {
                    StreamEvent::Text(chunk) => {
                        text.push_str(&chunk);
                        let _ = self.events.send(AgentEvent::Delta { text: chunk });
                    }
                    StreamEvent::Reasoning(chunk) => {
                        let _ = self.events.send(AgentEvent::Reasoning { text: chunk });
                    }
                    StreamEvent::Usage(usage) => {
                        total_usage.accumulate(&usage);
                        let _ = self.events.send(AgentEvent::Usage {
                            prompt: total_usage.prompt_tokens,
                            completion: total_usage.completion_tokens,
                            total: total_usage.total_tokens,
                        });
                    }
                    StreamEvent::ToolCall(call) => calls.push(call),
                    StreamEvent::Finished { reason } => finish_reason = reason,
                }
            }

            outcome.text = text.clone();
            outcome.usage = total_usage.clone();

            if calls.is_empty() {
                history.push(ChatMessage::assistant(Some(text.clone())));
                let _ = self.events.send(AgentEvent::Done {
                    text: text.clone(),
                    tool_calls: outcome.tool_calls,
                    rounds: round,
                });
                return Ok(outcome);
            }

            self.log.info(
                LogSource::Ai,
                format!(
                    "model requested {} tool call(s) ({finish_reason})",
                    calls.len()
                ),
            );
            let assistant = ChatMessage::assistant_tool_calls(
                if text.trim().is_empty() { None } else { Some(text.clone()) },
                calls.clone(),
            );
            messages.push(assistant.clone());
            history.push(assistant);

            for call in calls {
                outcome.tool_calls += 1;
                let model_name = call.function.name.clone();
                let real_name = tool_map
                    .get(&model_name)
                    .cloned()
                    .unwrap_or_else(|| model_name.clone());

                let arguments = match serde_json::from_str::<Value>(&call.function.arguments) {
                    Ok(Value::Object(map)) => Value::Object(map),
                    Ok(other) => other,
                    Err(e) => {
                        let message = format!(
                            "Your arguments for `{real_name}` were not valid JSON ({e}). Call the tool again with a JSON object."
                        );
                        self.push_tool_result(&mut messages, history, &call, &real_name, message, false);
                        continue;
                    }
                };

                let known = tools.iter().any(|t| t.name == real_name);
                if !known {
                    let message = format!(
                        "There is no MCP tool named `{real_name}`. Only use tools from the provided list."
                    );
                    self.push_tool_result(&mut messages, history, &call, &real_name, message, false);
                    continue;
                }

                let significant = crate::mcp::protocol::is_significant_tool(&real_name);
                let id = call.id.clone();
                let _ = self.events.send(AgentEvent::ToolStart {
                    id: id.clone(),
                    name: real_name.clone(),
                    arguments: arguments.clone(),
                    significant,
                });

                if significant && self.config.require_confirmation {
                    let request = ConfirmationRequest {
                        id: id.clone(),
                        tool: real_name.clone(),
                        arguments: arguments.clone(),
                        reason: confirmation_reason(&real_name),
                    };
                    let _ = self.events.send(AgentEvent::Confirmation {
                        request: request.clone(),
                    });
                    self.log.info(
                        LogSource::Ai,
                        format!("awaiting approval for {real_name}"),
                    );
                    if !gate.confirm(&request).await {
                        outcome.denied.push(real_name.clone());
                        let message = format!(
                            "The user declined this {real_name} operation. Do not repeat it; explain what you would have changed and ask how to proceed."
                        );
                        self.push_tool_result(&mut messages, history, &call, &real_name, message, false);
                        let _ = self.events.send(AgentEvent::ToolEnd {
                            id,
                            name: real_name,
                            ok: false,
                            summary: "declined by user".into(),
                            duration_ms: 0,
                        });
                        continue;
                    }
                }

                let started = std::time::Instant::now();
                match self.tools.call_tool(&real_name, arguments).await {
                    Ok(result) => {
                        let text = result.to_model_text(24_000);
                        let ok = !result.is_error;
                        self.push_tool_result(&mut messages, history, &call, &real_name, text.clone(), ok);
                        let _ = self.events.send(AgentEvent::ToolEnd {
                            id,
                            name: real_name,
                            ok,
                            summary: first_lines(&text, 400),
                            duration_ms: started.elapsed().as_millis() as u64,
                        });
                    }
                    Err(e) => {
                        let message = format!(
                            "The MCP tool `{real_name}` failed: {e}. Fix the call or choose another tool."
                        );
                        self.push_tool_result(&mut messages, history, &call, &real_name, message, false);
                        let _ = self.events.send(AgentEvent::ToolEnd {
                            id,
                            name: real_name,
                            ok: false,
                            summary: e.to_string(),
                            duration_ms: started.elapsed().as_millis() as u64,
                        });
                    }
                }
            }
        }

        let message = format!(
            "Stopped after {} tool rounds to avoid an unbounded loop. Ask me to continue if you want more changes.",
            self.config.max_tool_rounds
        );
        self.log.warn(LogSource::Ai, message.clone());
        let _ = self.events.send(AgentEvent::Failed {
            code: "tool_round_limit",
            message: message.clone(),
        });
        Ok(outcome)
    }

    fn push_tool_result(
        &self,
        messages: &mut Vec<ChatMessage>,
        history: &mut Vec<ChatMessage>,
        call: &ToolCall,
        name: &str,
        content: String,
        _ok: bool,
    ) {
        let message = ChatMessage::tool_result(call.id.clone(), name.to_string(), content);
        messages.push(message.clone());
        history.push(message);
    }

    /// System prompt: Roblox expertise, live project context and the rule that
    /// tools must be discovered, never invented.
    pub fn system_prompt(&self, context: &ProjectContext, tools: &[McpTool]) -> String {
        let mut prompt = String::new();
        prompt.push_str(
            "You are TerCode, an expert Roblox developer embedded in the user's desktop app. \
You build and modify real Roblox games by calling Roblox Studio tools through the Model Context Protocol (MCP), and you write production-quality Luau.\n\n\
## Ground rules\n\
- Only call tools that appear in the provided tool list. Never invent a tool name, argument or Studio path.\n\
- Roblox Studio tool calls require a `studio_id`. The app injects the active Studio automatically; if a tool returns an error about studios, call `list_roblox_studios` and retry with the correct id.\n\
- Read before you write: inspect the tree (`search_game_tree`, `inspect_instance`), read scripts (`script_read`), then change them (`multi_edit`).\n\
- Verify after you write: re-read the changed script or use `get_console_output` and report what you actually observed.\n\
- Prefer several small, verifiable tool calls over one huge speculative one.\n\
- When the user asks for a full game, plan it first (one short plan), then execute step by step, and summarise what exists in Studio at the end.\n\
- If Studio is not connected, say so plainly and generate the Luau files in the project instead of pretending to modify Studio.\n\n\
## Luau quality bar\n\
- `--!strict` where practical, typed public functions, no deprecated globals (`wait`, `spawn`, `delay`) — use `task.*`.\n\
- Server-authoritative gameplay: validate RemoteEvents on the server, never trust client input.\n\
- Use `CollectionService` tags, `Attributes`, and `ReplicatedStorage` modules for shared config.\n\
- DataStore access wrapped in `pcall` with retries; never store Instances.\n\
- Name things clearly, keep functions small, comment intent (not syntax).\n\
- UI: scale-aware (`UDim2.fromScale`), `ResetOnSpawn = false` for HUDs, readable contrast.\n\n\
## Style\n\
- Be concise and concrete. Use short paragraphs and bullet lists.\n\
- Mention tool results that matter (paths, instance counts, errors).\n\
- Never claim a change succeeded unless a tool result shows it.\n",
        );

        prompt.push_str("\n## Live context\n");
        prompt.push_str(&format!(
            "- Studio: {}\n",
            if context.studio_status.is_empty() {
                "unknown".to_string()
            } else {
                context.studio_status.clone()
            }
        ));
        prompt.push_str(&format!(
            "- MCP tools available: {}\n",
            if tools.is_empty() {
                "none (Studio not connected)".to_string()
            } else {
                tools
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ));
        if let Some(name) = &context.project_name {
            prompt.push_str(&format!("- Open project: {name}\n"));
        }
        if let Some(path) = &context.project_path {
            prompt.push_str(&format!("- Project folder: {path}\n"));
        }
        if !context.files.is_empty() {
            let listed: Vec<&str> = context.files.iter().take(120).map(|s| s.as_str()).collect();
            prompt.push_str(&format!("- Files: {}\n", listed.join(", ")));
        }
        if !context.templates.is_empty() {
            prompt.push_str(&format!(
                "- Game templates available: {}\n",
                context.templates.join(", ")
            ));
        }
        if let Some(active) = &context.active_file {
            prompt.push_str(&format!(
                "\n### Active file: {}\n```lua\n{}\n```\n",
                active.path,
                truncate(&active.contents, 12_000)
            ));
            if !active.diagnostics.is_empty() {
                prompt.push_str("Diagnostics in this file:\n");
                for diagnostic in active.diagnostics.iter().take(20) {
                    prompt.push_str(&format!(
                        "- line {} [{:?}] {}\n",
                        diagnostic.line, diagnostic.severity, diagnostic.message
                    ));
                }
            }
        }
        if !self.config.custom_instructions.trim().is_empty() {
            prompt.push_str("\n## User preferences\n");
            prompt.push_str(self.config.custom_instructions.trim());
            prompt.push('\n');
        }
        prompt
    }
}

fn tool_description(tool: &McpTool) -> String {
    let mut description = tool.description.clone().unwrap_or_default();
    if description.trim().is_empty() {
        description = tool
            .title
            .clone()
            .unwrap_or_else(|| format!("Roblox Studio tool {}", tool.name));
    }
    description.push_str("\n\nProvided by the connected Roblox Studio MCP server.");
    description
}

/// Guarantees the schema is an object schema, which OpenAI-compatible gateways
/// require for function calling.
fn normalize_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(map) if !map.is_empty() => {
            let mut clone = map.clone();
            clone
                .entry("type")
                .or_insert_with(|| Value::String("object".into()));
            if !clone.contains_key("properties") {
                clone.insert("properties".into(), json!({}));
            }
            Value::Object(clone)
        }
        _ => json!({ "type": "object", "properties": {} }),
    }
}

fn confirmation_reason(tool: &str) -> String {
    match tool {
        "multi_edit" => "This will modify scripts inside your Roblox Studio project.".into(),
        "save_place" => "This saves a new version of your place.".into(),
        "run_code" | "execute_lua" => {
            "This runs Luau code directly in Studio and can change anything in the place.".into()
        }
        "start_stop_play" | "run_script_in_play_mode" => {
            "This starts or stops a playtest in Studio.".into()
        }
        other if other.starts_with("generate_") => {
            "This generates and uploads an asset to your Roblox account.".into()
        }
        other if other.contains("delete") => "This deletes content from your place.".into(),
        _ => "This makes a significant change to your Roblox project.".into(),
    }
}

fn first_lines(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max).collect();
    out.push('…');
    out
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect::<String>() + "\n… [truncated]"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::ActivityLog;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    struct FakeTools {
        tools: Vec<McpTool>,
        calls: Mutex<Vec<(String, Value)>>,
    }

    impl FakeTools {
        fn new() -> Self {
            Self {
                tools: vec![
                    McpTool {
                        name: "script_read".into(),
                        title: None,
                        description: Some("Read a script".into()),
                        input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
                        annotations: None,
                    },
                    McpTool {
                        name: "multi_edit".into(),
                        title: None,
                        description: Some("Edit a script".into()),
                        input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
                        annotations: None,
                    },
                ],
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ToolProvider for FakeTools {
        async fn list_tools(&self) -> Vec<McpTool> {
            self.tools.clone()
        }
        async fn call_tool(&self, name: &str, arguments: Value) -> Result<CallToolResult> {
            self.calls
                .lock()
                .unwrap()
                .push((name.to_string(), arguments));
            Ok(CallToolResult {
                content: vec![crate::mcp::protocol::ContentBlock::Text {
                    text: "print(\"hello\")".into(),
                }],
                is_error: false,
                structured: None,
            })
        }
        async fn studio_status(&self) -> String {
            "connected to Fake Studio (2 tools)".into()
        }
    }

    /// Streams scripted SSE responses, one per request.
    struct ScriptedGateway {
        addr: std::net::SocketAddr,
        hits: Arc<AtomicUsize>,
    }

    impl ScriptedGateway {
        async fn start(chunks: Vec<String>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let hits = Arc::new(AtomicUsize::new(0));
            let counter = hits.clone();
            tokio::spawn(async move {
                let mut replies = chunks.into_iter();
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        return;
                    };
                    let reply = replies.next().unwrap_or_else(|| "data: [DONE]\n\n".into());
                    let counter = counter.clone();
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 65536];
                        let mut read = 0;
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
                                let len = text[..idx]
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
                        }
                        counter.fetch_add(1, Ordering::SeqCst);
                        let response = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                            reply.len(),
                            reply
                        );
                        let _ = socket.write_all(response.as_bytes()).await;
                        let _ = socket.flush().await;
                    });
                }
            });
            Self { addr, hits }
        }

        fn client(&self) -> Arc<TerRouterClient> {
            Arc::new(
                TerRouterClient::new(crate::ai::ClientOptions {
                    base_url: format!("http://{}/v1", self.addr),
                    api_key: "tr_live_test".into(),
                    request_timeout_secs: 15,
                    max_retries: 0,
                })
                .unwrap(),
            )
        }

        fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }
    }

    fn config() -> AgentConfig {
        AgentConfig {
            model: "anthropic/claude-sonnet-4.5".into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn answers_without_tools() {
        let gateway = ScriptedGateway::start(vec![concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Here is your \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"game.\"},\"finish_reason\":\"stop\"}]}\n\n"
        )
        .to_string()])
        .await;
        let tools = Arc::new(FakeTools::new());
        let agent = Agent::new(gateway.client(), tools, ActivityLog::new(), config());
        let mut history = Vec::new();
        let outcome = agent
            .run(
                &mut history,
                "build me a game",
                &ProjectContext::default(),
                Arc::new(AlwaysApprove),
            )
            .await
            .unwrap();
        assert_eq!(outcome.text, "Here is your game.");
        assert_eq!(outcome.tool_calls, 0);
        assert_eq!(history.len(), 2);
    }

    #[tokio::test]
    async fn executes_tool_call_then_finishes() {
        let first = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"script_read\",\"arguments\":\"{\\\"path\\\":\\\"game.ServerScriptService.Main\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n"
        )
        .to_string();
        let second = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"The script prints hello.\"},\"finish_reason\":\"stop\"}]}\n\n"
        )
        .to_string();
        let gateway = ScriptedGateway::start(vec![first, second]).await;
        let tools = Arc::new(FakeTools::new());
        let agent = Agent::new(
            gateway.client(),
            tools.clone(),
            ActivityLog::new(),
            config(),
        );
        let mut rx = agent.subscribe();
        let mut history = Vec::new();
        let outcome = agent
            .run(
                &mut history,
                "what does Main do?",
                &ProjectContext::default(),
                Arc::new(AlwaysApprove),
            )
            .await
            .unwrap();
        assert_eq!(outcome.tool_calls, 1);
        assert_eq!(outcome.text, "The script prints hello.");
        let calls = tools.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "script_read");
        assert_eq!(calls[0].1["path"], "game.ServerScriptService.Main");
        // History must contain user, assistant(tool_calls), tool, assistant.
        assert_eq!(history.len(), 4);
        assert_eq!(gateway.hits(), 2);

        let mut saw_tool_start = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, AgentEvent::ToolStart { .. }) {
                saw_tool_start = true;
            }
        }
        assert!(saw_tool_start);
    }

    #[tokio::test]
    async fn significant_tool_respects_denial() {
        let first = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_9\",\"function\":{\"name\":\"multi_edit\",\"arguments\":\"{\\\"path\\\":\\\"game.ServerScriptService.Main\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n"
        )
        .to_string();
        let second = "data: {\"choices\":[{\"delta\":{\"content\":\"Okay, I did not change anything.\"},\"finish_reason\":\"stop\"}]}\n\n".to_string();
        let gateway = ScriptedGateway::start(vec![first, second]).await;
        let tools = Arc::new(FakeTools::new());
        let agent = Agent::new(
            gateway.client(),
            tools.clone(),
            ActivityLog::new(),
            config(),
        );
        let mut history = Vec::new();
        let outcome = agent
            .run(
                &mut history,
                "edit main",
                &ProjectContext::default(),
                Arc::new(AlwaysDeny),
            )
            .await
            .unwrap();
        assert_eq!(outcome.denied, vec!["multi_edit".to_string()]);
        assert!(tools.calls.lock().unwrap().is_empty());
        assert!(outcome.text.contains("did not change"));
    }

    #[tokio::test]
    async fn invalid_tool_arguments_are_reported_to_the_model() {
        let first = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_2\",\"function\":{\"name\":\"script_read\",\"arguments\":\"{not json\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n"
        )
        .to_string();
        let second = "data: {\"choices\":[{\"delta\":{\"content\":\"Fixing that.\"},\"finish_reason\":\"stop\"}]}\n\n".to_string();
        let gateway = ScriptedGateway::start(vec![first, second]).await;
        let tools = Arc::new(FakeTools::new());
        let agent = Agent::new(
            gateway.client(),
            tools.clone(),
            ActivityLog::new(),
            config(),
        );
        let mut history = Vec::new();
        agent
            .run(
                &mut history,
                "read",
                &ProjectContext::default(),
                Arc::new(AlwaysApprove),
            )
            .await
            .unwrap();
        assert!(tools.calls.lock().unwrap().is_empty());
        let tool_message = history
            .iter()
            .find(|m| m.role == "tool")
            .expect("tool message");
        assert!(tool_message.content.as_deref().unwrap().contains("not valid JSON"));
    }

    #[tokio::test]
    async fn unknown_tool_is_refused() {
        let first = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c\",\"function\":{\"name\":\"make_magic\",\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n"
        )
        .to_string();
        let second = "data: {\"choices\":[{\"delta\":{\"content\":\"Sorry.\"},\"finish_reason\":\"stop\"}]}\n\n".to_string();
        let gateway = ScriptedGateway::start(vec![first, second]).await;
        let tools = Arc::new(FakeTools::new());
        let agent = Agent::new(gateway.client(), tools.clone(), ActivityLog::new(), config());
        let mut history = Vec::new();
        agent
            .run(
                &mut history,
                "x",
                &ProjectContext::default(),
                Arc::new(AlwaysApprove),
            )
            .await
            .unwrap();
        assert!(tools.calls.lock().unwrap().is_empty());
        assert!(history
            .iter()
            .any(|m| m.content.as_deref().unwrap_or("").contains("no MCP tool named")));
    }

    #[tokio::test]
    async fn missing_model_is_rejected_before_any_request() {
        let gateway = ScriptedGateway::start(vec![]).await;
        let agent = Agent::new(
            gateway.client(),
            Arc::new(FakeTools::new()),
            ActivityLog::new(),
            AgentConfig::default(),
        );
        let mut history = Vec::new();
        let err = agent
            .run(
                &mut history,
                "hi",
                &ProjectContext::default(),
                Arc::new(AlwaysApprove),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, TerError::NoModelSelected));
        assert_eq!(gateway.hits(), 0);
    }

    #[test]
    fn system_prompt_embeds_context_and_rules() {
        let gateway_tools = FakeTools::new();
        let client = Arc::new(
            TerRouterClient::new(crate::ai::ClientOptions {
                base_url: "https://api.terrouter.com/v1".into(),
                api_key: "tr_live_x".into(),
                request_timeout_secs: 30,
                max_retries: 0,
            })
            .unwrap(),
        );
        let agent = Agent::new(
            client,
            Arc::new(gateway_tools),
            ActivityLog::new(),
            AgentConfig {
                custom_instructions: "Prefer DataStore v2".into(),
                ..config()
            },
        );
        let context = ProjectContext {
            project_name: Some("Tycoon".into()),
            project_path: Some("C:/games/tycoon".into()),
            files: vec!["src/server/Main.server.luau".into()],
            active_file: Some(ActiveFileContext {
                path: "src/server/Main.server.luau".into(),
                contents: "print('x')".into(),
                diagnostics: vec![],
            }),
            studio_status: "connected".into(),
            templates: vec!["obby".into()],
        };
        let tools = vec![McpTool {
            name: "script_read".into(),
            title: None,
            description: None,
            input_schema: json!({"type":"object"}),
            annotations: None,
        }];
        let prompt = agent.system_prompt(&context, &tools);
        assert!(prompt.contains("studio_id"));
        assert!(prompt.contains("Tycoon"));
        assert!(prompt.contains("src/server/Main.server.luau"));
        assert!(prompt.contains("Prefer DataStore v2"));
        assert!(prompt.contains("script_read"));
    }

    #[test]
    fn schema_normalisation_fills_missing_type() {
        let schema = normalize_schema(&json!({"properties":{"a":{"type":"string"}}}));
        assert_eq!(schema["type"], "object");
        let empty = normalize_schema(&json!(null));
        assert_eq!(empty["type"], "object");
        assert!(empty["properties"].is_object());
    }
}
