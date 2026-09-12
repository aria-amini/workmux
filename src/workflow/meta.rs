//! Backend-agnostic helpers for per-workspace metadata and lookup.
//!
//! The git free functions in `crate::git::worktree` read and write the
//! `workmux.worktree.<handle>.*` namespace in git config; the jj backend
//! keeps the same keys in `.jj/workmux/metadata.toml` via
//! [`crate::vcs::VcsBackend::meta`]. Workflows that must work against both
//! backends call these helpers instead of the git free functions: for a
//! [`crate::vcs::GitBackend`] repo they are exact delegations, for a
//! [`crate::vcs::JjBackend`] repo they route to the TOML store.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::config::MuxMode;
use crate::git::WorktreeAttachment;
use crate::vcs::VcsBackend;

/// What [`crate::git::find_worktree_in`] reports for a worktree with no
/// branch checked out; reused so backend-agnostic callers render the same
/// placeholder for git and jj.
const DETACHED_BRANCH: &str = "(detached)";

/// Find a workspace by handle (directory basename) or branch/bookmark,
/// trying the handle first — the backend-agnostic analog of
/// `crate::git::find_worktree_in`, built on
/// [`crate::vcs::VcsBackend::list_workspaces_in`].
pub fn find_workspace_in(
    vcs: &dyn VcsBackend,
    name: &str,
    workdir: Option<&Path>,
) -> Result<(PathBuf, String)> {
    let entries = vcs.list_workspaces_in(workdir)?;

    for entry in &entries {
        if let Some(dir_name) = entry.path.file_name()
            && dir_name.to_string_lossy() == name
        {
            return Ok((
                entry.path.clone(),
                entry
                    .branch_or_bookmark
                    .clone()
                    .unwrap_or_else(|| DETACHED_BRANCH.to_string()),
            ));
        }
    }

    for entry in &entries {
        if let Some(branch) = &entry.branch_or_bookmark
            && branch == name
        {
            return Ok((entry.path.clone(), branch.clone()));
        }
    }

    Err(crate::git::WorktreeNotFound(name.to_string()).into())
}

pub fn set_meta(
    vcs: &dyn VcsBackend,
    handle: &str,
    key: &str,
    value: &str,
    workdir: Option<&Path>,
) -> Result<()> {
    vcs.meta().set(handle, key, value, workdir)
}

pub fn get_attachment(
    vcs: &dyn VcsBackend,
    handle: &str,
    workdir: Option<&Path>,
) -> WorktreeAttachment {
    match vcs.meta().get(handle, "attachment", workdir).as_deref() {
        Some("headless") => WorktreeAttachment::Headless,
        Some("multiplexer") => WorktreeAttachment::Multiplexer,
        Some(_) => WorktreeAttachment::Unknown,
        None => WorktreeAttachment::Legacy,
    }
}

pub fn set_attachment(
    vcs: &dyn VcsBackend,
    handle: &str,
    attachment: WorktreeAttachment,
    workdir: Option<&Path>,
) -> Result<()> {
    let value = attachment.as_meta_value()?;
    set_meta(vcs, handle, "attachment", value, workdir)
}

pub fn get_mode_opt(vcs: &dyn VcsBackend, handle: &str, workdir: Option<&Path>) -> Option<MuxMode> {
    match vcs.meta().get(handle, "mode", workdir).as_deref() {
        Some("session") => Some(MuxMode::Session),
        Some("window") => Some(MuxMode::Window),
        _ => None,
    }
}

pub fn get_target_window(
    vcs: &dyn VcsBackend,
    handle: &str,
    workdir: Option<&Path>,
) -> Option<String> {
    vcs.meta().get(handle, "target-window", workdir)
}

pub fn get_target_session(
    vcs: &dyn VcsBackend,
    handle: &str,
    workdir: Option<&Path>,
) -> Option<String> {
    vcs.meta().get(handle, "target-session", workdir)
}

