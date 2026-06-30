# User Expectations Around Open Source Licensing for Databases
## Research Focus: Elasticsearch License Changes and Broader Database Licensing Trends

**Research Date:** April 2026 (data collected April 2026, covering 2021–2025 events)
**Sources:** Socket.dev, InfoQ, IT Pro, Revenera, Pureinsights, RedMonk, Percona, AppFlowy, Medium/Goortani, Dattell, SigNoz, ITBrief.asia, Kuray.dev, OpenSearch.org, and others

---

## Executive Summary

User trust in database and search engine vendors is fundamentally shaped by licensing history. Elastic's 2021 shift from Apache 2.0 to SSPL/proprietary licenses triggered a fork (OpenSearch) that reached 496 contributors and 100M+ downloads in its first year, and despite Elastic's 2024 return to OSI-approved AGPL, developers remain broadly unwilling to switch back. The pattern repeated with Redis in 2024–2025. Users have internalized a strong heuristic: **OSI-approved permissive licenses signal trustworthiness; anything else signals risk**.

---

## Data Points: Elasticsearch License Change — Developer Reactions

1. **Trust violation as primary response.** When Elastic moved from Apache 2.0 to SSPL/ELv2 in January 2021, the dominant developer emotion was betrayal. One contributor stated: "I saw my modest contributions under the Apache license being locked up behind this bullshit license."

2. **Migration effort cements new choices.** Developers who migrated to OpenSearch report "zero motivation to move back to ElasticSearch" — the effort invested in migration becomes a psychological and technical anchor.

3. **Typical developer voice:** "Cost me a bunch of time fixing and migrating code when they pulled the plug. So not going to trust ES again." (Socket.dev)

4. **Return announcement called disingenuous.** Elastic's August 2024 AGPL announcement was described by one developer as reading "like an April fools joke" — the framing lacked accountability.

5. **No competitive reason to return.** Industry consultant quoted: "It isn't even close. Why would you pick Elastic as a first time user?" — positioning OpenSearch as the default for net-new users.

6. **Stock market validated community distrust.** Elastic N.V. shares dropped nearly 25% after Q1 2025 earnings warnings, partly attributed to customer commitment instability caused by licensing uncertainty.

7. **OpenSearch fork success as proof.** OpenSearch reached 496 contributors and 100 million downloads within its first year — demonstrating that a license-triggered fork can rapidly achieve viability.

8. **AWS created OpenSearch specifically because of licensing risk.** AWS needed to provide a managed search service without legal exposure to SSPL; this is a canonical example of licensing forcing major infrastructure decisions.

9. **Peter Zaitsev questioned trust recovery timeline.** The database industry expert publicly questioned whether community trust can recover quickly after a license reversal.

10. **Lars Larsson observation.** Companies "shouldn't expect community members to flock back" after past licensing shifts — trust does not auto-restore with license restoration.

11. **Contributor CLA retroactive effect.** When Elastic closed the source, contributors saw their Apache 2.0 contributions effectively monetized without reciprocation — a grievance that persists regardless of subsequent relicensing.

12. **AGPL return framed as strategic, not principled.** Multiple community voices characterized Elastic's AGPL adoption as protecting Elastic from cloud providers rather than genuinely restoring user freedoms.

---

## Data Points: Elasticsearch vs. OpenSearch — Licensing Preferences 2025

13. **Apache 2.0 remains the gold standard.** OpenSearch's Apache 2.0 license is consistently cited as a primary selection criterion for organizations with open-source policies.

14. **Organizations with OSI-only policies must choose OpenSearch.** Companies requiring OSI-approved licenses for all infrastructure components are structurally excluded from Elasticsearch under its SSPL/ELv2 dual license; AGPLv3 option does not resolve all constraints.

15. **OpenSearch transferred governance to Linux Foundation, September 2024.** This move directly addresses single-vendor dependency concerns — reducing AWS-lock-in perception alongside licensing openness.

16. **Security features free in OpenSearch, paid in Elasticsearch.** OpenSearch includes SSO, audit logging, anomaly detection, and ML features at zero cost; Elasticsearch gates these behind paid subscriptions. Licensing model affects total cost of ownership directly.

17. **OpenSearch supports OpenTelemetry; Elasticsearch pushes proprietary Elastic Agent.** Licensing philosophy extends to ecosystem tooling; users preferring open standards choose OpenSearch's approach.

