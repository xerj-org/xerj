# Resource Consumption

## Severity: HIGH | Frequency: HIGH

---

## Core Complaints

### Excessive RAM Requirements
- Production: 16-64GB RAM per node, half to JVM heap
- Minimum 3 nodes for HA = 48-192GB RAM baseline
- Platform consumes 50-70% of available RAM for heap management
- Elasticsearch baseline: ~4.5GB RAM (85x more than some alternatives)
- Fast SSDs required for acceptable performance

### CPU Overhead
- 15+ thread pools consuming CPU: GENERIC, WRITE, SEARCH, SEARCH_COORDINATION, CLUSTER_COORDINATION, MANAGEMENT, SYSTEM_WRITE, SYSTEM_CRITICAL_READ, SNAPSHOT, etc.
- Each pool has separate queue/rejection logic adding overhead
- GC cycles consume CPU proportional to heap size
- Segment merging is CPU-intensive and competes with search/index threads

### Disk Overhead
- Replicas multiply storage 2-3x
- Inverted indices + stored fields + doc values = significant overhead per document
- 12-16 files per Lucene segment = hundreds of open file descriptors per shard
- Default 1-second refresh creates new segment every second, consuming heavy I/O

---

## User Quotes

> "Elasticsearch clusters require a lot of memory, storage, and CPU, which can make it expensive at scale"
> -- G2 reviewer

> "You need a real cluster with a lot of fast IO disks"
> -- Community feedback

> "These must be state-of-the-art and offer great calculation speeds and ram"
> -- Victor Luis G., Student (Capterra)

> "If you don't size your setup correctly, it can become quite resource-intensive"
> -- Multiple reviewers

> "Resource usage can grow quickly without proper index lifecycle management"
> -- Gartner Peer Insights

---

## XERJ.ai Response
- No JVM = no heap overhead, no GC CPU cycles
- 2 thread models (Tokio async + rayon compute) vs 15+ pools
- 2 files per segment vs 12-16
- mmap segments: OS page cache manages hot data
- Domain-aware compression: 2-5x smaller on disk
- Single binary ~30MB Docker image vs ES's ~800MB
- Target: <500MB RSS for 1M indexed documents

## Sources
- G2, Gartner Peer Insights, Capterra reviews
- Datadog: 5 Performance and Scaling Problems
- Sirius Open Source: Cost of Elasticsearch
