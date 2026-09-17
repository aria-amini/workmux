//! jj implementation of the merge workflow.
//!
//! git drives merges from inside a checked-out worktree; jj operations are
//! repository-wide and only care about which workspace's `@` receives new
//! commits. Every command below is pinned to the workspace whose `@` it
//! must move, and strategy outcomes were verified against jj 0.45:
//!
//! - **rebase + fast-forward**: `jj rebase -d <target>` in the source
//!   workspace moves the whole branch (bookmarks follow the rewrite), then
//!   `jj bookmark set <target> -r <feature>` is a true fast-forward, then
//!   `jj new <target>` in the target workspace materializes the new state.
//! - **squash**: `jj squash --from '<branch set>' --into <target>` moves the
//!   combined branch diff into the target head commit; the source commits
//!   become empty (jj abandons them) and bookmarks follow the rewrite.
//! - **merge commit**: `jj new <target> <feature>` in the target workspace
//!   creates the two-parent merge commit as its `@`, then the target
//!   bookmark is moved onto it.
//!
//! Conflicts never stop mid-operation in jj: they are recorded in the
//! resulting commits. Each strategy therefore checks `conflict` on the
//! affected commit afterwards and, on conflict, `jj undo`s its own
//! operation so the repository is left exactly as before the attempt.
//!
//! `--ignore-immutable` is passed on history-rewriting operations: git has
//! no immutability concept, and workmux is an explicit user instruction to
//! merge, so default jj protection (e.g. a tracked `main@origin`) must not
//! silently block the requested operation.

use anyhow::{anyhow, Context, Result};
use std::path::Path;

use crate::vcs::jj_backend::quote;
use crate::vcs::VcsBackend;
use tracing::info;

use super::context::WorkflowContext;
use super::meta;
use super::types::MergeResult;

fn bookmark_revset(name: &str) -> String {
    format!("bookmarks(exact:{})", quote(name))
}

