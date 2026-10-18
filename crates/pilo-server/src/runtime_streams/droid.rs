use std::{
    collections::HashSet,
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use pilo_protocol::Envelope;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, ChildStdout},
    sync::{Mutex, oneshot},
};

use super::pi::{FactoryRuntime, PiProcess, read_rpc_line, stop_registered_pi};
use crate::ServerState;
use crate::environment::{cached_login_path, process_command, resolve_droid_launch};
use crate::output::ServerWriter;
use crate::session::{droid_session_dir_key, factory_sessions_dir};

const INIT_ID: &str = "pilo-acp-init";
const SESSION_ID: &str = "pilo-acp-session";
const AUTONOMY_ID: &str = "pilo-acp-autonomy";
const CONFIRMATION_MESSAGE: &str =
    "Droid asked for confirmation. This version does not show a confirmation dialog.";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DroidStartParams {
    stream_id: String,
    project: String,
    #[serde(default)]
    session_path: Option<String>,
    #[serde(default)]
    droid_executable: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DroidSendParams {
    stream_id: String,
    command: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DroidStopParams {
    stream_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DroidModelsParams {
    #[serde(default)]
    droid_executable: Option<String>,
    /// Also read every model's thinking choices (about 0.2 s per model).
    #[serde(default)]
    thinking: bool,
}

#[derive(Default)]
struct DroidTurn {
    assistant_started: bool,
    streamed_text: bool,
    settled: bool,
    chunked_messages: HashSet<String>,
    full_messages: HashSet<String>,
    started_tools: HashSet<String>,
    finished_tools: HashSet<String>,
}

enum ClassifiedLine {
    PromptResponse {
        id: String,
        success: bool,
        error: Option<String>,
        stop_reason: Option<String>,
    },
    ServerRequest {
        id: Value,
        method: String,
    },
    Events(Vec<Value>),
}

struct AcpHandshake<'a> {
    stdin: &'a Mutex<ChildStdin>,
    reader: &'a mut BufReader<ChildStdout>,
    writer: &'a ServerWriter,
    stream_id: &'a str,
    project: &'a str,
    resume_id: Option<&'a str>,
}

pub(crate) fn droid_exec_args(project: &str) -> Vec<String> {
    vec![
        "exec".to_owned(),
        "--output-format".to_owned(),
        "acp".to_owned(),
        "--auto".to_owned(),
        "medium".to_owned(),
        "--cwd".to_owned(),
        project.to_owned(),
    ]
}

pub(crate) async fn droid_start(
    state: &ServerState,
    params: DroidStartParams,
) -> Result<Value, String> {
    let login_path = cached_login_path(state).await;
    let launch = resolve_droid_launch(&login_path, params.droid_executable.as_deref());
    if !Path::new(&launch.program).is_file() {
        let requested = params
            .droid_executable
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("droid");
        return Err(format!(
            "Droid executable '{requested}' was not found in PATH"
        ));
    }

    let mut args = launch.prefix;
    args.extend(droid_exec_args(&params.project));
    let mut command = process_command(&launch.program, &args, &login_path);
    command
        .current_dir(&params.project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let old_output = state
        .pi_output_tasks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&params.stream_id);
    if let Some((_, tasks)) = old_output {
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
    }

    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start Droid: {error}"))?;
    let stdin = Arc::new(Mutex::new(
        child
            .stdin
            .take()
            .ok_or_else(|| "Droid stdin unavailable".to_owned())?,
    ));
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Droid stdout unavailable".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Droid stderr unavailable".to_owned())?;
    let resume_id = params
        .session_path
        .as_deref()
        .and_then(session_id_from_path);
    {
        let mut processes = state.pi.lock().await;
        processes.insert(
            params.stream_id.clone(),
            PiProcess {
                child,
                stdin: Arc::clone(&stdin),
                factory: None,
            },
        );
    }

    let mut reader = BufReader::new(stdout);
    let writer = state.writer.clone();
    let handshake_result = tokio::time::timeout(Duration::from_secs(30), async {
        acp_handshake(&mut AcpHandshake {
            stdin: &stdin,
            reader: &mut reader,
            writer: &writer,
            stream_id: &params.stream_id,
            project: &params.project,
            resume_id: resume_id.as_deref(),
        })
        .await
    })
    .await;

    let (runtime, session_result) = match handshake_result {
        Ok(Ok(started)) => started,
        Ok(Err(error)) => {
            let _ = stop_stream(state, &params.stream_id).await;
            return Err(error);
        }
        Err(_) => {
            let _ = stop_stream(state, &params.stream_id).await;
            return Err("timed out while starting Droid".to_owned());
        }
    };
    let session_id = runtime.session_id.clone();
    let runtime = Arc::new(StdMutex::new(runtime));
    {
        let mut processes = state.pi.lock().await;
        if let Some(process) = processes.get_mut(&params.stream_id) {
            process.factory = Some(Arc::clone(&runtime));
        }
    }
    let session_path =
        session_path_from_result(&session_result, &params.project, Some(session_id.as_str()));
    spawn_output_tasks(
        state,
        params.stream_id.clone(),
        stdin,
        reader,
        stderr,
        DroidTurn::default(),
        runtime,
    );
    Ok(json!({
        "started": true,
        "sessionId": session_id,
        "sessionPath": session_path,
    }))
}

pub(crate) async fn droid_send(
    state: &ServerState,
    params: DroidSendParams,
) -> Result<Value, String> {
    let command_type = params.command.get("type").and_then(Value::as_str);
    if command_type != Some("prompt") {
        return answer_model_command(state, &params).await;
    }
    let text = params
        .command
        .get("message")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| "Droid prompt text is required".to_owned())?;
    let id = params
        .command
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .unwrap_or("pilo-droid-prompt");
    let (stdin, runtime) = droid_io(state, &params.stream_id).await?;
    let session_id = {
        let runtime = runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        runtime.session_id.clone()
    };
    if session_id.is_empty() {
        return Err("Droid ACP session is not ready".to_owned());
    }
    let request = acp_request(
        id,
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{ "type": "text", "text": text }],
        }),
    );
    write_json_line(&stdin, &request).await?;
    let user_start = json!({
        "type": "message_start",
        "message": {
            "role": "user",
            "content": [{ "type": "text", "text": text }],
        },
    });
    forward_events(&state.writer, &params.stream_id, vec![user_start])
        .await
        .map_err(|_| format!("Droid stream '{}' event channel closed", params.stream_id))?;
    Ok(Value::Null)
}

pub(crate) async fn droid_stop(
    state: &ServerState,
    params: DroidStopParams,
) -> Result<Value, String> {
    stop_stream(state, &params.stream_id).await?;
    Ok(Value::Null)
}

