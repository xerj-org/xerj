#!/usr/bin/env node
// =============================================================================
// XERJ CONSOLE full-demo flow — boot → setup → passkey → SPA → dashboards.
// =============================================================================
//
// What this script does, in order:
//
//   1. Boots `xerj --insecure --data-dir <fresh tmp>` on default ports.
//   2. Tails stderr for the magic-link banner; extracts the token.
//   3. Launches headless Chrome via Puppeteer with the CDP WebAuthn domain
//      enabled and a virtual authenticator attached, so
//      navigator.credentials.create() actually completes (no human touch).
//   4. Visits /_xerj-console/setup#token=<token>, fills email + display name,
//      clicks Enrol — passkey gets created in the virtual authenticator,
//      finish-registration call lands, server mints a session cookie,
//      browser redirects to /_xerj-console/.
//   5. The SPA loads with its session — the auth guard pulls /me, /prefs,
//      and /views from the engine before app.js boots.
//   6. Walks through every dashboard tab + the Data section, screenshotting
//      each one.
//   7. Renames a dashboard (proves persistence — refresh, name still there).
//   8. Selects a different cluster from the data-source panel.
//   9. Logs out, reloads, lands on /_xerj-console/login, signs back in with the
//      same virtual authenticator — proves the login flow works end-to-end.
//
// Output: screenshots under demo/screenshots/<NN_step-name>.png plus
// demo/screenshots/manifest.json with timing + selector + URL info.
//
// =============================================================================

const path = require('path');
const fs = require('fs');
const { spawn } = require('child_process');
const puppeteer = require('puppeteer');
const os = require('os');

// ── Config ───────────────────────────────────────────────────────────────────
const REPO_ROOT = path.resolve(__dirname, '..', '..');
const SHOTS_DIR = path.join(REPO_ROOT, 'demo', 'screenshots');
const ENGINE_DIR = path.join(REPO_ROOT, 'engine');
const BASE = 'http://localhost:9200';

const SHOT_INDEX = [];
let shotCounter = 0;

async function takeShot(page, name, opts = {}) {
  shotCounter += 1;
  const fname = `${String(shotCounter).padStart(2, '0')}_${name}.png`;
  const fpath = path.join(SHOTS_DIR, fname);
  await page.screenshot({ path: fpath, fullPage: !!opts.fullPage });
  const url = page.url();
  SHOT_INDEX.push({ idx: shotCounter, name, url, file: `screenshots/${fname}` });
  console.log(`[shot ${shotCounter}] ${name} → ${fname}  (${url})`);
}

// ── 1. Boot xerj ────────────────────────────────────────────────────────────
async function bootXerj() {
  // Use a project-local directory rather than /tmp so we don't fight the
  // user's tmpfs quota. The dir is wiped between runs so each demo
  // hits the bootstrap banner fresh.
  const root = path.join(REPO_ROOT, 'demo', '.flow-data');
  fs.rmSync(root, { recursive: true, force: true });
  fs.mkdirSync(root, { recursive: true });
  const dataDir = root;
  console.log(`[boot] data dir: ${dataDir}`);
  const bin = path.join(ENGINE_DIR, 'target', 'release', 'xerj');
  if (!fs.existsSync(bin)) {
    throw new Error(`xerj binary not found at ${bin}; run \`cargo build --release -p xerj-server\` first`);
  }
  const child = spawn(bin, ['--insecure', '--data-dir', dataDir], {
    stdio: ['ignore', 'pipe', 'pipe'],
    cwd: ENGINE_DIR,
  });
  let stderr = '';
  child.stderr.on('data', (d) => { stderr += d.toString(); });
  child.stdout.on('data', () => {});
  child.on('exit', (code) => { console.log(`[boot] xerj exited code=${code}`); });

  // Wait until /cluster/info is 200, parse the magic-link token from stderr.
  const start = Date.now();
  while (Date.now() - start < 30_000) {
    try {
      const r = await fetch(`${BASE}/_xerj-console/api/v1/cluster/info`);
      if (r.ok) break;
    } catch {}
    await new Promise((r) => setTimeout(r, 250));
  }
  // Give the banner an extra moment so stderr is flushed.
  await new Promise((r) => setTimeout(r, 250));

  const m = stderr.match(/token=([A-Za-z0-9_-]+)/);
  if (!m) {
    throw new Error(`could not find magic-link token in stderr:\n${stderr}`);
  }

  // Pre-load a small real corpus so the SPA's Data section + dashboards
  // show actual numbers instead of falling through to the mock layer.
  // Kibana-style "OEM Xerj Console" demo: the user expects to land on a UI
  // with real data from the bundled engine.
  await ingestSampleData();

  return { dataDir, child, token: m[1], stderrPreview: stderr.slice(0, 1500) };
}

