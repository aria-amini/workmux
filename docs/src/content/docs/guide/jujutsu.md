---
title: "Jujutsu (jj)"
description: What workmux supports in a jj repository today, and which commands are still git-only
---

workmux has experimental support for [Jujutsu](https://jj-vcs.github.io/jj/) repositories. It is detected automatically: if the directory you run workmux in resolves to a jj repository (`jj git init`, with or without `--colocate`), workmux drives `jj` instead of `git`.

The vocabulary maps like this:

| git           | jj                                     |
| ------------- | -------------------------------------- |
| worktree      | workspace (`jj workspace add/forget`)  |
| branch        | bookmark (`jj bookmark create/delete`) |
| `HEAD`        | `@`, the working-copy commit           |
| `origin/main` | `main@origin`                          |

Per-worktree metadata that workmux keeps in `git config --local` for a git repository is instead stored in a TOML file at `.jj/workmux/metadata.toml` inside the primary workspace. It lives under `.jj/` on purpose: jj snapshots the working copy into `@` on almost every command, so anything workmux wrote at the workspace root would be committed into your own history.

## Supported commands

The daily lifecycle works end to end in a jj repository, driving `jj` workspaces instead of git worktrees:

- `workmux add` / `workmux remove` — create and tear down jj workspaces with a bookmark per workspace. New workspaces contain both `.jj` and `.git` through explicit `jj workspace add --colocate`, regardless of the `git.colocate` default.
- `workmux open` / `workmux close` — window lifecycle, with all window metadata stored in the workmux metadata file
- `workmux merge` — all three strategies map to jj primitives: `--rebase` (rebase the branch, then fast-forward the target bookmark), `--squash` (`jj squash` into the target head), and the default two-parent merge commit. Conflicts recorded by jj are detected and the operation is undone, leaving the repository untouched.
- `workmux rebase` / `workmux set-base`
- `workmux list` / `workmux status`
- `workmux dashboard` / `workmux sidebar` — worktree listing, agent status, dirty state, and stored-base editing
- `workmux resurrect`

## Known limitations

- **`workmux rename`** refuses in a jj repository: jj records each workspace's root path and cannot update it after a move. The error suggests the `remove` + `add` equivalent.
- **Pull-request flows** (`workmux add --pr`, the dashboard's PR actions and fork-remote setup) still operate on the git side. They work in a colocated repository (`.jj` + `.git`) and fail loudly in a jj-only one.
- **`rm --gone`** reports nothing for jj: gone-branch detection compares against git remote-tracking refs.
- **`workmux sandbox agent`** bails with an explicit error in a non-git repository, because it mounts the repository's git directories into the container.
- **Default merges create a merge commit with a fixed message** rather than opening an editor; amend the description with `jj describe` afterwards if you want something different.

## Notes

- Because jj snapshots the working copy on nearly every command, workmux stores its metadata at `.jj/workmux/metadata.toml` in the primary workspace — anything at the workspace root would be committed into your history.
- After `jj commit` advances `@` past the bookmark workmux created, workmux recovers the workspace's branch from the youngest bookmarked ancestor, so `list`, `merge`, and `rebase` keep working without manually re-pointing the bookmark.
