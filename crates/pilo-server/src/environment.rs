use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::process::Stdio;

use pilo_protocol::{EnvironmentInfo, PiExecutableInfo};
use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;

use crate::{ServerState, to_value};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectParams {
    pub(crate) project: String,
    #[serde(default)]
    pub(crate) pi_executable: Option<String>,
    /// Pi 运行位置："workspace" 在连接环境内，"local" 在 Pilo 本机。
    /// local 时无需（也不应）探测连接环境中的 Pi。
    #[serde(default)]
    pub(crate) pi_runtime: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct PiProbeParams {
    #[serde(default)]
    pub(crate) executable: Option<String>,
}

pub(crate) struct ToolchainInfo {
    pub(crate) path: String,
    pub(crate) home: String,
    pub(crate) shell: String,
    pub(crate) pi_executable: String,
    pub(crate) pi_version: String,
    pub(crate) node_executable: String,
    pub(crate) node_version: String,
    pub(crate) git_executable: String,
    pub(crate) git_version: String,
}

fn resolve_project_path(project: &str) -> Result<PathBuf, String> {
    let project = project
        .strip_prefix('"')
        .and_then(|path| path.strip_suffix('"'))
        .or_else(|| {
            project
                .strip_prefix('\'')
                .and_then(|path| path.strip_suffix('\''))
        })
        .unwrap_or(project);
    if project.is_empty() {
        return Err("project path cannot be empty".to_owned());
    }

    let home_relative = if project == "~" {
        Some("")
    } else {
        project.strip_prefix("~/").or_else(|| {
            if cfg!(windows) {
                project.strip_prefix("~\\")
            } else {
                None
            }
        })
    };
    if let Some(relative) = home_relative {
        let home = std::env::home_dir()
            .ok_or_else(|| "project home directory is not available".to_owned())?;
        return Ok(home.join(relative.trim_start_matches(std::path::is_separator)));
    }
    Ok(PathBuf::from(project))
}

pub(crate) async fn environment_inspect(
    state: &ServerState,
    params: ProjectParams,
) -> Result<Value, String> {
    let probe_project = params.project;
    let cwd = tokio::task::spawn_blocking(move || {
        let project_path = resolve_project_path(&probe_project)?;
        let cwd = std::fs::canonicalize(project_path)
            .map_err(|error| format!("project '{probe_project}' is not accessible: {error}"))?;
        if !cwd.is_dir() {
            return Err(format!("project '{probe_project}' is not a directory"));
        }
        Ok::<_, String>(cwd)
    })
    .await
    .map_err(|error| format!("project probe task failed: {error}"))??;
    let toolchain = cached_toolchain(state).await?;
    let pi = if params.pi_runtime.as_deref() == Some("local") {
        PiExecutableInfo {
            executable: String::new(),
            version: String::new(),
        }
    } else {
        probe_pi_executable(state, params.pi_executable.as_deref()).await?
    };
    let git_branch = if toolchain.git_executable.is_empty() {
        None
    } else {
        let value = command_text(
            &toolchain.git_executable,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            Some(&cwd),
            &toolchain.path,
        )
        .await
        .unwrap_or_default();
        (!value.is_empty()).then_some(value)
    };
    to_value(EnvironmentInfo {
        cwd: cwd.to_string_lossy().into_owned(),
        path: toolchain.path.clone(),
        home: toolchain.home.clone(),
        shell: toolchain.shell.clone(),
        pi_executable: pi.executable,
        pi_version: pi.version,
        node_executable: toolchain.node_executable.clone(),
        node_version: toolchain.node_version.clone(),
        git_executable: toolchain.git_executable.clone(),
        git_version: toolchain.git_version.clone(),
        git_branch,
    })
}

pub(crate) async fn environment_pi_probe(
    state: &ServerState,
    params: PiProbeParams,
) -> Result<Value, String> {
    to_value(probe_pi_executable(state, params.executable.as_deref()).await?)
}

pub(crate) async fn environment_droid_probe(
    state: &ServerState,
    params: PiProbeParams,
) -> Result<Value, String> {
    let path = cached_login_path(state).await;
    // Probe exactly what a session would launch, so an empty field reports the
    // dk shim (or `dk run <droid>`) instead of only a bare `droid`.
    let launch = resolve_droid_launch(&path, params.executable.as_deref());
    if !Path::new(&launch.program).is_file() {
        return to_value(PiExecutableInfo {
            executable: String::new(),
            version: String::new(),
        });
    }
    let mut args = launch.prefix.iter().map(String::as_str).collect::<Vec<_>>();
    args.push("--version");
    let version = command_text(&launch.program, &args, None, &path)
        .await
        .unwrap_or_default();
    let executable = launch.program;
    to_value(PiExecutableInfo {
        executable,
        version,
    })
}

async fn probe_pi_executable(
    state: &ServerState,
    requested: Option<&str>,
) -> Result<PiExecutableInfo, String> {
    let requested = requested.map(str::trim).filter(|value| !value.is_empty());
    if requested.is_none() {
        let toolchain = cached_toolchain(state).await?;
        if toolchain.pi_executable.is_empty() {
            return Err("Pi executable 'pi' was not found in PATH".to_owned());
        }
        return Ok(PiExecutableInfo {
            executable: toolchain.pi_executable.clone(),
            version: toolchain.pi_version.clone(),
        });
    }

    let path = cached_login_path(state).await;
    let executable = resolve_program(&path, requested.expect("requested path checked"));
    let version = command_text(&executable, &["--version"], None, &path).await?;
    Ok(PiExecutableInfo {
        executable,
        version,
    })
}

pub(crate) fn default_shell() -> String {
    if cfg!(windows) {
        return std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_owned());
    }
    if let Ok(shell) = std::env::var("SHELL")
        && Path::new(&shell).is_file()
    {
        return shell;
    }
    if let (Ok(home), Ok(passwd)) = (
        std::env::var("HOME"),
        std::fs::read_to_string("/etc/passwd"),
    ) {
        for line in passwd.lines() {
            let fields = line.split(':').collect::<Vec<_>>();
            if fields.len() >= 7 && fields[5] == home && Path::new(fields[6]).is_file() {
                return fields[6].to_owned();
            }
        }
    }
    "/bin/sh".to_owned()
}

