// ============================================================
// XERJ Console — field roles from a { field: type } map (pure)
//
// Split out of schema.js so the guest reader can derive roles without
// importing anything that fetches: `deriveRoles` is arithmetic on a mapping,
// nothing more. See schema.js for how the operator console feeds it.
// ============================================================

// Types that hold full-text we can `match` against.
const TEXT_TYPES = new Set(['text', 'match_only_text', 'semantic_text']);
// Preference when several text-ish fields exist. `body` (autoindex) and
// `message` (logs) first so the common corpora Just Work.
const TEXT_PREF = ['body', 'message', 'content', 'text', 'email_subject', 'title'];
const SEM_PREF  = ['body', 'content', 'text', 'message'];
// Keyword fields that make poor facets (plumbing / high-cardinality ids).
const FACET_SKIP = /^(ax_|email_message_id|email_in_reply_to|attachment_name|_)/;
// The title-like fields a person searches BY, beside the text field: an email's
// subject, a document's title, an attachment's file name. MATCH / PHRASE /
// PREFIX run over `searchFields` (search-body.js), so a word that appears only
// in a subject line finds the email (PR #945 review: "Lunch on Friday?" was not
// found by "Lunch" because only `body` was searched). Only fields the mapping
// really has are included.
const EXTRA_SEARCH = ['email_subject', 'title', 'attachment_name'];

function pick(prefList, present) {
  for (const p of prefList) if (present.includes(p)) return p;
  return null;
}

/** Derive the field roles the search UI needs from a { field: type } map. */
export function deriveRoles(fieldTypes) {
  const entries = Object.entries(fieldTypes || {});
  const names = entries.map(([f]) => f);
  const textFields = entries.filter(([, t]) => TEXT_TYPES.has(t)).map(([f]) => f);
  const semanticFields = entries.filter(([, t]) => t === 'semantic_text').map(([f]) => f);
  const keywordFields = entries
    .filter(([, t]) => t === 'keyword')
    .map(([f]) => f)
    .filter((f) => !FACET_SKIP.test(f));
  const dateFields = entries.filter(([, t]) => t === 'date' || t === 'date_nanos').map(([f]) => f);

  const textField = pick(TEXT_PREF, textFields) || textFields[0] || 'body';
  // Prefer a real semantic_text field; else the best text field (the engine
  // resolves body -> body_vector at query time); else the text field itself.
  const semanticField = semanticFields[0] || pick(SEM_PREF, textFields) || textField;
  const dateField = names.includes('@timestamp') ? '@timestamp'
                  : (dateFields[0] || (names.includes('email_date') ? 'email_date' : null));
  const isEmail = names.includes('email_from') || names.includes('email_subject');
  const searchFields = [textField, ...EXTRA_SEARCH.filter((f) => f !== textField && names.includes(f))];

  return { textField, semanticField, keywordFields, dateField, isEmail, searchFields, allFields: names };
}
