# Pricing Complexity and Opacity

## Severity: HIGH | Frequency: HIGH

---

## Core Complaints

### Confusing Tier Structure
- Standard (~$99/mo), Gold (discontinued), Platinum (~$131/mo), Enterprise (~$184/mo)
- Users confused about tier differences
- Gold tier discontinued for new customers, stranding existing subscribers
- Forum question: why choose Platinum over Enterprise since Enterprise includes searchable snapshots?

### Opaque Enterprise Pricing
- Enterprise pricing not publicly posted; requires contacting sales
- Quotes vary, making comparison difficult
- Users describe pricing as "frustratingly elusive"
- Three deployment models x multiple support tiers x resource-based calculations = impossible to budget

### Unpredictable Costs
- Costs depend on node configurations, data volumes, and usage patterns hard to predict
- Active deployments incur charges even with zero activity and no console logins
- Adding credit card during trial immediately converts to paid subscription with retroactive billing
- Going above free tier limits triggers full charges for entire instance size

### Essential Features Paywalled
- Advanced security (SSO, RBAC, audit logs): Platinum/Enterprise
- ML anomaly detection and inference: Platinum+
- Cross-cluster replication: Platinum
- Searchable snapshots: Enterprise
- SOC2 compliance reports: Enterprise
- `sub_searches` with `rank` function for hybrid search: commercial license required
- One user paid EUR 6,000 per machine just for security features

### January 2025 Price Increase
- Estimated 30% price increase for typical production workloads
- Elastic Cloud markup on top of underlying cloud provider costs

---

## User Quotes

> "The licensing models are very confusing with a strong push toward their hosted SaaS offering"
> -- G2 reviewer

> "Commercial licensing for advanced features like machine learning can get expensive"
> -- G2 reviewer

> "Cost optimization is a constant operational task"
> -- Multiple reviewers

> "SSO as an Enterprise feature" and "SOC2 reports requiring Enterprise subscriptions" despite minimal incremental cost to Elastic
> -- Hacker News commenter

---

## XERJ.ai Response
- Simple, transparent pricing (details TBD per PROD.brief.md)
- No feature gating in M1 -- all capabilities available
- No tiered licensing confusion
- Self-hosted with clear resource requirements
- Cost reduction is the core value proposition (80% less than ES)

## Sources
- Elastic Subscriptions page
- Quesma: Elasticsearch Pricing analysis
- Meilisearch: Elasticsearch Pricing Analysis
- PeerSpot: ELK Pricing discussions
- Hacker News discussions
