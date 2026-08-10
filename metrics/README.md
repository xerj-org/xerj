# metrics/

Committed adoption data, and the rules for reading it.

Everything here is either public GitHub metadata or a count of requests to a
web server we operate. **Nothing in this directory came from a user's machine.**

---

## Privacy posture, in one paragraph

XERJ ships **no telemetry**. The binary does not phone home, on first run or
ever, and there is no plan to add it: this is an Apache-2.0 tool that engineers
run on their own hardware, and a silent phone-home would cost more credibility
than the data is worth. What *is* instrumented is the **distribution surface** —
the website and the installer download — because a request to a web server is
already observable to whoever operates that server. The install counter
(`functions/get.js`) stores a timestamp, a country, a User-Agent string and a
coarse OS guess derived from it. It stores **no IP address, no cookie, and no
identifier of any kind**, so two installs by the same person are
indistinguishable from two installs by two people. That is by design and it is
the whole intended capability.

---

## `release-downloads.jsonl`

One JSON object per line, one line per day, appended by
[`.github/workflows/release-metrics.yml`](../.github/workflows/release-metrics.yml)
(collector: [`.github/scripts/snapshot-release-downloads.sh`](../.github/scripts/snapshot-release-downloads.sh)).

```jsonc
{
  "date": "2026-08-08",
  "collected_at": "2026-08-08T06:00:12Z",
  "repo": "xerj-org/xerj",
  "totals": { "releases": 12, "binary": 88, "checksum": 57, "all": 145 },
  "releases": {
    "v1.0.0-rc.12": {
      "published_at": "…", "prerelease": false,
      "assets": { "xerj-1.0.0-rc.12-x86_64-unknown-linux-musl.tar.gz": 1, … }
    }
  },
  "traffic_14d": { "clones": {"count":…, "uniques":…}, "views": {…} }
}
```

**Why this file exists.** GitHub reports `download_count` as a running total and
keeps no history. Nothing was snapshotting it, so the only uninflated adoption
number the project had could be read once but never trended. One line a day
turns a scalar into a series, at zero infrastructure cost.

JSONL, not JSON: a day is one appended line, so the diff is one line, two
concurrent writers cannot corrupt the array, and `jq -c` streams it. Re-running
the collector on the same day **replaces** that day's line rather than adding a
second one.

### Reading it correctly

- `totals.binary` counts **asset fetches, not installs**. It includes CI,
  mirrors, scanners and re-downloads.
- `totals.binary` vs `totals.checksum` is the useful pair. `landing/get` fetches
  exactly one binary and exactly one matching `.sha256` per run, in that order,
  **deliberately** — so installer-shaped traffic pairs 1:1. A binary count well
  above the checksum count is traffic that is not the installer. Do not reorder
  those two fetches in `landing/get` without replacing this fingerprint first.
- `traffic_14d` is a **14-day rolling window**, not a daily figure and not a
  lifetime total. Consecutive snapshots overlap by 13 days: **never sum them.**
  It is captured only because GitHub expires it — a day not snapshotted is gone
  permanently. It is `null` when the collecting token lacks push scope.
- Nothing here says whether a downloaded binary was ever run. Nothing can.

---

## What is *not* in this directory

Install-counter records live in the Cloudflare R2 bucket `xerj-installs`
(`functions/get.js`), not in git — they are request logs, they grow per request,
and they do not belong in the repository. Read them through the token-guarded
export:

```sh
curl -s "https://xerj.org/get?token=$INSTALLS_TOKEN&days=30" | jq .
```

The export returns aggregates and ships its own caveat text. The short version:
a request to `/get` is a request. It does not prove the download succeeded, the
checksum matched, or that the binary was ever executed.

---

## Getting the honest funnel

```sh
scripts/adoption-snapshot.sh
```

Pulls the numbers live and prints them in three sections: what you can quote,
what is contaminated (with the reason for each), and what is genuinely blind.
Repo-level totals on this project — stars, forks, clones — are inflated by a
synthetic cohort and must not be used as adoption signals; the script computes
the defensible pre-spike star count for you rather than making you remember it.
