use anyhow::{Context, Result, anyhow};

pub fn run(base: &str) -> Result<()> {
    let cwd = std::env::current_dir().context("Failed to determine current directory")?;
    let vcs = crate::vcs::detect::detect_backend_in(&cwd)?;

    if !vcs.branch_exists_in(base, Some(&cwd))? {
        return Err(anyhow!("Base reference '{}' does not exist", base));
    }

    let branch = vcs
        .get_current_branch_in(&cwd)?
        .context("Not on a branch or bookmark")?;

    if branch == base {
        return Err(anyhow!("Cannot set base branch to the current branch"));
    }

    vcs.meta()
        .set_branch_base(&branch, base, Some(&cwd))
        .with_context(|| format!("Failed to set base branch for '{}'", branch))?;

    println!("Set base branch for '{}' to '{}'", branch, base);
    Ok(())
}
