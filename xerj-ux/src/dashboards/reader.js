// ============================================================
// Section — READER  (any record, with the knowledge graph around it)
//
// Route `#/reader?index=<i>&id=<id>&brain=<b>`. Three panels: the shared
// search (same SearchBox state as Discover, so what you searched there is
// what you page through here), the results list, the open record rendered
// by its shape (ux/reader-render.js), and the brain's links to it.
//
// The shell (app.js) owns fetching: `reader` state carries the open hit, its
// related records (an email's attachments / an attachment's parent email)
// and the ego walk. This module only renders. Generalises #923's Case Review
// (email + PDF) to every record kind autoindex writes.
// ============================================================

import { SearchBox } from '../ux/charts-ops.js';
import { QUERY_TYPES } from '../data/search-body.js';
import { renderResultList, renderRecord, renderGraphPanel, recordTitle } from '../ux/reader-render.js';

export const reader = {
  id:   'reader',
  name: 'Reader',
  section: 'reader',
  render: ({ search, indices, reader: rd, guest }) => {
    const r = search?.result;
    const hits = r?.hits || [];
    const st = rd || {};
    const open = st.hit || null;
    const brain = st.brain || null;
    const idx = search?.index || st.index || '*';
    const idxList = guest ? [guest.index] : ((indices && indices.length) ? indices : ['*']);

    let emptyList;
    if (r?.pending) emptyList = 'Searching…';
    else if (r?.error) emptyList = `Search failed: ${r.error}`;
    else if (!hits.length && !(search?.q || '').trim()) emptyList = `No records in ${idx}. Index a folder first: xerj brain <folder>`;
    else emptyList = 'No matches. Try a broader query or another query type.';

    const meta = [];
    if (open) meta.push(recordTitle(open._source, open._id).slice(0, 48).toUpperCase());
    if (brain) meta.push(`BRAIN ${brain.toUpperCase()}`);

    return {
      title:  'READER',
      kicker: open ? `${String(idx).toUpperCase()} · ONE RECORD` : String(idx).toUpperCase(),
      meta,
      caption: '',
      panels: [
        { id: 'searchbox', eyebrow: 'FIND A RECORD', cols: 12, type: 'searchbox',
          render: () => SearchBox({
            value: search?.q ?? '',
            types: QUERY_TYPES,
            activeType: search?.type ?? 'match',
            indices: idxList,
            activeIndex: idx,
            filters: search?.filters ?? {},
            placeholder: 'find a record · press Enter · empty = newest first',
          }),
        },
        { id: 'results', eyebrow: `${r?.total != null ? r.total : hits.length} RESULTS · CLICK TO OPEN`, cols: 4, type: 'reader-list',
          render: () => renderResultList(hits, { selectedId: open?._id || st.id, brain, emptyText: emptyList }),
        },
        { id: 'record', eyebrow: open ? 'RECORD' : (st.loading ? 'RECORD · LOADING' : (st.error ? 'RECORD · NOT FOUND' : 'RECORD')), cols: 8, type: 'reader',
          render: () => (open
            ? renderRecord(open, { related: st.related, brain })
            : renderRecord(null, { emptyText: st.error ? st.error : (st.loading ? 'Loading…' : undefined) })),
        },
        { id: 'graph', eyebrow: 'LINKED RECORDS · FROM THE BRAIN', cols: 12, type: 'reader-graph',
          render: () => renderGraphPanel(open ? { ...(st.graph || { status: 'loading' }), brain, index: open._index } : { status: 'idle' }),
        },
      ],
    };
  },
};