async fn login_path(shell: &str) -> Option<String> {
    if cfg!(windows) {
        return std::env::var("PATH").ok();
    }
    const MARKER: &str = "__PILO_LOGIN_PATH__";
    let mut command = Command::new(shell);
    command
        .args(["-ilc", &format!("printf '{MARKER}%s' \"$PATH\"")])
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .rfind(MARKER)
        .map(|index| stdout[index + MARKER.len()..].trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub(crate) async fn cached_login_path(state: &ServerState) -> String {
    state
        .login_path
        .get_or_init(|| async {
            let shell = default_shell();
            login_path(&shell)
                .await
                .unwrap_or_else(|| std::env::var("PATH").unwrap_or_default())
        })
        .await
        .clone()
}

pub(crate) async fn cached_toolchain(state: &ServerState) -> Result<&ToolchainInfo, String> {
    state
        .toolchain
        .get_or_try_init(|| async {
            let shell = default_shell();
            let path = cached_login_path(state).await;
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_default();
            let pi_executable = find_command(&path, "pi").unwrap_or_default();
            let node_executable = find_command(&path, "node").unwrap_or_default();
            let git_executable = find_command(&path, "git").unwrap_or_default();

            let pi_version_probe = async {
                if pi_executable.is_empty() {
                    Ok(String::new())
                } else {
                    command_text(&pi_executable, &["--version"], None, &path).await
                }
            };
            let node_version_probe = async {
                if node_executable.is_empty() {
                    String::new()
                } else {
                    command_text(&node_executable, &["--version"], None, &path)
                        .await
                        .unwrap_or_default()
                }
            };
            let git_version_probe = async {
                if git_executable.is_empty() {
                    String::new()
                } else {
                    command_text(&git_executable, &["--version"], None, &path)
                        .await
                        .unwrap_or_default()
                }
            };
            let (pi_version, node_version, git_version) =
                tokio::join!(pi_version_probe, node_version_probe, git_version_probe);

            Ok(ToolchainInfo {
                path,
                home,
                shell,
                pi_executable,
                pi_version: pi_version?,
                node_executable,
                node_version,
                git_executable,
                git_version,
            })
        })
        .await
}

fn find_command(path: &str, name: &str) -> Option<String> {
    let extensions = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned())
            .split(';')
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    std::env::split_paths(path).find_map(|dir| {
        if cfg!(windows)
            && let Some(candidate) = extensions.iter().find_map(|extension| {
                let candidate = dir.join(format!("{name}{extension}"));
                candidate.is_file().then_some(candidate)
            })
        {
            return Some(candidate.to_string_lossy().into_owned());
        }
        let candidate = dir.join(name);
        candidate
            .is_file()
            .then(|| candidate.to_string_lossy().into_owned())
    })
}

pub(crate) fn resolve_program(path: &str, program: &str) -> String {
    let candidate = Path::new(program);
    if candidate.is_absolute() || program.contains('/') || program.contains('\\') {
        return program.to_owned();
    }
    find_command(path, program).unwrap_or_else(|| program.to_owned())
}

/// Program and leading arguments used to start Droid.
///
/// `prefix` is empty when `program` is the CLI itself (`droid` or the
/// `droid-dk` drop-in). `dk run <droid>` carries the native CLI path in
/// `prefix` so dk can inject its managed key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DroidLaunch {
    pub(crate) program: String,
    pub(crate) prefix: Vec<String>,
}

