// ============================================================
// Dashboard — SEARCH · DISCOVER  (interactive query console)
//
// Everything on this page comes from the engine, for the index you picked:
//   • the index picker is the engine's real index list (`*` + names);
//   • the query runs against the fields the index's MAPPING actually has
//     (data/schema.js → roles), not a fixed `message` / `embedding`;
//   • facets are terms aggregations on that mapping's keyword fields;
//   • the histogram is a date_histogram on its date field, when it has one;
//   • the REQUEST panel shows the body that was sent — built by the same
//     function that built it (data/search-body.js), so preview and execution
//     cannot drift (#923 review finding 8).
//
// There is no sample result set. While a search is in flight the table says
// so; when one fails it shows the error and zero rows (#923 finding 5). The
// page used to ship a fabricated query plan, fabricated QPS / latency series
// and hardcoded "52.4M documents" tiles; none of those had a live source, so
// they are gone rather than relabelled.
// ============================================================

import { esc }                          from '../ux/text.js';
import { SearchBox, Hits, Facet, QueryDSL, Citations } from '../ux/charts-ops.js';
import { VBar }                         from '../ux/charts-ext.js';
import { dashboardCitations }           from '../data/feedback-citations.js';
import { QUERY_TYPES, buildSearchBody, requestPath } from '../data/search-body.js';

/** What the REQUEST panel shows: the executed body when the engine has
 *  answered (or failed), else the body the current state WOULD send. */
export function previewRequest(search, r) {
  const index = r?.resolvedIndex || (search?.index && search.index !== '*' ? search.index : null);
  const body = r?.request
    || buildSearchBody(search?.q, search?.type, search?.filters, r?.roles, { sort: search?.sort });
  return { path: requestPath(index), body };
}

const row = (k, v) => `
  <div class="row-flex"><span class="hint" style="min-width:150px;">${esc(k)}</span><span class="mono">${esc(v)}</span></div>`;

export const searchDiscover = {
  id:   'search-discover',
  name: 'Search · Discover',
  render: ({ search, indices }) => {
    const r = search?.result;
    const req = previewRequest(search, r);
    const roles = r?.roles || null;
    // Index picker follows the engine. Before the first mapping load it holds
    // `*` alone — never an illustrative list of indices that do not exist
    // (#923 review finding 4).
    const idxList = ['*', ...((indices && indices.length) ? indices : [])];
    const resolved = r?.resolvedIndex || null;
    // `*` runs against ONE index (the console's search proxy takes a single
    // exact name). Say which, on the page, every time (#923 finding 3).
    const scope = !resolved ? ''
      : (search?.index === '*' || !search?.index)
        ? (r?.narrowed ? ` · * SEARCHES ONE INDEX AT A TIME — SHOWING ${resolved}` : ` · ${resolved}`)
        : ` · ${resolved}`;

    return {
      title:  'SEARCH · DISCOVER',
      kicker: 'INTERACTIVE QUERY CONSOLE',
      meta:   resolved ? [resolved.toUpperCase()] : [],
      caption: 'Type a query, press Enter. The query, the facets and the histogram all use the fields this index\'s mapping really has, and the REQUEST panel shows exactly what was sent. SEMANTIC and HYBRID use the index\'s semantic_text field; with the default embedder that is lexical feature hashing, not a neural model.',
      panels: [

        { id: 'searchbox', eyebrow: 'QUERY · TYPE · INDEX · FILTERS', cols: 12, type: 'searchbox',
          render: () => SearchBox({
            value: search?.q ?? '',
            types: QUERY_TYPES,
            activeType: QUERY_TYPES.includes(search?.type) ? search.type : 'match',
            indices: idxList,
            activeIndex: search?.index ?? '*',
            filters: search?.filters ?? {},
          }),
        },

        { id: 'hits', eyebrow: 'RESULTS' + scope, cols: 8, type: 'hits',
          render: () => Hits({
            hits: r?.hits || [],
            total: r?.total ?? 0,
            tookMs: r?.tookMs ?? 0,
            maxScore: r?.maxScore ?? null,
            pending: !r || !!r.pending,
            error: r?.error || null,
            sort: search?.sort,
            showTime: search?.showTime !== false,
            // Field display names — GH#1896 (65 reactions): users want to see
            // friendlier column headers than the internal field names.
            labels: { _index: 'INDEX', _id: 'ID', _score: 'SCORE', _ts: 'TIME', _source: 'SOURCE' },
          }),
        },

        { id: 'facets', eyebrow: 'FACETS · CLICK TO FILTER', cols: 4, type: 'facet',
          // One block per keyword field the backend derived from the mapping
          // (plus `_index`). No hardcoded level / service / host.
          render: () => {
            const f = r?.facets || {};
            const keys = Object.keys(f).filter((k) => (f[k] || []).length);
            if (!keys.length && r?.request && !r.request.aggs) {
              return '<div class="mono faint">No facets or histogram under HYBRID: the engine does not run aggregations beside a fusion query. Switch to MATCH or SEMANTIC to see them.</div>';
            }
            if (!keys.length) return '<div class="mono faint">No facets: this result has no keyword-field buckets.</div>';
            return keys.map((k) => Facet({ field: k, items: f[k] || [], active: search?.filters?.[k] })).join('');
          },
        },

        { id: 'histogram', eyebrow: `DATE_HISTOGRAM · ${r?.histogramField ? r.histogramField.toUpperCase() + ' · 1D' : 'NO DATE FIELD'}`, cols: 8, type: 'bar',
          render: () => {
            const buckets = Array.isArray(r?.histogram) ? r.histogram : [];
            if (r?.request && !r.request.aggs) return '<div class="mono faint">No histogram under HYBRID (the engine does not run aggregations beside a fusion query).</div>';
            if (!r?.histogramField) return '<div class="mono faint">This index\'s mapping has no date field, so there is nothing to bucket by time.</div>';
            if (!buckets.length) return '<div class="mono faint">No dated records in this result.</div>';
            return VBar({ items: buckets.slice(-60), h: 140, unit: 'hits/day' });
          },
        },

        { id: 'searchMetrics', eyebrow: 'FIELDS THIS QUERY USES · FROM THE MAPPING', cols: 4, type: 'metric',
          render: () => {
            if (!roles) return '<div class="mono faint">Reading the mapping…</div>';
            return `<div class="stack-3">
              ${row('index', resolved || '—')}
              ${row('match / phrase / prefix', ((roles.searchFields && roles.searchFields.length ? roles.searchFields : [roles.textField]).filter(Boolean).join(', ') || '—') + (roles.searchFieldsCapped ? ' (the first 12 text fields)' : ''))}
              ${row('semantic / hybrid', roles.semanticField || '—')}
              ${row('time', roles.dateField || '— none —')}
              ${row('facets', (roles.keywordFields || []).slice(0, 3).join(', ') || '— none —')}
            </div>`;
          },
        },

        { id: 'dsl', eyebrow: 'REQUEST · POST ' + req.path + (r?.request ? ' · AS SENT' : ' · PREVIEW'), cols: 12, type: 'markdown',
          render: () => QueryDSL(req.body),
        },

        { id: 'citations', eyebrow: 'WHY THIS PANEL EXISTS · USER FEEDBACK', cols: 12, type: 'citations',
          render: () => Citations({ items: dashboardCitations['search-discover'] || [], total: 5150 }),
        },

      ],
    };
  },
};
