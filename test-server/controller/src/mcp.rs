use std::time::Duration;

use kurbis_ai_protocol::{MAX_MCP_ARGUMENT_BYTES, McpActionRequest, McpActionResult, write_json};
use serde_json::{Value, json};
use tokio::{
    fs,
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    time,
};
use uuid::Uuid;

use crate::paths::RuntimePaths;

const MAX_LINE_BYTES: usize = 64 * 1024;
const ACTION_TIMEOUT: Duration = Duration::from_secs(20);
const MIN_ACTION_INTERVAL: Duration = Duration::from_millis(250);
const MAX_ACTIONS_PER_JOB: usize = 128;

pub async fn serve_stdio(paths: RuntimePaths, job_id: Uuid) -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut stdout = tokio::io::BufWriter::new(stdout);
    let mut action_count = 0usize;
    let mut last_action: Option<time::Instant> = None;

    loop {
        let frame = read_bounded_frame(&mut reader, MAX_LINE_BYTES).await?;
        let line = match frame {
            Frame::Eof => break,
            Frame::TooLong => {
                write_response(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": {"code": -32600, "message": "MCP message exceeds size limit"}
                    }),
                )
                .await?;
                continue;
            }
            Frame::InvalidUtf8 => {
                write_response(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": {"code": -32700, "message": "MCP message is not valid UTF-8"}
                    }),
                )
                .await?;
                continue;
            }
            Frame::Line(line) => line,
        };
        let request: Value = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(_) => {
                write_response(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": {"code": -32700, "message": "Invalid JSON-RPC message"}
                    }),
                )
                .await?;
                continue;
            }
        };
        // A JSON-RPC notification has no id and must never invoke a tool. In
        // particular, a notification-shaped tools/call must not mutate the
        // live server and must not consume an action budget.
        if request.get("id").is_some()
            && request.get("method").and_then(Value::as_str) == Some("tools/call")
        {
            if action_count >= MAX_ACTIONS_PER_JOB {
                write_response(
                    &mut stdout,
                    rpc_error(
                        request.get("id").cloned(),
                        -32002,
                        "This job has reached its server-action limit.",
                    ),
                )
                .await?;
                continue;
            }
            if let Some(previous) = last_action {
                let remaining = MIN_ACTION_INTERVAL.saturating_sub(previous.elapsed());
                if !remaining.is_zero() {
                    time::sleep(remaining).await;
                }
            }
            last_action = Some(time::Instant::now());
            action_count += 1;
        }
        if let Some(response) = handle_message(&paths, job_id, &request).await? {
            write_response(&mut stdout, response).await?;
        }
    }
    Ok(())
}

enum Frame {
    Eof,
    Line(String),
    TooLong,
    InvalidUtf8,
}

async fn read_bounded_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> std::io::Result<Frame> {
    let mut bytes = Vec::with_capacity(limit.min(4096));
    let mut too_long = false;
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            if bytes.is_empty() && !too_long {
                return Ok(Frame::Eof);
            }
            return if too_long {
                Ok(Frame::TooLong)
            } else {
                Ok(match String::from_utf8(bytes) {
                    Ok(line) => Frame::Line(line),
                    Err(_) => Frame::InvalidUtf8,
                })
            };
        }
        if let Some(newline) = chunk.iter().position(|byte| *byte == b'\n') {
            if !too_long {
                if bytes.len().saturating_add(newline) > limit {
                    too_long = true;
                } else {
                    bytes.extend_from_slice(&chunk[..newline]);
                }
            }
            reader.consume(newline + 1);
            if too_long {
                return Ok(Frame::TooLong);
            }
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            return Ok(match String::from_utf8(bytes) {
                Ok(line) => Frame::Line(line),
                Err(_) => Frame::InvalidUtf8,
            });
        }
        if !too_long {
            let remaining = limit.saturating_sub(bytes.len());
            if chunk.len() > remaining {
                bytes.extend_from_slice(&chunk[..remaining]);
                too_long = true;
            } else {
                bytes.extend_from_slice(chunk);
            }
        }
        let consumed = chunk.len();
        reader.consume(consumed);
    }
}