/// Resolve the Droid process for a connection PATH.
///
/// An explicit override is launched as-is, including the name `droid`, which
/// opts out of dk. Otherwise dk 1.1.0's `droid-dk` shim is the program: it is
/// the GUI entry (`dk run droid` with the caller's arguments) and does not
/// require a separate `droid` file on PATH. If the shim is absent but both
/// `dk` and `droid` are present, launch `dk run <droid>`. Anything else uses
/// the native `droid` CLI. Pilo does not read `FACTORY_API_KEY`.
pub(crate) fn resolve_droid_launch(path: &str, override_executable: Option<&str>) -> DroidLaunch {
    if let Some(requested) = override_executable
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return DroidLaunch {
            program: resolve_program(path, requested),
            prefix: Vec::new(),
        };
    }
    if let Some(shim) = find_command(path, "droid-dk") {
        return DroidLaunch {
            program: shim,
            prefix: Vec::new(),
        };
    }
    let droid = find_command(path, "droid");
    if let Some(droid) = droid.as_deref()
        && let Some(dk) = find_command(path, "dk")
    {
        return DroidLaunch {
            program: dk,
            prefix: vec!["run".to_owned(), droid.to_owned()],
        };
    }
    DroidLaunch {
        program: droid.unwrap_or_else(|| "droid".to_owned()),
        prefix: Vec::new(),
    }
}

pub(crate) fn process_command(program: &str, args: &[String], path: &str) -> Command {
    let mut command = Command::new(program);
    command.args(args).env("PATH", path);
    command
}

#[cfg(windows)]
async fn command_text(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    path: &str,
) -> Result<String, String> {
    let program = program.to_owned();
    let args = args
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    let cwd = cwd.map(Path::to_path_buf);
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new(&program);
        command
            .args(&args)
            .env("PATH", path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("failed to run '{program}': {error}"))?;
        let started = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() < std::time::Duration::from_secs(8) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "timed out while running '{program} {}'",
                        args.join(" ")
                    ));
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("failed to wait for '{program}': {error}"));
                }
            }
        }
        let output = child
            .wait_with_output()
            .map_err(|error| format!("failed to collect '{program}' output: {error}"))?;
        command_output_text(&output.stdout, &output.stderr, output.status.success())
    })
    .await
    .map_err(|error| format!("tool probe task failed: {error}"))?
}

