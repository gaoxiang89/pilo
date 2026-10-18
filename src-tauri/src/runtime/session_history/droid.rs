use serde_json::Value;

use super::types::{
    ConversationEventDto, SessionHistory, SessionHistoryModel, SessionHistoryStats, TurnCompletion,
};

pub(super) fn is_droid_session_path(path: &str) -> bool {
    path.replace('\\', "/").contains("/.factory/sessions/")
}

/// Sidecar `<id>.settings.json` next to a Factory session JSONL.
pub(super) fn droid_settings_path(path: &str) -> Option<String> {
    path.strip_suffix(".jsonl")
        .map(|stem| format!("{stem}.settings.json"))
}

/// Model and reasoning effort from the sidecar, in Pilo's history shape.
/// Droid spells "no reasoning" as `none` or `off`; unknown efforts are dropped.
pub(super) fn apply_droid_settings(history: &mut SessionHistory, bytes: &[u8]) {
    let Ok(settings) = serde_json::from_slice::<Value>(bytes) else {
        return;
    };
    if let Some(id) = settings
        .get("model")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        history.model = Some(SessionHistoryModel {
            provider: "factory".to_owned(),
            id: id.to_owned(),
        });
    }
    let level = match settings.get("reasoningEffort").and_then(Value::as_str) {
        Some("none" | "off") => Some("off"),
        Some(level @ ("minimal" | "low" | "medium" | "high" | "xhigh" | "max")) => Some(level),
        _ => None,
    };
    if let Some(level) = level {
        history.thinking_level = Some(level.to_owned());
    }
}

pub(super) fn parse_droid_history(bytes: &[u8]) -> SessionHistory {
    let mut events = Vec::new();
    let mut name = None;
    let mut assistant_open = false;
    let mut saw_assistant_text = false;
    let mut stats = SessionHistoryStats::default();
    for line in bytes.split(|byte| *byte == b'\n') {
        let line = trim_ascii(line);
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if is_header(&value) {
            name = name.or_else(|| text_field(&value, &["title", "name"]));
            continue;
        }
        push_record(
            &mut events,
            &mut stats,
            &mut assistant_open,
            &mut saw_assistant_text,
            &value,
        );
    }
    if assistant_open {
        events.push(ConversationEventDto::AssistantTurnEnd {
            stop_reason: None,
            error_message: None,
            completion: TurnCompletion::Complete,
            timestamp_ms: None,
        });
    }
    SessionHistory {
        events,
        name,
        source_message_count: stats.total_messages,
        stats,
        ..SessionHistory::default()
    }
}

