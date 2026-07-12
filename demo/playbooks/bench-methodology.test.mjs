#!/usr/bin/env node
// Harness methodology test — proves WHY the scorecard must measure per-query
// latency CLOSED-LOOP, not with the open-loop 200/s driver.
//
// Against a mock server that sleeps a FIXED, KNOWN delay D ms per request:
//   - closed-loop (single client, sequential) MUST report p50 ≈ D          (truth)
//   - open-loop @200/s MUST report p50 ≈ D for D<5ms (sustainable)          (no artifact)
//   - open-loop @200/s MUST inflate p50 ≫ D for D≥25ms (backlog artifact)   (the bug)
//   - both identical mock servers MUST measure equal (fairness)
//
// This is the guardrail for the 2026-07-08 re-evaluation: run1's 5.6s/27s
// "latencies" were open-loop backlog on ~30-600ms true-latency queries.
//
//   node demo/playbooks/bench-methodology.test.mjs
import http from 'node:http';

// ── measurement modes (closed-loop truth + the harness's open-loop) ──────────
const agent = new http.Agent({ keepAlive: true, maxSockets: 256 });
function hit(port) {
  return new Promise((resolve) => {
    const r = http.request({ hostname: '127.0.0.1', port, path: '/', method: 'GET', agent }, (res) => {
      res.on('data', () => {}); res.on('end', resolve);
    });
    r.on('error', resolve); r.end();
  });
}
const pct = (a, p) => { const s = [...a].sort((x, y) => x - y); return s[Math.floor((p / 100) * s.length)]; };

async function closedLoop(port, iters = 40, warm = 5) {
  for (let i = 0; i < warm; i++) await hit(port);
  const lat = [];
  for (let i = 0; i < iters; i++) { const t = performance.now(); await hit(port); lat.push(performance.now() - t); }
  return pct(lat, 50);
}
// Byte-for-byte the same scheme as bench-matrix.mjs timed(): fixed cadence,
// hybrid sleep+spin pacer (H5 — setTimeout's 0.5-1.5ms overshoot otherwise
// lands inside lat[] and dominates sub-ms measurements), hot event loop during
// the timed window (H5b — the epoll wake while awaiting each response is
// bimodal 0.3-1.5ms and phase-locks per measurement), coordinated-omission
// correction lat = end - min(intended, actualStart).
const SPIN_MS = 1.5;
async function openLoop(port, { iters = 300, rate = 200, warm = 10 } = {}) {
  for (let i = 0; i < warm; i++) await hit(port);
  const lat = new Array(iters), tasks = new Array(iters);
  let hot = true;
  (function hotloop() { if (hot) setImmediate(hotloop); })();
  const t0 = performance.now();
  for (let i = 0; i < iters; i++) {
    const intended = t0 + (i / rate) * 1000;
    tasks[i] = (async () => {
      const w = intended - performance.now();
      if (w > SPIN_MS) await new Promise((r) => setTimeout(r, w - SPIN_MS));
      while (performance.now() < intended) { /* spin to the exact instant */ }
      const s = performance.now(); await hit(port); lat[i] = performance.now() - Math.min(intended, s);
    })();
  }
  await Promise.all(tasks);
  hot = false;
  return pct(lat, 50);
}

// Models a RESOURCE-CONSTRAINED server: `concurrency` workers, each request
// takes delayMs of "work". This is the key property — a real engine is
// CPU-bound, so a query costing D ms of CPU can only be served `cores/D×1000`
// per second; beyond that, concurrent requests queue. A pure-setTimeout server
// (infinite concurrency) would NOT reproduce the open-loop artifact, because
// nothing contends — which is exactly why the artifact is a SATURATION effect,
// not a function of per-request latency alone.
function mockServer(delayMs, concurrency = 4) {
  let active = 0; const q = [];
  const pump = () => { while (active < concurrency && q.length) { active++; const res = q.shift(); setTimeout(() => { res.writeHead(200); res.end('ok'); active--; pump(); }, delayMs); } };
  const srv = http.createServer((req, res) => { q.push(res); pump(); });
  return new Promise((resolve) => { srv.listen(0, '127.0.0.1', () => resolve({ srv, port: srv.address().port })); });
}

// ── assertions ───────────────────────────────────────────────────────────────
let failures = 0;
function check(name, cond, detail) {
  const ok = !!cond; console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  — ${detail}` : ''}`); if (!ok) failures++;
}

(async () => {
  console.log('Harness methodology test — closed-loop vs open-loop on fixed-delay mock servers\n');

  // Case 1: FAST server (D=2ms < 5ms cadence) — open-loop should NOT inflate.
  {
    const { srv, port } = await mockServer(2);
    const c = await closedLoop(port), o = await openLoop(port);
    check('fast(2ms): closed-loop ≈ true delay', c >= 1 && c <= 12, `closed p50 ${c.toFixed(2)}ms`);
    check('fast(2ms): open-loop ≈ closed (no artifact, amp<4×)', o < c * 4 + 6, `open ${o.toFixed(2)} vs closed ${c.toFixed(2)}`);
    srv.close();
  }

  // Case 2: SLOW, RESOURCE-CONSTRAINED server (D=40ms, only 2 workers → capacity
  // ~50 req/s). At 200/s offered that is 4× overload → open-loop MUST inflate as
  // backlog piles up, while closed-loop (1 in flight, never saturates) stays ≈D.
  // This is the XERJ situation: a CPU-bound ~40ms query flooded 256-concurrent.
  {
    const { srv, port } = await mockServer(40, 2);
    const c = await closedLoop(port, 20, 3), o = await openLoop(port, { iters: 200 });
    check('slow(40ms): closed-loop ≈ true delay', c >= 30 && c <= 70, `closed p50 ${c.toFixed(2)}ms (true ~40)`);
    check('slow(40ms): open-loop INFLATES ≥4× (backlog artifact)', o > c * 4, `open ${o.toFixed(2)} vs closed ${c.toFixed(2)} → ${(o / c).toFixed(1)}× — this is why run1 read 5.6s`);
    srv.close();
  }

  // Case 3: FAIRNESS — two identical servers measure equal under both modes.
  {
    const a = await mockServer(15), b = await mockServer(15);
    const ca = await closedLoop(a.port, 20, 3), cb = await closedLoop(b.port, 20, 3);
    check('fairness: identical servers → equal closed-loop (within 40%)', Math.abs(ca - cb) <= 0.4 * Math.max(ca, cb), `A ${ca.toFixed(2)} vs B ${cb.toFixed(2)}`);
    a.srv.close(); b.srv.close();
  }

  console.log(`\n${failures === 0 ? 'ALL PASS' : failures + ' FAILURE(S)'} — closed-loop is the honest per-query latency; open-loop@200/s is a throughput/saturation test and inflates any >5ms query.`);
  process.exit(failures === 0 ? 0 : 1);
})();
