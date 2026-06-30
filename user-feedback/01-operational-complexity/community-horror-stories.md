# Community Horror Stories: Operational Nightmares

## Source: Reddit, Hacker News, DevRant, Team Blind, Engineering Blogs

---

## Direct Quotes from Engineers

### "Pain in the ass since day one"
> "Our elasticsearch cluster has been a pain in the ass since day one. The main fix always [is] 'just double the size of the server.'"
> -- HN user "atonse" (ES costs exceeded entire pre-ES AWS bill)

### "Single most common source of infrastructure downtime"
> "In my experience, Elasticsearch is the single most common source of infrastructure downtime and service failure."
> -- HN user "_ondq"

### "Single node failure crashed the whole cluster"
> "A single node failure has lead to the whole cluster crashing down around me on more than one occasion."
> -- HN user "riceo100"

### "Steam locomotive instruction manual"
> "Most articles discussing Elastic search administration read like an instruction manual for something like steam locomotive -- describing just how to constantly shovel the coal in, relieving the steam pressure in just the right way."
> -- HN user "arwhatever"

### "You need a dedicated team"
> "You still need a dedicated team to handle this. Eventually moved to Splunk, wavefront like solutions."
> -- HN user "ram_rar"

### "Exhaust all other options first"
> "As someone who has been using Elasticsearch since 2015...I advise people against using it until they have tried and exhausted all other options. The effort alone to maintain, monitor, and scale is massive unless you have a big team."
> -- HN user "scottydelta"

### "Nearly impossible to debug"
> "Once you put it in production, you can't just run it from docker on your workstation...There are so many switches and dials to tune."
> "It's nearly impossible to debug...if you consider that 'it's slow' is what you have to debug."
> -- HN user "jniedrauer"

### "ELK is dead to me"
> "After many hours of digging through documentation, I gave up. The process was a train wreck of obscurity, complexity and heavy weight processes. ELK is dead to me."
> -- HN user "latchkey"

### "14-hour shard relocation nightmare"
A team spent 14 hours manually relocating shards, only to discover the real issue was a misconfigured disk watermark setting fixable in 2 minutes.
-- Mezmo engineering blog

### "2-week Kubernetes setup struggle"
> "Spent at least 2 weeks this year trying to get Kubernetes and logstash/elasticsearch to work together"
> Problems with golang client, X-Pack "mucking up things royally," emotional state: "defeated and annoyed"
> -- HN user "AndyNemmity"

### "Mysteriously fails 3-4 times a year"
> "It mysteriously fails 3-4 times a year...JVM issues and indexers spiral out of control using 99% CPU."
> -- HN user "scomp"

### "Falls flat on its face"
> "My biggest problem with Elasticsearch is how easy it is to get data in there and think everything is just fine...until it falls flat on its face."
> -- HN user "BoorishBears"

### "Slow and expensive way to store logs"
> "ELK is a slow and expensive way to store and retrieve logs. The reason people use it is that nothing else exists."
> -- HN user "jrockway"

### Backup API hatred
> "I hate the elasticsearch backup api. From beginning to end it's a painful experience."
> "Documentation != API != Reality."
> -- DevRant user "IntrusionCM"

### 90-hour recovery
> "A particularly nasty query was executed over and over again...so many nodes had became slow and unresponsive that another...previously unseen memory leak started to occur...the whole thing turned into a death spiral of doom...we rebuilt the whole cluster from scratch...The recovery took another 90 hours."
> -- HN user "karlney"

### FogBugz: "pants-on-head crazy"
> "FogBugz was still on twelve ElasticSearch 1.6 nodes when I left in 2018...To keep performance adequate, we scheduled cache flush operations that...we knew were pants-on-head crazy."
> -- HN user "krallja"

---

## XERJ.ai Response
Every one of these complaints traces to one or more of:
1. JVM/GC → Rust eliminates entirely
2. Cluster coordination → single-node M1 eliminates entirely
3. Configuration sprawl → <50 settings with sensible defaults
4. Shard management → no shards, automatic segment management
5. Upgrade complexity → replace binary + restart

## Sources
- Hacker News threads: #30791471, #9475620, #22396918, #32383902, #22685831, #25794987, #26316401, #33562359
- DevRant: #2802326, #2385069
- Mezmo engineering blog
- Zalando engineering blog
