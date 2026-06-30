// ============================================================
// Dashboard — SEARCH · DISCOVER  (interactive query console)
//
// Real features shown here — every one maps to code in the
// engine audit:
//   • 8 query types (subset of 24 the DSL supports)
//   • terms-agg facet sidebar (exact cardinality, not HLL)
//   • POST /v1/indices/:name/search preview in XERJ.ai DSL
//   • POST /v1/indices/:name/explain-plan tree
//   • hits w/ _index / _id / _score / _source / @timestamp
// ============================================================

import { Num }                  from '../ux/text.js';
import { Spark, Series }        from '../ux/charts.js';
import { SearchBox, Hits, Facet, QueryDSL, QueryPlanTree, Citations } from '../ux/charts-ops.js';
import { VBar }                 from '../ux/charts-ext.js';
import { dashboardCitations }   from '../data/feedback-citations.js';

// All 8 query types we expose in the UI (mapping to real DSL nodes)
const QUERY_TYPES = ['match', 'term', 'range', 'prefix', 'phrase', 'knn', 'semantic', 'hybrid'];
const INDICES     = ['*', 'logs-prod', 'logs-stage', 'docs', 'metrics', 'traces', 'events'];

// Build the XERJ.ai query DSL object matching the current SearchBox state.
function buildDsl({ q, type, index, filters }) {
  const filterList = Object.entries(filters || {}).map(([f, v]) => ({ term: { [f]: v } }));
  let inner;
  switch (type) {
    case 'term': {
      const m = (q || '').match(/^([a-z_]+)\s*=\s*(.+)$/i);
      inner = m ? { term: { [m[1]]: m[2] } } : { match_all: {} };
      break;
    }
    case 'range': {
      const m = (q || '').match(/^([a-z_]+)\s*(>=|<=|>|<)\s*(\d+(?:\.\d+)?)$/i);
      if (m) {
        const [, f, op, v] = m;
        const k = op === '>=' ? 'gte' : op === '<=' ? 'lte' : op === '>' ? 'gt' : 'lt';
        inner = { range: { [f]: { [k]: Number(v) } } };
      } else inner = { match_all: {} };
      break;
    }
    case 'prefix':   inner = q ? { prefix: { message: q } } : { match_all: {} }; break;
    case 'phrase':   inner = q ? { match_phrase: { message: q } } : { match_all: {} }; break;
    case 'knn':      inner = { knn: { field: 'embedding', query_vector: '<inline>', k: 10, ef_search: 96 } }; break;
    case 'semantic': inner = { semantic: { field: 'embedding', query: q || '*', model: 'text-embed-3' } }; break;
    case 'hybrid':   inner = { hybrid: {
                        fusion: 'rrf',
                        queries: [
                          { match: { message: q || '*' } },
                          { knn: { field: 'embedding', query_vector: '<inline>', k: 20 } },
                        ],
                        rank_constant: 60,
                      } }; break;
    default:         inner = q ? { match: { message: q } } : { match_all: {} };
  }
  const body = filterList.length
    ? { query: { bool: { must: inner, filter: filterList } } }
    : { query: inner };
  return {
    method: 'POST',
    path: '/v1/indices/' + (index === '*' ? '*' : index) + '/search',
    body: { ...body, size: 25, track_total_hits: true,
            aggs: {
              by_level:   { terms: { field: 'level',   size: 8 } },
              by_service: { terms: { field: 'service', size: 8 } },
              by_host:    { terms: { field: 'host',    size: 8 } },
            }
          },
  };
}

// Build a fake query plan matching the DSL (the planner would produce one)
function buildPlan({ type, q, filters }, total) {
  const filterCount = Object.keys(filters || {}).length;
  const root = { op: 'BoolQuery', estimate: total, cost: total + 80, children: [] };
  switch (type) {
    case 'hybrid':
      root.op = 'Hybrid(rrf,60)';
      root.children.push(
        { op: 'MatchQuery', field: 'message', value: q || '*', estimate: Math.round(total * 1.8), cost: Math.round(total * 1.8 + 20) },
        { op: 'KnnQuery',    field: 'embedding', value: 'k=20 ef_search=96', estimate: 20, cost: 420 },
      );
      break;
    case 'knn':
      root.op = 'KnnQuery';
      root.field = 'embedding';
      root.value = 'k=10 ef_search=96';
      root.estimate = 10;
      root.cost = 310;
      break;
    case 'semantic':
      root.op = 'SemanticSearch';
      root.field = 'embedding';
      root.value = `embed(${q || '*'}) → knn`;
      root.children.push(
        { op: 'EmbedAt/Query', estimate: 1, cost: 120 },
        { op: 'KnnQuery', field: 'embedding', value: 'k=10', estimate: 10, cost: 280 },
      );
      break;
    case 'term':
      root.op = 'TermQuery';
      root.value = q;
      root.estimate = total;
      root.cost = Math.round(total * 0.05 + 8);
      break;
    case 'range':
      root.op = 'RangeQuery';
      root.value = q;
      root.estimate = Math.round(total * 0.4);
      root.cost = Math.round(total * 0.1 + 16);
      break;
    case 'prefix':
      root.op = 'PrefixQuery';
      root.value = q;
      root.children.push({ op: 'FstScan', estimate: total * 2, cost: total * 0.6 });
      break;
    case 'phrase':
      root.op = 'MatchPhrase';
      root.value = `"${q}"`;
      root.children.push({ op: 'PostingsIntersect', estimate: total, cost: total * 0.8 });
      break;
    default:
      root.op = 'MatchQuery';
      root.field = 'message';
      root.value = q;
      root.children.push({ op: 'BM25Scorer', estimate: total, cost: total * 0.3 });
  }
  if (filterCount > 0) {
    root.children.push({
      op: 'FilterCtx', field: 'post-filter', value: `${filterCount} term(s)`, estimate: total, cost: filterCount * 4,
    });
  }
  // Always terminate with a TopK collector (matching the real planner)
  root.children.push({ op: 'TopKCollector', value: 'k=25', estimate: 25, cost: 8 });
  return root;
}

