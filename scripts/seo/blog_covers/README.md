# `scripts/seo/blog_covers/`: the blog cover series

Every post on xerj.org/blog gets a cover from one series, the **engineering
field-log poster**, printed in the brand book's palette: light paper
(`#f6f4ee`, the day theme the `/blog/` index is served in) on a 1px square
grid, a square-matrix motif drawn from that post's own numbers in ink
(`#0B0B0D`), a mono kicker, the title in Big Shoulders Display 900, an ochre
(`#A06800`) subtitle and the post URL. Brand yellow (`#FFC400`) is earned: a
fill on the one result the post is about, never text. Flat: 1px lines, no
gradients, no grain, no shadows. The og PNG carries a 1px `#d6d4cb` frame so
it keeps its edge on white timelines (X, Slack, LinkedIn). All of it comes from code: HTML + SVG
rendered by headless Chromium. No image model is involved, and the same
inputs give the same pixels.

The og card carries the title and the motif's figure labels, because a
social feed shows it on its own. The in-post hero and the index thumbnail are
**art only**: no title, no subtitle, no labels (`motifs.LABELS = False`), and
the motif's real bounding box is fitted, centred, into the frame. The page
around them already carries the title, so the cover never repeats the header.

| output | size | used for |
|---|---|---|
| `landing/og/blog-<slug>.png` | 1200×630, 96-colour PNG | `og:image`, `twitter:image`, Article JSON-LD `image` |
| `landing/blog/covers/<slug>-hero.webp` | 1600×640, lossless WebP | the cover at the top of the post |
| `landing/blog/covers/<slug>-thumb.webp` | 640×336, WebP q80 | the card thumbnail on `/blog/` (lazy-loaded) |

## Run it

```sh
python3 -m venv /tmp/covers && /tmp/covers/bin/pip install playwright pillow
/tmp/covers/bin/playwright install chromium     # or: export CHROME=/usr/bin/google-chrome
/tmp/covers/bin/python scripts/seo/blog_covers/covers.py                    # every post
/tmp/covers/bin/python scripts/seo/blog_covers/covers.py --only <slug>      # one post
/tmp/covers/bin/python scripts/seo/blog_covers/covers.py --contact /tmp/sheet.png
```

This is an offline authoring tool. CI does not run it (the `scripts/seo/`
gates stay stdlib-only). The outputs are committed, and `seo_lint.py` checks
that every `og:image` exists and is 1200×630.

The fonts are vendored in `fonts/` (Big Shoulders Display and JetBrains Mono,
both SIL OFL 1.1, licence texts beside them), so a render does not depend on
what is installed on the machine. They are the families `landing/style.css`
names as `--font-display` and `--font-mono`. The colours are
`landing/style.css`'s day tokens (paper, mute, line, cmp) plus the brand-book
ink, yellow and ochre (`landing/brand.html`), copied into `TOKENS` in `covers.py`.

## Adding a post

1. Add an entry to `POSTS` in `covers.py`: `slug`, `date` (the post's
   published date), `title`, `subtitle` (a phrase lifted from the post),
   `motif`, `alt` (the hero/thumbnail alt: describe the art only, since
   those covers carry no text) and `og_alt` (the og card: its title plus the art).
2. Pick a motif in `motifs.py` or write one: `motif(w, h, rng) -> svg`.
   Every number it draws must appear in the post. Name the source in the
   motif's docstring. `rng` is seeded with the slug and may only decide
   placement (jitter, scatter), never a value.
3. Render, then wire it:
   - `scripts/seo/pagedata.py`: `og_image="og/blog-<slug>.png"` and
     `og_image_alt=` the same text as `og_alt`, then
     `python3 scripts/seo/fix_heads.py --write`;
   - the post: `<figure class="blog-cover"><img src="/blog/covers/<slug>-hero.webp"
     width="1600" height="640" alt="…" fetchpriority="high" decoding="async"></figure>`
     right after the `blog-crumb` line;
   - `landing/blog/index.html`: `<img class="bl-thumb" src="/blog/covers/<slug>-thumb.webp"
     width="640" height="336" alt="…" loading="lazy" decoding="async">` as the
     first child of the post's `bl-card`.
4. Look at the contact sheet before you commit. If a label collides with
   the motif, fix the motif. Do not shrink the type.

## The motifs

| motif | post | what it draws |
|---|---|---|
| `jevbars` | does-xerj-beat-jev | nDCG@10 square-bars per dataset, one denominator; the FiQA bill |
| `vote` | jev-vs-a-bm25-vote | 500 Banking77 decisions per arm, filled = correct, the vote's extra wins in yellow |
| `index28` | cutting-the-index-28-percent | 5.52 MB and 3.99 MB blocks to scale, the 28% cut in yellow; .seg/.meta bars |
| `names` | one-vulnerability-three-names | three advisory ids converging on one signed record |
| `gates` | the-rc80-gates | ten features as gates: pass / fail / split / unmeasured |
| `tiles` | the-corpus-hub | a wall of 100 pack tiles, the launch four in yellow |
| `scars` | eval-scars-from-agent-memory | six scratch marks, one per pattern |
| `network` | the-exploit-hub | PoC documents linked to the three CVEs of the eval |
| `route` | corpus-hub-v2 | 251 packs, the 14 catalogue corpora, a question routed to lz4 |
| `budget` | rc99-smart-autoindex-harness-tools | eight squares for the 8 GiB tier, two of them the 2 GiB memtable budget; eight small squares for the 8 MiB document ceiling; one outlined square for JSON streamed past 64 MiB |

Inks: yellow fill (with ochre for its labels and hairlines) for the one
result the post is about, grey-blue (day `--z-cmp`) for the comparison series
and never for XERJ, ink for structure, and mute/line for annotation and empty
cells. Every motif is built from squares on a grid: `dot()` draws a filled
cell, `ring()` an empty one.
