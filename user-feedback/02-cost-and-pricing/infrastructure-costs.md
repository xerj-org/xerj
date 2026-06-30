# Infrastructure and Hidden Costs

## Severity: HIGH | Frequency: HIGH

---

## Core Complaints

### Raw Infrastructure Expense
- Minimal production clusters cost $300-500/month on AWS
- Large deployments run $10K-$100K+/month
- Storing 1TB on AWS costs approximately $1,500/month (EC2 + storage)
- One organization: Elasticsearch bill hit $8,000/month for search features users rarely utilized
- Mid-sized deployments: licensing + support in high five figures annually
- Larger environments: total costs often exceed $500,000/year
- Customer ingesting 20TB/day: 3-year TCO exceeds $65 million

### Storage Bloat
- A 100GB dataset can consume 300GB in Elasticsearch (JSON storage + inverted indices + replicas)
- Replicas double or triple storage requirements
- Compressed inverted indexes frequently outweigh the compressed source data
- One company needed 11 Elastic nodes with 2TB SSD each (~$60,000/year) for only 30 days of log retention

### Data Transfer Costs
- Multi-AZ deployments incur inter-zone data transfer charges
- A logging cluster ingesting 5TB/day: ~$1,500/month in pure cross-AZ traffic alone

### Memory as the Bottleneck
- Platform consumes 50-70% of available RAM for heap management
- Users often run out of heap before disk -- expensive RAM is the bottleneck, not cheap storage
- Elasticsearch requires approximately 4.5GB RAM baseline (85x more than some alternatives)
- The 64GB RAM node is the "standard unit of currency" in search pricing

### Hidden Cost Multiplier
- Hidden costs regularly exceed visible infrastructure costs by 2-3x for self-managed clusters
- Includes: training, monitoring tools, operational overhead, consulting, security hardening
- A relatively small environment can exceed $2 million over three years

### Personnel Costs
- Specialized Elasticsearch Engineer: $103,425-$155,000/year
- Fully loaded 3-person internal team: ~$600,000/year
- Finding engineers who understand Lucene internals, GC tuning, cluster management is expensive

---

## User Quotes

> "I think the pricing of Elastic Search is really, really expensive"
> -- G2 reviewer

> "License quite expensive; consultation and operation cost also high"
> -- Julie Zhong, Data Analytics, Netnordic (TrustRadius)

> "Cost of product may inviabilize its use for small applications"
> -- Luis Felipe A., Programmer (Capterra)

> "Total cost of ownership can be prohibitive for teams that simply need good search functionality"
> -- G2 reviewer

> "The cost is too high, the endpoint client is heavy, and ease of use is low"
> -- G2 reviewer

---

## XERJ.ai Response
- 80% infrastructure cost reduction target
- Rust: no JVM, ~30MB Docker image vs ES's ~800MB
- <500MB RSS for 1M docs vs ES's 2-4GB heap
- Domain-aware compression: 2-5x better than LZ4/zstd on logs
- Single binary = no monitoring cluster, no separate visualization layer
- No specialized talent required for basic operation
- 2 files per segment vs 12-16 (fewer file descriptors, less I/O)

## Sources
- Sirius Open Source: Cost of Elasticsearch
- Upsolver: Reduce Elasticsearch Costs by 90-99%
- Medium: Hidden Costs of Elasticsearch
- Medium: The Day Our Elasticsearch Bill Hit $8,000
- OpsMatters: True Cost of ELK Stack
- Grafana Loki migration story (Arquivei)