async fn handle_message(
    paths: &RuntimePaths,
    job_id: Uuid,
    request: &Value,
) -> anyhow::Result<Option<Value>> {
    let method = request.get("method").and_then(Value::as_str);
    let id = request.get("id").cloned();
    let Some(method) = method else {
        return Ok(Some(rpc_error(id, -32600, "Expected a JSON-RPC method")));
    };
    if method.starts_with("notifications/") || request.get("id").is_none() {
        return Ok(None);
    }
    let result = match method {
        "initialize" => {
            let requested_version = request
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2025-03-26");
            Ok(json!({
                "protocolVersion": requested_version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "kurbis-ai", "version": env!("CARGO_PKG_VERSION")}
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => call_tool(paths, job_id, request).await,
        _ => Err((-32601, "Method not found".to_owned())),
    };
    let response = match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": result}),
        Err((code, message)) => rpc_error(id, code, &message),
    };
    Ok(Some(response))
}

async fn call_tool(
    paths: &RuntimePaths,
    job_id: Uuid,
    request: &Value,
) -> Result<Value, (i32, String)> {
    let name = request
        .pointer("/params/name")
        .and_then(Value::as_str)
        .ok_or_else(|| (-32602, "Missing tool name".to_owned()))?;
    let arguments = request
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let encoded = serde_json::to_vec(&arguments)
        .map_err(|error| (-32603, format!("Invalid tool arguments: {error}")))?;
    if encoded.len() > MAX_MCP_ARGUMENT_BYTES {
        return Err((-32602, "Tool arguments exceed size limit".into()));
    }
    let id = Uuid::new_v4();
    let action = McpActionRequest {
        id,
        job_id,
        tool: name.to_owned(),
        arguments,
    };
    let request_path = paths.mcp_requests().join(format!("{id}.json"));
    let result_path = paths.mcp_results().join(format!("{id}.json"));
    if let Ok(metadata) = fs::symlink_metadata(&request_path).await
        && (metadata.file_type().is_symlink() || !metadata.file_type().is_file())
    {
        return Err((-32603, "MCP request path is not a regular file".into()));
    }
    write_json(&request_path, &action)
        .map_err(|error| (-32603, format!("Could not queue server action: {error}")))?;
    eprintln!(
        "MCP action queued: job={} tool={} request={}",
        job_id, name, id
    );

    let deadline = time::Instant::now() + ACTION_TIMEOUT;
    loop {
        if fs::try_exists(&result_path).await.unwrap_or(false) {
            let metadata = fs::symlink_metadata(&result_path)
                .await
                .map_err(|error| (-32603, format!("Could not inspect server result: {error}")))?;
            if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
                return Err((-32603, "MCP result path is not a regular file".into()));
            }
            let bytes = fs::read(&result_path)
                .await
                .map_err(|error| (-32603, format!("Could not read server result: {error}")))?;
            let _ = fs::remove_file(&result_path).await;
            let result: McpActionResult = serde_json::from_slice(&bytes)
                .map_err(|error| (-32603, format!("Invalid server result: {error}")))?;
            if result.id != id || result.job_id != job_id {
                return Err((-32603, "Server returned an unrelated action result".into()));
            }
            return Ok(json!({
                "content": [{"type": "text", "text": result.output}],
                "isError": !result.success
            }));
        }
        if time::Instant::now() >= deadline {
            let _ = fs::remove_file(&request_path).await;
            return Err((
                -32001,
                "Timed out waiting for the Pumpkin server action".into(),
            ));
        }
        time::sleep(Duration::from_millis(50)).await;
    }
}

async fn write_response<W: AsyncWrite + Unpin>(
    output: &mut W,
    response: Value,
) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(&response)?;
    if bytes.len() > MAX_LINE_BYTES {
        let id = response.get("id").cloned();
        let fallback = rpc_error(id, -32003, "MCP response exceeds size limit");
        let bytes = serde_json::to_vec(&fallback)?;
        output.write_all(&bytes).await?;
    } else {
        output.write_all(&bytes).await?;
    }
    output.write_all(b"\n").await?;
    output.flush().await?;
    Ok(())
}

