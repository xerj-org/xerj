# Find the right installed skill by what you want to do

An agent with dozens or hundreds of installed skills (folders holding a
`SKILL.md`) has to pick one by the task in front of it. `xerj autoindex
--skills` finds the folders agents keep skills in and indexes each one, and
`xerj search` ranks the passages that match the task's words. Each folder goes
through the ordinary autoindex pipeline under its own prefix.

Start a node first (`xerj --data-dir ./data --insecure`), then, from the
project you are working in:

```sh
xerj autoindex --skills
```

It looks in these folders, in this order, and skips any that do not exist
without a word:

| Folder | Prefix |
|---|---|
| `.claude/skills` in the home directory | `skills-claude` |
| `.codex/skills` in the home directory | `skills-codex` |
| `.agents/skills` in the home directory | `skills-agents` |
| `.claude/skills` in the current folder | `skills-project` |

The home directory is `HOME`, or `USERPROFILE` when `HOME` is unset. A folder
reached twice (a project opened at the home directory) is indexed once. The
project slot follows the current folder: run it in another project and
`skills-project` is rebuilt from that project's skills.

Each folder it found is named first, then each run ends with its own
`xerj-done` line. On a test machine with 28 public plugin skills in the home
`.claude/skills`, 13 in `.agents/skills`, one in the project and no
`.codex/skills` (home directory shortened to `/home/you`):

```
skills: /home/you/.claude/skills -> --prefix skills-claude
skills: /home/you/.agents/skills -> --prefix skills-agents
skills: /home/you/proj/.claude/skills -> --prefix skills-project
xerj-done ok=true exit=3 reason=completed-with-junk wall=5.1s files=137 ...
xerj-done ok=true exit=3 reason=completed-with-junk wall=7.9s files=114 ...
xerj-done ok=true exit=0 reason=completed wall=2.5s files=7 ...
```

Exit 3 counts as success: an empty file was skipped. The command exits with
the worst of its runs' codes. With no folder found it exits 1 and lists the
folders it looked in. `--json` is refused, since each run would print its own
document.

Skills installed as links are skipped, as links are everywhere in autoindex.
A folder that holds links gets one note naming the flags that follow them
(`--follow-symlinks --follow-symlinks-outside-root`).

## Search

```sh
xerj search --prefix skills -k 3 "fill in a PDF form"
```

```
xerj search: these hits are lexical; this command does not consult the embedding.

─── /home/you/.agents/skills/pdf/SKILL.md:304  (score 2010.85)
─── /home/you/.agents/skills/pdf/SKILL.md:8  (score 2010.28)
─── /home/you/.claude/skills/claude-automation-recommender/references/mcp-servers.md:47  (score 2009.34)
```

(Passage text trimmed.) `--prefix skills` searches every folder at once. When
the hits come from more than one folder, each path is the full path: open the
`SKILL.md` in the folder it names. Hits from one folder start at that folder
(`hook-development/SKILL.md:116`), as does `--prefix skills-claude`, which
searches one folder.

Over MCP, the `xerj_search` tool takes `"prefix": "skills"` the same way. Its
hits, like those of `xerj search --json`, keep the short path; the hit's
`_index` (`skills-claude-docs`) names the folder's prefix.

## Skills kept somewhere else

Give the folder its own prefix under the same stem, and the same search finds
it:

```sh
xerj autoindex <folder> --prefix skills-<name>
```

`claude`, `codex`, `agents` and `project` are taken: `--skills` rebuilds
`skills-project` from the current folder, so a folder indexed under that name
by hand would be replaced.

If an earlier run indexed a folder under the bare `--prefix skills`, `--skills`
says so, because that index would show each skill twice:

```
note: --prefix skills already holds /home/you/.claude/skills, and `xerj search --prefix skills` searches it beside these folders, so a skill in both shows up twice. If it is one of the folders above: xerj autoindex remove --prefix skills
```

## Keeping it current

Re-run `xerj autoindex --skills` after skills change. Every file is hashed
again, but only changed files are indexed: a re-run with no change reported
`files=0` for each folder.

To keep it current without re-running, start with a watch:

```sh
xerj autoindex --skills --no-graph --watch
```

Each folder gets its own watch. On the test machine the first passes finished
in 12 s, and a line appended to one `SKILL.md` was searchable 4 s later. A
watch needs an index built with `--no-graph`: over folders a plain `--skills`
run indexed, each pass fails with a rebuild instruction, so remove those
prefixes first (`xerj autoindex remove --prefix skills-claude`, and so on).
Ctrl-C stops every watch.

## Honest scope

- The ranking is lexical: it matches words, not meaning. Use the words a skill
  would use about itself ("hook", "MCP server", "release notes").
- Each folder is its own index, and a word is weighed against the folder it is
  in, so scores from two folders compare less well than scores within one. On
  78 skills split over three folders, with 20 task queries, the right skill was
  first 15 times and in the top 3 all 20 times; the same skills in one folder
  were first 18 times and in the top 3 all 20 times.
- A search always returns its closest matches, even when no installed skill
  fits. Read the hit before acting on it.
- Once `xerj init` has run in a project, its `.claude/skills` holds XERJ's own
  teaching skill, which may rank high for skill-finding queries.
- Tested on Linux only. The numbers above come from one run on one machine and
  are not a benchmark.

The skills search part of [#2644](https://github.com/xerj-org/xerj/issues/2644).
