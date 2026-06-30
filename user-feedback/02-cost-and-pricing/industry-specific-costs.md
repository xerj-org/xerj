# Industry-Specific Cost and Compliance Burdens

## Severity: HIGH | Frequency: MODERATE

---

## Financial Services / FinTech

### End-of-Life Compliance Risk
- Running unsupported ES versions violates PCI DSS, SOC 2, and regional financial regulations
- "Transaction data, fraud detection models, and compliance logging all depend on search infrastructure"

### Licensing "Double Penalty"
- License fees use total addressable memory (ERU) to calculate costs
- Adding RAM to improve performance ALSO increases the license fee
- Searchable snapshots (needed for cheapest storage) restricted to Enterprise tier
- "Paradox where utilizing the most cost-effective storage requires paying the maximum software license fee"

### Audit Trail Performance Tax
- Audit trail brings "inevitable drawback of I/O performance penalty"
- Audit log index can grow so large it threatens production cluster stability
- Recommended to use external storage for audit logs

### Data Retention Regulatory Burden
- GDPR, MiFID II, PCI DSS, SOX require 3-10 year data retention
- ES native features "provide a good starting point but manual setups cannot keep up"

---

## Healthcare

### HIPAA Gaps
- Open-source version "does not implement all safeguards required for HIPAA compliance"
- Must use paid Elastic Cloud or third-party security plugins

### Major Breaches
- Catholic Health: 483,000 patients exposed (November 2024) -- ES database "accessed without authentication" for 6 weeks
- University of Chicago Medicine: 1,679,993 records exposed from misconfigured ES server

### EHR Performance
- Parent-child queries "5 to 10 times slower than equivalent nested queries"
- Denormalized structures cause "document size grows tremendously"

---

## Gaming / iGaming

### Data Leaks
- Unprotected ES server exposed 180 million bets with real names, addresses, emails, passwords, account balances, login credentials
- Operator acknowledged being "grateful it was [the researcher] to discover this"

### Latency
- If all search threads busy, requests wait in queue
- GC cycles, disk IO, network saturation all degrade gaming latency

---

## Cybersecurity / SIEM

### Alert Fatigue
- 71% of SOC personnel experience burnout from alert volume
- 62% of alerts entirely ignored; accuracy drops 40% after extended shifts

### Detection Limitations
- High cardinality fields cause "rule timeout or circuit breaker error"
- "Limited support for indicator match rules"
- No native SOAR capabilities (Splunk has them)
- Ingestion delays must stay under 6 minutes to avoid missed alerts

---

## E-commerce

### Irrelevant Search Results
- Searching "clementines" returns sandwiches and biscuits
- "Orange juice" surfaces washing machine additive
- "70% of top 50 e-commerce sites cannot return relevant results for synonyms"

### Integration Issues
- Magento 2: "Out of the box Elasticsearch just doesn't provide faster and more relevant search"
- SKUs with hyphens produce "extremely poor search results"
- Faceting count inconsistency when applying filters

---

## Government / Defense

### Data Sovereignty
- ES "doesn't recommend distributing data across multiple locations globally"
- Treats all nodes as colocated; ignores cross-datacenter latency
- Network partition risk: "remote shards will be out of date"

---

## Cross-Industry: GDPR

### Right-to-Be-Forgotten Conflict
- "Deleted documents are not really deleted, but only marked as such" due to immutable segments
- Documents "only really deleted when segments are merged (at some hard to define point)"
- Organizations "may not be able to guarantee this will happen within 30 days" as GDPR requires

---

## XERJ.ai Response
- Secure by default: TLS + auth out of box (no HIPAA/compliance gaps)
- No licensing "double penalty" (simple pricing, no ERU)
- Compression reduces storage costs for long retention requirements
- Single-node in M1: simpler compliance posture
- Explicit segment purge for GDPR compliance (TTL + forced merge)
- AI-native search relevance (hybrid search) for better e-commerce results

## Sources
- Finextra, Search Guard, DataSunrise, HIPAA Journal
- LCB.org, Elastic Blog, Elastic Docs
- Quesma: Elastic Pricing
- Eivind Arvesen: Elasticsearch and GDPR