/// Read the Droid model catalog before any Droid session exists.
///
/// `session/new` always writes a Factory session file, so the probe runs in a
/// scratch directory that no project watches and deletes that session after
/// the catalog is read. The catalog comes from the Factory account and user
/// settings, not the project, so the scratch cwd does not change it.
pub(crate) async fn droid_models(
    state: &ServerState,
    params: DroidModelsParams,
) -> Result<Value, String> {
    // Stay under the WebUI's 45 s request limit.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let login_path = cached_login_path(state).await;
    let launch = resolve_droid_launch(&login_path, params.droid_executable.as_deref());
    if !Path::new(&launch.program).is_file() {
        let requested = params
            .droid_executable
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("droid");
        return Err(format!(
            "Droid executable '{requested}' was not found in PATH"
        ));
    }
    let workdir = std::env::temp_dir().join("pilo-droid-catalog");
    std::fs::create_dir_all(&workdir)
        .map_err(|error| format!("failed to prepare Droid catalog directory: {error}"))?;
    let project = workdir.to_string_lossy().into_owned();

    let mut args = launch.prefix;
    args.extend(droid_exec_args(&project));
    let mut command = process_command(&launch.program, &args, &login_path);
    command
        .current_dir(&workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start Droid: {error}"))?;
    let stdin = Arc::new(Mutex::new(
        child
            .stdin
            .take()
            .ok_or_else(|| "Droid stdin unavailable".to_owned())?,
    ));
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Droid stdout unavailable".to_owned())?;
    let mut reader = BufReader::new(stdout);
    let catalog = match tokio::time::timeout(
        Duration::from_secs(30),
        catalog_handshake(&stdin, &mut reader, &project),
    )
    .await
    {
        Ok(Ok(mut runtime)) => {
            if params.thinking {
                describe_model_thinking(&stdin, &mut reader, &mut runtime, deadline).await;
            }
            Ok(Ok(runtime))
        }
        other => other,
    };
    let process = PiProcess {
        child,
        stdin,
        factory: None,
    };
    if let Err(error) = process.stop().await {
        eprintln!("[droid] catalog probe did not stop cleanly: {error}");
    }
    remove_catalog_sessions(&project);
    let runtime = match catalog {
        Ok(Ok(runtime)) => runtime,
        Ok(Err(error)) => return Err(error),
        Err(_) => return Err("timed out while reading the Droid model catalog".to_owned()),
    };
    Ok(json!({
        "models": catalog_models(&runtime),
        "model": current_model(&runtime),
    }))
}

async fn catalog_handshake(
    stdin: &Mutex<ChildStdin>,
    reader: &mut BufReader<ChildStdout>,
    project: &str,
) -> Result<FactoryRuntime, String> {
    write_json_line(stdin, &initialize_request()).await?;
    let initialized = read_catalog_response(stdin, reader, INIT_ID).await?;
    let protocol_version = initialized
        .get("protocolVersion")
        .and_then(Value::as_i64)
        .unwrap_or(1);
    write_json_line(stdin, &session_request(project, None)).await?;
    let session = read_catalog_response(stdin, reader, SESSION_ID).await?;
    runtime_from_session(&session, protocol_version)
}

/// Droid reports thinking choices only for the selected model, so the probe
/// selects each model once in its throwaway session. That change stays in the
/// session; Droid does not write it to `~/.factory/settings.json`. Models not
/// reached within the budget keep no thinking levels, and the live session
/// still reports them once selected.
async fn describe_model_thinking(
    stdin: &Mutex<ChildStdin>,
    reader: &mut BufReader<ChildStdout>,
    runtime: &mut FactoryRuntime,
    deadline: tokio::time::Instant,
) {
    let current = runtime.model_id.clone();
    for index in 0..runtime.models.len() {
        let Some(id) = runtime.models[index]
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        let (values, value) = if current.as_deref() == Some(id.as_str()) {
            (
                runtime.thinking_values.clone(),
                runtime.thinking_value.clone(),
            )
        } else {
            let request_id = format!("pilo-acp-catalog-{index}");
            let request = set_config_request(
                &request_id,
                &runtime.session_id,
                &runtime.model_config_id,
                &id,
                runtime.protocol_version,
            );
            if write_json_line(stdin, &request).await.is_err() {
                return;
            }
            let response = tokio::time::timeout_at(
                deadline,
                read_catalog_response(stdin, reader, &request_id),
            )
            .await;
            let result = match response {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => continue,
                Err(_) => return,
            };
            result
                .get("configOptions")
                .and_then(Value::as_array)
                .and_then(|options| options.iter().find(|option| is_thinking_option(option)))
                .map(thinking_choice)
                .unwrap_or_default()
        };
        let model = std::mem::take(&mut runtime.models[index]);
        runtime.models[index] = with_thinking_choice(model, &values, value.as_deref());
    }
}

async fn read_catalog_response(
    stdin: &Mutex<ChildStdin>,
    reader: &mut BufReader<ChildStdout>,
    expected_id: &str,
) -> Result<Value, String> {
    loop {
        let line = read_rpc_line(reader)
            .await
            .map_err(|error| error.to_string())?;
        let Some(bytes) = line else {
            return Err("Droid exited before the ACP response arrived".to_owned());
        };
        let value = parse_line(&bytes);
        match classify_line(&value, &mut DroidTurn::default()) {
            ClassifiedLine::ServerRequest { id, method } => {
                answer_server_request(stdin, &id, &method).await?;
            }
            ClassifiedLine::PromptResponse {
                id, success, error, ..
            } if id == expected_id => {
                if success {
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                }
                return Err(error.unwrap_or_else(|| "Droid returned an error".to_owned()));
            }
            ClassifiedLine::PromptResponse { .. } | ClassifiedLine::Events(_) => {}
        }
    }
}

/// Delete the throwaway sessions under the probe's Factory session directory.
///
/// Droid writes `<id>.jsonl` and `<id>.settings.json` after `session/new`
/// returns, sometimes after the process exits, so clear the directory now and
/// once more shortly after. Only catalog probes use this directory.
fn remove_catalog_sessions(project: &str) {
    let Some(dir) = factory_sessions_dir().map(|root| root.join(droid_session_dir_key(project)))
    else {
        return;
    };
    let _ = std::fs::remove_dir_all(&dir);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(dir)).await;
    });
}