fn push_record(
    events: &mut Vec<ConversationEventDto>,
    stats: &mut SessionHistoryStats,
    assistant_open: &mut bool,
    saw_assistant_text: &mut bool,
    value: &Value,
) {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if kind == "agent_turn_outcome" {
        if !*saw_assistant_text && let Some(text) = failure_reason(value) {
            push_assistant_text(events, stats, assistant_open, saw_assistant_text, text);
        }
        return;
    }
    let role = value
        .get("role")
        .or_else(|| value.pointer("/message/role"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if let Some(parts) = value.pointer("/message/content").and_then(Value::as_array)
        && (role == "user" || role == "assistant")
    {
        push_content_message(
            events,
            stats,
            assistant_open,
            saw_assistant_text,
            value,
            &role,
            parts,
        );
        return;
    }
    if role == "user" || kind == "user" {
        *saw_assistant_text = false;
        close_assistant(events, assistant_open);
        let text = record_text(value).unwrap_or_default();
        events.push(ConversationEventDto::UserMessageStart {
            text,
            images: Vec::new(),
            timestamp_ms: None,
            source_entry_id: None,
        });
        stats.user_messages += 1;
        stats.total_messages += 1;
        return;
    }
    if matches!(kind.as_str(), "tool_call" | "tool_use") {
        let Some(tool_call_id) = tool_id(value) else {
            return;
        };
        let Some(tool_name) = tool_name(value) else {
            return;
        };
        open_assistant(events, assistant_open);
        events.push(ConversationEventDto::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args: value
                .get("args")
                .or_else(|| value.get("input"))
                .or_else(|| value.get("parameters"))
                .cloned()
                .unwrap_or(Value::Null),
            timestamp_ms: None,
            source_entry_id: None,
            source_content_index: None,
        });
        stats.tool_calls += 1;
        return;
    }
    if matches!(kind.as_str(), "tool_result" | "tool_end") {
        let Some(tool_call_id) = tool_id(value) else {
            return;
        };
        open_assistant(events, assistant_open);
        events.push(ConversationEventDto::ToolExecutionEnd {
            tool_call_id,
            tool_name: tool_name(value).unwrap_or_else(|| "tool".to_owned()),
            result: value
                .get("result")
                .or_else(|| value.get("value"))
                .or_else(|| value.get("output"))
                .cloned()
                .unwrap_or(Value::Null),
            is_error: value
                .get("isError")
                .or_else(|| value.get("is_error"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            timestamp_ms: None,
            source_entry_id: None,
        });
        stats.tool_results += 1;
        return;
    }
    if role == "assistant" || kind == "assistant" || kind == "message" {
        let Some(text) = record_text(value) else {
            return;
        };
        push_assistant_text(events, stats, assistant_open, saw_assistant_text, text);
    }
}

fn push_assistant_text(
    events: &mut Vec<ConversationEventDto>,
    stats: &mut SessionHistoryStats,
    assistant_open: &mut bool,
    saw_assistant_text: &mut bool,
    text: String,
) {
    open_assistant(events, assistant_open);
    events.push(ConversationEventDto::AssistantTextDelta {
        delta: text,
        timestamp_ms: None,
        source_entry_id: None,
        source_content_index: None,
    });
    close_assistant(events, assistant_open);
    stats.assistant_messages += 1;
    stats.total_messages += 1;
    *saw_assistant_text = true;
}

fn failure_reason(value: &Value) -> Option<String> {
    let reason = value.get("reason").and_then(Value::as_str).unwrap_or("");
    let lower = reason.to_ascii_lowercase();
    if lower.contains("fail")
        || lower.contains("error")
        || lower.contains("cancel")
        || lower.contains("auth")
    {
        let text = reason.replace('_', " ");
        (!text.is_empty()).then_some(text)
    } else {
        None
    }
}

fn push_content_message(
    events: &mut Vec<ConversationEventDto>,
    stats: &mut SessionHistoryStats,
    assistant_open: &mut bool,
    saw_assistant_text: &mut bool,
    value: &Value,
    role: &str,
    parts: &[Value],
) {
    if role == "user" {
        let visibility = value
            .pointer("/message/visibility")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let text = parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<String>();
        if visibility == "user_only" {
            if !text.is_empty() {
                push_assistant_text(events, stats, assistant_open, saw_assistant_text, text);
            }
        } else if visibility != "llm_only" && !text.is_empty() {
            *saw_assistant_text = false;
            close_assistant(events, assistant_open);
            events.push(ConversationEventDto::UserMessageStart {
                text,
                images: Vec::new(),
                timestamp_ms: None,
                source_entry_id: None,
            });
            stats.user_messages += 1;
            stats.total_messages += 1;
        }
        for part in parts {
            if part.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let Some(tool_call_id) = tool_id(part) else {
                continue;
            };
            open_assistant(events, assistant_open);
            events.push(ConversationEventDto::ToolExecutionEnd {
                tool_call_id,
                tool_name: tool_name(part).unwrap_or_else(|| "tool".to_owned()),
                result: part
                    .get("content")
                    .or_else(|| part.get("result"))
                    .cloned()
                    .unwrap_or(Value::Null),
                is_error: part
                    .get("is_error")
                    .or_else(|| part.get("isError"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                timestamp_ms: None,
                source_entry_id: None,
            });
            stats.tool_results += 1;
        }
        return;
    }

    for part in parts {
        match part.get("type").and_then(Value::as_str).unwrap_or_default() {
            "thinking" => {
                let Some(text) = part.get("thinking").and_then(Value::as_str) else {
                    continue;
                };
                if text.is_empty() {
                    continue;
                }
                open_assistant(events, assistant_open);
                events.push(ConversationEventDto::AssistantThinkingStart {
                    timestamp_ms: None,
                    source_entry_id: None,
                    source_content_index: None,
                });
                events.push(ConversationEventDto::AssistantThinkingDelta {
                    delta: text.to_owned(),
                    timestamp_ms: None,
                    source_entry_id: None,
                    source_content_index: None,
                });
                events.push(ConversationEventDto::AssistantThinkingEnd {
                    timestamp_ms: None,
                    source_entry_id: None,
                    source_content_index: None,
                });
            }
            "text" => {
                let Some(text) = part.get("text").and_then(Value::as_str) else {
                    continue;
                };
                if text.is_empty() {
                    continue;
                }
                open_assistant(events, assistant_open);
                events.push(ConversationEventDto::AssistantTextDelta {
                    delta: text.to_owned(),
                    timestamp_ms: None,
                    source_entry_id: None,
                    source_content_index: None,
                });
                *saw_assistant_text = true;
            }
            "tool_use" => {
                let Some(tool_call_id) = tool_id(part) else {
                    continue;
                };
                let Some(tool_name) = tool_name(part) else {
                    continue;
                };
                open_assistant(events, assistant_open);
                events.push(ConversationEventDto::ToolExecutionStart {
                    tool_call_id,
                    tool_name,
                    args: part
                        .get("input")
                        .or_else(|| part.get("args"))
                        .cloned()
                        .unwrap_or(Value::Null),
                    timestamp_ms: None,
                    source_entry_id: None,
                    source_content_index: None,
                });
                stats.tool_calls += 1;
            }
            _ => {}
        }
    }
    if *assistant_open {
        close_assistant(events, assistant_open);
        stats.assistant_messages += 1;
        stats.total_messages += 1;
    }
}

fn open_assistant(events: &mut Vec<ConversationEventDto>, assistant_open: &mut bool) {
    if *assistant_open {
        return;
    }
    *assistant_open = true;
    events.push(ConversationEventDto::AssistantMessageStart {
        timestamp_ms: None,
        source_entry_id: None,
    });
}

fn close_assistant(events: &mut Vec<ConversationEventDto>, assistant_open: &mut bool) {
    if !*assistant_open {
        return;
    }
    *assistant_open = false;
    events.push(ConversationEventDto::AssistantTurnEnd {
        stop_reason: None,
        error_message: None,
        completion: TurnCompletion::Complete,
        timestamp_ms: None,
    });
}

fn is_header(value: &Value) -> bool {
    matches!(
        value.get("type").and_then(Value::as_str),
        Some("session" | "session_start" | "metadata" | "session_meta")
    ) || value.get("sessionId").is_some()
        && value.get("role").is_none()
        && value.get("message").is_none()
}

fn record_text(value: &Value) -> Option<String> {
    text_field(value, &["text", "finalText", "content", "message"])
        .or_else(|| value.pointer("/message/content").and_then(collect_text))
}

fn text_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(collect_text))
}

fn collect_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Array(blocks) => {
            let text = blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<String>();
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn tool_id(value: &Value) -> Option<String> {
    ["toolCallId", "toolId", "tool_use_id", "id"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn tool_name(value: &Value) -> Option<String> {
    ["toolName", "name", "tool"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map(|index| index + 1)
        .unwrap_or(start);
    &bytes[start..end]
}

#[cfg(test)]
mod tests {
    use super::{apply_droid_settings, droid_settings_path, parse_droid_history};

    #[test]
    fn settings_sidecar_supplies_model_and_reasoning() {
        assert_eq!(
            droid_settings_path("/home/me/.factory/sessions/-app/abc.jsonl").as_deref(),
            Some("/home/me/.factory/sessions/-app/abc.settings.json")
        );
        let mut history = parse_droid_history(b"");
        apply_droid_settings(
            &mut history,
            br#"{"model":"claude-sonnet-5-5","reasoningEffort":"none"}"#,
        );
        let model = history.model.as_ref().unwrap();
        assert_eq!(
            (model.provider.as_str(), model.id.as_str()),
            ("factory", "claude-sonnet-5-5")
        );
        assert_eq!(history.thinking_level.as_deref(), Some("off"));
    }

    #[test]
    fn history_projects_user_assistant_and_tools() {
        let body = br#"{"type":"session","sessionId":"abc","title":"Demo"}
{"type":"message","role":"user","text":"hello"}
{"type":"message","role":"assistant","text":"hi"}
{"type":"tool_call","name":"read","tool_use_id":"call-1","input":{"path":"a"}}
{"type":"tool_result","toolId":"call-1","name":"read","value":"ok"}
"#;
        let history = parse_droid_history(body);
        assert_eq!(history.name.as_deref(), Some("Demo"));
        assert!(history.events.iter().any(|event| matches!(event, super::ConversationEventDto::UserMessageStart { text, .. } if text == "hello")));
        assert_eq!(history.stats.tool_calls, 1);
        assert_eq!(history.stats.tool_results, 1);
    }

    #[test]
    fn factory_jsonl_keeps_visible_text_and_tool_parts() {
        let body = br#"{"type":"session_start","id":"abc","title":"Demo","cwd":"/tmp/pilo-droid-probe"}
{"type":"message","message":{"role":"user","visibility":"llm_only","content":[{"type":"text","text":"hidden"}]}}
{"type":"message","message":{"role":"user","content":[{"type":"text","text":"pong"}]}}
{"type":"message","message":{"role":"user","visibility":"user_only","content":[{"type":"text","text":"login failed"}]}}
{"type":"agent_turn_outcome","reason":"model_authentication_failed","resultKind":"text"}
{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"ok"},{"type":"tool_use","id":"call-1","name":"read","input":{"path":"a"}}]}}
{"type":"message","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call-1","content":"file","is_error":false}]}}
"#;
        let history = parse_droid_history(body);
        assert_eq!(history.name.as_deref(), Some("Demo"));
        assert_eq!(history.stats.user_messages, 1);
        assert_eq!(history.stats.tool_calls, 1);
        assert_eq!(history.stats.tool_results, 1);
        assert!(history.events.iter().any(|event| matches!(
            event,
            super::ConversationEventDto::UserMessageStart { text, .. } if text == "pong"
        )));
        assert!(history.events.iter().any(|event| matches!(
            event,
            super::ConversationEventDto::AssistantTextDelta { delta, .. } if delta == "login failed"
        )));
        assert!(!history.events.iter().any(|event| matches!(
            event,
            super::ConversationEventDto::UserMessageStart { text, .. } if text == "hidden" || text == "login failed"
        )));
        assert!(!history.events.iter().any(|event| matches!(
            event,
            super::ConversationEventDto::AssistantTextDelta { delta, .. } if delta == "model authentication failed"
        )));
    }

    #[test]
    fn failed_turn_outcome_becomes_assistant_text_when_nothing_else_was_saved() {
        let body = br#"{"type":"session_start","id":"abc","title":"Demo"}
{"type":"message","message":{"role":"user","content":[{"type":"text","text":"pong"}]}}
{"type":"agent_turn_outcome","reason":"model_authentication_failed","resultKind":"text"}
"#;
        let history = parse_droid_history(body);
        assert!(history.events.iter().any(|event| matches!(
            event,
            super::ConversationEventDto::AssistantTextDelta { delta, .. } if delta == "model authentication failed"
        )));
    }
}
