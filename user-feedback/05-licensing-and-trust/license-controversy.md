# Licensing Controversy and Trust Erosion

## Severity: HIGH | Frequency: HIGH
A watershed event that permanently fractured the community.

---

## Timeline

### Phase 1: Apache 2.0 Era (Pre-2021)
- Elasticsearch and Kibana licensed under Apache 2.0
- Large community of contributors built code under open-source promise
- AWS launched "Amazon Elasticsearch Service" as managed offering

### Phase 2: SSPL/ELv2 Switch (January 2021)
- Elastic changed to dual SSPL + Elastic License v2
- Neither license recognized as open source by OSI
- Stated motivation: AWS offering competing service and causing "market confusion"
- SSPL prohibits cloud providers from offering ES as a service without open-sourcing entire stack

### Phase 3: AWS Fork / OpenSearch (April 2021)
- AWS forked last Apache 2.0 version (7.10.2) → OpenSearch
- OpenSearch gained 496 contributors and 100M+ downloads in first year
- Community permanently split

### Phase 4: AGPL Return (August 2024)
- Elastic added AGPLv3 as third licensing option
- Came 3.5 years after removing Apache 2.0
- Widely viewed as strategic/financial move, not genuine commitment

### Phase 5: Linux Foundation Takes OpenSearch (September 2024)
- AWS transferred OpenSearch governance to Linux Foundation
- OpenSearch permanently independent

---

## Developer Reactions

> "My trust was violated. Not lifting a finger to help them."
> -- Contributor who refuses to return

> "Cost me a bunch of time fixing and migrating code when they pulled the plug."
> -- Developer forced to migrate

> "We have zero motivation to move back to ElasticSearch"
> -- Teams that migrated to OpenSearch

> "At any time they can just change their minds again. It's pretty clear they can't be trusted."
> -- Organization on risk of future changes

> "As a contributor I feel betrayed"
> -- Community contributor, Hacker News

> "OpenSearch has become the default choice for new users. It isn't even close."
> -- Industry consultant

### On the AGPL Return
- Announcement described as "tone-deaf and disconnected" with Kendrick Lamar references and dismissal of critics as "trolls"
- Peter Zaitsev: "Can we count on Elastic to stick to open source this time?"
- Guido Iaquinti (CTO, SafetyClerk): "Trust is something that takes a long time to build but can be shattered in an instant"
- Many see it as damage control: "losing adoption," "bleeding customers to OpenSearch"

---

## Impact on Market
- Elastic stock down 26% YTD (late 2025), trading 52% below 52-week high
- Only 100 new paying customers in one quarter vs 200+ previously
- Stock plunged 16.2% in single day (November 2025)
- Increasing competition from Datadog, Splunk/Cisco, CrowdStrike, cloud-native alternatives

---

## XERJ.ai Response
- Clear, stable license from day one
- No license rug-pull risk
- Trust is a competitive advantage when ES has squandered it
- PROD.brief.md question: "OSS play, Hyperscalers play, on-premise play, cloud-native play?" -- license choice is a strategic decision

## Sources
- Socket.dev: Developers Burned by License Change Aren't Going Back
- IT Pro: Elastic Returns to Open Source But Can It Regain Trust
- InfoQ: Elastic Open Source Again
- Hacker News discussions
- Motley Fool, IndexBox, SiliconAngle: stock/revenue analysis
