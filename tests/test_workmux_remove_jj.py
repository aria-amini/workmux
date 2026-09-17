"""Tests for `workmux remove` semantics that are specific to jj workspaces.

jj's `workspace forget` abandons an empty `@` but keeps a non-empty one as
a head commit, so working-copy changes survive removal: `workmux remove`
must not block on dirty workspaces for jj. Removing a name that resolves
to the primary workspace (e.g. a bookmark checked out at the repo root)
must fail without touching the primary checkout.
"""

import subprocess
from pathlib import Path

import pytest

from .conftest import (
    TmuxEnvironment,
    run_workmux_command,
    write_workmux_config,
)
from .support.jj_repo import (
    assert_jj_workspace_exists,
    assert_jj_workspace_removed,
    setup_jj_repo,
    skip_if_jj_unavailable,
)


@pytest.fixture
def jj_colocated_repo_path(mux_server: TmuxEnvironment) -> Path:
    skip_if_jj_unavailable()
    path = mux_server.tmp_path / "jj_remove_colocated_repo"
    path.mkdir()
    setup_jj_repo(path, colocate=True, env_vars=mux_server.env)
    write_workmux_config(path, base_branch="main")
    return path


def jj(env: TmuxEnvironment, repo: Path, *args: str) -> str:
    result = subprocess.run(
        ["jj", "--no-pager", "--color=never", *args],
        cwd=repo,
        env=env.env,
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout


def worktree_path(repo: Path, name: str) -> Path:
    return repo.parent / f"{repo.name}__worktrees" / name


@pytest.mark.tmux_only
class TestRemoveDirtyWorkspace:
    """Removing a jj workspace with working-copy changes needs no --force."""

    def test_removes_and_preserves_changes(
        self,
        mux_server: TmuxEnvironment,
        workmux_exe_path,
        jj_colocated_repo_path,
    ):
        env = mux_server
        repo = jj_colocated_repo_path
        name = "jj-dirty-remove"

        run_workmux_command(env, workmux_exe_path, repo, f"add {name} --background")
        assert_jj_workspace_exists(env, repo, name)

        ws = worktree_path(repo, name)
        (ws / "wip.txt").write_text("unfinished work\n")
        # Snapshot the edit into the workspace's @ before removal; only
        # snapshotted changes survive the forget.
        jj(env, ws, "status")

        result = run_workmux_command(env, workmux_exe_path, repo, f"remove {name}")

        assert result.exit_code == 0, result.stderr
        assert not ws.exists(), "Workspace directory should be removed"
        assert_jj_workspace_removed(env, repo, name)
        assert "had working-copy changes" in result.stdout

        head_ids = jj(
            env,
            repo,
            "log",
            "-r",
            "heads(all())",
            "--no-graph",
            "-T",
            'commit_id ++ "\\n"',
        ).split()
        diffs = [jj(env, repo, "diff", "-r", id, "--git") for id in head_ids]
        assert any("unfinished work" in diff for diff in diffs), (
            f"Working-copy changes should survive in a commit head:\n{head_ids}"
        )

    def test_remove_force_still_removes(
        self,
        mux_server: TmuxEnvironment,
        workmux_exe_path,
        jj_colocated_repo_path,
    ):
        env = mux_server
        repo = jj_colocated_repo_path
        name = "jj-dirty-force"

        run_workmux_command(env, workmux_exe_path, repo, f"add {name} --background")
        ws = worktree_path(repo, name)
        (ws / "wip.txt").write_text("wip\n")
        jj(env, ws, "status")

        result = run_workmux_command(env, workmux_exe_path, repo, f"remove -f {name}")

        assert result.exit_code == 0, result.stderr
        assert not ws.exists()
        assert_jj_workspace_removed(env, repo, name)


@pytest.mark.tmux_only
class TestRemoveRefusesPrimaryWorkspace:
    """A name resolving to the primary workspace (bookmark on the repo
    root's @) must fail loudly instead of removing the main checkout."""

    def test_bookmark_on_primary_at_errors(
        self,
        mux_server: TmuxEnvironment,
        workmux_exe_path,
        jj_colocated_repo_path,
    ):
        env = mux_server
        repo = jj_colocated_repo_path
        bookmark = "on-primary"

        # Move a bookmark onto the primary workspace's @, the way a
        # colocated repo's HEAD points at the root checkout.
        jj(env, repo, "bookmark", "set", bookmark)

        result = run_workmux_command(
            env,
            workmux_exe_path,
            repo,
            f"remove {bookmark}",
            expect_fail=True,
        )

        assert "main worktree" in result.stderr, result.stderr
        assert_jj_workspace_exists(env, repo, "default")
        assert repo.is_dir()