fn spawn_output_tasks(
    state: &ServerState,
    stream_id: String,
    stdin: Arc<Mutex<ChildStdin>>,
    mut reader: BufReader<ChildStdout>,
    stderr: tokio::process::ChildStderr,
    mut turn: DroidTurn,
    runtime: Arc<StdMutex<FactoryRuntime>>,
) {
    let stdin_identity = Arc::downgrade(&stdin);
    let writer = state.writer.clone();
    let pi = Arc::clone(&state.pi);
    let starts = Arc::clone(&state.pi_starts);
    let stops = Arc::clone(&state.pi_stops);
    let outputs = Arc::clone(&state.pi_output_tasks);
    let stdout_identity = stdin_identity.clone();
    let stdout_stream = stream_id.clone();
    let stdout_task = tokio::spawn(async move {
        loop {
            match read_rpc_line(&mut reader).await {
                Ok(Some(bytes)) => {
                    let value = parse_line(&bytes);
                    note_runtime_config(&runtime, &value);
                    match classify_line(&value, &mut turn) {
                        ClassifiedLine::ServerRequest { id, method } => {
                            if answer_server_request(&stdin, &id, &method).await.is_err() {
                                break;
                            }
                            emit_unhandled_request(&writer, &stdout_stream, &method).await;
                        }
                        ClassifiedLine::PromptResponse {
                            id,
                            success,
                            error,
                            stop_reason,
                        } => {
                            if complete_model_wait(&runtime, &id, success, error.clone()) {
                                continue;
                            }
                            let events = finish_prompt(
                                &mut turn,
                                &id,
                                success,
                                error.as_deref(),
                                stop_reason.as_deref(),
                            );
                            if forward_events(&writer, &stdout_stream, events)
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        ClassifiedLine::Events(events) => {
                            if forward_events(&writer, &stdout_stream, events)
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let _ = writer
                        .send(Envelope::event(
                            stdout_stream.clone(),
                            "pi.error",
                            json!({ "message": error.to_string() }),
                        ))
                        .await;
                    break;
                }
            }
        }
        let exit_status = match stop_registered_pi(
            &pi,
            &starts,
            &stops,
            &outputs,
            &stdout_stream,
            Some(&stdout_identity),
        )
        .await
        {
            Ok(Some(status)) => Some(status),
            Ok(None) => return,
            Err(_) => None,
        };
        let code = exit_status.as_ref().and_then(|status| status.code());
        let success = exit_status.as_ref().is_some_and(|status| status.success());
        let _ = writer
            .send(Envelope::event(
                stdout_stream.clone(),
                "pi.stdout_closed",
                json!({ "code": code, "success": success }),
            ))
            .await;
        let output = {
            let mut outputs = outputs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if outputs
                .get(&stdout_stream)
                .is_some_and(|(identity, _)| identity.ptr_eq(&stdout_identity))
            {
                outputs.remove(&stdout_stream)
            } else {
                None
            }
        };
        if let Some((_, [_, stderr_task])) = output {
            stderr_task.abort();
            let _ = stderr_task.await;
        }
    });

    let stderr_stream = stream_id.clone();
    let writer = state.writer.clone();
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut buffer = vec![0_u8; 4096];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => {
                    let bytes = without_dk_key_banner(&buffer[..read]);
                    if bytes.is_empty() {
                        continue;
                    }
                    if writer
                        .send(Envelope::event_with_binary(
                            stderr_stream.clone(),
                            "pi.stderr",
                            Value::Null,
                            vec![bytes],
                        ))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    state
        .pi_output_tasks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(stream_id, (stdin_identity, [stdout_task, stderr_task]));
}

async fn stop_stream(state: &ServerState, stream_id: &str) -> Result<(), String> {
    stop_registered_pi(
        &state.pi,
        &state.pi_starts,
        &state.pi_stops,
        &state.pi_output_tasks,
        stream_id,
        None,
    )
    .await?;
    Ok(())
}

async fn acp_handshake(
    handshake: &mut AcpHandshake<'_>,
) -> Result<(FactoryRuntime, Value), String> {
    write_json_line(handshake.stdin, &initialize_request()).await?;
    let initialized = read_response(handshake, INIT_ID).await?;
    let protocol_version = initialized
        .get("protocolVersion")
        .and_then(Value::as_i64)
        .unwrap_or(1);
    write_json_line(
        handshake.stdin,
        &session_request(handshake.project, handshake.resume_id),
    )
    .await?;
    let session = read_response(handshake, SESSION_ID).await?;
    let runtime = runtime_from_session(&session, protocol_version)?;
    if let Some(request) = autonomy_request(&runtime.session_id, &session, protocol_version) {
        write_json_line(handshake.stdin, &request).await?;
        if let Err(error) = read_response(handshake, AUTONOMY_ID).await {
            eprintln!("[droid] ACP autonomy stayed at the Droid default: {error}");
        }
    }
    Ok((runtime, session))
}

async fn read_response(
    handshake: &mut AcpHandshake<'_>,
    expected_id: &str,
) -> Result<Value, String> {
    loop {
        let line = read_rpc_line(handshake.reader)
            .await
            .map_err(|error| error.to_string())?;
        let Some(bytes) = line else {
            return Err("Droid exited before the ACP response arrived".to_owned());
        };
        let value = parse_line(&bytes);
        match classify_line(&value, &mut DroidTurn::default()) {
            ClassifiedLine::ServerRequest { id, method } => {
                answer_server_request(handshake.stdin, &id, &method).await?;
                emit_unhandled_request(handshake.writer, handshake.stream_id, &method).await;
            }
            ClassifiedLine::PromptResponse {
                id, success, error, ..
            } if id == expected_id => {
                if success {
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                }
                return Err(error.unwrap_or_else(|| "Droid returned an error".to_owned()));
            }
            ClassifiedLine::PromptResponse { .. } | ClassifiedLine::Events(_) => {}
        }
    }
}

fn initialize_request() -> Value {
    acp_request(
        INIT_ID,
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {},
            "clientInfo": {
                "name": "pilo",
                "title": "Pilo",
                "version": env!("CARGO_PKG_VERSION"),
            },
        }),
    )
}

fn session_request(project: &str, session_id: Option<&str>) -> Value {
    match session_id {
        Some(session_id) => acp_request(
            SESSION_ID,
            "session/load",
            json!({
                "sessionId": session_id,
                "cwd": project,
                "mcpServers": [],
            }),
        ),
        None => acp_request(
            SESSION_ID,
            "session/new",
            json!({
                "cwd": project,
                "mcpServers": [],
            }),
        ),
    }
}

fn acp_request(id: &str, method: &str, params: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
}

fn set_config_request(
    id: &str,
    session_id: &str,
    config_id: &str,
    value: &str,
    protocol_version: i64,
) -> Value {
    let mut params = json!({
        "sessionId": session_id,
        "configId": config_id,
        "value": value,
    });
    if protocol_version >= 2 {
        params["type"] = json!("id");
    }
    acp_request(id, "session/set_config_option", params)
}

fn runtime_from_session(result: &Value, protocol_version: i64) -> Result<FactoryRuntime, String> {
    let session_id = text_field(result.get("sessionId"))
        .ok_or_else(|| "Droid did not return an ACP session id".to_owned())?;
    let options = result
        .get("configOptions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let model_option = options.iter().find(|option| is_model_option(option));
    let (models, model_id, model_config_id) = match model_option {
        Some(option) => (
            map_acp_models(option),
            text_field(option.get("currentValue")),
            option_config_id(option).unwrap_or_else(|| "model".to_owned()),
        ),
        None => (Vec::new(), None, "model".to_owned()),
    };
    let mut runtime = FactoryRuntime {
        models,
        model_id,
        pending: std::collections::HashMap::new(),
        session_id,
        model_config_id,
        protocol_version,
        thinking_config_id: None,
        thinking_values: Vec::new(),
        thinking_value: None,
    };
    apply_thinking_option(&mut runtime, options);
    Ok(runtime)
}

/// Droid's `thought_level` choices for the selected model. An update that
/// omits the option keeps the last one.
fn apply_thinking_option(runtime: &mut FactoryRuntime, options: &[Value]) {
    let Some(option) = options.iter().find(|option| is_thinking_option(option)) else {
        return;
    };
    runtime.thinking_config_id = option_config_id(option);
    (runtime.thinking_values, runtime.thinking_value) = thinking_choice(option);
}

/// Droid values and the current value of a `thought_level` option.
fn thinking_choice(option: &Value) -> (Vec<String>, Option<String>) {
    (
        select_choices(option)
            .into_iter()
            .filter_map(option_value)
            .collect(),
        text_field(option.get("currentValue")),
    )
}

fn is_thinking_option(option: &Value) -> bool {
    option.get("category").and_then(Value::as_str) == Some("thought_level")
        || option_config_id(option).as_deref() == Some("reasoning_effort")
}

/// Droid level name in Pi's thinking vocabulary. Droid spells "no reasoning"
/// as `none` for some models and `off` for others.
fn pi_thinking_level(value: &str) -> Option<&'static str> {
    match value {
        "none" | "off" => Some("off"),
        "minimal" => Some("minimal"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("xhigh"),
        "max" => Some("max"),
        _ => None,
    }
}

fn pi_thinking_levels(values: &[String]) -> Vec<&'static str> {
    let mut levels = Vec::new();
    for level in values.iter().filter_map(|value| pi_thinking_level(value)) {
        if !levels.contains(&level) {
            levels.push(level);
        }
    }
    levels
}

fn thinking_levels(runtime: &FactoryRuntime) -> Vec<&'static str> {
    pi_thinking_levels(&runtime.thinking_values)
}

fn thinking_level(runtime: &FactoryRuntime) -> &'static str {
    runtime
        .thinking_value
        .as_deref()
        .and_then(pi_thinking_level)
        .unwrap_or("off")
}