#[cfg(not(windows))]
async fn command_text(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    path: &str,
) -> Result<String, String> {
    let args = args
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    let mut command = process_command(program, &args, path);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = tokio::time::timeout(std::time::Duration::from_secs(8), command.output())
        .await
        .map_err(|_| format!("timed out while running '{program} {}'", args.join(" ")))?
        .map_err(|error| format!("failed to run '{program}': {error}"))?;
    command_output_text(&output.stdout, &output.stderr, output.status.success())
}

fn command_output_text(stdout: &[u8], stderr: &[u8], success: bool) -> Result<String, String> {
    if !success {
        return Err(String::from_utf8_lossy(stderr).trim().to_owned());
    }
    let text = if stdout.is_empty() { stderr } else { stdout };
    Ok(String::from_utf8_lossy(text).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::resolve_droid_launch;

    fn path_text(path: &std::path::Path) -> String {
        path.to_string_lossy().into_owned()
    }

    struct PathFixture {
        dir: PathBuf,
        path: String,
    }

    impl PathFixture {
        fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("pilo-droid-launch-{nanos}"));
            fs::create_dir_all(&dir).expect("temp dir");
            let path = std::env::join_paths([&dir])
                .expect("path")
                .to_string_lossy()
                .into_owned();
            Self { dir, path }
        }

        fn add(&self, name: &str) -> PathBuf {
            let file = self.dir.join(name);
            fs::write(&file, b"#!/bin/sh\n").expect("write command");
            file
        }
    }

    impl Drop for PathFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn droid_dk_shim_wins_when_dk_and_the_native_cli_are_also_installed() {
        let fixture = PathFixture::new();
        fixture.add("droid");
        fixture.add("dk");
        let shim = fixture.add("droid-dk");
        let launch = resolve_droid_launch(&fixture.path, None);
        assert_eq!(launch.program, path_text(&shim));
        assert!(launch.prefix.is_empty());
    }

    #[test]
    fn droid_dk_shim_does_not_require_a_native_cli_file() {
        let fixture = PathFixture::new();
        let shim = fixture.add("droid-dk");
        let launch = resolve_droid_launch(&fixture.path, None);
        assert_eq!(launch.program, path_text(&shim));
        assert!(launch.prefix.is_empty());
    }

    #[test]
    fn dk_runs_the_native_cli_when_the_shim_is_absent() {
        let fixture = PathFixture::new();
        let droid = fixture.add("droid");
        let dk = fixture.add("dk");
        let launch = resolve_droid_launch(&fixture.path, None);
        assert_eq!(launch.program, path_text(&dk));
        assert_eq!(launch.prefix, vec!["run".to_owned(), path_text(&droid)]);
    }

    #[test]
    fn droid_dk_shim_is_used_when_the_dk_command_is_absent() {
        let fixture = PathFixture::new();
        fixture.add("droid");
        let shim = fixture.add("droid-dk");
        let launch = resolve_droid_launch(&fixture.path, None);
        assert_eq!(launch.program, path_text(&shim));
        assert!(launch.prefix.is_empty());
    }

    #[test]
    fn native_droid_is_used_when_dk_is_not_installed() {
        let fixture = PathFixture::new();
        let droid = fixture.add("droid");
        let launch = resolve_droid_launch(&fixture.path, Some("  "));
        assert_eq!(launch.program, path_text(&droid));
        assert!(launch.prefix.is_empty());
    }

    #[test]
    fn explicit_droid_override_skips_dk() {
        let fixture = PathFixture::new();
        let droid = fixture.add("droid");
        fixture.add("dk");
        let launch = resolve_droid_launch(&fixture.path, Some("droid"));
        assert_eq!(launch.program, path_text(&droid));
        assert!(launch.prefix.is_empty());
    }

    #[test]
    fn dk_without_a_shim_or_native_cli_does_not_invent_a_launcher() {
        let fixture = PathFixture::new();
        fixture.add("dk");
        let launch = resolve_droid_launch(&fixture.path, None);
        assert_eq!(launch.program, "droid");
        assert!(launch.prefix.is_empty());
    }
}