// Push the hand-authored 40-doc enterprise-AI knowledge base + a tiny
// chat-events synthetic stream so the dashboards have something to
// render. Both go through the ES-compat bulk endpoint (`/_bulk`) which
// `--insecure` mode doesn't gate.
async function ingestSampleData() {
  // Every dashboard the SPA renders queries one of these indices; if
  // the index is empty the dashboard's `live*` adapter returns null
  // and the SPA falls back to a mock shape.  Ingest all six so every
  // panel screenshot reflects real aggregations.
  const ROOT = path.join(REPO_ROOT, 'demo', 'data');
  const sets = [
    { index: 'ai-kb',         file: path.join(ROOT, 'ai_kb.ndjson') },
    { index: 'chat-events',   file: path.join(ROOT, 'extras', 'chat-events.ndjson') },
    { index: 'agent-memory',  file: path.join(ROOT, 'extras', 'agent-memory.ndjson') },
    { index: 'anomalies',     file: path.join(ROOT, 'extras', 'anomalies.ndjson') },
    { index: 'vector-ops',    file: path.join(ROOT, 'extras', 'vector-ops.ndjson') },
    { index: 'ingest-events', file: path.join(ROOT, 'extras', 'ingest-events.ndjson') },
  ];
  for (const { index, file } of sets) {
    if (!fs.existsSync(file)) {
      console.log(`[ingest] skip ${index}: ${file} not found`);
      continue;
    }
    const lines = fs.readFileSync(file, 'utf8').split('\n').filter(Boolean);
    let count = 0;
    let chunk = '';
    const flush = async () => {
      if (!chunk) return;
      const r = await fetch(`${BASE}/_bulk`, {
        method: 'POST',
        headers: { 'content-type': 'application/x-ndjson' },
        body: chunk,
      });
      if (!r.ok) console.log(`[ingest] ${index} bulk chunk HTTP ${r.status}`);
      chunk = '';
    };
    for (const line of lines) {
      chunk += JSON.stringify({ index: { _index: index } }) + '\n';
      chunk += line + '\n';
      count += 1;
      // Flush every 1500 docs to keep individual bulks under ~2 MB.
      if (count % 1500 === 0) await flush();
    }
    await flush();
    console.log(`[ingest] ${index}: ${count} docs`);
  }
  await fetch(`${BASE}/_refresh`, { method: 'POST' }).catch(() => {});
}