/// Attach a model's thinking choices in Pi's per-model shape.
fn with_thinking_choice(mut model: Value, values: &[String], value: Option<&str>) -> Value {
    let levels = pi_thinking_levels(values);
    if levels.is_empty() {
        return model;
    }
    model["reasoning"] = json!(levels.iter().any(|level| *level != "off"));
    model["thinkingLevels"] = json!(levels);
    model["defaultThinkingLevel"] = json!(value.and_then(pi_thinking_level).unwrap_or("off"));
    model
}

fn with_thinking(model: Value, runtime: &FactoryRuntime) -> Value {
    with_thinking_choice(
        model,
        &runtime.thinking_values,
        runtime.thinking_value.as_deref(),
    )
}

fn autonomy_request(session_id: &str, result: &Value, protocol_version: i64) -> Option<Value> {
    let options = result.get("configOptions")?.as_array()?;
    let option = options.iter().find(|option| is_autonomy_option(option))?;
    let choices = option.get("options")?.as_array()?;
    // Droid 0.230 names the level `medium`; 0.244 names it `auto-medium`.
    let target = ["medium", "auto-medium"].into_iter().find(|target| {
        choices
            .iter()
            .any(|choice| option_value(choice).as_deref() == Some(*target))
    })?;
    if option.get("currentValue").and_then(Value::as_str) == Some(target) {
        return None;
    }
    let config_id = option_config_id(option)?;
    Some(set_config_request(
        AUTONOMY_ID,
        session_id,
        &config_id,
        target,
        protocol_version,
    ))
}

fn is_model_option(option: &Value) -> bool {
    option.get("category").and_then(Value::as_str) == Some("model")
        || matches!(
            option_config_id(option).as_deref(),
            Some("model") | Some("models")
        )
}

fn is_autonomy_option(option: &Value) -> bool {
    matches!(
        option_config_id(option).as_deref(),
        Some("autonomy_level") | Some("autonomy")
    )
}

fn option_config_id(option: &Value) -> Option<String> {
    text_field(option.get("id")).or_else(|| text_field(option.get("configId")))
}

/// ACP select options are either flat or grouped (`{ group, name, options }`).
/// Flatten groups and drop repeated ids, as Zed and t3code do.
fn select_choices(option: &Value) -> Vec<&Value> {
    let Some(options) = option.get("options").and_then(Value::as_array) else {
        return Vec::new();
    };
    options
        .iter()
        .flat_map(
            |candidate| match candidate.get("options").and_then(Value::as_array) {
                Some(grouped) if candidate.get("value").is_none() => grouped.iter().collect(),
                _ => vec![candidate],
            },
        )
        .collect()
}

fn map_acp_models(option: &Value) -> Vec<Value> {
    let mut seen = HashSet::new();
    select_choices(option)
        .into_iter()
        .filter_map(map_acp_model)
        .filter(|model| {
            model
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| seen.insert(id.to_owned()))
        })
        .collect()
}

fn map_acp_model(option: &Value) -> Option<Value> {
    if option.get("deprecated").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let id = option_value(option)?;
    let name = option_name(option).unwrap_or_else(|| id.clone());
    if name.to_ascii_lowercase().contains("[deprecated]") {
        return None;
    }
    Some(pi_model(&id, &name, "factory"))
}

fn option_value(option: &Value) -> Option<String> {
    text_field(option.get("value")).or_else(|| text_field(option.get("id")))
}

fn option_name(option: &Value) -> Option<String> {
    text_field(option.get("name")).or_else(|| option_value(option))
}

fn pi_model(id: &str, name: &str, provider: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "provider": provider,
        "reasoning": false,
    })
}

