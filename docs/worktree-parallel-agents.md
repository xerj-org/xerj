# Parallel Agents in Git Worktrees (`--worktree shared|isolated`)

When parallel AI agents work across linked Git worktrees of the same repository (`git worktree add`, [worktrunk](https://github.com/max-sixty/worktrunk), or Copilot/Codex worktrees), running `xerj autoindex` without a worktree policy either collides on the default `--prefix ax` or duplicates the full repository index across every worktree.

`xerj autoindex <folder> --worktree shared|isolated` provides two deterministic local-disk modes:

## 1. Shared Mode (`--worktree shared`)

Use `--worktree shared` when parallel agents want one shared baseline index rooted at the repository's main checkout:

```bash
xerj autoindex . --worktree shared --no-graph
```

- Inspects `<folder>/.git`: when `<folder>` is a linked Git worktree (`gitdir: ...`), resolves `commondir` to the main repository checkout and indexes the main checkout instead of the worktree copy.
- When `--prefix` is omitted, defaults to `ax-<main-repo-slug>` (for example `ax-xerj`), so every worktree of that repository reads and updates the same corpus (`ax-xerj-docs`) and shares the same `~/.xerj/autoindex/<key>/` resume journal and `StateLock`.

## 2. Isolated Mode (`--worktree isolated`)

Use `--worktree isolated` when an agent is making large branch-local changes in its worktree and wants its own isolated index without colliding with sibling worktrees:

```bash
xerj autoindex . --worktree isolated --no-graph
```

- Indexes the worktree folder in place with its own resume journal.
- When `--prefix` is omitted, defaults to `ax-<main-repo-slug>-<worktree-slug>` (for example `ax-xerj-wt-auth`), preventing `foreign_prefix_roots` collisions and lock contention between parallel agents.