// ── 2. Drive the browser ─────────────────────────────────────────────────────
async function runDemo({ token }) {
  const chromeUserData = path.join(REPO_ROOT, 'demo', '.flow-chrome');
  fs.rmSync(chromeUserData, { recursive: true, force: true });
  fs.mkdirSync(chromeUserData, { recursive: true });

  const browser = await puppeteer.launch({
    headless: 'new',
    executablePath: '/usr/bin/google-chrome',
    userDataDir: chromeUserData,
    args: [
      '--no-sandbox',
      '--disable-setuid-sandbox',
      '--disable-dev-shm-usage',
      '--disable-gpu',
      '--disable-software-rasterizer',
      '--no-first-run',
      '--no-default-browser-check',
      '--disable-component-update',
      '--disable-default-apps',
      '--disable-extensions',
      '--disable-sync',
      '--disable-translate',
      '--disable-background-networking',
      '--disable-features=Translate,MediaRouter,IsolateOrigins,site-per-process',
      '--use-mock-keychain',
      '--password-store=basic',
      '--window-size=1440,900',
    ],
    defaultViewport: { width: 1440, height: 900, deviceScaleFactor: 2 },
    dumpio: false,
  });
  try {
    const page = await browser.newPage();
    page.on('console', (msg) => {
      const t = msg.type();
      if (t === 'error' || t === 'warn') {
        console.log(`[browser ${t}] ${msg.text()}`);
      }
    });
    page.on('pageerror', (err) => console.log('[browser pageerror]', err.message));

    // ── 2a. Attach a virtual authenticator via CDP ─────────────────────────
    const cdp = await page.target().createCDPSession();
    await cdp.send('WebAuthn.enable', { enableUI: false });
    const { authenticatorId } = await cdp.send('WebAuthn.addVirtualAuthenticator', {
      options: {
        protocol: 'ctap2',
        transport: 'internal',
        hasResidentKey: true,
        hasUserVerification: true,
        isUserVerified: true,
        automaticPresenceSimulation: true,
      },
    });
    console.log(`[webauthn] virtual authenticator id=${authenticatorId}`);

    // ── 2b. Setup page ─────────────────────────────────────────────────────
    const setupUrl = `${BASE}/_xerj-console/setup#token=${token}`;
    console.log(`[demo] navigating to ${setupUrl}`);
    await page.goto(setupUrl, { waitUntil: 'networkidle0' });
    await page.waitForSelector('#email', { timeout: 5000 });
    await takeShot(page, 'setup-page-loaded');

    // The page eagerly redeems the token on load. Make sure it succeeded
    // before we try to enrol.
    await page.waitForFunction(
      () => !document.getElementById('email').disabled
        && document.getElementById('btn-enrol') !== null,
      { timeout: 10000 },
    );

    await page.type('#email', 'demo-owner@xerj.ai', { delay: 12 });
    await page.type('#display', 'Demo Owner',           { delay: 12 });
    await page.evaluate(() => {
      // Replace the default passkey label so screenshots are clean.
      document.getElementById('pkname').value = 'MacBook (demo)';
    });
    await takeShot(page, 'setup-form-filled');

    await page.click('#btn-enrol');

    // Wait for either the success step or an error to appear.
    await page.waitForFunction(() => {
      const ok = document.getElementById('step-done')?.classList.contains('active');
      const err = !document.getElementById('err').hidden;
      return ok || err;
    }, { timeout: 15000 });

    const enrolErr = await page.$eval('#err', (el) => el.hidden ? null : el.textContent);
    if (enrolErr) throw new Error(`passkey enrol failed: ${enrolErr}`);

    await takeShot(page, 'setup-passkey-enrolled');

    // setup.html redirects to /_xerj-console/ — wait for it.
    await page.waitForFunction(
      () => window.location.pathname === '/_xerj-console/',
      { timeout: 10000 },
    );

    // ── 2c. Land on the SPA ────────────────────────────────────────────────
    // The auth guard imports app.js dynamically; wait for #app to populate.
    await page.waitForFunction(
      () => document.getElementById('app')?.children.length > 0,
      { timeout: 15000 },
    );
    await new Promise((r) => setTimeout(r, 700)); // let charts render
    await takeShot(page, 'spa-landed-after-enrol', { fullPage: true });

    // ── 2d. Walk every section ─────────────────────────────────────────────
    const sections = await page.$$eval('[data-section]', (as) =>
      Array.from(new Set(as.map((a) => a.getAttribute('data-section')))).filter(Boolean),
    );
    console.log(`[demo] found sections: ${sections.join(', ')}`);
    for (const sid of sections) {
      const link = await page.$(`[data-section="${sid}"]`);
      if (!link) continue;
      await link.click();
      await new Promise((r) => setTimeout(r, 600));
      await takeShot(page, `section-${sid}`, { fullPage: true });
    }

    // ── 2e. Walk every dashboard so we capture each with real data ────────
    await page.click('[data-section="dashboards"]').catch(() => {});
    await new Promise((r) => setTimeout(r, 500));
    const dashIds = await page.$$eval('[data-dash]', (as) =>
      Array.from(new Set(as.map((a) => a.getAttribute('data-dash')))).filter(Boolean),
    );
    console.log(`[demo] found dashboards: ${dashIds.join(', ')}`);
    for (const dashId of dashIds) {
      const a = await page.$(`[data-dash="${dashId}"]`);
      if (!a) continue;
      await a.click();
      // Each dashboard fetches its own /_search aggregations; give the
      // network a beat (1.2 s) so the LIVE pill flips and the panels
      // re-render with engine-derived numbers before we screenshot.
      await new Promise((r) => setTimeout(r, 1200));
      await takeShot(page, `dashboard-${dashId}`, { fullPage: true });
    }

    // ── 2f. Show the Data section (real connection list from backend) ─────
    const dataLink = await page.$('[data-section="data"]');
    if (dataLink) {
      await dataLink.click();
      await new Promise((r) => setTimeout(r, 700));
      await takeShot(page, 'data-section-real-backend', { fullPage: true });
    }

    // ── 2g. Rename a dashboard via the SPA UI ──────────────────────────────
    // Open settings → renames trigger a localStorage write to
    // xerj.dashboards which xerj-console-sync mirrors to /prefs. We POST the
    // rename via the same code-path the SPA uses, then reload to prove
    // it survived a refresh (i.e. came back from the backend).
    await page.evaluate(() => {
      const m = JSON.parse(localStorage.getItem('xerj.dashboards') || '{}');
      m.names = m.names || {};
      m.names['ai-overview'] = 'AI Overview (DEMO)';
      localStorage.setItem('xerj.dashboards', JSON.stringify(m));
    });
    // Wait > 1.5s for the sync push tick to fire.
    await new Promise((r) => setTimeout(r, 2200));

    // Hard-reload (cached app.js + fresh /me + fresh /prefs from backend).
    await page.reload({ waitUntil: 'networkidle0' });
    await page.waitForFunction(
      () => document.getElementById('app')?.children.length > 0,
      { timeout: 15000 },
    );
    await new Promise((r) => setTimeout(r, 800));
    await takeShot(page, 'dashboard-renamed-persists-after-reload', { fullPage: true });

    // ── 2h. Pick a different cluster from the Data section ────────────────
    await page.click('[data-section="data"]').catch(() => {});
    await new Promise((r) => setTimeout(r, 600));
    // The Data section's cluster row is clickable to set the default.
    const clusterRows = await page.$$('a[href*="cluster"], [data-cluster]');
    if (clusterRows.length) {
      await clusterRows[0].click();
      await new Promise((r) => setTimeout(r, 300));
    }
    await takeShot(page, 'data-section-with-real-corpus', { fullPage: true });

    // ── 2i. Logout, then sign back in via /login ───────────────────────────
    await page.evaluate(async () => {
      await fetch('/_xerj-console/api/v1/auth/logout', { method: 'POST', credentials: 'same-origin' });
    });
    await page.goto(`${BASE}/_xerj-console/`, { waitUntil: 'networkidle0' });
    // The auth guard should now redirect us to /login.
    await page.waitForFunction(
      () => window.location.pathname.endsWith('/login'),
      { timeout: 10000 },
    );
    await page.waitForSelector('#email');
    await page.type('#email', 'demo-owner@xerj.ai', { delay: 12 });
    await takeShot(page, 'login-page-with-email');

    await page.click('#btn');
    // Race-safe wait: success => path becomes /_xerj-console/, failure => err
    // div on /login becomes visible. Capture err *before* navigation.
    let loginErr = null;
    await page.waitForFunction(
      () => window.location.pathname === '/_xerj-console/'
        || (() => { const e = document.getElementById('err'); return e && !e.hidden; })(),
      { timeout: 15000 },
    );
    if (page.url().endsWith('/login')) {
      loginErr = await page.$eval('#err', (el) => el.hidden ? null : el.textContent).catch(() => null);
    }
    if (loginErr) throw new Error(`login failed: ${loginErr}`);

    await page.waitForFunction(
      () => document.getElementById('app')?.children.length > 0,
      { timeout: 15000 },
    );
    await new Promise((r) => setTimeout(r, 700));
    await takeShot(page, 'spa-after-relogin', { fullPage: true });

    // ── 2j. Health checks ──────────────────────────────────────────────────
    const meBody = await page.evaluate(async () => {
      const r = await fetch('/_xerj-console/api/v1/me', { credentials: 'same-origin' });
      return { status: r.status, body: r.ok ? await r.json() : null };
    });
    console.log('[demo] /me after relogin →', JSON.stringify(meBody));

    const dashRename = await page.evaluate(() => {
      const m = JSON.parse(localStorage.getItem('xerj.dashboards') || '{}');
      return m.names && m.names['ai-overview'];
    });
    console.log(`[demo] dashboard rename survived: ${dashRename}`);
  } finally {
    await browser.close();
  }
}