fn text_field(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// `configOptions` from a `config_option_update` notification or a response.
fn updated_config_options(value: &Value) -> Option<&[Value]> {
    let options = value
        .pointer("/params/update/configOptions")
        .or_else(|| value.pointer("/result/configOptions"))
        .and_then(Value::as_array)?;
    let update_kind = value
        .pointer("/params/update/sessionUpdate")
        .and_then(Value::as_str);
    if value.pointer("/result/configOptions").is_none()
        && update_kind != Some("config_option_update")
    {
        return None;
    }
    Some(options)
}

fn model_id_from_options(options: &[Value], config_id: &str) -> Option<String> {
    options
        .iter()
        .find(|option| option_config_id(option).as_deref() == Some(config_id))
        .and_then(|option| text_field(option.get("currentValue")))
}

fn catalog_models(runtime: &FactoryRuntime) -> Vec<Value> {
    if !runtime.models.is_empty() {
        return runtime.models.clone();
    }
    runtime
        .model_id
        .as_deref()
        .map(|id| vec![pi_model(id, id, "factory")])
        .unwrap_or_default()
}

fn current_model(runtime: &FactoryRuntime) -> Value {
    let Some(id) = runtime.model_id.as_deref() else {
        return Value::Null;
    };
    let model = runtime
        .models
        .iter()
        .find(|model| model.get("id").and_then(Value::as_str) == Some(id))
        .cloned()
        .unwrap_or_else(|| pi_model(id, id, "factory"));
    with_thinking(model, runtime)
}

/// A model switch also replaces the thinking choices, so read both.
fn note_runtime_config(runtime: &StdMutex<FactoryRuntime>, value: &Value) {
    let Some(options) = updated_config_options(value) else {
        return;
    };
    let mut runtime = runtime
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(model_id) = model_id_from_options(options, &runtime.model_config_id) {
        runtime.model_id = Some(model_id);
    }
    apply_thinking_option(&mut runtime, options);
}

fn complete_model_wait(
    runtime: &StdMutex<FactoryRuntime>,
    id: &str,
    success: bool,
    error: Option<String>,
) -> bool {
    let sender = runtime
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .pending
        .remove(id);
    let Some(sender) = sender else {
        return false;
    };
    let result = if success {
        Ok(())
    } else {
        Err(error.unwrap_or_else(|| "Droid rejected the setting change".to_owned()))
    };
    let _ = sender.send(result);
    true
}

async fn droid_io(
    state: &ServerState,
    stream_id: &str,
) -> Result<(Arc<Mutex<ChildStdin>>, Arc<StdMutex<FactoryRuntime>>), String> {
    let processes = state.pi.lock().await;
    let process = processes
        .get(stream_id)
        .ok_or_else(|| format!("Droid stream '{stream_id}' is not running"))?;
    let runtime = process
        .factory
        .clone()
        .ok_or_else(|| format!("Droid stream '{stream_id}' has no ACP session"))?;
    Ok((Arc::clone(&process.stdin), runtime))
}

async fn answer_model_command(
    state: &ServerState,
    params: &DroidSendParams,
) -> Result<Value, String> {
    let command_type = params
        .command
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("");
    let (_, runtime) = droid_io(state, &params.stream_id).await?;
    match command_type {
        "get_available_models" => {
            let runtime = runtime
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Ok(json!({ "models": catalog_models(&runtime) }))
        }
        "get_state" => {
            let runtime = runtime
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Ok(json!({
                "model": current_model(&runtime),
                "thinkingLevel": thinking_level(&runtime),
            }))
        }
        "get_available_thinking_levels" => {
            let runtime = runtime
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Ok(json!({ "levels": thinking_levels(&runtime) }))
        }
        "set_model" => set_droid_model(state, params, runtime).await,
        "set_thinking_level" => set_droid_thinking_level(state, params, runtime).await,
        _ => Err("this command is not available for Droid sessions".to_owned()),
    }
}

async fn set_droid_model(
    state: &ServerState,
    params: &DroidSendParams,
    runtime: Arc<StdMutex<FactoryRuntime>>,
) -> Result<Value, String> {
    let model_id = params
        .command
        .get("modelId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "Droid model id is required".to_owned())?
        .to_owned();
    let config_id = {
        let runtime = runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !runtime.models.is_empty()
            && !runtime
                .models
                .iter()
                .any(|model| model.get("id").and_then(Value::as_str) == Some(model_id.as_str()))
        {
            return Err(format!("Droid model '{model_id}' is not available"));
        }
        runtime.model_config_id.clone()
    };
    set_droid_config(
        state,
        params,
        &runtime,
        &config_id,
        &model_id,
        "pilo-droid-model",
    )
    .await?;
    let mut runtime = runtime
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if runtime.model_id.as_deref() != Some(model_id.as_str()) {
        runtime.model_id = Some(model_id);
    }
    Ok(current_model(&runtime))
}

async fn set_droid_thinking_level(
    state: &ServerState,
    params: &DroidSendParams,
    runtime: Arc<StdMutex<FactoryRuntime>>,
) -> Result<Value, String> {
    let level = params
        .command
        .get("level")
        .and_then(Value::as_str)
        .filter(|level| !level.is_empty())
        .ok_or_else(|| "Droid thinking level is required".to_owned())?;
    let (config_id, value) = {
        let runtime = runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_id = runtime
            .thinking_config_id
            .clone()
            .ok_or_else(|| "this Droid model has no thinking levels".to_owned())?;
        let value = runtime
            .thinking_values
            .iter()
            .find(|value| pi_thinking_level(value) == Some(level))
            .cloned()
            .ok_or_else(|| format!("thinking level '{level}' is not available for this model"))?;
        (config_id, value)
    };
    set_droid_config(
        state,
        params,
        &runtime,
        &config_id,
        &value,
        "pilo-droid-thinking",
    )
    .await?;
    let mut runtime = runtime
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    runtime.thinking_value = Some(value);
    Ok(Value::Null)
}

/// Send `session/set_config_option` and wait for Droid's response.
async fn set_droid_config(
    state: &ServerState,
    params: &DroidSendParams,
    runtime: &StdMutex<FactoryRuntime>,
    config_id: &str,
    value: &str,
    default_id: &str,
) -> Result<(), String> {
    let (session_id, protocol_version) = {
        let runtime = runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (runtime.session_id.clone(), runtime.protocol_version)
    };
    let id = params
        .command
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .unwrap_or(default_id);
    let (stdin, _) = droid_io(state, &params.stream_id).await?;
    let (sender, receiver) = oneshot::channel();
    runtime
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .pending
        .insert(id.to_owned(), sender);
    let request = set_config_request(id, &session_id, config_id, value, protocol_version);
    if let Err(error) = write_json_line(&stdin, &request).await {
        runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .remove(id);
        return Err(error);
    }
    match tokio::time::timeout(Duration::from_secs(8), receiver).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("Droid setting change was cancelled".to_owned()),
        Err(_) => {
            runtime
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pending
                .remove(id);
            Err("timed out while changing the Droid setting".to_owned())
        }
    }
}

fn session_id_from_path(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    if !normalized.contains("/.factory/sessions/") {
        return None;
    }
    Path::new(&normalized)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .map(str::to_owned)
}

fn session_path_from_result(
    result: &Value,
    project: &str,
    session_id: Option<&str>,
) -> Option<String> {
    for pointer in [
        "/sessionFile",
        "/sessionPath",
        "/path",
        "/session/sessionFile",
        "/session/sessionPath",
        "/session/path",
    ] {
        if let Some(path) = result.pointer(pointer).and_then(Value::as_str)
            && !path.is_empty()
        {
            return Some(path.to_owned());
        }
    }
    let session_id = session_id?;
    let candidate = factory_sessions_dir()?
        .join(droid_session_dir_key(project))
        .join(format!("{session_id}.jsonl"));
    candidate
        .is_file()
        .then(|| candidate.to_string_lossy().into_owned())
}

async fn write_json_line(stdin: &Mutex<ChildStdin>, value: &Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    let mut stdin = stdin.lock().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        stdin.write_all(&bytes).await?;
        stdin.flush().await
    })
    .await
    .map_err(|_| "timed out while writing to Droid".to_owned())?
    .map_err(|error| format!("failed to write to Droid: {error}"))
}

fn permission_response(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "outcome": { "outcome": "cancelled" },
        },
    })
}

async fn answer_server_request(
    stdin: &Mutex<ChildStdin>,
    id: &Value,
    method: &str,
) -> Result<(), String> {
    let response = if method == "session/request_permission" {
        permission_response(id)
    } else {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": "Pilo does not handle this ACP request",
            },
        })
    };
    write_json_line(stdin, &response).await
}

async fn emit_unhandled_request(writer: &ServerWriter, stream_id: &str, method: &str) {
    let message = if method == "session/request_permission" || method.contains("permission") {
        CONFIRMATION_MESSAGE.to_owned()
    } else {
        format!("Droid requested {method}, which this version does not handle.")
    };
    let _ = writer
        .send(Envelope::event(
            stream_id.to_owned(),
            "pi.error",
            json!({ "message": message }),
        ))
        .await;
}

