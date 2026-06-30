# Vendor Lock-In Concerns

## Severity: MODERATE | Frequency: MODERATE

---

## Core Complaints

### Ecosystem Coupling
- Tight coupling between Elastic managed services and Elastic-specific features (ML, security, APM)
- Proprietary features and APIs create switching costs
- Advanced capabilities increasingly restricted to paid Elastic Cloud tiers

### Sales Process
- Enterprise sales model with "high vendor management overhead"
- 3-6 month acquisition timelines
- Elastic made it "very difficult to try it out at scale" and "only wanted to talk to the CTO instead of the persons in charge"
- Some organizations driven toward AWS OpenSearch by frustrating sales experience
- Limited cloud trial of only 14 days

### Demonstrated Willingness to Change Terms
- The 2021 license switch proved Elastic can unilaterally change terms on users who invested heavily
- Creates ongoing risk: "at any time they can just change their minds again"

---

## XERJ.ai Response
- Standard APIs (REST + gRPC + protobuf)
- ES-compatible query DSL on port 9200 for easy migration in AND out
- No proprietary lock-in features
- Self-hosted by design: runs on your infrastructure

## Sources
- wz-it.com: OpenSearch vs Elasticsearch Vendor Lock-in
- SquareShift: Vendor Dependency
- Hacker News discussions
