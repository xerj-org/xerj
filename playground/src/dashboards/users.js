// ============================================================
// Section — USERS
//
// Users + Roles + API keys + sessions. Spaces-and-RBAC is the
// 2nd most painful operational category in the Kibana corpus
// after alerting (213 items, 47% negative). Recurring pain:
//   • "permission denied" with no indication of what's missing
//   • Spaces multi-tenancy too complex, feature privileges
//     scattered across index, app, and role-mapping
//   • API key rotation is manual
//
// XERJ response: one token, indices ARE the boundary. No
// "Spaces". Role = a set of index prefixes + operations.
// ============================================================

import { Num }                    from '../ux/text.js';
import { Spark, Dist }            from '../ux/charts.js';
import { TopN, Events }           from '../ux/layout.js';
import { Table, Markdown }        from '../ux/tables.js';
import { Citations }              from '../ux/charts-ops.js';

export const users = {
  id: 'users',
  name: 'Users',
  section: 'users',
  render: ({ data, time }) => ({
    title: 'USERS',
    kicker: 'IDENTITY · ACCESS · TOKENS',
    meta: [time, 'RBAC'],
    caption: 'No Spaces. No feature privileges matrix. No separate "role mapping" app. A user has a token. A role is a set of index prefixes and operations. A session is a token with an expiry. Everything else is hidden complexity we refuse to ship.',
    panels: [

      { id: 'users',     eyebrow: 'USERS',      cols: 3, type: 'metric',
        render: () => Num({ value: '128', unit: 'active', hint: '12 online now', emphasis: true }),
      },
      { id: 'roles',     eyebrow: 'ROLES',      cols: 2, type: 'metric',
        render: () => Num({ value: '8',   unit: 'defined', hint: 'flat model', emphasis: false }),
      },
      { id: 'apiKeys',   eyebrow: 'API KEYS',   cols: 2, type: 'metric',
        render: () => Num({ value: '42',  unit: 'live', hint: '3 expire < 7d', emphasis: false }),
      },
      { id: 'sessions',  eyebrow: 'SESSIONS',   cols: 2, type: 'metric',
        render: () => Num({ value: '86',  unit: 'active', delta: 3.2, emphasis: false }),
      },
      { id: 'lastLogin', eyebrow: 'LAST LOGIN', cols: 3, type: 'metric',
        render: () => Num({ value: '12s', unit: 'ago', hint: 'deploy@xerj.ai', emphasis: false }),
      },

      { id: 'userList', eyebrow: 'USERS · MOST ACTIVE', cols: 6, type: 'topn',
        render: () => TopN({
          items: [
            { label: 'deploy',          value: 2804 },
            { label: 'oncall-bot',      value: 1920 },
            { label: 'searcher',        value: 1612 },
            { label: 'metrics-exporter',value: 1402 },
            { label: 'alice@eng',       value: 1180 },
            { label: 'bob@eng',         value:  980 },
            { label: 'carol@pm',        value:  770 },
            { label: 'dave@sre',        value:  612 },
          ],
          total: 11280,
          n: 8,
        }),
      },

      { id: 'roles', eyebrow: 'ROLES · INDEX PREFIX × OPS', cols: 6, type: 'table',
        render: () => Table({
          columns: ['ROLE', 'INDICES', 'OPS'],
          rows: [
            ['admin',          '*',                    'read, write, admin, delete'],
            ['developer',      'logs-*, metrics-*',    'read, write'],
            ['sre',            '*',                    'read, write, admin'],
            ['pm',             'logs-prod, metrics',   'read'],
            ['oncall',         '*',                    'read, alert.ack'],
            ['agent-token',    'agent-memory, embeddings', 'read, write'],
            ['audit-ro',       'audit',                'read'],
            ['guest',          'docs',                 'read'],
          ],
          align: ['left', 'left', 'left'],
        }),
      },

      { id: 'recent', eyebrow: 'RECENT AUTH EVENTS', cols: 12, type: 'events',
        render: () => Events({ items: [
          { at: '23:14:02', sev: 'info', msg: 'login · deploy@xerj.ai · src=10.0.3.42 · method=bearer · ok' },
          { at: '23:13:51', sev: 'info', msg: 'login · alice@eng    · src=10.0.4.11 · method=sso · ok' },
          { at: '23:13:22', sev: 'warn', msg: 'token rotated · api-key-k9q8 · expires in 30d' },
          { at: '23:12:48', sev: 'err',  msg: 'permission denied · bob@eng · index=audit · op=read · missing role audit-ro' },
          { at: '23:12:09', sev: 'info', msg: 'login · oncall-bot   · src=10.0.0.4  · method=api-key · ok' },
          { at: '23:11:44', sev: 'info', msg: 'token created · bob@eng · api-key-77mx · expires=30d' },
          { at: '23:11:03', sev: 'warn', msg: 'token expiring · alice@eng · api-key-4ka2 · 6h' },
        ]}),
      },

      { id: 'model', eyebrow: 'THE PERMISSION MODEL · WHY IT IS LIKE THIS', cols: 12, type: 'markdown',
        render: () => Markdown(
`## One token, no magic

XERJ.ai has exactly three concepts for access control:

- **Token** — a user or machine bearer. Can have an expiry. Can be revoked.
- **Role** — a set of \`(index_prefix, operation)\` pairs. Flat. No inheritance.
- **Session** — a token that's currently open. Auto-expires.

That's it. There are no **Spaces**, no **feature privileges matrix**, no
separate **role mapping** configuration. Everything goes through one check:

\`\`\`
allow(token, index, op) :=
   ∃ role ∈ token.roles :
     ∃ (prefix, ops) ∈ role :
       index.startsWith(prefix) ∧ op ∈ ops
\`\`\`

If an operation is denied, the response includes the **exact missing role +
prefix**. No more "permission denied" with no hint — see the auth event
above for \`bob@eng\`.

The corpus has 213 items in \`categories/08-spaces-and-rbac/\`, many of
which are "I can see the dashboard but can't edit it" or "why doesn't my
Space work". That complexity is gone.`
        ),
      },

      { id: 'citations', eyebrow: 'WHY THIS SECTION EXISTS · USER FEEDBACK', cols: 12, type: 'citations',
        render: () => Citations({
          items: [
            { id: 'gh-4453',  source: 'github', score: 38,
              title: 'Saved object authorization - Phase 1',
              url: 'https://github.com/elastic/kibana/issues/4453' },
            { id: 'gh-18331', source: 'github', score: 47,
              title: 'Anonymous access',
              url: 'https://github.com/elastic/kibana/issues/18331' },
            { id: 'gh-17888', source: 'github', score: 45,
              title: 'Per-user profiles, settings in Kibana',
              url: 'https://github.com/elastic/kibana/issues/17888' },
          ],
          total: 213,
        }),
      },

    ],
  }),
};