18. **Migration from Elasticsearch to OpenSearch is backward-compatible; reverse is not.** This asymmetry makes migration to OpenSearch a one-way door for most organizations, solidifying market share gains.

19. **OpenSearch 3.0 released April 2025** with Apache Lucene 10, delivering up to 25% faster range queries and 2.5x faster concurrent k-NN search — demonstrating community-driven development pace under permissive licensing.

20. **Performance gap is real but contextual.** Elastic claims 40–140% speed advantage; a March 2025 Trail of Bits benchmark found OpenSearch faster on the "Big 5" workload and vector search. Users report the gap matters primarily at enterprise scale.

21. **Elasticsearch AGPL option adds complexity, not simplicity.** Adding a third license option (AGPL alongside SSPL and ELv2) is seen as muddying compliance rather than simplifying it.

22. **OpenSearch appeals to AWS-native shops.** Native IAM, KMS, and CloudWatch integration makes OpenSearch a natural fit in AWS environments, with Apache 2.0 removing legal friction.

23. **Elasticsearch retains edge for Elastic Stack loyalty.** Organizations already deep in Kibana, Elastic APM, and Elastic Security choose to stay — licensing is one factor among many for existing customers.

---

## Data Points: Industry-Wide Database Licensing Trends 2024–2025

24. **Redis March 2024 relicensing triggered massive fork.** Redis Labs moved from BSD 3-Clause to RSALv2 + SSPL; within weeks, Valkey achieved 20,000+ GitHub stars as the community alternative.

25. **Valkey fork influenced Redis to reverse course.** Redis announced AGPLv3 adoption on May 1, 2025 (Redis 8), reversing the SSPL decision after observing community flight to Valkey.

26. **Salvatore "antirez" Sanfilippo on SSPL failure:** "The SSPL, in practical terms, failed to be accepted by the community. The OSI wouldn't accept it." — the creator of Redis publicly declared SSPL a failure.

27. **Redis CEO acknowledged commercial pressure.** "The majority of Redis' commercial sales are channeled through largest cloud providers, who commoditize our investments" — the honest business rationale for license changes, which users find frustrating.

28. **Debian, Red Hat, and Fedora dropped MongoDB after SSPL.** These distributions refused to package SSPL software, treating it as non-open-source — demonstrating concrete ecosystem consequences of non-OSI licenses.

29. **SSPL not recognized as free software by OSI, Red Hat, or Debian.** The rejection of SSPL by major open source authorities is a concrete, citable reason users avoid SSPL-licensed databases.

30. **SSPL called "fauxpen" by critics.** The term encapsulates user sentiment: source is visible but key freedoms are absent, violating the Open Source Definition's "no discrimination against fields of use" principle.

31. **Broad pattern of commercial OSS relicensing.** CockroachDB, TimescaleDB, Redis, Confluent, and Elastic all moved away from permissive open source in the 2018–2024 period — users have learned to expect this from commercial OSS vendors.

32. **AGPL emerging as new equilibrium.** RedMonk analysis (May 2025) identifies AGPL as the new commercial open source licensing sweet spot — OSI-approved but protective against cloud provider extraction.

33. **Triple licensing model emerging.** Vendors with established forks are experimenting with AGPL + commercial + source-available options to serve different market segments.

34. **Grafana and MinIO moved to AGPL in 2021** (April/May), establishing an early precedent for AGPL as a commercial-open hybrid that predates Elastic's and Redis's AGPL adoption.

35. **Zitadel transitioned to AGPL in March 2025** — continued industry movement toward AGPL as the "safe" commercial open source license.

36. **Open Source Initiative tracked license pageviews.** Top OSI licenses by community interest in 2025: MIT, Apache 2.0, BSD 3-clause, BSD 2-clause, GPL 2.0, GPL 3.0 — permissive and classic copyleft dominate user intent.

---

## Data Points: User Attitudes About Vendor Lock-In

37. **62% of respondents use open source software specifically to avoid vendor lock-in.** (Percona Open Source Data Management Software Survey) — vendor lock-in avoidance is the second most-cited reason for open source adoption after cost savings.

38. **Top reasons for open source database adoption:** (1) Cost savings, (2) Avoiding vendor lock-in, (3) Community support, (4) Ease of use, (5) Security. (Percona)

39. **SaaS costs growing at 5x the rate of inflation.** Rising SaaS expenses make license stability a critical budget concern — unexpected license shifts disrupt multi-year cost planning.