/// Run a `jj` command with the given workspace as working directory,
/// requiring success.
fn run_jj(workspace: &Path, args: &[&str]) -> Result<()> {
    let mut command = std::process::Command::new("jj");
    command.arg("--no-pager").arg("--color=never");
    command.current_dir(workspace);
    command.args(args);
    let output = command
        .output()
        .with_context(|| format!("Failed to execute jj {}", args.join(" ")))?;
    if !output.status.success() {
        return Err(anyhow!(
            "jj {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Whether the revision selected by `revset` is conflicted. Runs with
/// `--ignore-working-copy` so the check never snapshots (a snapshot would
/// append an operation and break the `jj undo` conflict recovery).
fn revset_conflicted(workspace: &Path, revset: &str) -> Result<bool> {
    let mut command = std::process::Command::new("jj");
    command.arg("--no-pager").arg("--color=never");
    command.arg("--ignore-working-copy").args([
        "log",
        "--no-graph",
        "-r",
        revset,
        "-T",
        r#"if(conflict,"1","0")"#,
    ]);
    command.current_dir(workspace);
    let output = command
        .output()
        .with_context(|| format!("Failed to execute jj log -r {revset}"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "jj log -r {revset} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim() == "1")
}

fn conflict_error(branch: &str, target: &str, workspace: &Path) -> anyhow::Error {
    anyhow!(
        "Merge produced conflicts that jj recorded in commits. The operation was undone.\n\n\
        To resolve, rebase the branch in the workspace at {}:\n\
          jj rebase -d {} --edit\n\n\
        Then retry the merge of '{}' or complete it with plain jj commands.",
        workspace.display(),
        target,
        branch,
    )
}

/// Merge the workspace/branch `name` into its target, then run the
/// backend-aware cleanup. Mirrors `super::merge::merge`'s resolution
/// order, with jj primitives for the strategy execution.
#[allow(clippy::too_many_arguments)]
pub(super) fn merge(
    name: &str,
    into_branch: Option<&str>,
    rebase: bool,
    squash: bool,
    keep: bool,
    ignore_uncommitted: bool,
    no_verify: bool,
    no_hooks: bool,
    notification: bool,
    context: &WorkflowContext,
) -> Result<MergeResult> {
    let vcs: &dyn VcsBackend = context.vcs.as_ref();

    context.chdir_to_main_worktree()?;

    let (worktree_path, branch_to_merge) =
        meta::find_workspace_in(vcs, name, Some(&context.execution_dir)).map_err(|_| {
            anyhow!(
                "Worktree '{}' not found. Use 'workmux list' to see available worktrees.",
                name
            )
        })?;

    if context.is_main_worktree(&worktree_path) {
        return Err(anyhow!(
            "Cannot merge branch '{}' because it is checked out in the main workspace at '{}'. \
             Create a linked workspace for '{}' first.",
            branch_to_merge,
            context.main_worktree_root.display(),
            branch_to_merge
        ));
    }

    let handle = worktree_path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| {
            anyhow!(
                "Could not derive handle from workspace path: {}",
                worktree_path.display()
            )
        })?;

    // Target branch: --into, else the stored base, else the main branch.
    let detected_base: Option<String> = if into_branch.is_some() {
        None
    } else {
        vcs.meta()
            .get_branch_base(branch_to_merge.as_str(), Some(&context.execution_dir))
            .filter(|base| {
                vcs.branch_exists_in(base, Some(&context.execution_dir))
                    .unwrap_or(false)
            })
    };

    let target_branch = into_branch
        .map(str::to_string)
        .or(detected_base)
        .unwrap_or_else(|| context.main_branch.clone());

    // Locate the workspace holding the target bookmark; fall back to the
    // primary workspace (the analog of git merging in the main worktree
    // when the target branch is checked out nowhere).
    let entries = vcs.list_workspaces_in(Some(&context.execution_dir))?;
    let target_worktree_path = entries
        .iter()
        .find(|entry| entry.branch_or_bookmark.as_deref() == Some(target_branch.as_str()))
        .map(|entry| entry.path.clone())
        .unwrap_or_else(|| context.main_worktree_root.clone());
    let target_window_name = if target_worktree_path == context.main_worktree_root {
        context.main_branch.clone()
    } else {
        target_worktree_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("Invalid workspace path for target branch"))?
            .to_string()
    };
    let attachment = meta::get_attachment(vcs, handle, Some(&context.execution_dir));

    // jj has no index: "uncommitted changes" is the diff of `@`.
    let source_dirty = !keep && vcs.get_status(&worktree_path, None)?.is_dirty;
    if source_dirty && !ignore_uncommitted {
        return Err(anyhow!(
            "Workspace for '{}' has uncommitted changes. Please describe or commit them, or use --ignore-uncommitted.",
            branch_to_merge
        ));
    }
    // jj has no staging area, so the git flow's "commit staged changes with
    // the editor" step has no equivalent: uncommitted work lives in `@` and
    // was either accepted (above) or refused.

    if branch_to_merge == target_branch {
        return Err(anyhow!(
            "Cannot merge branch '{}' into itself.",
            branch_to_merge
        ));
    }

    if !vcs.branch_exists_in(branch_to_merge.as_str(), Some(&context.execution_dir))? {
        return Err(anyhow!("Bookmark '{branch_to_merge}' does not exist"));
    }
    if !vcs.branch_exists_in(target_branch.as_str(), Some(&context.execution_dir))? {
        return Err(anyhow!("Bookmark '{target_branch}' does not exist"));
    }

    // The target workspace's `@` must be clean: rewriting the target head
    // (squash) or moving its bookmark (fast-forward) would strand those
    // changes the same way git refuses to merge into a dirty worktree.
    let target_status = vcs.get_status(&target_worktree_path, None)?;
    if target_status.is_dirty {
        return Err(anyhow!(
            "Target workspace ({}) has uncommitted changes. Please commit them before merging.",
            target_worktree_path.display()
        ));
    }

    let feature = bookmark_revset(branch_to_merge.as_str());
    let target = bookmark_revset(target_branch.as_str());

    super::merge::run_pre_merge_hooks(
        context,
        handle,
        branch_to_merge.as_str(),
        target_branch.as_str(),
        &worktree_path,
        no_verify,
        no_hooks,
    )?;

    if rebase {
        // 1. Rebase the whole source branch onto the target head, in the
        //    source workspace so its `@` follows the rewrite. Bookmarks
        //    pointing into the branch (including the feature bookmark)
        //    move with it.
        println!("Rebasing '{}' onto '{}'...", branch_to_merge, target_branch);
        run_jj(
            &worktree_path,
            &["rebase", "--ignore-immutable", "-d", target.as_str()],
        )?;
        if revset_conflicted(&worktree_path, "@")? {
            let _ = run_jj(&worktree_path, &["undo"]);
            return Err(conflict_error(
                branch_to_merge.as_str(),
                target_branch.as_str(),
                &worktree_path,
            ));
        }

        // 2. Fast-forward the target bookmark onto the rebased head.
        run_jj(
            &target_worktree_path,
            &[
                "bookmark",
                "set",
                target_branch.as_str(),
                "-r",
                feature.as_str(),
            ],
        )?;

        // 3. Materialize the new target state in the target workspace.
        run_jj(&target_worktree_path, &["new", target.as_str()])?;
        info!(branch = branch_to_merge, "merge:fast-forward complete");
    } else if squash {
        // Move the combined diff of the whole feature branch into the
        // target head commit. Source commits become empty and are
        // abandoned by jj; bookmarks follow the rewrite.
        let from = format!("(::{} ~ ::{})", feature, target);
        run_jj(
            &target_worktree_path,
            &[
                "squash",
                "--ignore-immutable",
                "--from",
                from.as_str(),
                "--into",
                target.as_str(),
                "--use-destination-message",
            ],
        )?;
        if revset_conflicted(&target_worktree_path, target.as_str())? {
            let _ = run_jj(&target_worktree_path, &["undo"]);
            return Err(conflict_error(
                branch_to_merge.as_str(),
                target_branch.as_str(),
                &target_worktree_path,
            ));
        }
        run_jj(&target_worktree_path, &["new", target.as_str()])?;
        info!(branch = branch_to_merge, "merge:squash merge complete");
    } else {
        // Default: a two-parent merge commit in the target workspace.
        let message = format!("Merge branch '{}' into {}", branch_to_merge, target_branch);
        run_jj(
            &target_worktree_path,
            &[
                "new",
                target.as_str(),
                feature.as_str(),
                "-m",
                message.as_str(),
            ],
        )?;
        if revset_conflicted(&target_worktree_path, "@")? {
            let _ = run_jj(&target_worktree_path, &["undo"]);
            return Err(conflict_error(
                branch_to_merge.as_str(),
                target_branch.as_str(),
                &target_worktree_path,
            ));
        }
        run_jj(
            &target_worktree_path,
            &["bookmark", "set", target_branch.as_str(), "-r", "@"],
        )?;
        info!(branch = branch_to_merge, "merge:standard merge complete");
    }

    if notification {
        super::merge::show_notification(&format!(
            "Merged '{}' into '{}'",
            branch_to_merge, target_branch
        ));
    }

    if keep {
        info!(branch = branch_to_merge, "merge:skipping cleanup");
        return Ok(MergeResult {
            branch_merged: branch_to_merge.clone(),
            main_branch: target_branch.clone(),
            had_staged_changes: false,
            cleanup_scheduled: false,
            cleanup_error: None,
        });
    }

    info!(branch = branch_to_merge, "merge:cleanup start");
    let cleanup_result = match super::cleanup::cleanup(
        context,
        branch_to_merge.as_str(),
        handle,
        &worktree_path,
        super::cleanup::CleanupOptions {
            force: true,
            keep_branch: false,
            no_hooks,
            show_hook_output: true,
        },
    ) {
        Ok(result) => result,
        Err(error) => {
            return Ok(MergeResult {
                branch_merged: branch_to_merge.clone(),
                main_branch: target_branch.clone(),
                had_staged_changes: false,
                cleanup_scheduled: false,
                cleanup_error: Some(error),
            });
        }
    };
    let cleanup_scheduled = cleanup_result.deferred_cleanup.is_some();
    let mode = meta::get_mode_opt(vcs, handle, Some(&context.execution_dir))
        .unwrap_or(crate::config::MuxMode::Window);
    let cleanup_error = if attachment.manages_mux() {
        super::cleanup::navigate_to_target_and_close(
            context.mux.as_ref(),
            &context.prefix,
            &target_window_name,
            handle,
            &cleanup_result,
            mode,
            context.config.default_session(),
        )
        .err()
    } else {
        None
    };

    Ok(MergeResult {
        branch_merged: branch_to_merge.clone(),
        main_branch: target_branch.clone(),
        had_staged_changes: false,
        cleanup_scheduled,
        cleanup_error,
    })
}
