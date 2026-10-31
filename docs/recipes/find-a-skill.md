# Find the right installed skill by what you want to do

An agent with dozens or hundreds of installed skills (folders holding a
`SKILL.md`) has to pick one by the task in front of it. `xerj autoindex` turns
the skills folder into a searchable index, and `xerj search` ranks the passages
that match the task's words. Nothing here is skill-specific code: it is the
ordinary autoindex loop pointed at a skills folder under its own prefix.

Start a node first (`xerj --data-dir ./data --insecure`), then index the folder
once. Claude Code keeps user skills in `~/.claude/skills` and project skills in
`.claude/skills`; use whichever folder holds yours.

```sh
xerj autoindex ~/.claude/skills --prefix skills
```

On a folder of 29 public skills (144 files) this finished in 5.6 s with
`exit=3 reason=completed-with-junk`: one empty file was skipped, which counts as
success. `xerj autoindex` does not expand a leading `~` itself, so in a shell
that leaves it alone (for example `cmd.exe`) pass the full path.

Then search by the task, in plain words:

```sh
xerj search --prefix skills -k 3 "build a server that gives Claude new tools"
```

```
xerj search: these hits are lexical; this command does not consult the embedding.

─── build-mcp-server/SKILL.md:118  (score 1020.14)
─── build-mcp-server/SKILL.md:194  (score 1017.79)
─── build-mcp-server/SKILL.md:3  (score 1017.64)
```

(`-k 3` keeps the first three hits; without it the default is five. Passage
text trimmed.) Each hit's path starts with the skill's folder, so the
skill to open is `build-mcp-server/SKILL.md`. A hit can also come from a file
the skill bundles, such as `hook-development/references/patterns.md`; the
folder still names the skill.

Over MCP, the `xerj_search` tool takes the same prefix:
`{"query": "write a new skill", "prefix": "skills", "size": 3}` returned the
same three passages, in the same order, as
`xerj search --prefix skills -k 3 "write a new skill"`
(`skill-development/SKILL.md`, then two from `skill-creator/SKILL.md`).
`prefix` and `index` cannot be combined.

## A second skills folder

Each folder takes its own prefix. Indexing a project's `.claude/skills` under
`--prefix skills` after the user folder is refused before anything is written,
with a ready-to-copy command:

```
error: autoindex: this folder needs its own --prefix. Do not delete the other folder's indices.
```

```sh
xerj autoindex .claude/skills --prefix project-skills
xerj search --prefix project-skills "an interactive HTML page with controls"
```

`xerj search --prefix` takes one prefix, so search each folder's prefix in turn.

## Keeping it current

Re-run the same `xerj autoindex` command after skills change. Every file is
hashed again, but only changed files are indexed again: after one `SKILL.md`
was edited the re-run reported `files=1` (files indexed), and a re-run with no
change reported `files=0`. A skill removed from the folder is removed from the
index on the next run.

## Honest scope

- The ranking is lexical: it matches words, not meaning. Use the words a skill
  would use about itself ("hook", "MCP server", "release notes").
- A search always returns its closest matches. Asked about filling PDF forms,
  with no PDF skill installed, it still returned three unrelated passages. Read
  the hit before acting on it.
- This recipe makes no claim that a hit carries the skill's `name` or
  `description` as fields; it reads the passage and its path.
- Once `xerj init` has run in a project, that project's `.claude/skills` holds
  XERJ's own teaching skill, whose words ("skill", "search", "task") may rank
  it high for skill-finding queries under `project-skills`.
- The numbers above come from one run on one machine and are not a benchmark.

The skills search part of [#2644](https://github.com/xerj-org/xerj/issues/2644).
