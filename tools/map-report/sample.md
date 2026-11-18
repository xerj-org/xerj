# XERJ data map

An offline inventory of the selected catalog datasets.

File paths and skip reasons are redacted. Dataset/index/format and symbol names remain; review these before sharing.

Catalog snapshot, not a live inventory&#58; at most 500 datasets and 500 skipped-file entries. Counts are per dataset; files shared across datasets are not deduplicated. Lists and enrichments are bounded and may be incomplete.

Suggested searches are unexecuted lexical probes, not guaranteed answers. Commands use your search client&#39;s endpoint/auth settings; select the original node with XERJ&#95;URL if needed. On a multi-prefix map, suggestions target the first prefix alphabetically.

## Datasets

2 catalog dataset entries shown.

| Dataset | Index | Records | Files | Formats |
| --- | --- | --- | --- | --- |
| docs | mapfixture-docs | 9 | 3 | code, txt-prose |
| data | mapfixture-data | 3 | 1 | csv |

## File families

Catalog format labels. A dataset can contain more than one format; these are dataset counts, not file counts.

| Format | Datasets containing format |
| --- | --- |
| code | 1 |
| csv | 1 |
| txt-prose | 1 |

## Skipped and refused files

Catalog exclusions include skipped, junk and refused files. An empty list does not prove every file was indexed.

| Path | Format | Status | Reason |
| --- | --- | --- | --- |
| &#91;path redacted&#93; | binary | junk | &#91;reason omitted&#58; may contain paths&#93; |

## Top symbols

Up to 10 names per dataset, for the first 20 datasets. Counts are indexed definition records, not calls or relevance. Only keyword-mapped name fields are aggregated.

| Index | Symbol | Definition records |
| --- | --- | --- |
| mapfixture-docs | ParcelStore | 1 |
| mapfixture-docs | insert | 1 |
| mapfixture-docs | new | 1 |
| mapfixture-docs | startup | 1 |
| mapfixture-docs | store | 1 |
| mapfixture-docs | validate&#95;id | 1 |

## Most-linked files — fixture

Top 10 incoming graph nodes, hydrated only from the selected datasets. Edge counts belong to each node, not a sum over all records in a file. At most 5 brains are inspected. Relationships are deterministic links, not semantic understanding.

| File | Incoming live edges |
| --- | --- |
| &#91;path redacted&#93; | 3 |
| &#91;path redacted&#93; | 2 |
| &#91;path redacted&#93; | 2 |
| &#91;path redacted&#93; | 1 |

## Questions to explore

Commands below use POSIX shell quoting.

Where is ParcelStore described or implemented?

<pre><code>xerj search --prefix &#39;mapfixture&#39; &#39;ParcelStore&#39;</code></pre>

Where is insert described or implemented?

<pre><code>xerj search --prefix &#39;mapfixture&#39; &#39;insert&#39;</code></pre>

Where is new described or implemented?

<pre><code>xerj search --prefix &#39;mapfixture&#39; &#39;new&#39;</code></pre>

Where is startup described or implemented?

<pre><code>xerj search --prefix &#39;mapfixture&#39; &#39;startup&#39;</code></pre>

Where is store described or implemented?

<pre><code>xerj search --prefix &#39;mapfixture&#39; &#39;store&#39;</code></pre>
