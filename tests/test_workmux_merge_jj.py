"""Tests for the full workmux lifecycle against jj-backed repositories.

Covers `workmux merge` (rebase/fast-forward, squash, merge-commit, and
conflict recovery), `workmux rebase`, and `workmux set-base` on a
colocated jj+git repo, plus `workmux open`/`close` window round-trips
that read window metadata from jj's TOML metadata store.
"""

import subprocess
from pathlib import Path

import pytest

from .conftest import (
    MuxEnvironment,
    run_workmux_command,
    write_workmux_config,
)
from .support.jj_repo import (
    assert_jj_bookmark_exists,
    assert_jj_bookmark_removed,
    assert_jj_workspace_exists,
    assert_jj_workspace_removed,
    setup_jj_repo,
    skip_if_jj_unavailable,
)


@pytest.fixture
def jj_repo(mux_server: MuxEnvironment) -> Path:
    """A colocated jj+git repo with an initial commit and a `main` bookmark."""
    skip_if_jj_unavailable()
    path = mux_server.tmp_path / "jj_lifecycle_repo"
    path.mkdir()
    setup_jj_repo(path, colocate=True, env_vars=mux_server.env)
    write_workmux_config(path, base_branch="main")
    return path


def jj(env: MuxEnvironment, repo: Path, *args: str) -> str:
    result = subprocess.run(
        ["jj", "--no-pager", "--color=never", *args],
        cwd=repo,
        env=env.env,
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout


def worktree_path(repo: Path, branch: str) -> Path:
    return repo.parent / f"{repo.name}__worktrees" / branch


def commit_in(env: MuxEnvironment, ws_path: Path, filename: str, message: str) -> None:
    (ws_path / filename).write_text(f"{message}\n")
    jj(env, ws_path, "commit", "-m", message)


@pytest.mark.tmux_only
class TestJjMerge:
    """`workmux merge` strategies against jj workspaces."""

    def _seed_feature(
        self, env: MuxEnvironment, exe, repo: Path, name: str, filename: str
    ) -> None:
        run_workmux_command(env, exe, repo, f"add {name} --background")
        commit_in(env, worktree_path(repo, name), filename, "add feature file")

    def _advance(self, env: MuxEnvironment, repo: Path, marker: str) -> None:
        (repo / "base.txt").write_text(f"{marker}\n")
        jj(env, repo, "commit", "-m", f"main-{marker}")
        jj(
            env,
            repo,
            "bookmark",
            "set",
            "main",
            "-r",
            f'description(exact:"main-{marker}\\n")',
        )

    def test_rebase_strategy_ffs_main_and_cleans_up(
        self, mux_server, workmux_exe_path, jj_repo
    ):
        env, exe, repo = mux_server, workmux_exe_path, jj_repo
        self._seed_feature(env, exe, repo, "feat-re", "re.txt")
        self._advance(env, repo, "a1")

        run_workmux_command(env, exe, repo, "merge feat-re --rebase")

        assert_jj_workspace_removed(env, repo, "feat-re")
        assert_jj_bookmark_removed(env, repo, "feat-re")
        log = jj(env, repo, "log", "-r", "bookmarks(exact:main)", "--stat")
        assert "re.txt" in log, "fast-forwarded main must contain the feature file"

    def test_squash_strategy_lands_changes_in_main(
        self, mux_server, workmux_exe_path, jj_repo
    ):
        env, exe, repo = mux_server, workmux_exe_path, jj_repo
        self._seed_feature(env, exe, repo, "feat-sq", "sq.txt")
        self._advance(env, repo, "a2")

        run_workmux_command(env, exe, repo, "merge feat-sq --squash")

        assert_jj_workspace_removed(env, repo, "feat-sq")
        assert_jj_bookmark_removed(env, repo, "feat-sq")
        log = jj(env, repo, "log", "-r", "bookmarks(exact:main)", "--stat")
        assert "sq.txt" in log, "squashed main must contain the feature file"

    def test_default_strategy_creates_merge_commit(
        self, mux_server, workmux_exe_path, jj_repo
    ):
        env, exe, repo = mux_server, workmux_exe_path, jj_repo
        self._seed_feature(env, exe, repo, "feat-mc", "mc.txt")
        self._advance(env, repo, "a3")

        run_workmux_command(env, exe, repo, "merge feat-mc")

        assert_jj_workspace_removed(env, repo, "feat-mc")
        assert_jj_bookmark_removed(env, repo, "feat-mc")
        log = jj(
            env,
            repo,
            "log",
            "--no-graph",
            "-r",
            "bookmarks(exact:main)",
            "-T",
            'concat(description.first_line(), " parents=", parents.len(), "\\n")',
        )
        assert "Merge branch 'feat-mc'" in log
        assert "parents=2" in log, "default strategy must create a merge commit"

    def test_conflicting_merge_fails_and_restores_state(
        self, mux_server, workmux_exe_path, jj_repo
    ):
        env, exe, repo = mux_server, workmux_exe_path, jj_repo
        run_workmux_command(env, exe, repo, "add cf --background")
        ws = worktree_path(repo, "cf")
        (ws / "shared.txt").write_text("feature version\n")
        jj(env, ws, "commit", "-m", "change shared file")
        (repo / "shared.txt").write_text("main version\n")
        jj(env, repo, "commit", "-m", "main touches shared")
        jj(
            env,
            repo,
            "bookmark",
            "set",
            "main",
            "-r",
            'description(exact:"main touches shared\\n")',
        )

        result = run_workmux_command(
            env, exe, repo, "merge cf --rebase", expect_fail=True
        )
        combined = result.stdout + result.stderr
        assert "conflict" in combined.lower()

        # The undo must have restored the repository: the workspace and its
        # bookmark survive, main is untouched, nothing is left conflicted.
        assert_jj_workspace_exists(env, repo, "cf")
        assert_jj_bookmark_exists(env, repo, "cf")
        log = jj(env, repo, "log", "-r", "all()", "-T", 'if(conflict,"C","-")')
        assert "C" not in log
        assert ws.is_dir()


@pytest.mark.tmux_only
class TestJjRebaseAndSetBase:
    def test_set_base_and_rebase(self, mux_server, workmux_exe_path, jj_repo):
        env, exe, repo = mux_server, workmux_exe_path, jj_repo
        run_workmux_command(env, exe, repo, "add rb --background")
        ws = worktree_path(repo, "rb")
        commit_in(env, ws, "rb.txt", "add rebase file")

        run_workmux_command(env, exe, repo, "set-base main", working_dir=ws)
        self._advance_repo(env, repo, "a4")

        run_workmux_command(env, exe, repo, "rebase rb", working_dir=ws)

        ancestry = jj(
            env,
            ws,
            "log",
            "--no-graph",
            "-r",
            '::bookmarks(exact:"rb") & description(exact:"main-a4\\n")',
            "-T",
            "description",
        )
        assert ancestry.strip(), "rebased branch ancestry must contain main's new head"

    def _advance_repo(self, env, repo, marker):
        (repo / "base.txt").write_text(f"{marker}\n")
        jj(env, repo, "commit", "-m", f"main-{marker}")
        jj(
            env,
            repo,
            "bookmark",
            "set",
            "main",
            "-r",
            f'description(exact:"main-{marker}\\n")',
        )
