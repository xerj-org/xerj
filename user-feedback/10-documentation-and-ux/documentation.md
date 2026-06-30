# Documentation and UX Issues

## Severity: MODERATE | Frequency: HIGH

---

## Core Complaints

### Poor Documentation Quality
- "The documentation provided by Elastic is far too complex and vague to be of any real help when troubleshooting" -- Gartner Peer Insights
- "Documentation for Java library is not that great compared to the APIs documentation" -- TrustRadius
- Material "inconsistent and incomplete, especially regarding best practices"
- "Lack of comprehensive tutorials" and "absence of detailed examples for advanced functionalities"
- "Configuration process lacking... incomplete documentation, misleading forums"
- Hardware configuration and capacity planning guidance at scale particularly lacking
- "Documentation on the website is quite hard to navigate and examples are often out of date"

### API-Centric / Non-Technical User Barriers
- "Elasticsearch alone isn't an end-user product; requires Kibana or Grafana" -- Dominic R., Systems Architect
- "Very API-centric and although Kibana continues to improve, non-technical users find it challenging"
- "Built-in visualizations confusing and difficult to use"
- "UI is simple, could be made more robust and dynamic" -- Data Scientist (Capterra)
- "Writing complicated queries is tedious; JSON interface difficult to parse"
- "Reporting customization for executive summaries feels clunky compared to Kibana's investigative prowess" -- Gartner
- "Does not currently have a way to export charts and graphs"

### Query DSL Verbosity
- JSON-based query language is verbose compared to SQL
- Even simple queries require deeply nested JSON structures
- Error-prone to write and modify by hand

---

## XERJ.ai Response
- Minimal API surface (10 REST endpoints, not hundreds)
- Clear, structured error messages with actionable guidance
- Native API designed for simplicity (not nested JSON DSL)
- ES-compat layer for users already familiar with ES DSL
- API documentation generated from protobuf/OpenAPI specs (always in sync)
- No separate visualization product needed for basic operations

## Sources
- G2, Gartner, TrustRadius, Capterra reviews
- AltExSoft: The Good and Bad of Elasticsearch
- Meilisearch: Elasticsearch Review
