# skills-intent: finding a skill by task among 388

Issue #2644 asks that an intent query over a library of 200+ skills put the
right skill in the top 3 for a small labelled set, with the results committed
and the losses kept. This folder is that eval.

Run it with:

```sh
bash scripts/bench/skills-intent.sh
```

## What it does

1. Downloads one pinned commit of
   [alirezarezvani/claude-skills](https://github.com/alirezarezvani/claude-skills)
   (MIT licence, commit `e4a1dc3695908dd5a6c811cc76ab7aa4cf95f2f0`) and checks
   the archive's SHA-256. Nothing from it is committed here.
2. `install_fixture.py` installs every distinct skill flat into a throwaway
   home's `.claude/skills/<name>/`, the way a person installs skills one by
   one: 388 skills. The library also ships a `.gemini/skills`
   tree of links and a second copy of some skills one level deeper; those are
   skipped, and the shorter path wins when two folders share a name.
3. Starts a throwaway node, runs `xerj autoindex --skills` with `HOME` and
   `USERPROFILE` set to that home, from a folder with no project skills.
4. `score.py` runs `xerj search --prefix skills -k 50 --json` for each line of
   `queries.tsv`, folds the passages into skills (a hit's path starts with its
   skill folder), and records the expected skill's rank among the first 10
   skills, or `miss`.

## How the queries were written

A separate agent saw only the skill list (name, source path, description),
never ran a search and never saw a result. It took every 15th skill of the
sorted list (26 skills), swapped two for their neighbours because another
skill does the same job (`experiment-designer` beside `ab-test-setup`,
`stress-test` beside `roast`), and wrote one plain task sentence per skill
without the words of the skill's name. `queries.tsv` was frozen
(SHA-256 `0aab5ad10ecde191d2494184a9fbce00143401c61c905b38204c4800dcad1d44`)
before the first search ran, and has not changed since.

## Result

`results/2026-10-31.tsv` holds every rank and the top three skills each query
returned. On 2026-10-31 (xerj v1.0.0-rc.100 built from #2802, Linux x86_64
under WSL2, 16 cores):

```
xerj-done ok=true exit=3 reason=completed-with-junk wall=85.1s files=2551 records=23199
queries 26  top1 13/26  top3 22/26  top10 24/26  miss 2/26
```

The four that missed the top 3, kept as they are:

| Rank | Expected | Query |
|---|---|---|
| 6 | `linkedin-engagement` | Draft connection request notes and DMs for prospects on the professional networking site |
| 7 | `extract` | Package the bug fix pattern we solved last week into its own agent skill |
| miss | `arquiteto-de-empresa` | Document how my whole startup works as markdown files in a git repo |
| miss | `cco-review` | Poke holes in our plan to cut customer success headcount and resegment accounts |

## What this does not show

- The ranking is lexical: it matches words, not meaning. Each of the four
  that missed the top 3 words the task differently from the skill: "customer
  success" where the skill says "CS", "the professional networking site"
  where it says "LinkedIn", "startup" and "git repo" where it says "company"
  and "version-controlled", "bug fix" where it says "debugging solution".
- One library, one query writer, 26 queries, one run. It shows the feature
  works at this size; it is not a general accuracy figure.
- Wall time is this machine's; rerun on yours for your own.
