//! `AgentWorkspace` OS sandbox policy construction for ACP subprocesses.
//!
//! Maps user-authorized project workspace folders to writable roots, carves out
//! `.git` metadata (including worktree `gitdir` and `commondir` targets) as
//! read-only, denies `<app_data_root>` completely, and configures fail-closed
//! execution with Landlock fallback disabled.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use mera_core::project::WorkspaceFolder;
use mera_sandbox::SandboxPolicy;

fn canonicalize_best_effort(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn resolve_git_pointer_file(git_file: &Path, worktree_root: &Path) -> Vec<PathBuf> {
    let Ok(contents) = std::fs::read_to_string(git_file) else {
        return Vec::new();
    };
    let mut resolved = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        let Some(raw_target) = trimmed.strip_prefix("gitdir:") else {
            continue;
        };
        let target_str = raw_target.trim();
        if target_str.is_empty() {
            continue;
        }
        let target_path = Path::new(target_str);
        let abs_gitdir = if target_path.is_absolute() {
            target_path.to_path_buf()
        } else {
            worktree_root.join(target_path)
        };
        let canon_gitdir = canonicalize_best_effort(&abs_gitdir);
        resolved.push(canon_gitdir.clone());

        let commondir_file = canon_gitdir.join("commondir");
        if let Ok(common_raw) = std::fs::read_to_string(&commondir_file) {
            let common_rel = common_raw.trim();
            if !common_rel.is_empty() {
                let common_path = if Path::new(common_rel).is_absolute() {
                    PathBuf::from(common_rel)
                } else {
                    canon_gitdir.join(common_rel)
                };
                resolved.push(canonicalize_best_effort(&common_path));
            }
        }
    }
    resolved
}

/// Build the unified fail-closed [`SandboxPolicy`] for an `AgentWorkspace` subprocess.
///
/// - `folders` become `read_write_paths`.
/// - Every workspace folder's `.git` entry (whether a directory, absent path, or
///   worktree file pointing to a `gitdir` / `commondir`) is carved out as read-only.
/// - `app_data_root` is added to `deny_paths` (blocked for both reads and writes).
/// - `fail_closed = true`, `allow_landlock_fallback = false`, `NetworkPolicy::FullAccess`.
#[must_use]
pub fn build_agent_workspace_sandbox_policy(
    folders: &[WorkspaceFolder],
    app_data_root: &Path,
) -> SandboxPolicy {
    let mut rw_set = BTreeSet::new();
    let mut ro_carveout_set = BTreeSet::new();

    for folder in folders {
        let raw_path = PathBuf::from(folder.path());
        if raw_path.as_os_str().is_empty() {
            continue;
        }
        let folder_root = canonicalize_best_effort(&raw_path);
        rw_set.insert(folder_root.clone());

        let dot_git = folder_root.join(".git");
        ro_carveout_set.insert(dot_git.clone());
        if dot_git.is_file() {
            for target in resolve_git_pointer_file(&dot_git, &folder_root) {
                ro_carveout_set.insert(target);
            }
        }
    }

    let deny_paths = if app_data_root.as_os_str().is_empty() {
        Vec::new()
    } else {
        vec![canonicalize_best_effort(app_data_root)]
    };

    SandboxPolicy::for_agent_workspace(
        rw_set.into_iter().collect(),
        ro_carveout_set.into_iter().collect(),
        deny_paths,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mera_core::model::WorkspaceFolderId;
    use mera_sandbox::{
        NetworkPolicy, SandboxExecRequest, SandboxProfileKind, TerminationReason, run_sandboxed,
    };

    #[test]
    fn builds_policy_with_git_worktree_carveouts_and_app_data_deny() {
        let temp = tempfile::tempdir().unwrap();
        let main_repo = temp.path().join("main-repo");
        let worktree_gitdir = main_repo.join(".git/worktrees/wt1");
        std::fs::create_dir_all(&worktree_gitdir).unwrap();
        std::fs::write(worktree_gitdir.join("commondir"), "../..\n").unwrap();

        let worktree = temp.path().join("wt1");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", worktree_gitdir.display()),
        )
        .unwrap();

        let app_data = temp.path().join("app-data");
        std::fs::create_dir_all(&app_data).unwrap();

        let folders = vec![WorkspaceFolder::new(
            WorkspaceFolderId::new("f-wt"),
            worktree.to_str().unwrap(),
        )];
        let policy = build_agent_workspace_sandbox_policy(&folders, &app_data);

        assert_eq!(policy.profile, SandboxProfileKind::AgentWorkspace);
        assert_eq!(policy.network, NetworkPolicy::FullAccess);
        assert!(policy.fail_closed);
        assert!(!policy.allow_landlock_fallback);
        assert!(
            policy
                .read_write_paths
                .contains(&canonicalize_best_effort(&worktree))
        );
        assert!(
            policy
                .read_only_carveouts
                .contains(&canonicalize_best_effort(&worktree).join(".git"))
        );
        assert!(
            policy
                .read_only_carveouts
                .contains(&canonicalize_best_effort(&worktree_gitdir))
        );
        assert!(
            policy
                .read_only_carveouts
                .contains(&canonicalize_best_effort(&main_repo.join(".git")))
        );
        assert_eq!(policy.deny_paths, vec![canonicalize_best_effort(&app_data)]);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn seatbelt_enforces_agent_workspace_git_ro_and_app_data_deny() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("ws");
        let git_dir = workspace.join(".git");
        let app_data = workspace.join(".mera-app-data");
        std::fs::create_dir_all(git_dir.join("hooks")).unwrap();
        std::fs::create_dir_all(&app_data).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(app_data.join("secret.txt"), "top-secret\n").unwrap();

        let folders = vec![WorkspaceFolder::new(
            WorkspaceFolderId::new("f1"),
            workspace.to_str().unwrap(),
        )];
        let policy = build_agent_workspace_sandbox_policy(&folders, &app_data);

        let ok_file = workspace.join("src.txt");
        let git_hook = git_dir.join("hooks/pre-commit");
        let secret_file = app_data.join("secret.txt");
        let app_data_probe = app_data.join("evil.txt");

        let script = format!(
            "echo ok > '{}' && ! (echo bad > '{}') && ! cat '{}' >/dev/null 2>&1 && ! (echo bad > '{}')",
            ok_file.display(),
            git_hook.display(),
            secret_file.display(),
            app_data_probe.display(),
        );
        let out = run_sandboxed(
            SandboxExecRequest::new(policy, "/bin/sh").with_args(["-c".into(), script.into()]),
        )
        .await
        .expect("run sandboxed agent workspace check");

        assert_eq!(out.termination_reason, TerminationReason::Exited);
        assert!(
            out.exit_status.is_some_and(|s| s.success()),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(std::fs::read_to_string(&ok_file).unwrap(), "ok\n");
        assert!(!git_hook.exists());
        assert!(!app_data_probe.exists());
    }
}