async fn forward_events(
    writer: &ServerWriter,
    stream_id: &str,
    events: Vec<Value>,
) -> Result<(), ()> {
    for event in events {
        let bytes = serde_json::to_vec(&event).map_err(|_| ())?;
        writer
            .send(Envelope::event_with_binary(
                stream_id.to_owned(),
                "pi.rpc_json",
                Value::Null,
                vec![bytes],
            ))
            .await
            .map_err(|_| ())?;
    }
    Ok(())
}

fn parse_line(bytes: &[u8]) -> Value {
    let text = String::from_utf8_lossy(bytes);
    serde_json::from_str(text.trim().trim_end_matches('\r')).unwrap_or(Value::Null)
}

fn classify_line(value: &Value, turn: &mut DroidTurn) -> ClassifiedLine {
    if value.is_null() {
        return ClassifiedLine::Events(Vec::new());
    }
    let method = value.get("method").and_then(Value::as_str);
    let id = value.get("id").filter(|id| !id.is_null());
    let has_result = value.get("result").is_some() || value.get("error").is_some();
    if let (Some(method), Some(id)) = (method, id)
        && !has_result
    {
        return ClassifiedLine::ServerRequest {
            id: id.clone(),
            method: method.to_owned(),
        };
    }
    if has_result && method.is_none() {
        let id = id.and_then(Value::as_str).unwrap_or("").to_owned();
        if !id.is_empty() {
            let error = value.get("error").map(error_text);
            let stop_reason = value
                .pointer("/result/stopReason")
                .and_then(Value::as_str)
                .map(str::to_owned);
            return ClassifiedLine::PromptResponse {
                id,
                success: error.is_none(),
                error,
                stop_reason,
            };
        }
    }
    ClassifiedLine::Events(translate_acp_value(turn, value))
}

fn finish_prompt(
    turn: &mut DroidTurn,
    id: &str,
    success: bool,
    error: Option<&str>,
    stop_reason: Option<&str>,
) -> Vec<Value> {
    let mut events = Vec::new();
    if success {
        events.extend(events_for_stop(turn, stop_reason.unwrap_or("end_turn")));
        events.push(prompt_response(id, true, None));
    } else {
        let message = error.unwrap_or("Droid rejected the prompt");
        if !turn.streamed_text {
            events.extend(append_text(turn, message));
        }
        events.extend(events_for_stop(turn, "error"));
        events.push(prompt_response(id, false, Some(message)));
    }
    events
}

fn prompt_response(id: &str, success: bool, error: Option<&str>) -> Value {
    if success {
        json!({
            "type": "response",
            "id": id,
            "command": "prompt",
            "success": true,
            "data": { "disposition": "started" },
        })
    } else {
        json!({
            "type": "response",
            "id": id,
            "command": "prompt",
            "success": false,
            "error": error.unwrap_or("Droid rejected the prompt"),
        })
    }
}

fn error_text(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .unwrap_or("Droid returned an error")
        .to_owned()
}

fn translate_acp_value(turn: &mut DroidTurn, value: &Value) -> Vec<Value> {
    let update = if value.get("method").and_then(Value::as_str) == Some("session/update") {
        value.pointer("/params/update").unwrap_or(value)
    } else if value.get("sessionUpdate").is_some() {
        value
    } else {
        return Vec::new();
    };
    match update.get("sessionUpdate").and_then(Value::as_str) {
        Some("agent_message_chunk") => agent_text(turn, update, true),
        Some("agent_message") => agent_text(turn, update, false),
        Some("agent_thought_chunk" | "agent_thought") => thinking_text(turn, update),
        Some("tool_call" | "tool_call_update") => tool_update(turn, update),
        Some("state_update") if update.get("state").and_then(Value::as_str) == Some("idle") => {
            events_for_stop(
                turn,
                update
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or("end_turn"),
            )
        }
        _ => Vec::new(),
    }
}

fn agent_text(turn: &mut DroidTurn, update: &Value, chunk: bool) -> Vec<Value> {
    let message_id = text_field(update.get("messageId"));
    if chunk {
        if message_id
            .as_ref()
            .is_some_and(|id| turn.full_messages.contains(id))
        {
            return Vec::new();
        }
        if let Some(id) = message_id {
            turn.chunked_messages.insert(id);
        }
    } else if let Some(id) = message_id {
        if turn.chunked_messages.contains(&id) || turn.full_messages.contains(&id) {
            return Vec::new();
        }
        turn.full_messages.insert(id);
    }
    let Some(text) = content_text(update) else {
        return Vec::new();
    };
    append_text(turn, &text)
}

fn thinking_text(turn: &mut DroidTurn, update: &Value) -> Vec<Value> {
    let Some(text) = content_text(update) else {
        return Vec::new();
    };
    if text.is_empty() {
        return Vec::new();
    }
    let mut events = ensure_assistant(turn);
    events.push(json!({
        "type": "message_update",
        "assistantMessageEvent": { "type": "thinking_delta", "delta": text },
    }));
    events
}