pub fn get_window_session(
    vcs: &dyn VcsBackend,
    handle: &str,
    workdir: Option<&Path>,
) -> Option<String> {
    vcs.meta().get(handle, "window-session", workdir)
}

pub fn get_window_token(
    vcs: &dyn VcsBackend,
    handle: &str,
    workdir: Option<&Path>,
) -> Option<String> {
    vcs.meta().get(handle, "window-token", workdir)
}

/// Return the workspace's window token, generating and persisting one when
/// absent — the backend-agnostic analog of
/// `crate::git::ensure_worktree_window_token_in` (same 16-byte hex format).
pub fn ensure_window_token(
    vcs: &dyn VcsBackend,
    handle: &str,
    workdir: Option<&Path>,
) -> Result<String> {
    if let Some(token) = get_window_token(vcs, handle, workdir) {
        return Ok(token);
    }

    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).context("Failed to generate worktree window token")?;
    let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    set_meta(vcs, handle, "window-token", &token, workdir)?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    #[test]
    fn find_workspace_matches_git_find_worktree_for_git_repo() {
        let temp = tempfile::tempdir().unwrap();
        test_support::init_repo(temp.path());
        let vcs = crate::vcs::detect::detect_backend_in(temp.path()).unwrap();

        let worktree_path = temp.path().join("feature-wt");
        vcs.create_workspace_in(
            &crate::vcs::CreateWorkspaceOptions {
                path: worktree_path.clone(),
                name_or_branch: "feature".to_string(),
                create_branch: true,
                base: Some("main".to_string()),
                track_upstream: false,
            },
            Some(temp.path()),
        )
        .unwrap();

        // By handle.
        assert_eq!(
            find_workspace_in(vcs.as_ref(), "feature-wt", Some(temp.path())).unwrap(),
            (worktree_path.clone(), "feature".to_string())
        );
        // By branch.
        assert_eq!(
            find_workspace_in(vcs.as_ref(), "feature", Some(temp.path())).unwrap(),
            (worktree_path.clone(), "feature".to_string())
        );
        // Unknown name errors like the git free function.
        assert!(find_workspace_in(vcs.as_ref(), "nope", Some(temp.path())).is_err());

        let direct = crate::git::find_worktree_in("feature-wt", Some(temp.path())).unwrap();
        assert_eq!(direct, (worktree_path, "feature".to_string()));
    }

    #[test]
    fn meta_helpers_round_trip_for_both_backends() {
        for backend in ["git", "jj"] {
            let temp = tempfile::tempdir().unwrap();
            match backend {
                "git" => test_support::init_repo(temp.path()),
                "jj" => test_support::init_colocated_repo(temp.path()),
                _ => unreachable!(),
            }
            let vcs = crate::vcs::detect::detect_backend_in(temp.path()).unwrap();

            assert_eq!(get_mode_opt(vcs.as_ref(), "h", Some(temp.path())), None);
            set_meta(vcs.as_ref(), "h", "mode", "session", Some(temp.path())).unwrap();
            assert_eq!(
                get_mode_opt(vcs.as_ref(), "h", Some(temp.path())),
                Some(MuxMode::Session)
            );

            assert_eq!(
                get_attachment(vcs.as_ref(), "h", Some(temp.path())),
                WorktreeAttachment::Legacy
            );
            set_attachment(
                vcs.as_ref(),
                "h",
                WorktreeAttachment::Multiplexer,
                Some(temp.path()),
            )
            .unwrap();
            assert_eq!(
                get_attachment(vcs.as_ref(), "h", Some(temp.path())),
                WorktreeAttachment::Multiplexer
            );

            let token = ensure_window_token(vcs.as_ref(), "h", Some(temp.path())).unwrap();
            assert_eq!(token.len(), 32);
            // Second call must return the same token, not generate a new one.
            assert_eq!(
                ensure_window_token(vcs.as_ref(), "h", Some(temp.path())).unwrap(),
                token
            );
        }
    }
}
