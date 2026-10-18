mod activity;
mod index;
mod io;
mod search;
mod watch;

use std::path::PathBuf;

pub(crate) use activity::session_activity;
pub(crate) use index::session_scan;
pub(crate) use io::{session_delete, session_discover, session_read};
pub(crate) use search::session_search;
pub(crate) use watch::{session_watch_start, session_watch_stop};

#[cfg(test)]
pub(crate) use index::{
    SESSION_INDEX_HEADER_BYTES, SESSION_INDEX_PREFIX_STAGES, SESSION_INDEX_TAIL_BYTES,
    SessionIndexSummary, scan_index, summarize_index_bytes,
};
#[cfg(test)]
pub(crate) use watch::{collect_session_change_paths, is_session_change};

pub(crate) fn factory_sessions_dir() -> Option<PathBuf> {
    std::env::var_os("FACTORY_SESSIONS_DIR")
        .or_else(|| std::env::var_os("DROID_SESSIONS_DIR"))
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".factory/sessions"))
        })
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(|home| PathBuf::from(home).join(".factory/sessions"))
        })
}

pub(crate) fn is_factory_session_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    normalized.contains("/.factory/sessions/")
}

/// Factory 把项目路径编成会话目录名：去掉一个前导 `/`，删掉 `:`，把 `/`（含 Windows `\`）
/// 换成 `-`，再加前缀 `-`。已对照 Windows 上的 droid 0.244：`C:\Users\me` → `-C-Users-me`。
/// UNC 按同样规则推断。
pub(crate) fn droid_session_dir_key(project: &str) -> String {
    let project = project.trim().replace('\\', "/").replace(':', "");
    let stripped = project.strip_prefix('/').unwrap_or(&project);
    format!("-{}", stripped.replace('/', "-"))
}

fn session_dir_key(project: &str) -> String {
    let project = project.trim();
    // Pi removes one leading separator, including for UNC paths.
    let normalized = project
        .strip_prefix('/')
        .or_else(|| project.strip_prefix('\\'))
        .unwrap_or(project);
    format!("--{}--", normalized.replace(['/', '\\', ':'], "-"))
}

fn agent_dir() -> Option<PathBuf> {
    std::env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".pi/agent")))
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".pi/agent"))
        })
}

#[cfg(test)]
mod tests {
    use super::{droid_session_dir_key, session_dir_key};

    #[test]
    fn droid_session_directory_keys_cover_posix_drive_and_unc_paths() {
        for (project, expected) in [
            (
                "/Users/enoreyes/code/work/myapp",
                "-Users-enoreyes-code-work-myapp",
            ),
            (r"D:\work", "-D-work"),
            ("D:/work", "-D-work"),
            (r"C:\Users\gaoxi\git\pilo", "-C-Users-gaoxi-git-pilo"),
            ("//host/share/work", "--host-share-work"),
            (r"\\host\share\work", "--host-share-work"),
        ] {
            assert_eq!(
                droid_session_dir_key(project),
                expected,
                "project={project:?}"
            );
        }
    }

    #[test]
    fn session_directory_keys_match_pi_for_drive_unc_and_posix_paths() {
        for (project, expected) in [
            (r"D:\work", "--D--work--"),
            ("D:/work", "--D--work--"),
            (r"\\host\share\work", "---host-share-work--"),
            ("//host/share/work", "---host-share-work--"),
            ("/home/user/work", "--home-user-work--"),
            (" /home/user/work ", "--home-user-work--"),
            ("/", "----"),
        ] {
            assert_eq!(session_dir_key(project), expected, "project={project:?}");
        }
    }
}
