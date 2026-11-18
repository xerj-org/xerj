# `xerj search`: exact-phrase ranking

Measures whether a plain-words `xerj search` puts the file the question is
about at rank 1, on XERJ's own Rust sources as the corpus.

Three question sets, 100 each, generated deterministically by
`make_questions.py` from `engine/crates/**/*.rs` (seeded; the generated
`questions.json` is committed):

| set | query | expected file |
|---|---|---|
| `A_exact_string` | a string literal of 4+ words that occurs in exactly one file, as typed (an error message, an assertion text) | the file containing it |
| `B_words_of_symbol` | a `fn` name of 3+ snake_case parts defined in exactly one file, words joined by spaces (`cat pending tasks`) | the defining file |
| `C_identifier` | the same `fn` name verbatim (`cat_pending_tasks`) | the defining file |

## Run

```sh
# corpus: the .rs files under engine/crates at the commit being measured
mkdir corpus && git -C ../.. archive f9dcf626 engine/crates | tar -x -C corpus \
  && mv corpus/engine/crates corpus/crates && rmdir corpus/engine \
  && find corpus -type f ! -name '*.rs' -delete && find corpus -type d -empty -delete
python3 make_questions.py            # optional: regenerates questions.json byte-identically
python3 run.py /path/to/xerj 9330 main
```

`run.py` starts a throwaway `--insecure` loopback node (temp data dir and
HOME), autoindexes `./corpus`, and writes `results/<label>.json` with the
rank of the expected file among distinct `ax_path`s in the top 10.

## Results (2026-10-18, linux x86_64, 8 vCPU)

Corpus and questions from `main` at f9dcf626. `main` is a release build of
that commit, `fix` is the same commit plus the exact-phrase leg.

| set | build | rank 1 | top 5 | top 10 |
|---|---|---:|---:|---:|
| A_exact_string | main | 16 | 36 | 45 |
| A_exact_string | fix | 91 | 95 | 95 |
| B_words_of_symbol | main | 13 | 31 | 36 |
| B_words_of_symbol | fix | 15 | 32 | 38 |
| C_identifier | main | 92 | 92 | 92 |
| C_identifier | fix | 92 | 92 | 92 |

Per question, 81 ranks improved and one got worse (`B`: "a rare term
outweighs a term every document shares", rank 5 → 6). Raw rows are in
`results/`.