40. **Experts recommend community-driven over vendor-controlled projects.** PostgreSQL and MySQL cited as models because governance is distributed, making unilateral license changes structurally harder.

41. **"How would we get out if needed?" is now a standard evaluation question.** Organizations are advised to assess exit paths before adopting any database service.

42. **Data portability and backup ownership cited as primary defenses.** Organizations maintain controls outside vendor systems to reduce lock-in regardless of license.

43. **Open standards preference.** Businesses seek databases supporting "standard technologies widely supported across platforms" — PostgreSQL, MySQL, Apache Kafka — over vendor-specific implementations.

44. **FerretDB cited as anti-lock-in tool.** MongoDB-compatible proxy stored on PostgreSQL eliminates MongoDB vendor dependency, showing users actively seek license-neutral alternatives.

45. **Growing companies face heightened lock-in vulnerability.** Initial infrastructure choices become entrenched as scale increases; licensing risk compounds over time.

46. **Linux Foundation governance seen as lock-in mitigation.** OpenSearch's transfer to Linux Foundation is explicitly cited as reducing single-vendor risk — governance matters alongside license text.

---

## Data Points: Organizational and Enterprise Decision-Making

47. **Licensing changes create compliance department burden.** Legal review of SSPL compliance requirements has real cost; organizations report avoiding SSPL-licensed tools to eliminate ambiguity.

48. **Some organizations avoided SSPL adoption entirely due to compliance fears.** The legal complexity of SSPL, particularly around "offering the service to third parties," created enterprise-wide avoidance.

49. **Amanda Brock (OpenUK CEO) characterized Redis's pattern as "undermining open source's fundamental free flow of value."** Industry leadership voices carry weight in enterprise procurement decisions.

50. **Microsoft acknowledged RSAL/SSPL licenses pushed users toward alternatives.** When a major cloud provider signals that a license model drives customers to forks, enterprises take notice.

51. **AWS, Google, Oracle backed Valkey fork over Redis SSPL.** Cloud provider endorsement of forks signals that SSPL-licensed tools will lack managed service support — a decisive factor for cloud-native users.

52. **Community perception of "open source" designation carries weight.** Elastic CEO Shay Banon acknowledged: "It's still magical to say 'open source'" — recognizing brand value of the designation for developer adoption.

53. **78% of technical professionals consider PostgreSQL important for AI/ML initiatives.** (ITBrief.asia survey, open source database report) — community-governed, permissively licensed databases win enterprise AI workloads.

54. **25% of survey respondents rate PostgreSQL as "mission critical" for AI activities.** — reinforcing preference for stable, openly-licensed infrastructure for high-stakes workloads.

55. **Licensing revisions by Redis and Elastic cited explicitly in industry reports as creating "uncertainty."** Enterprise buyers slow procurement decisions when licensing stability is in question.

56. **"Open source" label used in marketing regardless of license compliance.** Users have become skeptical of vendor claims, requiring verification of OSI approval before trusting "open source" branding.

---

## Data Points: Historical Context and Ecosystem Impact

57. **Elastic's 2021 license change was immediate and unilateral.** No community consultation period; users discovered the change had already happened, establishing a pattern of vendor-first decision-making.

58. **AWS rebranded its fork "Amazon OpenSearch Service" after Elastic's trademark objections.** The original naming dispute ("Amazon Elasticsearch Service") was the catalyst for the entire ecosystem split — naming and licensing are intertwined.

59. **OpenSearch's Apache 2.0 license was explicitly chosen to avoid cloud provider disputes.** The fork's permissive license was a deliberate policy position, not merely a default.

60. **Elastic's licensing change's stated justification was trademark violation, not pure code reuse.** Users note the stated reason (confusion about who made the product) differs from the licensing mechanism deployed (restricting code usage) — perceived as pretextual.

61. **Adrian Cockcroft referenced 2018 disagreements between Elastic and AWS over feature contributions.** The licensing dispute had roots in a years-long governance and contribution conflict, not a sudden decision.

62. **Elastic CEO stated the SSPL era "forced AWS to fork."** Retrospectively framing the SSPL period as achieving its intended effect — but users see this as confirmation that license weaponization is Elastic's playbook.

63. **Community members whose Apache 2.0 contributions were relicensed have not forgiven.** This specific grievance — personal contributions absorbed into a proprietary regime — is distinct from general licensing concerns and more visceral.

64. **OpenSearch ecosystem diversity grew through permissive licensing.** Managed service providers, enterprises, and independent contributors are more willing to invest in Apache 2.0 projects because their contributions cannot be retroactively captured.