fn rpc_error(id: Option<Value>, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": {"code": code, "message": message}
    })
}

fn tools() -> Value {
    json!([
        {
            "name": "get_server_status",
            "description": "Read current Pumpkin server status, performance, players, worlds, time, and weather.",
            "inputSchema": {"type":"object", "properties":{}, "additionalProperties":false}
        },
        {
            "name": "list_players",
            "description": "List online players with UUID, world, coordinates, gamemode, permission level, and ping.",
            "inputSchema": {"type":"object", "properties":{}, "additionalProperties":false}
        },
        {
            "name": "list_worlds",
            "description": "List loaded worlds and their dimensions, time, weather, and player counts.",
            "inputSchema": {"type":"object", "properties":{}, "additionalProperties":false}
        },
        {
            "name": "list_entities",
            "description": "Inspect loaded entities in a world (defaults to the requesting operator's world).",
            "inputSchema": {"type":"object", "properties":{
                "world":{"type":"string"}, "limit":{"type":"integer","minimum":1,"maximum":200}
            }, "additionalProperties":false}
        },
        {
            "name": "spawn_entity",
            "description": "Immediately spawn a Minecraft entity in a loaded world. Coordinates default to the requesting operator's position.",
            "inputSchema": {"type":"object", "properties":{
                "entity_type":{"type":"string"}, "world":{"type":"string"},
                "x":{"type":"number"}, "y":{"type":"number"}, "z":{"type":"number"}
            }, "required":["entity_type"], "additionalProperties":false}
        },
        {
            "name": "run_server_command",
            "description": "Immediately dispatch one vanilla or installed-plugin command as the authorized operator. This acts directly on the live server; no confirmation is requested.",
            "inputSchema": {"type":"object", "properties":{
                "command":{"type":"string","minLength":1,"maxLength":2048}
            }, "required":["command"], "additionalProperties":false}
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn implements_stdio_initialize_tool_list_and_notification_without_extra_output() {
        let temp = tempfile::tempdir().unwrap();
        let paths = RuntimePaths::new(temp.path().join("runtime"));
        paths.create_dirs().await.unwrap();
        let initialize = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}});
        let result = handle_message(&paths, Uuid::new_v4(), &initialize)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result["result"]["protocolVersion"], "2025-03-26");
        let list = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
        let result = handle_message(&paths, Uuid::new_v4(), &list)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result["result"]["tools"].as_array().unwrap().len(), 6);
        let notification = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        assert!(
            handle_message(&paths, Uuid::new_v4(), &notification)
                .await
                .unwrap()
                .is_none()
        );
        let notification_call = json!({
            "jsonrpc":"2.0",
            "method":"tools/call",
            "params":{"name":"run_server_command","arguments":{"command":"say must-not-run"}}
        });
        assert!(
            handle_message(&paths, Uuid::new_v4(), &notification_call)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            fs::read_dir(paths.mcp_requests())
                .await
                .unwrap()
                .next_entry()
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn bounded_framing_discards_oversized_frames_and_preserves_utf8_lines() {
        let mut reader = BufReader::new(std::io::Cursor::new(
            b"123456789\n\xF0\x9F\x8E\x83\n".to_vec(),
        ));
        assert!(matches!(
            read_bounded_frame(&mut reader, 4).await.unwrap(),
            Frame::TooLong
        ));
        assert!(matches!(
            read_bounded_frame(&mut reader, 16).await.unwrap(),
            Frame::Line(line) if line == "🎃"
        ));
    }

    #[tokio::test]
    async fn rejects_unknown_methods_and_missing_tool_names() {
        let temp = tempfile::tempdir().unwrap();
        let paths = RuntimePaths::new(temp.path().join("runtime"));
        paths.create_dirs().await.unwrap();
        let result = handle_message(&paths, Uuid::new_v4(), &json!({"id":4,"method":"unknown"}))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result["error"]["code"], -32601);
        let result = handle_message(
            &paths,
            Uuid::new_v4(),
            &json!({"id":5,"method":"tools/call","params":{}}),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result["error"]["code"], -32602);
    }
}
