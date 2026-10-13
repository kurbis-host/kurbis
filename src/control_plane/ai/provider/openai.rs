//! OpenAI-compatible Chat Completions APIs, as Ollama, vLLM, LiteLLM, Open
//! WebUI and similar gateways serve them. Stored turns are translated to chat
//! messages on every call and each response back into transcript blocks.
//! These APIs return no reasoning that could be replayed, so none is stored.

use reqwest::header::{self, HeaderMap, HeaderName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json, value::RawValue};
use uuid::Uuid;

use crate::{contracts::ai::AiEffort, control_plane::ai::catalog::Tokens};

use super::{Block, ModelRequest, ModelResponse, Stop, WireFormat};

pub(super) struct ChatCompletions;

#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Tool>,
    max_tokens: u32,
    reasoning_effort: AiEffort,
}

#[derive(Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Serialize)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: FunctionCall,
}

#[derive(Serialize)]
struct FunctionCall {
    name: String,
    arguments: String,
}

/// A tool declaration as the transcript stores it.
#[derive(Deserialize)]
struct StoredTool {
    name: String,
    #[serde(default)]
    description: Option<String>,
    input_schema: Value,
}

#[derive(Serialize)]
struct Tool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: Function,
}

#[derive(Serialize)]
struct Function {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    parameters: Value,
}

impl From<StoredTool> for Tool {
    fn from(tool: StoredTool) -> Self {
        Self {
            kind: "function",
            function: Function {
                name: tool.name,
                description: tool.description,
                parameters: tool.input_schema,
            },
        }
    }
}

impl WireFormat for ChatCompletions {
    const PATH: &'static str = "/chat/completions";
    const AUTH: (HeaderName, &'static str) = (header::AUTHORIZATION, "Bearer ");

    fn headers(_request: &ModelRequest<'_>) -> HeaderMap {
        HeaderMap::new()
    }

    fn body(request: &ModelRequest<'_>) -> serde_json::Result<Vec<u8>> {
        let tools: Vec<StoredTool> = serde_json::from_str(request.tools.get())?;
        serde_json::to_vec(&Body {
            model: &request.model.id,
            messages: messages(request)?,
            tools: tools.into_iter().map(Tool::from).collect(),
            max_tokens: request.max_tokens,
            reasoning_effort: request.effort,
        })
    }

    fn response(request: &ModelRequest<'_>, body: &[u8]) -> Result<ModelResponse, String> {
        let completion: Completion =
            serde_json::from_slice(body).map_err(|error| error.to_string())?;
        let usage = completion.usage.unwrap_or_default();
        let choice = completion
            .choices
            .into_iter()
            .next()
            .ok_or("the response has no choices")?;
        let text = plain_text(choice.message.content);
        let calls = choice.message.tool_calls.unwrap_or_default();
        let called = !calls.is_empty();
        let mut blocks = Vec::with_capacity(calls.len() + 1);
        if !text.trim().is_empty() {
            blocks.push(json!({"type": "text", "text": text}));
        }
        for call in calls {
            blocks.push(json!({
                "type": "tool_use",
                // Tool results are keyed by call id within a job, and not
                // every server returns ids, let alone unique ones.
                "id": format!("call_{}", Uuid::new_v4().simple()),
                "name": call.function.name,
                "input": input(call.function.arguments),
            }));
        }
        let refused = choice
            .message
            .refusal
            .is_some_and(|refusal| !refusal.trim().is_empty());
        let stop = match choice.finish_reason.as_deref() {
            Some("length") => Stop::MaxTokens,
            _ if called => Stop::ToolUse,
            _ if refused => Stop::Refusal,
            Some("content_filter") => Stop::Refusal,
            None | Some("stop") => Stop::EndTurn,
            Some(other) => Stop::Other(Some(other.to_owned())),
        };
        let prompt = usage.prompt_tokens.unwrap_or(0);
        let cached = usage
            .prompt_tokens_details
            .and_then(|details| details.cached_tokens)
            .unwrap_or(0)
            .min(prompt);
        Ok(ModelResponse {
            content: RawValue::from_string(Value::Array(blocks).to_string())
                .map_err(|error| error.to_string())?,
            stop,
            // Gateways often report a variant of the model (`gpt-oss-120b-medium`),
            // so attempts are charged at the requested model's prices.
            attempts: vec![(
                request.model.id.clone(),
                Tokens {
                    input: prompt - cached,
                    output: usage.completion_tokens.unwrap_or(0),
                    cache_creation: 0,
                    cache_read: cached,
                },
            )],
        })
    }
}

/// The transcript as chat messages: thinking is dropped, tool calls become
/// function calls and each tool result its own `tool` message.
fn messages(request: &ModelRequest<'_>) -> serde_json::Result<Vec<Message>> {
    let mut messages = vec![Message::System {
        content: plain_text(serde_json::from_str(request.system.get())?),
    }];
    for turn in &request.messages {
        let mut texts = Vec::new();
        let mut tool_calls = Vec::new();
        for block in serde_json::from_str::<Vec<Block>>(turn.content.get())? {
            match block {
                Block::Text { text } => texts.push(text),
                Block::ToolUse { id, name, input } => tool_calls.push(ToolCall {
                    id,
                    kind: "function",
                    function: FunctionCall {
                        name,
                        arguments: arguments(input),
                    },
                }),
                Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let output = plain_text(content);
                    messages.push(Message::Tool {
                        tool_call_id: tool_use_id,
                        content: if is_error {
                            format!("Error: {output}")
                        } else {
                            output
                        },
                    });
                }
                Block::Thinking { .. } | Block::Other => {}
            }
        }
        let text = texts.join("\n");
        if turn.role == "assistant" {
            if !text.is_empty() || !tool_calls.is_empty() {
                messages.push(Message::Assistant {
                    content: (!text.is_empty()).then_some(text),
                    tool_calls,
                });
            }
        } else if !text.is_empty() {
            messages.push(Message::User { content: text });
        }
    }
    Ok(messages)
}

