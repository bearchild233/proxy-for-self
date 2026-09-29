//! 跨账号只重放完整的可移植输入，不携带账号专属句柄和加密推理缓存。
use serde_json::{Value, json};
use std::io::{self, Write};

use super::{CodexResponsesRequest, OpenAiSessionState};

// 给 Redis 64 KiB pin 上限留出身份与连接元数据空间，超限交由客户端重放。
const MAX_HISTORY_BYTES: usize = 48 * 1024;

struct SizeBudget(usize);
impl Write for SizeBudget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other("history limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn fits(value: &impl serde::Serialize) -> bool {
    serde_json::to_writer(SizeBudget(MAX_HISTORY_BYTES), value).is_ok()
}

pub(super) fn request_history(
    request: &CodexResponsesRequest,
    previous: Option<&OpenAiSessionState>,
) -> Option<Vec<Value>> {
    // conversation 及 item_reference 依赖上游持久状态，不能声称持有完整历史。
    if request.body().contains_key("conversation") {
        return None;
    }
    let mut history = if request.previous_response_id().is_some() {
        previous?.replay_history.clone()?
    } else {
        Vec::new()
    };
    let value = request.body().get("input")?;
    if !fits(value) {
        return None;
    }
    let input = match value {
        Value::String(text) => vec![json!({"role":"user", "content":text})],
        Value::Array(items) => items.clone(),
        _ => return None,
    };
    history.extend(portable_items(&input)?);
    bounded(history)
}

pub(super) fn complete_history(
    history: Option<Vec<Value>>,
    output: &[Value],
) -> Option<Vec<Value>> {
    let mut history = history?;
    history.extend(portable_items(output)?);
    bounded(history)
}

fn bounded(history: Vec<Value>) -> Option<Vec<Value>> {
    fits(&history).then_some(history)
}

fn portable_items(items: &[Value]) -> Option<Vec<Value>> {
    if !fits(&items) {
        return None;
    }
    let mut result = Vec::new();
    for item in items {
        let mut object = item.as_object()?.clone();
        match object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message")
        {
            // 推理缓存与账号绑定，仅保留用户可见对话及已完成的工具调用记录。
            "reasoning" => continue,
            "message" => match object.get("content")? {
                Value::String(_) => {}
                Value::Array(content) => {
                    for part in content {
                        if !matches!(
                            part.get("type").and_then(Value::as_str),
                            Some("input_text" | "output_text")
                        ) || part.get("text").and_then(Value::as_str).is_none()
                            || part.get("annotations").is_some_and(|value| {
                                value.as_array().is_none_or(|items| !items.is_empty())
                            })
                        {
                            return None;
                        }
                    }
                }
                _ => return None,
            },
            "function_call" | "custom_tool_call" => {
                object.get("call_id").and_then(Value::as_str)?;
            }
            "function_call_output" | "custom_tool_call_output" => {
                if object.get("call_id").and_then(Value::as_str).is_none()
                    || object.get("output").and_then(Value::as_str).is_none()
                {
                    return None;
                }
            }
            // 文件、图片、内置工具、压缩上下文等不猜测跨账号可移植性。
            _ => return None,
        }
        object.remove("id");
        object.remove("status");
        result.push(Value::Object(object));
    }
    Some(result)
}
