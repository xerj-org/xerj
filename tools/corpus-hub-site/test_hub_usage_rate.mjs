// node --test tools/corpus-hub-site/test_hub_usage_rate.mjs  (Node 18+; no dependencies)
// The per-source rate limit: an IPv6 source is bucketed by its /64, so one /64
// cannot pass the limit by changing the host bits of its address.
//
// The limiter drops its bucket on the minute boundary, so the clock is frozen
// while a test runs: otherwise a run that crosses a minute boundary would
// reset the bucket mid-test and the assertion would depend on timing.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { overRate } from '../../functions/_lib/hub-usage.js';

const RATE_PER_MIN = 30;

function freezeClock() {
  const real = Date.now;
  const fixed = real.call(Date);
  Date.now = () => fixed;
  return () => { Date.now = real; };
}

const req = (ip) => new Request('https://hub.xerj.org/', { headers: { 'CF-Connecting-IP': ip } });

test('a second host in the same IPv6 /64 shares the bucket of the first', async (t) => {
  const restore = freezeClock();
  t.after(restore);

  const first = '2001:db8::1';
  const second = '2001:db8::2';

  for (let i = 0; i < RATE_PER_MIN; i += 1) {
    assert.equal(await overRate(req(first)), false, `request ${i + 1} should be under the limit`);
  }
  // The bucket now holds RATE_PER_MIN requests. The next one is over the limit,
  // whichever host of the /64 sends it.
  assert.equal(await overRate(req(second)), true,
    'a second host in the same /64 must share the rate-limit bucket');
});

test('one IPv6 address written two ways shares one bucket', async (t) => {
  const restore = freezeClock();
  t.after(restore);

  const short = '2001:dba::1';
  const long = '2001:0dba:0000:0000:0000:0000:0000:0001';

  for (let i = 0; i < RATE_PER_MIN; i += 1) {
    assert.equal(await overRate(req(short)), false, `request ${i + 1} should be under the limit`);
  }
  assert.equal(await overRate(req(long)), true,
    'the compressed and the expanded form of one address must share the bucket');
});

test('two different IPv6 /64s keep separate buckets', async (t) => {
  const restore = freezeClock();
  t.after(restore);

  const one = '2001:dbb::1';
  const other = '2001:dbc::1';

  for (let i = 0; i < RATE_PER_MIN; i += 1) {
    assert.equal(await overRate(req(one)), false, `request ${i + 1} should be under the limit`);
  }
  assert.equal(await overRate(req(other)), false,
    'a different /64 must not inherit the bucket of another /64');
});

test('an IPv4 address keeps its own bucket', async (t) => {
  const restore = freezeClock();
  t.after(restore);

  const first = '198.51.100.7';
  const second = '198.51.100.8';

  for (let i = 0; i < RATE_PER_MIN; i += 1) {
    assert.equal(await overRate(req(first)), false, `request ${i + 1} should be under the limit`);
  }
  assert.equal(await overRate(req(second)), false,
    'a different IPv4 address must not inherit the bucket of another address');
});
