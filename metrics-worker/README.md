# xerj-metrics

A Cloudflare Worker that **records GitHub traffic and website installs daily**
and serves a charts dashboard.

GitHub's traffic API (`/repos/xerj-org/xerj/traffic/{clones,views}`) only returns
a rolling **14 days**, so the numbers vanish unless something snapshots them. The
cron (`0 6 * * *`) merges the current 14-day window into `metrics/dashboard.json`
in the `xerj-installs` R2 bucket (upsert by date, so a missed run self-heals),
and counts the `installs/<date>/` objects the installer already writes. No IPs,
cookies or identifiers are stored or read.

- **Dashboard:** `/?key=$REFRESH_KEY` — Chart.js over the recorded data. Every route is key-gated (403 without the key); nothing is public.
- **Raw data:** `/data.json?key=$REFRESH_KEY`.
- **Manual snapshot:** `/refresh?key=$REFRESH_KEY`.
- **GitHub-trending history:** `/trending-history?key=$REFRESH_KEY` lists the dated snapshots; add `&date=YYYY-MM-DD` for one day. Each daily snapshot also writes `trending/<date>.json` to R2.

## Deploy

```sh
cd metrics-worker
wrangler deploy
printf '%s' "$GITHUB_PAT_WITH_TRAFFIC_SCOPE" | wrangler secret put GITHUB_TOKEN
printf '%s' "$(openssl rand -hex 16)"          | wrangler secret put REFRESH_KEY
curl "https://<worker-url>/refresh?key=<that key>"   # seed the first snapshot
```

Secrets: `GITHUB_TOKEN` (a PAT with traffic read), `REFRESH_KEY` (gates every route).
Binding: `INSTALLS` -> R2 bucket `xerj-installs` (shared with the installer).
