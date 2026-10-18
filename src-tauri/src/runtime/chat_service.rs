use std::sync::Arc;

use pilo_protocol::BuiltinExtension;

use crate::domain::{AgentKind, Project};

use super::{
    PiloRuntime, RuntimeEventBus, chat_sessions::ChatSessionLaunch, host_paths::HostPaths,
    pi_workspace, session_activity::reject_external_session_owner,
    session_snapshot::PiSessionSnapshot, storage,
};

#[derive(Clone, Debug)]
pub(crate) struct ChatSessionRequest {
    pub project_id: String,
    pub session_key: String,
    pub session_path: Option<String>,
    pub no_session: bool,
    pub extensions: Vec<String>,
    pub builtin_extensions: Vec<BuiltinExtension>,
    pub agent: AgentKind,
}

fn get_project(paths: &HostPaths, project_id: &str) -> Result<Project, String> {
    storage::get_project(&storage::open_with_paths(paths)?, project_id)?
        .ok_or_else(|| format!("Project '{project_id}' was not found"))
}

async fn resolve_launch(
    paths: &HostPaths,
    runtime: &PiloRuntime,
    request: &ChatSessionRequest,
) -> Result<(Project, ChatSessionLaunch, String), String> {
    let project = get_project(paths, &request.project_id)?;
    let catalog_probe = request
        .builtin_extensions
        .iter()
        .any(|extension| matches!(extension, BuiltinExtension::ModelCatalog));
    let agent = if catalog_probe {
        AgentKind::Pi
    } else if request
        .session_path
        .as_deref()
        .is_some_and(|path| AgentKind::from_session_path(path) == AgentKind::Droid)
    {
        AgentKind::Droid
    } else {
        request.agent
    };
    let profile =
        pi_workspace::resolve_pi_runtime_with_paths(paths, &project, request.extensions.clone())?;
    let mut builtin_extensions = if agent == AgentKind::Droid {
        Vec::new()
    } else {
        profile.builtin_extensions
    };
    // Probes are throwaway sessions; only normal chats load background assistance.
    if agent == AgentKind::Pi && !catalog_probe {
        builtin_extensions.push(BuiltinExtension::NextPrompt);
        builtin_extensions.push(BuiltinExtension::CacheKeepAlive);
    }
    for extension in &request.builtin_extensions {
        if !matches!(extension, BuiltinExtension::ModelCatalog) {
            return Err("unsupported client-selected builtin extension".to_owned());
        }
        builtin_extensions.push(extension.clone());
    }
    reject_external_session_owner(runtime, &profile.project, request.session_path.as_deref())
        .await?;
    Ok((
        profile.project,
        ChatSessionLaunch {
            session_path: request.session_path.clone(),
            no_session: request.no_session,
            extensions: if agent == AgentKind::Droid {
                Vec::new()
            } else {
                profile.extensions
            },
            builtin_extensions,
            disable_builtin_tools: profile.disable_builtin_tools,
            disable_extension_discovery: profile.disable_extension_discovery,
            disable_context_files: profile.disable_context_files,
            agent,
        },
        project.connection.id,
    ))
}

pub(crate) async fn prepare(
    paths: &HostPaths,
    runtime: &PiloRuntime,
    events: RuntimeEventBus,
    request: ChatSessionRequest,
) -> Result<PiSessionSnapshot, String> {
    let (project, launch, connection_id) = resolve_launch(paths, runtime, &request).await?;
    let snapshot = runtime
        .chat_sessions
        .prepare(
            Arc::clone(&runtime.servers),
            events,
            project,
            request.session_key.clone(),
            launch,
        )
        .await?;
    if let Err(error) = super::cache_keep_alive::configure_session(
        paths,
        runtime,
        &request.session_key,
        &connection_id,
    )
    .await
    {
        log::warn!(target: "cache-keep-alive", "failed to configure session: {error}");
    }
    Ok(snapshot)
}

pub(crate) async fn start(
    paths: &HostPaths,
    runtime: &PiloRuntime,
    events: RuntimeEventBus,
    request: ChatSessionRequest,
) -> Result<PiSessionSnapshot, String> {
    let (project, launch, connection_id) = resolve_launch(paths, runtime, &request).await?;
    let snapshot = runtime
        .chat_sessions
        .ensure(
            Arc::clone(&runtime.servers),
            events,
            project,
            request.session_key.clone(),
            launch,
        )
        .await?;
    if let Err(error) = super::cache_keep_alive::configure_session(
        paths,
        runtime,
        &request.session_key,
        &connection_id,
    )
    .await
    {
        log::warn!(target: "cache-keep-alive", "failed to configure session: {error}");
    }
    Ok(snapshot)
}