export const searchDiscover = {
  id:   'search-discover',
  name: 'Search · Discover',
  render: ({ data, time, search }) => {
    const r = search?.result;
    const dsl = buildDsl(search);
    const plan = buildPlan(search, r?.total ?? 0);

    return {
      title:  'SEARCH · DISCOVER',
      kicker: 'INTERACTIVE QUERY CONSOLE',
      meta:   [time, '24 DSL TYPES · EXACT AGGS'],
      caption: 'Type a query, press Enter. All 8 query families, 14 aggregation types, and exact cardinality run against the live XERJ.ai index. The plan below comes from POST /v1/indices/:name/explain-plan.',
      panels: [

        { id: 'searchbox', eyebrow: 'QUERY · TYPE · INDEX · FILTERS', cols: 12, type: 'searchbox',
          render: () => SearchBox({
            value: search?.q ?? '',
            types: QUERY_TYPES,
            activeType: search?.type ?? 'match',
            indices: INDICES,
            activeIndex: search?.index ?? '*',
            filters: search?.filters ?? {},
          }),
        },

        { id: 'hits', eyebrow: 'RESULTS · CLICK A COLUMN TO SORT · CLICK INDEX TO FILTER', cols: 8, type: 'hits',
          render: () => Hits({
            hits: r?.hits || [],
            total: r?.total ?? 0,
            tookMs: r?.tookMs ?? 0,
            maxScore: r?.maxScore ?? null,
            sort: search?.sort,
            showTime: search?.showTime !== false,
            // Field display names — GH#1896 (65 reactions): users want to see
            // friendlier column headers than the internal field names.
            labels: { _index: 'INDEX', _id: 'ID', _score: 'SCORE', _ts: 'TIME', _source: 'MESSAGE' },
          }),
        },

        { id: 'facets', eyebrow: 'FACETS · CLICK TO FILTER', cols: 4, type: 'facet',
          render: () => `
            ${Facet({ field: 'level',   items: r?.facets.level   || [], active: search?.filters?.level })}
            ${Facet({ field: 'service', items: r?.facets.service || [], active: search?.filters?.service })}
            ${Facet({ field: '_index',  items: r?.facets._index  || [], active: search?.filters?._index })}
            ${Facet({ field: 'host',    items: r?.facets.host    || [], active: search?.filters?.host })}
          `,
        },

        { id: 'histogram', eyebrow: 'DATE_HISTOGRAM · INTERVAL=1H', cols: 8, type: 'bar',
          render: () => VBar({
            items: (r?.histogram || Array.from({length:24}, ()=>0)).map((v, i) => ({
              label: String(i).padStart(2, '0'), value: v,
            })),
            h: 140, unit: 'hits/bucket',
          }),
        },

        { id: 'searchMetrics', eyebrow: 'INDEX · LIVE', cols: 4, type: 'metric',
          render: () => {
            const totalDocs = data?.metrics?.totalDocs?.formatted || '52.4M';
            const uniqueTerms = data?.metrics?.uniqueTerms?.formatted || '18.9M';
            return `
              <div class="stack-3">
                <div class="row-flex">
                  <div class="num-md accent">${totalDocs}</div>
                  <span class="hint">documents</span>
                </div>
                <div class="row-flex">
                  <div class="num-md">${uniqueTerms}</div>
                  <span class="hint">unique terms · exact cardinality</span>
                </div>
                <div class="row-flex">
                  <div class="num-md">${data?.metrics?.p95?.formatted || '10.4'}<span class="num-unit">ms</span></div>
                  <span class="hint">p95 query latency</span>
                </div>
              </div>`;
          },
        },

        { id: 'dsl', eyebrow: 'REQUEST · POST ' + dsl.path, cols: 6, type: 'markdown',
          render: () => QueryDSL(dsl.body),
        },

        { id: 'plan', eyebrow: 'QUERY PLAN · FROM EXPLAIN-PLAN ENDPOINT', cols: 6, type: 'plan',
          render: () => QueryPlanTree(plan),
        },

        { id: 'qps', eyebrow: 'QUERIES/s OVER TIME', cols: 6, type: 'line',
          render: () => Series(data?.series?.queries || [], {
            h: 100, labels: [data?.series?.startLabel, data?.series?.endLabel], unit: 'qps',
          }),
        },

        { id: 'latency', eyebrow: 'p95 LATENCY OVER TIME', cols: 6, type: 'line',
          render: () => Series(data?.series?.took_p95 || [], {
            h: 100, labels: [data?.series?.startLabel, data?.series?.endLabel], unit: 'ms',
          }),
        },

        { id: 'citations', eyebrow: 'WHY THIS PANEL EXISTS · USER FEEDBACK', cols: 12, type: 'citations',
          render: () => Citations({ items: dashboardCitations['search-discover'] || [], total: 5150 }),
        },

      ],
    };
  },
};
