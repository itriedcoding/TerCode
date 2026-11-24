//! Ter Router (OpenAI-compatible) wire types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn assistant(content: Option<String>) -> Self {
        Self {
            role: "assistant".into(),
            content,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn assistant_tool_calls(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: "assistant".into(),
            content,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            name: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            name: Some(name.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ToolCall {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default = "default_function")]
    pub kind: String,
    pub function: FunctionCall,
}

fn default_function() -> String {
    "function".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FunctionCall {
    #[serde(default)]
    pub name: String,
    /// JSON encoded as a string, exactly as the model emits it.
    #[serde(default)]
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionDefinition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolDefinition {
    pub fn function(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            kind: "function".into(),
            function: FunctionDefinition {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: bool,
    pub temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    /// Ask the gateway to include token usage on the final chunk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<Value>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            stream: true,
            temperature: 0.3,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            stream_options: Some(serde_json::json!({ "include_usage": true })),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

impl Usage {
    pub fn accumulate(&mut self, other: &Usage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct StreamDelta {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<StreamToolCall>>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct StreamToolCall {
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<StreamFunction>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct StreamFunction {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct StreamChoice {
    #[serde(default)]
    pub delta: StreamDelta,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct StreamChunk {
    #[serde(default)]
    pub choices: Vec<StreamChoice>,
    #[serde(default)]
    pub usage: Option<Usage>,
    #[serde(default)]
    pub error: Option<ApiErrorBody>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ApiErrorBody {
    #[serde(default)]
    pub message: String,
    #[serde(default, rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorEnvelope {
    pub error: ApiErrorBody,
}

/// Event surfaced to the UI while a completion streams.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Text(String),
    Reasoning(String),
    /// A complete tool call, emitted once its argument stream finishes.
    ToolCall(ToolCall),
    Usage(Usage),
    Finished { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub owned_by: Option<String>,
    #[serde(default)]
    pub context_length: Option<u64>,
    #[serde(default)]
    pub pricing: Option<Value>,
    #[serde(default)]
    pub capabilities: Option<Value>,
    /// Preserved so the UI can show provider-specific metadata.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelList {
    #[serde(default)]
    pub data: Vec<ModelInfo>,
}

/// Non-streaming response (used for validation and simple calls).
#[derive(Debug, Clone, Deserialize)]
pub struct ChatResponse {
    #[serde(default)]
    pub choices: Vec<ResponseChoice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponseChoice {
    #[serde(default)]
    pub message: Option<ChatMessage>,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

/// Accumulates streamed tool-call fragments into complete calls.
///
/// Models stream arguments in pieces keyed by `index`; some providers omit
/// `index` and repeat `id`. Both shapes are handled.
#[derive(Debug, Default)]
pub struct ToolCallAccumulator {
    pending: Vec<PartialToolCall>,
}

#[derive(Debug, Default)]
struct PartialToolCall {
    index: usize,
    id: String,
    name: String,
    arguments: String,
}

impl ToolCallAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, delta: &StreamToolCall) {
        let slot = if let Some(existing) = self
            .pending
            .iter_mut()
            .find(|p| p.index == delta.index && (!p.id.is_empty() || delta.id.is_none()))
        {
            existing
        } else if let Some(existing) = delta
            .id
            .as_ref()
            .and_then(|id| self.pending.iter_mut().find(|p| !p.id.is_empty() && &p.id == id))
        {
            existing
        } else {
            self.pending.push(PartialToolCall {
                index: delta.index,
                ..Default::default()
            });
            self.pending.last_mut().expect("just pushed")
        };

        if let Some(id) = &delta.id {
            if !id.is_empty() {
                slot.id = id.clone();
            }
        }
        if let Some(function) = &delta.function {
            if let Some(name) = &function.name {
                if !name.is_empty() {
                    slot.name.push_str(name);
                }
            }
            if let Some(args) = &function.arguments {
                slot.arguments.push_str(args);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Drain accumulated fragments into finished calls, assigning ids to
    /// providers that stream calls without one.
    pub fn drain(&mut self) -> Vec<ToolCall> {
        let mut out = Vec::new();
        for (n, partial) in self.pending.drain(..).enumerate() {
            if partial.name.is_empty() {
                continue;
            }
            let id = if partial.id.is_empty() {
                format!("call_{}_{}", partial.index, n)
            } else {
                partial.id
            };
            out.push(ToolCall {
                id,
                kind: "function".into(),
                function: FunctionCall {
                    name: partial.name,
                    arguments: if partial.arguments.trim().is_empty() {
                        "{}".to_string()
                    } else {
                        partial.arguments
                    },
                },
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulator_merges_argument_fragments() {
        let mut acc = ToolCallAccumulator::new();
        acc.push(&StreamToolCall {
            index: 0,
            id: Some("call_1".into()),
            function: Some(StreamFunction {
                name: Some("script_read".into()),
                arguments: Some("{\"path\":".into()),
            }),
        });
        acc.push(&StreamToolCall {
            index: 0,
            id: None,
            function: Some(StreamFunction {
                name: None,
                arguments: Some("\"game.Workspace\"}".into()),
            }),
        });
        let calls = acc.drain();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function.name, "script_read");
        assert_eq!(calls[0].function.arguments, "{\"path\":\"game.Workspace\"}");
        assert!(acc.is_empty());
    }

    #[test]
    fn accumulator_handles_two_parallel_calls() {
        let mut acc = ToolCallAccumulator::new();
        acc.push(&StreamToolCall {
            index: 0,
            id: Some("a".into()),
            function: Some(StreamFunction {
                name: Some("one".into()),
                arguments: Some("{}".into()),
            }),
        });
        acc.push(&StreamToolCall {
            index: 1,
            id: Some("b".into()),
            function: Some(StreamFunction {
                name: Some("two".into()),
                arguments: Some("{\"x\":1}".into()),
            }),
        });
        let calls = acc.drain();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.name, "one");
        assert_eq!(calls[1].function.name, "two");
    }

    #[test]
    fn accumulator_synthesises_missing_id_and_empty_args() {
        let mut acc = ToolCallAccumulator::new();
        acc.push(&StreamToolCall {
            index: 3,
            id: None,
            function: Some(StreamFunction {
                name: Some("get_console_output".into()),
                arguments: None,
            }),
        });
        let calls = acc.drain();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].id.starts_with("call_3"));
        assert_eq!(calls[0].function.arguments, "{}");
    }

    #[test]
    fn usage_accumulates() {
        let mut total = Usage::default();
        total.accumulate(&Usage {
            prompt_tokens: 10,
            completion_tokens: 4,
            total_tokens: 14,
        });
        total.accumulate(&Usage {
            prompt_tokens: 1,
            completion_tokens: 2,
            total_tokens: 3,
        });
        assert_eq!(total.total_tokens, 17);
    }

    #[test]
    fn tool_message_serialises_without_nulls() {
        let msg = ChatMessage::tool_result("call_1", "script_read", "print('hi')");
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "tool");
        assert_eq!(json["tool_call_id"], "call_1");
        assert!(json.get("tool_calls").is_none());
    }
}
