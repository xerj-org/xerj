# API, Client Libraries, and Developer Experience Pain

## Severity: HIGH | Frequency: HIGH

---

## Core Complaints

### No Reference Documentation
> "Elasticsearch is a mess. It's so full of historical warts. One major problem is none of their documentation is actually reference documentation -- if you look for the formal schema (for things like mappings and the query DSL), the list of endpoints and their allowed parameters, the full list of settings etc., you won't find them listed anywhere."
> -- HN user "atombender"

### Client Libraries Are Terrible
> "The client libraries provided by Elasticsearch are just so, so bad. Fragmented, badly to not documented, different in every language."
> -- HN user "9dev"

> "The ElasticSearch Golang library is a glorified wrapper around an HTTP Client that adds only frustration"
> -- HN user "studmuffin650"

> "Go's type system is insufficiently typeful to handle the unrestrained madness that is ElasticSearch JSON"
> -- HN user "cratermoon"

### Specific Client Issues
- **Python client**: Unicode encoding errors in bulk helper, resource warnings from unclosed transport, unclosed CLOSE_WAIT connections
- **.NET client (NEST)**: Performance hit upgrading 5.x to 6.x with complex documents (GeoJSON)
- **JavaScript client**: Ongoing specification and helper issues
- **Rust client**: TLS defaults and HTTP transport issues with Docker containers

### Constant Reorganization Breaks Everything
> "For the love of internet-god, please stop your constant moving of stuff around."
> -- HN user "vacri"

> They've "had to basically rework everything due to Elastic changing up the core architecture, naming, logos, etc."
> -- HN user "geerlingguy"

### Rebranding Makes Troubleshooting Impossible
> "Their renaming of ElasticSearch to Elastic really irked me...searching for questions related to them on Google is harder now."
> -- HN user "AznHisoka"

### Query DSL Verbosity
> "Elasticsearch quite conveniently makes it so difficult and fucking annoying to do data analytics"
> A simple task (average energy usage by day-of-week and 15-min intervals) required an ES expert "five attempts...over two days" -- "literally just a SELECT MOD GROUP BY in SQL"
> -- HN user "0db532a0"

> "As soon as you step outside those bounds, the query syntax is horrible and they slow down dramatically."
> -- melloy.life blog

### No Query Explain
> "Unlike SQL, where you get a nice explain plan and various tools to see, in ElasticSearch you just get a blanket 'done'"
> -- melloy.life blog

### Logstash Documentation
> "Awful documentation. Deprecated shit everywhere. Inconsistent stackoverflow information and TWO external websites to help make logstash actually functional."
> -- HN user "bpchaps"

---

## XERJ.ai Response
- OpenAPI spec generated from code → always-accurate reference docs
- Protobuf definitions as the canonical API contract
- Consistent REST + gRPC + ES-compat: three interfaces, one behavior
- No client library needed for basic usage (curl works)
- No rebranding, no constant reorganization
- Simple API: 10 endpoints, not hundreds
- Clear error messages with suggested fixes

## Sources
- Hacker News: #16488925, #36276198, #18683212, #11123479
- DevRant: #2385069
- melloy.life blog
