// GET /api/v1/contributors/<login> — full contribution details (signed in only).
//
//   401 without a session · 404 unknown login · 503 when this deploy has no details
//   200 { ok, is_me, contributor: { login, name, work: [...], also_pinned_in }, generated }
//   work[]: every selected commit, newest first: repo, commit (full sha), commit_url,
//   title, committed_at, files, lines, langs, areas (every folder), topics, shelf,
//   spdx, suggested_use, reviewed, pin (only when not the commit), agreed (date),
//   corpora: [{ slug, page, status, manifest, last_build }].
//   is_me: the signed-in GitHub login is this contributor ("this is me" view).
// No consent channel/scope, no e-mail, no IP data: see private_data.py.
import { json, requireUser } from '../../../_lib/session.js';
import { contributorFor, details } from '../../../_lib/details.js';

const LOGIN_RE = /^[A-Za-z0-9][A-Za-z0-9-]{0,38}$/;

export function contributorResponse(user, login, data) {
  if (!LOGIN_RE.test(login || '')) return json({ ok: false, error: 'invalid login' }, 400);
  if (!data) return json({ ok: false, error: 'details are not in this deploy yet' }, 503);
  const key = login.toLowerCase();
  const c = data.contributors[key];
  if (!c) return json({ ok: false, error: 'unknown contributor' }, 404);
  const work = c.work.map((w) => ({
    ...w,
    commit_url: w.commit_url || (w.commit ? `${w.repo_url.replace(/\/$/, '')}/commit/${w.commit}` : ''),
    pin: w.pin || w.commit || '',
  }));
  return json({ ok: true, is_me: contributorFor(user, data) === key, contributor: { ...c, work }, generated: data.generated });
}

export async function onRequestGet({ request, env, params }) {
  const [user, deny] = await requireUser(request, env);
  if (deny) return deny;
  return contributorResponse(user, String(params.login || ''), details());
}
