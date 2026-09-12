use anyhow::{Context, Result, anyhow};

use crate::{config, git, workflow};

pub fn run(name: Option<&str>) -> Result<()> {
    let name_to_rebase = super::resolve_name(name)?;
    let config = config::Config::load(None)?;
    let cwd = std::env::current_dir().context("Failed to determine current directory")?;
    let vcs = crate::vcs::detect::detect_backend_in(&cwd)?;

    let (worktree_path, branch_to_rebase) =
        workflow::meta::find_workspace_in(vcs.as_ref(), &name_to_rebase, Some(&cwd)).map_err(
            |_| {
                anyhow!(
                    "Worktree '{}' not found. Use 'workmux list' to see available worktrees.",
                    name_to_rebase
                )
            },
        )?;

    let main_branch = if let Some(ref branch) = config.main_branch {
        branch.clone()
    } else {
        let main_root = vcs
            .get_main_worktree_root_in(Some(&worktree_path))
            .context("Could not find the main worktree")?;
        vcs.get_default_branch_in(Some(&main_root))
            .context("Failed to determine the main branch")?
    };

    let base_branch = match vcs
        .meta()
        .get_branch_base(&branch_to_rebase, Some(&worktree_path))
    {
        Some(base) if vcs.local_branch_exists_in(&base, Some(&worktree_path))? => base,
        _ => main_branch,
    };

    if branch_to_rebase == base_branch {
        return Err(anyhow!(
            "Cannot rebase branch '{}' onto itself.",
            branch_to_rebase
        ));
    }

    if !vcs.local_branch_exists_in(&base_branch, Some(&worktree_path))? {
        return Err(anyhow!("Base branch '{}' does not exist", base_branch));
    }

    println!("Rebasing '{}' onto '{}'...", branch_to_rebase, base_branch);

    if vcs.name() == "jj" {
        // jj records conflicts in the rebased commits instead of stopping;
        // surface them as an error while leaving the conflicted commits in
        // place for `jj resolve` (undoing here would discard the user's
        // explicit rebase request).
        let mut command = std::process::Command::new("jj");
        command
            .arg("--no-pager")
            .arg("--color=never")
            .arg("--ignore-immutable")
            .args(["rebase", "-d", &format!("bookmarks(exact:{base_branch})")])
            .current_dir(&worktree_path);
        let output = command.output().context("Failed to execute jj rebase")?;
        if !output.status.success() {
            return Err(anyhow!(
                "jj rebase failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        check_jj_rebase_conflicts(&worktree_path)?;
    } else {
        git::rebase_branch_onto_base(&worktree_path, &base_branch).with_context(|| {
            format!(
                "Rebase failed, likely due to conflicts.\n\n\
                Please resolve them manually inside the worktree at '{}'.\n\
                Then, run 'git rebase --continue' to proceed or 'git rebase --abort' to cancel.",
                worktree_path.display()
            )
        })?;
    }
    println!("✓ Rebased '{}' onto '{}'", branch_to_rebase, base_branch);

    Ok(())
}

/// Whether the workspace's `@` carries conflicts after `jj rebase`.
fn check_jj_rebase_conflicts(worktree_path: &std::path::Path) -> Result<()> {
    let output = std::process::Command::new("jj")
        .arg("--no-pager")
        .arg("--color=never")
        .arg("--ignore-working-copy")
        .args([
            "log",
            "--no-graph",
            "-r",
            "@",
            "-T",
            r#"if(conflict,"1","0")"#,
        ])
        .current_dir(worktree_path)
        .output()
        .context("Failed to inspect jj rebase result")?;
    if !output.status.success() {
        return Err(anyhow!(
            "jj log failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if String::from_utf8_lossy(&output.stdout).trim() == "1" {
        return Err(anyhow!(
            "Rebase produced conflicts, recorded in the rebased commits.\n\n\
            Resolve them in the workspace at '{}' with 'jj resolve' or 'jj diffedit', then continue.",
            worktree_path.display()
        ));
    }
    Ok(())
}
