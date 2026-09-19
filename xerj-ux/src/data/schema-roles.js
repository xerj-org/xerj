// ============================================================
// XERJ Console — field roles from a { field: type } map (pure)
//
// Split out of schema.js so the guest reader can derive roles without
// importing anything that fetches: `deriveRoles` is arithmetic on a mapping,
// nothing more. See schema.js for how the operator console feeds it.
// ============================================================

// Types that hold full-text we can `match` against.
const TEXT_TYPES = new Set(['text', 'match_only_text', 'semantic_text']);
// Types whose value is ONE term: `match` on them needs the whole string.
const KEYWORD_TYPES = new Set(['keyword', 'constant_keyword', 'wildcard']);
// Preference when several text-ish fields exist. `body` (autoindex) and
// `message` (logs) first so the common corpora Just Work.
const TEXT_PREF = ['body', 'message', 'content', 'text', 'email_subject', 'title'];
const SEM_PREF  = ['body', 'content', 'text', 'message'];
// Keyword fields that make poor facets (plumbing / high-cardinality ids).
const FACET_SKIP = /^(ax_|email_message_id|email_in_reply_to|attachment_name|_)/;
// The title-like fields a person searches BY, beside the text fields: an
// email's subject, a document's title, an attachment's file name. MATCH /
// PHRASE / PREFIX run over `searchFields` (search-body.js), so a word that
// appears only in a subject line finds the email (PR #945 review: "Lunch on
// Friday?" was not found by "Lunch" because only `body` was searched). Only
// fields the mapping really has are included, and only when their type is one
// search-body.js has a clause for (text-like or keyword-like).
const EXTRA_SEARCH = ['email_subject', 'title', 'attachment_name'];
/**
 * How many text-typed fields MATCH / PHRASE / PREFIX run over. EVERY text-typed
 * field is searched, in TEXT_PREF order and then mapping order, up to this cap.
 * PR #945 review (second round): one index can hold its text under more than
 * one name — autoindex's prose extractors write `body`, its line extractor
 * (Makefile, .ini, logs) writes `text` — and searching only the preferred one
 * made the other's records unfindable, including by the catalog's own sample
 * query. The cap only bounds the request size on a mapping with dozens of text
 * fields; `searchFieldsCapped` says when it bit.
 */
export const MAX_TEXT_SEARCH_FIELDS = 12;

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

  // Every text-typed field, preferred names first; then the title-like extras.
  const orderedText = [
    ...TEXT_PREF.filter((f) => textFields.includes(f)),
    ...textFields.filter((f) => !TEXT_PREF.includes(f)),
  ];
  const searchText = orderedText.length ? orderedText.slice(0, MAX_TEXT_SEARCH_FIELDS) : [textField];
  const types = Object.fromEntries(entries);
  const searchable = (f) => TEXT_TYPES.has(types[f]) || KEYWORD_TYPES.has(types[f]);
  const searchFields = [...searchText, ...EXTRA_SEARCH.filter((f) => !searchText.includes(f) && searchable(f))];
  // The search fields whose value is ONE term (autoindex types a subject,
  // title or file name `keyword` on most mailboxes — low cardinality once every
  // attachment page copies its parent's subject). search-body.js gives these a
  // contains-the-word clause instead of `match`, which needs the whole string.
  const keywordSearchFields = searchFields.filter((f) => KEYWORD_TYPES.has(types[f]));

  return {
    textField, semanticField, keywordFields, dateField, isEmail,
    searchFields, keywordSearchFields,
    searchFieldsCapped: orderedText.length > MAX_TEXT_SEARCH_FIELDS,
    hasAttachments: names.includes('attachment_name'),
    types, allFields: names,
  };
}

/**
 * `roles` with `field` added to its search fields — a corpus card's sample
 * query names the field it was written for, and running its text anywhere else
 * is not running that query. A field the mapping does not have, or one whose
 * type has no clause here, changes nothing.
 */
export function withSearchField(roles, field) {
  if (!roles || typeof field !== 'string' || !field) return roles;
  const list = Array.isArray(roles.searchFields) ? roles.searchFields : [];
  if (list.includes(field)) return roles;
  const t = roles.types && roles.types[field];
  if (!TEXT_TYPES.has(t) && !KEYWORD_TYPES.has(t)) return roles;
  return {
    ...roles,
    searchFields: [...list, field],
    keywordSearchFields: KEYWORD_TYPES.has(t) ? [...(roles.keywordSearchFields || []), field] : (roles.keywordSearchFields || []),
  };
}