fn content_text(update: &Value) -> Option<String> {
    match update.get("content") {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        Some(Value::Object(block)) => block
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned),
        Some(Value::Array(blocks)) => {
            let text = blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn tool_update(turn: &mut DroidTurn, update: &Value) -> Vec<Value> {
    let Some(tool_call_id) = tool_id(update).map(str::to_owned) else {
        return Vec::new();
    };
    let status = update
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    let name = tool_name(update)
        .or_else(|| update.get("title").and_then(Value::as_str))
        .or_else(|| update.get("kind").and_then(Value::as_str))
        .unwrap_or("tool")
        .to_owned();
    let mut events = Vec::new();
    if !turn.started_tools.contains(&tool_call_id) {
        events.extend(tool_start(
            turn,
            &json!({
                "toolCallId": tool_call_id,
                "toolName": name,
                "args": update.get("rawInput").cloned().unwrap_or(Value::Null),
            }),
        ));
        turn.started_tools.insert(tool_call_id.clone());
    }
    if matches!(status, "completed" | "failed") && !turn.finished_tools.contains(&tool_call_id) {
        events.extend(tool_end(
            turn,
            &json!({
                "toolCallId": tool_call_id,
                "toolName": name,
                "result": tool_result_text(update),
                "isError": status == "failed",
            }),
        ));
        turn.finished_tools.insert(tool_call_id);
    }
    events
}

fn tool_result_text(update: &Value) -> Value {
    let Some(content) = update.get("content") else {
        return Value::Null;
    };
    if let Some(text) = content.as_str() {
        return Value::String(text.to_owned());
    }
    let Some(items) = content.as_array() else {
        return content.clone();
    };
    let mut text = String::new();
    for item in items {
        let Some(piece) = item
            .pointer("/content/text")
            .or_else(|| item.get("text"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(piece);
    }
    if text.is_empty() {
        Value::Null
    } else {
        Value::String(text)
    }
}

fn tool_start(turn: &mut DroidTurn, value: &Value) -> Vec<Value> {
    let Some(tool_call_id) = tool_id(value) else {
        return Vec::new();
    };
    let Some(tool_name) = tool_name(value) else {
        return Vec::new();
    };
    let mut events = ensure_assistant(turn);
    events.push(json!({
        "type": "tool_execution_start",
        "toolCallId": tool_call_id,
        "toolName": tool_name,
        "args": tool_args(value),
    }));
    events
}

fn tool_end(turn: &mut DroidTurn, value: &Value) -> Vec<Value> {
    let Some(tool_call_id) = tool_id(value) else {
        return Vec::new();
    };
    let tool_name = tool_name(value).unwrap_or("tool");
    let mut events = ensure_assistant(turn);
    events.push(json!({
        "type": "tool_execution_end",
        "toolCallId": tool_call_id,
        "toolName": tool_name,
        "result": value.get("result").cloned().unwrap_or(Value::Null),
        "isError": value.get("isError").and_then(Value::as_bool).unwrap_or(false),
    }));
    events
}

fn events_for_stop(turn: &mut DroidTurn, stop_reason: &str) -> Vec<Value> {
    if turn.settled {
        return Vec::new();
    }
    let mut events = Vec::new();
    let normalized = stop_reason.to_ascii_lowercase();
    if !turn.streamed_text && normalized != "end_turn" && !stop_reason.is_empty() {
        events.extend(append_text(turn, &stop_reason.replace('_', " ")));
    }
    events.push(json!({ "type": "agent_settled" }));
    turn.settled = true;
    turn.assistant_started = false;
    turn.streamed_text = false;
    events
}

fn append_text(turn: &mut DroidTurn, text: &str) -> Vec<Value> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut events = ensure_assistant(turn);
    events.push(json!({
        "type": "message_update",
        "assistantMessageEvent": { "type": "text_delta", "delta": text },
    }));
    turn.streamed_text = true;
    events
}

fn ensure_assistant(turn: &mut DroidTurn) -> Vec<Value> {
    turn.settled = false;
    if turn.assistant_started {
        return Vec::new();
    }
    turn.assistant_started = true;
    vec![json!({
        "type": "message_start",
        "message": { "role": "assistant", "content": [] },
    })]
}

fn tool_id(value: &Value) -> Option<&str> {
    ["toolCallId", "toolId", "tool_use_id", "id"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
}

fn tool_name(value: &Value) -> Option<&str> {
    ["toolName", "name", "title", "kind", "tool"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
}

/// `dk run` prints the selected key to stderr before Droid starts. Drop that
/// banner so a startup error does not carry a fragment of `FACTORY_API_KEY`.
fn without_dk_key_banner(chunk: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(chunk) else {
        return chunk.to_vec();
    };
    if !text.contains("Using key #") {
        return chunk.to_vec();
    }
    let mut kept = String::new();
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("Using key #") {
            continue;
        }
        kept.push_str(line);
    }
    kept.into_bytes()
}

fn tool_args(value: &Value) -> Value {
    value
        .get("args")
        .or_else(|| value.get("rawInput"))
        .or_else(|| value.get("parameters"))
        .or_else(|| value.get("input"))
        .cloned()
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::{
        DroidTurn, autonomy_request, current_model, droid_exec_args, finish_prompt,
        initialize_request, note_runtime_config, permission_response, runtime_from_session,
        session_request, set_config_request, thinking_level, thinking_levels, translate_acp_value,
        without_dk_key_banner,
    };
    use serde_json::json;

    #[test]
    fn dk_key_banner_is_removed_from_stderr() {
        let raw = b"Using key #1 (fk-abc...wxyz): bar\nDroid warning\n";
        assert_eq!(without_dk_key_banner(raw), b"Droid warning\n");
        assert_eq!(without_dk_key_banner(b"plain stderr\n"), b"plain stderr\n");
    }

    #[test]
    fn exec_args_select_acp_and_medium_autonomy() {
        let args = droid_exec_args("/work/app");
        assert_eq!(args[0], "exec");
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--output-format", "acp"])
        );
        assert!(args.windows(2).any(|pair| pair == ["--auto", "medium"]));
        assert!(args.windows(2).any(|pair| pair == ["--cwd", "/work/app"]));
        assert!(!args.iter().any(|arg| arg == "--input-format"));
        assert!(!args.iter().any(|arg| arg == "--skip-permissions-unsafe"));
    }

    #[test]
    fn initialize_request_is_acp_v1() {
        let request = initialize_request();
        assert_eq!(request["jsonrpc"], "2.0");
        assert!(request.get("factoryProtocolVersion").is_none());
        assert_eq!(request["method"], "initialize");
        assert_eq!(request["params"]["protocolVersion"], 1);
        assert_eq!(request["params"]["clientInfo"]["name"], "pilo");
        assert!(
            request["params"]["clientCapabilities"]
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
        );
    }

    #[test]
    fn new_and_load_session_use_acp_methods() {
        let created = session_request("/work/app", None);
        assert_eq!(created["method"], "session/new");
        assert_eq!(created["params"]["cwd"], "/work/app");
        assert!(
            created["params"]["mcpServers"]
                .as_array()
                .is_some_and(|items| items.is_empty())
        );
        let loaded = session_request("/work/app", Some("session-1"));
        assert_eq!(loaded["method"], "session/load");
        assert_eq!(loaded["params"]["sessionId"], "session-1");
        assert_eq!(loaded["params"]["cwd"], "/work/app");
    }

    #[test]
    fn model_switch_uses_set_config_option() {
        let request = set_config_request("rpc-1", "session-1", "model", "glm-5.3", 1);
        assert_eq!(request["method"], "session/set_config_option");
        assert_eq!(request["params"]["sessionId"], "session-1");
        assert_eq!(request["params"]["configId"], "model");
        assert_eq!(request["params"]["value"], "glm-5.3");
        assert!(request["params"].get("type").is_none());
        let v2 = set_config_request("rpc-1", "session-1", "model", "glm-5.3", 2);
        assert_eq!(v2["params"]["type"], "id");
    }

    #[test]
    fn autonomy_request_sets_medium_once() {
        let session = json!({
            "sessionId": "session-1",
            "configOptions": [{
                "id": "autonomy_level",
                "currentValue": "low",
                "options": [{ "value": "low" }, { "value": "medium" }]
            }]
        });
        let request = autonomy_request("session-1", &session, 1).unwrap();
        assert_eq!(request["params"]["configId"], "autonomy_level");
        assert_eq!(request["params"]["value"], "medium");
        let already = json!({
            "sessionId": "session-1",
            "configOptions": [{
                "id": "autonomy_level",
                "currentValue": "medium",
                "options": [{ "value": "medium" }]
            }]
        });
        assert!(autonomy_request("session-1", &already, 1).is_none());
    }

    #[test]
    fn autonomy_request_accepts_auto_prefixed_levels() {
        let session = json!({
            "sessionId": "session-1",
            "configOptions": [{
                "id": "autonomy_level",
                "currentValue": "auto-high",
                "options": [
                    { "value": "normal" },
                    { "value": "auto-medium" },
                    { "value": "auto-high" }
                ]
            }]
        });
        let request = autonomy_request("session-1", &session, 1).unwrap();
        assert_eq!(request["params"]["value"], "auto-medium");
    }

    #[test]
    fn agent_chunks_and_tool_updates_project_to_pi_events() {
        let mut turn = DroidTurn::default();
        let events = translate_acp_value(
            &mut turn,
            &json!({
                "method": "session/update",
                "params": {
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "messageId": "msg-1",
                        "content": { "type": "text", "text": "Hi" }
                    }
                }
            }),
        );
        assert_eq!(events[0]["type"], "message_start");
        assert_eq!(events[1]["assistantMessageEvent"]["delta"], "Hi");
        let events = translate_acp_value(
            &mut turn,
            &json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call-1",
                "title": "Read file",
                "kind": "read",
                "status": "pending",
                "rawInput": { "path": "a" }
            }),
        );
        assert_eq!(events[0]["type"], "tool_execution_start");
        assert_eq!(events[0]["toolCallId"], "call-1");
        assert_eq!(events[0]["toolName"], "Read file");
        assert_eq!(events[0]["args"]["path"], "a");
        let events = translate_acp_value(
            &mut turn,
            &json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-1",
                "status": "completed",
                "content": [{ "type": "content", "content": { "type": "text", "text": "ok" } }]
            }),
        );
        assert_eq!(events.last().unwrap()["type"], "tool_execution_end");
        assert_eq!(events.last().unwrap()["result"], "ok");
        assert_eq!(events.last().unwrap()["isError"], false);
    }

    #[test]
    fn user_chunks_are_not_duplicated_and_full_message_follows_chunks() {
        let mut turn = DroidTurn::default();
        let events = translate_acp_value(
            &mut turn,
            &json!({
                "method": "session/update",
                "params": {
                    "update": {
                        "sessionUpdate": "user_message_chunk",
                        "content": { "type": "text", "text": "hello" }
                    }
                }
            }),
        );
        assert!(events.is_empty());
        let _ = translate_acp_value(
            &mut turn,
            &json!({
                "sessionUpdate": "agent_message_chunk",
                "messageId": "msg-1",
                "content": { "type": "text", "text": "Hi" }
            }),
        );
        let events = translate_acp_value(
            &mut turn,
            &json!({
                "sessionUpdate": "agent_message",
                "messageId": "msg-1",
                "content": [{ "type": "text", "text": "Hi there" }]
            }),
        );
        assert!(events.is_empty());
    }

    #[test]
    fn stop_reason_settles_and_refusal_is_visible() {
        let mut turn = DroidTurn::default();
        let _ = translate_acp_value(
            &mut turn,
            &json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "Hi" }
            }),
        );
        let events = finish_prompt(&mut turn, "prompt-1", true, None, Some("end_turn"));
        assert_eq!(events[0]["type"], "agent_settled");
        assert_eq!(events[1]["command"], "prompt");
        assert_eq!(events[1]["success"], true);
        let mut turn = DroidTurn::default();
        let events = translate_acp_value(
            &mut turn,
            &json!({
                "sessionUpdate": "state_update",
                "state": "idle",
                "stopReason": "refusal"
            }),
        );
        assert_eq!(events[1]["assistantMessageEvent"]["delta"], "refusal");
        assert_eq!(events.last().unwrap()["type"], "agent_settled");
        let again = translate_acp_value(
            &mut turn,
            &json!({
                "sessionUpdate": "state_update",
                "state": "idle",
                "stopReason": "refusal"
            }),
        );
        assert!(again.is_empty());
    }

    #[test]
    fn config_options_skip_deprecated_models() {
        let runtime = runtime_from_session(
            &json!({
                "sessionId": "session-1",
                "configOptions": [{
                    "id": "model",
                    "category": "model",
                    "currentValue": "auto",
                    "options": [
                        { "value": "auto", "name": "Auto" },
                        { "value": "old", "name": "Old [Deprecated]" },
                        { "value": "gone", "name": "Gone", "deprecated": true }
                    ]
                }]
            }),
            1,
        )
        .unwrap();
        assert_eq!(runtime.session_id, "session-1");
        assert_eq!(runtime.model_config_id, "model");
        assert_eq!(runtime.model_id.as_deref(), Some("auto"));
        assert_eq!(runtime.models.len(), 1);
        assert_eq!(runtime.models[0]["id"], "auto");
        assert_eq!(runtime.models[0]["provider"], "factory");
        assert_eq!(runtime.models[0]["reasoning"], false);
    }

    #[test]
    fn config_options_flatten_grouped_models() {
        let runtime = runtime_from_session(
            &json!({
                "sessionId": "session-1",
                "configOptions": [{
                    "id": "model",
                    "category": "model",
                    "currentValue": "b",
                    "options": [
                        { "value": "a", "name": "A" },
                        { "group": "custom", "name": "Custom", "options": [
                            { "value": "b", "name": "B" },
                            { "value": "a", "name": "A again" }
                        ]}
                    ]
                }]
            }),
            1,
        )
        .unwrap();
        let ids: Vec<_> = runtime.models.iter().map(|model| &model["id"]).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(current_model(&runtime)["name"], "B");
    }

    #[test]
    fn reasoning_effort_maps_to_pi_thinking_levels() {
        let runtime = runtime_from_session(
            &json!({
                "sessionId": "session-1",
                "configOptions": [
                    {
                        "id": "model",
                        "category": "model",
                        "currentValue": "haiku",
                        "options": [{ "value": "haiku", "name": "Haiku" }]
                    },
                    {
                        "id": "reasoning_effort",
                        "category": "thought_level",
                        "currentValue": "high",
                        "options": [
                            { "value": "off" },
                            { "value": "low" },
                            { "value": "high" },
                            { "value": "max" }
                        ]
                    }
                ]
            }),
            1,
        )
        .unwrap();
        assert_eq!(
            runtime.thinking_config_id.as_deref(),
            Some("reasoning_effort")
        );
        assert_eq!(thinking_levels(&runtime), ["off", "low", "high", "max"]);
        assert_eq!(thinking_level(&runtime), "high");
        let model = current_model(&runtime);
        assert_eq!(model["reasoning"], true);
        assert_eq!(model["defaultThinkingLevel"], "high");

        // Switching to `auto` replaces the choices; Droid spells it `none`.
        let runtime = std::sync::Mutex::new(
            runtime_from_session(&json!({ "sessionId": "session-1" }), 1).unwrap(),
        );
        note_runtime_config(
            &runtime,
            &json!({
                "params": { "update": {
                    "sessionUpdate": "config_option_update",
                    "configOptions": [{
                        "id": "reasoning_effort",
                        "category": "thought_level",
                        "currentValue": "none",
                        "options": [{ "value": "none", "name": "None" }]
                    }]
                }}
            }),
        );
        let runtime = runtime.lock().unwrap();
        assert_eq!(thinking_levels(&runtime), ["off"]);
        assert_eq!(thinking_level(&runtime), "off");
    }

    #[test]
    fn permission_response_cancels_without_a_factory_envelope() {
        let response = permission_response(&json!("perm-1"));
        assert_eq!(response["jsonrpc"], "2.0");
        assert!(response.get("factoryProtocolVersion").is_none());
        assert_eq!(response["result"]["outcome"]["outcome"], "cancelled");
    }
}