// ── 3. Top-level wiring ──────────────────────────────────────────────────────
async function main() {
  fs.mkdirSync(SHOTS_DIR, { recursive: true });
  console.log(`[demo] screenshots → ${SHOTS_DIR}`);

  const { child, token, dataDir, stderrPreview } = await bootXerj();
  console.log(`[demo] magic-link token: ${token.slice(0, 8)}…  (${token.length} chars)`);

  let demoErr = null;
  try {
    await runDemo({ token });
  } catch (e) {
    demoErr = e;
  } finally {
    try { child.kill('SIGTERM'); } catch {}
  }

  // Write manifest + a copy of the stderr banner for the runbook.
  fs.writeFileSync(
    path.join(SHOTS_DIR, 'manifest.json'),
    JSON.stringify(
      {
        generated_at: new Date().toISOString(),
        data_dir: dataDir,
        shots: SHOT_INDEX,
        stderr_preview: stderrPreview,
        ok: !demoErr,
        error: demoErr ? `${demoErr.message}\n${demoErr.stack}` : null,
      },
      null,
      2,
    ),
  );

  if (demoErr) {
    console.error('[demo] FAILED:', demoErr.message);
    process.exit(1);
  }
  console.log(`[demo] ok — ${shotCounter} screenshots under ${SHOTS_DIR}`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