/// Text of a string or of a list of text blocks or parts.
fn plain_text(value: Value) -> String {
    match value {
        Value::String(text) => text,
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Function-call arguments of a stored tool input. Input kept as a string was
/// unreadable when it arrived and is replayed unchanged.
fn arguments(input: Value) -> String {
    match input {
        Value::String(raw) => raw,
        input => input.to_string(),
    }
}

/// Tool input from function-call arguments. Unreadable arguments are kept as
/// a string, which the tool check refuses.
fn input(arguments: Value) -> Value {
    match arguments {
        Value::Null => json!({}),
        Value::String(raw) if raw.trim().is_empty() => json!({}),
        Value::String(raw) => match serde_json::from_str(&raw) {
            Ok(Value::Object(object)) => Value::Object(object),
            _ => Value::String(raw),
        },
        other => other,
    }
}

#[derive(Deserialize)]
struct Completion {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Value,
    #[serde(default)]
    tool_calls: Option<Vec<ResponseToolCall>>,
    #[serde(default)]
    refusal: Option<String>,
}

#[derive(Deserialize)]
struct ResponseToolCall {
    function: CalledFunction,
}

#[derive(Deserialize)]
struct CalledFunction {
    name: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Default, Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use crate::{
        config::AiProvider,
        control_plane::ai::{catalog::Model, prompt, provider::Turn},
    };

    use super::super::tests::{model, raw};
    use super::*;

    fn gpt() -> Model {
        model(AiProvider::OpenAi, "gpt-oss-120b")
    }

    fn read(body: Value) -> Result<ModelResponse, String> {
        let model = gpt();
        let system = raw(r#""""#);
        let tools = raw("[]");
        let request = ModelRequest {
            model: &model,
            max_tokens: 16,
            effort: AiEffort::Low,
            system: &system,
            tools: &tools,
            messages: Vec::new(),
        };
        ChatCompletions::response(&request, body.to_string().as_bytes())
    }

    fn completion(message: Value, finish_reason: Value) -> Value {
        json!({
            "model": "gpt-oss-120b-medium",
            "choices": [{"index": 0, "message": message, "finish_reason": finish_reason}],
            "usage": {"prompt_tokens": 127, "completion_tokens": 43, "total_tokens": 170},
        })
    }

    #[test]
    fn the_transcript_becomes_chat_messages() {
        let model = gpt();
        let system = raw(&Value::from("Be brief.").to_string());
        let tools = raw(&prompt::tools().to_string());
        let question = raw(&prompt::user_turn("Steve", Uuid::nil(), "who is online?").to_string());
        let calls = raw(&json!([
            {"type": "thinking", "thinking": "Checking.", "signature": "s"},
            {"type": "text", "text": "Let me look."},
            {"type": "tool_use", "id": "call_1", "name": "list_players", "input": {}},
            {"type": "tool_use", "id": "call_2", "name": "run_server_command", "input": "{not json"},
        ])
        .to_string());
        let results = raw(&json!([
            prompt::tool_result("call_1", "Steve, Alex", false),
            prompt::tool_result("call_2", "Tool input must be a JSON object.", true),
        ])
        .to_string());
        let thinking_only = raw(r#"[{"type":"thinking","thinking":"","signature":"s"}]"#);
        let request = ModelRequest {
            model: &model,
            max_tokens: 8192,
            effort: AiEffort::High,
            system: &system,
            tools: &tools,
            messages: vec![
                Turn {
                    role: "user",
                    content: &question,
                },
                Turn {
                    role: "assistant",
                    content: &calls,
                },
                Turn {
                    role: "user",
                    content: &results,
                },
                Turn {
                    role: "assistant",
                    content: &thinking_only,
                },
            ],
        };
        let body: Value =
            serde_json::from_slice(&ChatCompletions::body(&request).unwrap()).unwrap();
        assert_eq!(body["model"], "gpt-oss-120b");
        assert_eq!(body["max_tokens"], 8192);
        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["tools"].as_array().unwrap().len(), 5);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][4]["function"]["name"], "run_server_command");
        assert_eq!(
            body["tools"][4]["function"]["parameters"]["required"],
            json!(["command"])
        );
        assert_eq!(
            body["messages"],
            json!([
                {"role": "system", "content": "Be brief."},
                {"role": "user", "content": format!("Steve (operator, UUID {}) asks:\nwho is online?", Uuid::nil())},
                {"role": "assistant", "content": "Let me look.", "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "list_players", "arguments": "{}"}},
                    {"id": "call_2", "type": "function", "function": {"name": "run_server_command", "arguments": "{not json"}},
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "Steve, Alex"},
                {"role": "tool", "tool_call_id": "call_2", "content": "Error: Tool input must be a JSON object."},
            ])
        );
    }

    #[test]
    fn tool_calls_become_blocks_with_ids_unique_to_the_job() {
        let response = read(completion(
            json!({"role": "assistant", "content": "", "tool_calls": [
                {"id": "toolu_vrtx_1", "type": "function", "function": {"name": "list_players", "arguments": "{\"reason\":\"asked\"}"}},
                {"id": "call_0", "type": "function", "function": {"name": "list_worlds", "arguments": ""}},
                {"type": "function", "function": {"name": "list_entities", "arguments": {"limit": 5}}},
                {"id": "call_0", "type": "function", "function": {"name": "run_server_command", "arguments": "{\"command\": \"time set"}},
            ]}),
            json!("tool_calls"),
        ))
        .unwrap();
        assert_eq!(response.stop, Stop::ToolUse);
        let blocks = response.blocks().unwrap();
        let mut ids = Vec::new();
        let inputs = blocks
            .into_iter()
            .map(|block| match block {
                Block::ToolUse { id, input, .. } => {
                    ids.push(id);
                    input
                }
                other => panic!("unexpected block {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            inputs,
            [
                json!({"reason": "asked"}),
                json!({}),
                json!({"limit": 5}),
                json!("{\"command\": \"time set"),
            ]
        );
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 4);
        assert!(
            ids.iter()
                .all(|id| id.starts_with("call_") && id.len() <= 64)
        );
        assert_eq!(
            response.attempts,
            [(
                "gpt-oss-120b".to_owned(),
                Tokens {
                    input: 127,
                    output: 43,
                    cache_creation: 0,
                    cache_read: 0
                }
            )]
        );
    }

    #[test]
    fn finish_reasons_map_to_stops() {
        let answer = read(completion(
            json!({"role": "assistant", "content": "Steve and Alex are online.", "tool_calls": null}),
            json!("stop"),
        ))
        .unwrap();
        assert_eq!(answer.stop, Stop::EndTurn);
        assert!(matches!(
            &answer.blocks().unwrap()[..],
            [Block::Text { text }] if text == "Steve and Alex are online."
        ));
        for (message, finish_reason, stop) in [
            (
                json!({"content": "Partial"}),
                json!("length"),
                Stop::MaxTokens,
            ),
            (
                json!({"content": "", "tool_calls": [{"function": {"name": "list_players", "arguments": "{}"}}]}),
                json!("length"),
                Stop::MaxTokens,
            ),
            (
                json!({"content": "", "tool_calls": [{"function": {"name": "list_players", "arguments": "{}"}}]}),
                json!("stop"),
                Stop::ToolUse,
            ),
            (
                json!({"content": null, "refusal": "No."}),
                json!("stop"),
                Stop::Refusal,
            ),
            (
                json!({"content": ""}),
                json!("content_filter"),
                Stop::Refusal,
            ),
            (
                json!({"content": [{"type": "text", "text": "Done."}]}),
                Value::Null,
                Stop::EndTurn,
            ),
            (
                json!({"content": "?"}),
                json!("tool_calls"),
                Stop::Other(Some("tool_calls".to_owned())),
            ),
        ] {
            assert_eq!(read(completion(message, finish_reason)).unwrap().stop, stop);
        }
        assert!(read(json!({"error": {"message": "all adapters failed"}})).is_err());
        assert!(read(json!([])).is_err());
    }

    #[test]
    fn cached_prompt_tokens_are_read_separately() {
        let mut body = completion(json!({"content": "ok"}), json!("stop"));
        body["usage"] = json!({"prompt_tokens": 100, "completion_tokens": 5,
                               "prompt_tokens_details": {"cached_tokens": 40}});
        let response = read(body).unwrap();
        assert_eq!(
            response.attempts[0].1,
            Tokens {
                input: 60,
                output: 5,
                cache_creation: 0,
                cache_read: 40
            }
        );
        let unmetered = read(json!({"choices": [{"message": {"content": "ok"}}]})).unwrap();
        assert_eq!(unmetered.attempts[0].1, Tokens::default());
    }
}
