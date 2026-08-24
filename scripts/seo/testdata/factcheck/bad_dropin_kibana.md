---
title: "Is XERJ a drop-in replacement for Elasticsearch?"
evidence:
  - claim: "XERJ speaks the Elasticsearch wire protocol"
    source: "demo/playbooks/ES_COMPATIBILITY.md"
expect: [FC-DROPIN, FC-KIBANA, FC-CLIENTS-TESTED, FC-ALERTING, FC-SCROLL, FC-SPAN, FC-PROFILE-EXPLAIN, FC-CONFORMANCE-CAVEAT]
---

# Is XERJ a drop-in replacement for Elasticsearch?

XERJ is a drop-in replacement for Elasticsearch. Kibana connects to it directly,
and your existing clients, dashboards and tooling connect unchanged. We have
tested it with the official Elasticsearch clients.

Scroll is supported, span queries work, and alerting rules fire through
`_watcher` exactly as they do on Elasticsearch. The profile API shows you where
time went, and 99.8% of the conformance suite passes.