---

## Synthesis: What Users Expect from Truly Open Source Databases

**Users expect:**
- OSI-approved licensing (MIT, Apache 2.0, or established copyleft like AGPL/GPL)
- Governance that distributes control (foundations, community boards, not single vendor)
- No history of unilateral license changes — track record matters more than current license
- Full feature availability without paywalls, especially for security features
- Interoperability via open standards (OpenTelemetry, SQL, etc.) rather than proprietary APIs
- Clear contribution terms that prevent retroactive capture of community work
- Permissive data export without vendor approval

**Red flags users now screen for:**
- SSPL or other non-OSI licenses marketed as "open"
- CLA terms that give vendor unlimited rights to relicense contributions
- Security features gated behind paid tiers
- Single vendor controlling both license and roadmap
- No foundation or independent governance body

---

## Sources

- [Developers Burned by Elasticsearch's License Change Aren't Going Back - Socket.dev](https://socket.dev/blog/developers-burned-by-elasticsearch-license-change-arent-going-back)
- [Elastic Returns to Open Source: Will the Community Follow? - InfoQ](https://www.infoq.com/news/2024/09/elastic-open-source-agpl/)
- [Elastic Returns to Open Source, But Can It Regain Community Trust? - IT Pro](https://www.itpro.com/software/open-source/elastic-returns-to-open-source-but-can-it-regain-the-communitys-trust-some-industry-players-arent-holding-their-breath)
- [Elastic's Return to Open Source - Revenera Blog](https://www.revenera.com/blog/software-composition-analysis/elastics-return-to-open-source/)
- [Elastic's Journey from Apache 2.0 to AGPL 3 - Pureinsights](https://pureinsights.com/blog/2024/elastics-journey-from-apache-2-0-to-agpl-3/)
- [Elasticsearch vs OpenSearch in 2025: What the Fork? - Pureinsights](https://pureinsights.com/blog/2025/elasticsearch-vs-opensearch-in-2025-what-the-fork/)
- [OSS: Two Steps Forward, One Step Back - RedMonk](https://redmonk.com/sogrady/2025/05/06/oss-forward-back/)
- [Redis's U-Turn: Abandoning SSPL and Returning to Open Source - Kuray.dev](https://kuray.dev/blog/backend-development/rediss-u-turn-abandoning-sspl-and-returning-to-open-source-202505)
- [Open Source Database Report: AI, Cloud & Licensing - ITBrief.asia](https://itbrief.asia/story/open-source-database-report-highlights-ai-cloud-licensing)
- [Can Open Source Software Save You From Vendor Lock-In? - Percona](https://www.percona.com/blog/can-open-source-software-save-you-from-vendor-lock-in/)
- [Vendor Lock-In Risk Mitigation with Open Source Tools - AppFlowy](https://appflowy.com/blog/Vendor-Lock-In-Risk-Mitigation-with-Open-Source-Tools)
- [OpenSearch vs. Elasticsearch: A Comprehensive Comparison in 2025 - Medium/Frank Goortani](https://medium.com/@FrankGoortani/opensearch-vs-elasticsearch-a-comprehensive-comparison-in-2025-aff5a8533422)
- [Elasticsearch vs OpenSearch: Key Differences 5 Years Down the Line - SigNoz](https://signoz.io/comparisons/elasticsearch-vs-opensearch/)
- [Top Open Source Licenses in 2025 - Open Source Initiative](https://opensource.org/blog/top-open-source-licenses-in-2025)
- [FAQ on Software Licensing - Elastic](https://www.elastic.co/pricing/faq/licensing)
- [Complete Guide to OpenSearch in 2025 - Instaclustr](https://www.instaclustr.com/education/opensearch/complete-guide-to-opensearch-in-2025/)
- [OpenSearch in 2025: Much More Than an Elasticsearch Fork - InfoWorld](https://www.infoworld.com/article/3971473/opensearch-in-2025-much-more-than-an-elasticsearch-fork.html)
- [The Case Against the Server Side Public License (SSPL) - The New Stack](https://thenewstack.io/the-case-against-the-server-side-public-license-sspl/)
- [Server Side Public License - Wikipedia](https://en.wikipedia.org/wiki/Server_Side_Public_License)
- [Understanding Vendor Lock-in for Databases - Aerospike](https://aerospike.com/blog/vendor-lock-in/)
