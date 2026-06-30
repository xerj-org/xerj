# HN Discussions: Search Database Clustering, Durability & Scaling

Collected from Hacker News threads 2019–2026. Each row is a discrete user comment or attributed quote.

Sources fetched:
- https://news.ycombinator.com/item?id=41173288 — Full text search over Postgres: Elasticsearch vs. alternatives (Aug 2024)
- https://news.ycombinator.com/item?id=41985176 — Vector databases are the wrong abstraction (Oct 2024)
- https://news.ycombinator.com/item?id=46579954 — Elasticsearch was never a database (Jan 2026)
- https://news.ycombinator.com/item?id=42566192 — Databases in 2024: A Year in Review (Dec 2024)
- https://news.ycombinator.com/item?id=46496103 — Databases in 2025: A Year in Review (Jan 2026)
- https://news.ycombinator.com/item?id=44954123 — What place do vector-native databases have in 2025? (Aug 2025)
- https://news.ycombinator.com/item?id=39119198 — Are we at peak vector database? (Jan 2024)
- https://news.ycombinator.com/item?id=22396918 — Guide to running Elasticsearch in production (Feb 2020)
- https://news.ycombinator.com/item?id=38902042 — Show HN: Quickwit – OSS Alternative to Elasticsearch (Jan 2024)
- https://news.ycombinator.com/item?id=44658978 — Manticore Search: Fast, efficient, drop-in replacement for Elasticsearch (Jun 2025)
- https://news.ycombinator.com/item?id=33562359 — We upgraded an old, 3PB large, Elasticsearch cluster without downtime (Nov 2022)
- https://news.ycombinator.com/item?id=36223387 — Open source Elasticsearch alternative in Rust (Jun 2023)
- https://news.ycombinator.com/item?id=37540421 — Ask HN: Are there any unsolved problems with vector databases (Sept 2023)
- https://news.ycombinator.com/item?id=41394797 — Elasticsearch is open source, again (Aug 2024)
- https://news.ycombinator.com/item?id=38449827 — S3 Express Is All You Need (Nov 2023)
- https://news.ycombinator.com/item?id=47499356 — From zero to a RAG system: successes and failures (Mar 2026)
- https://news.ycombinator.com/item?id=46616529 — Ask HN: How are you doing RAG locally? (Feb 2026)
- https://news.ycombinator.com/item?id=44194468 — I made a search engine worse than Elasticsearch (Jun 2025)
- https://news.ycombinator.com/item?id=36945082 — Pure vector databases are a dead end (Jul 2023)
- https://news.ycombinator.com/item?id=32938304 — ZincSearch – lightweight alternative to Elasticsearch (Sept 2022)
- https://news.ycombinator.com/item?id=13493219 — The probability of data loss in large clusters (Jan 2017)
- https://news.ycombinator.com/item?id=30791471 — Elasticsearch is not a pain in the ass to scale (Mar 2022)
- https://news.ycombinator.com/item?id=28103389 — The Elasticsearch Saga Continues (Aug 2021)
- https://news.ycombinator.com/item?id=39101682 — Qdrant raised $28M Series A (Jan 2024)
- https://news.ycombinator.com/item?id=35311047 — Weaviate or Qdrant? (Mar 2023)
- https://news.ycombinator.com/item?id=21227479 — AWS Elasticsearch: a fundamentally-flawed offering (Oct 2019)
- https://news.ycombinator.com/item?id=38611942 — Qdrant winning horse? (Dec 2023)
- https://news.ycombinator.com/item?id=9488115 — Ask HN: Using AWS S3 as a database (May 2015)
- https://news.ycombinator.com/item?id=35550567 — Do you need a vector database? (Apr 2023)

---

| # | Quote | HN User | Thread | Date |
|---|-------|---------|--------|------|
| 1 | "ES is just unreliable. Can be running smoothly for a year and boom it falls over and you're left scratching your head." | wordofx | Full text search over Postgres: ES vs. alternatives | Aug 2024 |
| 2 | "ES (and Solr too) are pathological with respect to garbage collection...that means you have caches that last just long enough to get evicted into the old generation, only to become useless shortly thereafter." | fizx | Full text search over Postgres: ES vs. alternatives | Aug 2024 |
| 3 | "It got a lot better in the ~7 series IIRC when they added checksums to the on-disk files." | fizx | Full text search over Postgres: ES vs. alternatives | Aug 2024 |
| 4 | "Elasticsearch falling over is what happens when you don't do [proper cluster sizing]. It scales fine until it doesn't and then you hit a brick wall." | jillesvangurp | Full text search over Postgres: ES vs. alternatives | Aug 2024 |
| 5 | "PostgreSQL FTS cannot do BM25 scoring because it doesn't maintain statistics for word frequencies across the entire corpus." | simonw | Full text search over Postgres: ES vs. alternatives | Aug 2024 |
| 6 | "For internet user facing full-text search I would always prefer to use a separate tool and not a SQL database." | samsk | Full text search over Postgres: ES vs. alternatives | Aug 2024 |
| 7 | "Most companies have some need for [both semantic and keyword search] because they're building a search on 'their' data/business." | jillesvangurp | Full text search over Postgres: ES vs. alternatives | Aug 2024 |
| 8 | "User 1 will bias the results of user 2. Perhaps enough for user 2 to discover what words are in user 1 corpus." | sroussey | Full text search over Postgres: ES vs. alternatives | Aug 2024 |
| 9 | "HNSW indices using pgvector consume a _ton_ of resources - even a small index of tens of millions of embeddings may be hundreds of gigabytes on disk." | whakim | Vector databases are the wrong abstraction | Oct 2024 |
| 10 | "The real challenge is embedding model migration: You can't really solve that by simply streamline the vectorization...You need the non-fancy migration process: create a new collection, batch generate new vectors with the new model." | codingjaguar | Vector databases are the wrong abstraction | Oct 2024 |
| 11 | "What if the API calls to open AI fail or get rate limited. How is that surfaced." | hhdhdbdb | Vector databases are the wrong abstraction | Oct 2024 |
| 12 | "Having the DB handle fetching data from external sources feels like the wrong abstraction to me." | petesergeant | Vector databases are the wrong abstraction | Oct 2024 |
| 13 | "We managed 200M long and short form embeddings...I think in 0 cases would we go back and use vector dbs." | ramoz | Vector databases are the wrong abstraction | Oct 2024 |
| 14 | "Search engines are designed to build an index which augments a data store...Anyone recommending using a search engine as primary data store is taking on risk of data loss." | rectang | Elasticsearch was never a database | Jan 2026 |
| 15 | "Any sufficiently successful data store eventually sprouts...inconsistency-ridden, slow implementation of half of a relational database." | roywiggins | Elasticsearch was never a database | Jan 2026 |
| 16 | "I don't have sympathy for anyone involved other than unsuspecting clients. These people knew or more or less knew and chose to ignore." | PedroBatista | Elasticsearch was never a database | Jan 2026 |
| 17 | "High load, high Ram usage: db goes down. No transaction: if second insert fails you need to delete first insert by hand. You need to find a way to insert in bulk." | kubi07 | Elasticsearch was never a database | Jan 2026 |
| 18 | "We use ES extensively as database...You have to get used to eventual consistency...should switch to SQL as soon the product is successful." | thisisananth | Elasticsearch was never a database | Jan 2026 |
| 19 | "The json key value store is fully consistent...refresh is needed for search api. In some cases it does make sense to treat it as database." | ananthakumaran | Elasticsearch was never a database | Jan 2026 |
| 20 | "I would never bet my savings on ES being stable...I recommend orgs use some other system for retaining logs." | unethical_ban | Elasticsearch was never a database | Jan 2026 |
| 21 | "Accenture managed to build a data platform...with Elasticsearch as primary database. I raised concerns early...decided to not make my work rely on their work." | speedgoose | Elasticsearch was never a database | Jan 2026 |
| 22 | "It has an index? It has data that can be queried with indexes? it is a database. Let's not turn the word database into a buzzword." | throw_m239339 | Elasticsearch was never a database | Jan 2026 |
| 23 | "It is called ElasticSEARCH, not Elasticdatabase." | this_user | Elasticsearch was never a database | Jan 2026 |
| 24 | "vector databases like Milvus got lots of new features to support RAG, Agent development, features like BM25, hybrid search etc." | bzGoRust | Databases in 2024: A Year in Review | Dec 2024 |
| 25 | "we had to restrict ours to views only because it kept trying to run updates. still breaks sometimes when it hallucinates column names but at least it can't do anything destructive." | dmarwicke | Databases in 2025: A Year in Review | Jan 2026 |
| 26 | "Chroma supports multiple search methods - including vector, full-text, and regex search." | philip1209 | What place do vector-native databases have in 2025? | Aug 2025 |
| 27 | "The core issue with scaling vector database indexes is that they don't handle WHERE...with vector databases - as the index size grows, not only does it get slower - it...The solution is to have many small indexes, which Chroma calls 'Collections'." | philip1209 | What place do vector-native databases have in 2025? | Aug 2025 |
| 28 | "Chroma is built on object storage - vectors are stored on AWS S3." | philip1209 | What place do vector-native databases have in 2025? | Aug 2025 |
| 29 | "Writes become slow due to consistency, disk becomes a majority vector indexes, and...The solution for Chroma Cloud is a distributed system - which allows strong consistency..." | philip1209 | What place do vector-native databases have in 2025? | Aug 2025 |
| 30 | "if you want or need to optimize for speed, cost, scalability or accuracy...dedicated solutions have more advanced search features enable more accurate results...search indexing is resource intensive and can contend for resources with postgres/redis." | jeffchuber | What place do vector-native databases have in 2025? | Aug 2025 |
| 31 | "IMO we are well past peak cosine-similarity-search as a service. Most people I talk to in the space don't bother using specialized vector DBs for that." | reissbaker | Are we at peak vector database? | Jan 2024 |
| 32 | "You know it will be 'peak vector database' when you'll see blog posts on migrating your relational data to a vector database (with a follow-up 2 years later about moving back to PostgreSQL due to the shitshow that ensued)." | macspoofing | Are we at peak vector database? | Jan 2024 |
| 33 | "we went with pgvector integrated into our existing PostgreSQL database. Operationally simple, performs perfectly adequately." | thraxil | Are we at peak vector database? | Jan 2024 |
| 34 | "For embedding generation versus lookup, the game changed when we switched from word2vec to LLMs. Embedding costs dwarf search costs by ~1 billion times." | gdiamos | Are we at peak vector database? | Jan 2024 |
| 35 | "Vector DBs have advantages once you're dealing with millions of vectors...stick with PgVector until you're dealing with non trivial scale." | serjester | Are we at peak vector database? | Jan 2024 |
| 36 | "Opted for storing vectors to disk in files for later retrieval and doing everything in memory rather than paying 4-5 figure bills from vector DB providers." | infecto | Are we at peak vector database? | Jan 2024 |
| 37 | "Most enterprise/b2b chat/copilot apps have relatively small amounts of data whose embeddings can fit in RAM...vector DBs are much more niche than an RDBMS." | seattleeng | Are we at peak vector database? | Jan 2024 |
| 38 | "Traditional DBs already kinda support vector DBs via pg_vector extensions and such. Lantern offers an extension for postgres that is open source and is better for vector DB use cases." | yolovoe | Are we at peak vector database? | Jan 2024 |
| 39 | "Most clusters that lose data do so because of GC pauses cause nodes to drop out of the cluster." | DmitryOlshansky | Guide to running Elasticsearch in production | Feb 2020 |
| 40 | "ES is not a database - there are notable edge cases where it drops data." | labawi | Guide to running Elasticsearch in production | Feb 2020 |
| 41 | "140 shards per node is on the low side, one can easily scale to 500+ for small shards." | DmitryOlshansky | Guide to running Elasticsearch in production | Feb 2020 |
| 42 | "ES performance starts to degrade once you get past 40 or so nodes, with GC pause problems becoming critical as cluster size increases." | hilbertseries | Guide to running Elasticsearch in production | Feb 2020 |
| 43 | "Not a single line about GC tuning...default CMS to be quite horrible even in recommended ~31g sizes." | DmitryOlshansky | Guide to running Elasticsearch in production | Feb 2020 |
| 44 | "300MB of RAM per node...to tail some files and parse JSON with ELK stacks requiring 5 nodes with 64G RAM each while holding only weeks of data." | jrockway | Guide to running Elasticsearch in production | Feb 2020 |
| 45 | "Better strategy is to store logs in flat files with several replicas...you can afford to index other things, like x-request-id and maybe a trigram index of messages." | jrockway | Guide to running Elasticsearch in production | Feb 2020 |
| 46 | "We have invested tremendously in data replication and cluster coordination subsystems since the 2015 Jepsen analysis, closing previously identified divergence and document loss problems." | Jason Tedor (Elastic) | Guide to running Elasticsearch in production | Feb 2020 |
| 47 | "almost anything is more storage-efficient than Elasticsearch, FTS is so expensive." | dikei | Quickwit – OSS Alternative to Elasticsearch | Jan 2024 |
| 48 | "Quickwit stores indexes on S3 while competitors use EBS, explaining superior cost metrics." | fulmicoton | Quickwit – OSS Alternative to Elasticsearch | Jan 2024 |
| 49 | "One company migrating from Elasticsearch achieved 5x compute cost reduction and 2x storage savings while extending retention from 3 to 30 days." | (reported by fulmicoton) | Quickwit – OSS Alternative to Elasticsearch | Jan 2024 |
| 50 | "Exactly-once semantics thanks to native Kafka support, addressing a significant operational pain point for log ingestion reliability." | (announcement) | Quickwit – OSS Alternative to Elasticsearch | Jan 2024 |
| 51 | "Quickwit lacks dedicated metrics storage and doesn't support document updates/deletions efficiently—focusing instead on immutable, log-optimized architecture." | francoismassot | Quickwit – OSS Alternative to Elasticsearch | Jan 2024 |
| 52 | "One should not use 'drop-in' when they have their own query language and seemingly input shape for the /search endpoint." | mdaniel | Manticore Search drop-in replacement debate | Jun 2025 |
| 53 | "Autosharding, authentication, dynamic mapping are missing but in progress." | snikolaev (Manticore maintainer) | Manticore Search drop-in replacement debate | Jun 2025 |
| 54 | "easy to setup, lean on resources and quite fast with minimal friction compared to Elasticsearch." | cess11 | Manticore Search drop-in replacement debate | Jun 2025 |
| 55 | "rock solid for us for the past 16 years...serving searches across nearly 300M short documents." | pQd | Manticore Search drop-in replacement debate | Jun 2025 |
| 56 | "modern multithreading architecture with efficient query parallelization, real-time indexing, minimal RAM, and avoids garbage collection." | snikolaev | Manticore Search drop-in replacement debate | Jun 2025 |
| 57 | "Wonder if there are specific benchmarks...which measure performance and if they compared tail latencies as opposed to averages." | another_twist | Manticore Search drop-in replacement debate | Jun 2025 |
| 58 | "Relying on Elasticsearch mega-clusters...is akin to running an ultra-marathon with really sharp scissors." | metadat | We upgraded a 3PB Elasticsearch cluster without downtime | Nov 2022 |
| 59 | "A nasty query was executed over and over...nodes became unresponsive triggering a memory leak...a death spiral. After 48 hours of failed recovery attempts, we rebuilt the whole cluster from scratch requiring 90 hours of restoration from S3 snapshots." | karlney | We upgraded a 3PB Elasticsearch cluster without downtime | Nov 2022 |
| 60 | "~300 nodes strikes a good balance...after that the impact of loosing just one node would be too big." | karlney | We upgraded a 3PB Elasticsearch cluster without downtime | Nov 2022 |
| 61 | "The update from 5.x to 6.x gave us some headaches due to the removal of mapping types." | nullify88 | We upgraded a 3PB Elasticsearch cluster without downtime | Nov 2022 |
| 62 | "Ancient ES 1.6 infrastructure with a custom plugin and problematic cache flush operations in production." | krallja | We upgraded a 3PB Elasticsearch cluster without downtime | Nov 2022 |
| 63 | "We used terraform to build the 7.5.2 cluster with about 28 nodes. The strategy involved snapshots, dual-writing to both old and new systems via Redis tracking, and gradual customer account migration for validation." | taf2 | We upgraded a 3PB Elasticsearch cluster without downtime | Nov 2022 |
| 64 | "10x easier, 140x lower storage cost, high performance, petabyte scale - Elasticsearch/Splunk/Datadog alternative." | prabhatsharma | Open source ES alternative in Rust (OpenObserve) | Jun 2023 |
| 65 | "Setting up observability often involved setting up 4 different tools (grafana for dashboarding, elasticsearch/loki/etc for logs, jaeger for tracing, thanos, cortex etc for metrics)." | prabhatsharma | Open source ES alternative in Rust (OpenObserve) | Jun 2023 |
| 66 | "Indexes for vector databases in high dimensions are nowhere near as effective as the 2-d indexes used in GIS or the 1-d B-tree indexes that are commonly used in databases." | PaulHoule | Ask HN: Unsolved problems with vector databases | Sept 2023 |
| 67 | "You have tradeoffs between faster algorithms that miss some results and slower algorithms that are more correct." | PaulHoule | Ask HN: Unsolved problems with vector databases | Sept 2023 |
| 68 | "Despite working on similarity search since 2005, the technical landscape hasn't substantially advanced, suggesting fundamental indexing problems remain unresolved." | PaulHoule | Ask HN: Unsolved problems with vector databases | Sept 2023 |
| 69 | "Elastic made it very difficult to try it out at scale and only wanted to talk to the CTO instead of the persons in charge of the PoCs. Eventually migrated to Loki after OpenSearch proved problematic." | OldOneEye | Elasticsearch is open source, again | Aug 2024 |
| 70 | "Elastic is a pretty arduous enterprise sales process which turned a lot of small/mid customers away." | nijave | Elasticsearch is open source, again | Aug 2024 |
| 71 | "As a contributor I feel betrayed by the license change." | crewdragon | Elasticsearch is open source, again | Aug 2024 |
| 72 | "Migrated from AWS managed ES to Elastic.co, noting dramatic improvements in security, interface, and storage configuration quality." | simlevesque | Elasticsearch is open source, again | Aug 2024 |
| 73 | "S3 Express approaches HDD random read speeds (single-digit ms), so we can build production systems that don't need an SSD cache." | Sirupsen | S3 Express Is All You Need | Nov 2023 |
| 74 | "Quickwit offers 6.4x higher storage costs against 2x cheaper GET requests, with single-region replication. This creates narrow viable use cases that competes more with EBS than S3." | fulmicoton | S3 Express Is All You Need | Nov 2023 |
| 75 | "Simply using docling and transforming PDFs to markdown and have a vector database doing the rest is ridiculous." | _the_inflator | From zero to a RAG system: successes and failures | Mar 2026 |
| 76 | "Teams extracted database information into vector stores, only to find the LLM confused multiple rows within single chunks—retrieving full chunks rather than targeted data." | RansomStark | From zero to a RAG system: successes and failures | Mar 2026 |
| 77 | "The retrieval part is so critical...how do you deal with time series data?" | leflob | From zero to a RAG system: successes and failures | Mar 2026 |
| 78 | "We did it in an engineering setting and had very mixed results. Big 800 page machine manuals are hard to contextualise." | physicsguy | From zero to a RAG system: successes and failures | Mar 2026 |
| 79 | "10-84% of symbol references in AI configuration files become stale within weeks—confident but incorrect information actively misleads models rather than improving retrieval." | ravikirany22 | From zero to a RAG system: successes and failures | Mar 2026 |
| 80 | "Unstructured data dumps into vector databases without preprocessing, metadata, or labeling produce unreliable systems requiring substantial schema design overhead." | brianykim | From zero to a RAG system: successes and failures | Mar 2026 |
| 81 | "85% of the time we don't need the vectordb—semi-structured metadata matching proved sufficient." | (anonymous commenter) | Ask HN: How are you doing RAG locally? | Feb 2026 |
| 82 | "Code likes bm25+trigram, that gets better results while keeping search responses snappy." | (anonymous commenter) | Ask HN: How are you doing RAG locally? | Feb 2026 |
| 83 | "Most vectordb is a hammer looking for a nail." | (anonymous commenter) | Ask HN: How are you doing RAG locally? | Feb 2026 |
| 84 | "FAISS runs in RAM. If your dataset can't fit into ram, FAISS is not the right tool." | (anonymous commenter) | Ask HN: How are you doing RAG locally? | Feb 2026 |
| 85 | "If you make your own search engine, it's almost guaranteed to be worse than ElasticSearch." | sh34r | I made a search engine worse than Elasticsearch (2024) | Jun 2025 |
| 86 | "Having elasticsearch, as this resource hungry slow to update JVM based thing always seems so horrible in Django based projects." | stuaxo | I made a search engine worse than Elasticsearch (2024) | Jun 2025 |
| 87 | "Running search in a different process or container means I lose the advantages of tight integration. Keeping indexes on the local disk (just like SQLite) is a really simple deployment model." | bob1029 | I made a search engine worse than Elasticsearch (2024) | Jun 2025 |
| 88 | "There are posts here every few months/weeks of someone boasting that they are running circles around Lucene. Usually, if you go look at such implementations, you'll find they implemented 1% of the features." | jillesvangurp | I made a search engine worse than Elasticsearch (2024) | Jun 2025 |
| 89 | "Pure vector databases are a dead end. Almost every search engine (Vespa, Elastic, etc) and every database (Postgres, SQLite, Redis, etc) already has a solution for searching vectors in addition to everything else you need to query or search." | spullara | Pure vector databases are a dead end | Jul 2023 |
| 90 | "Maintaining and keeping a second system in sync to do vector search is painful. I've never been more jealous of people using Postgres." | sv123 | Pure vector databases are a dead end | Jul 2023 |
| 91 | "You can do it in any database as long as the corpus is small. But, yes for large numbers of vectors doing a brute force search doesn't scale well." | spullara | Pure vector databases are a dead end | Jul 2023 |
| 92 | "ZincSearch currently does not provide an Elasticsearch compatible query API...Can ZincSearch be deployed in HA mode? Currently, No." | darkwater | ZincSearch – lightweight alternative to Elasticsearch | Sept 2022 |
| 93 | "ES is a real memory hog. You either need to spin up some real beefy servers or it will regularly crash." | cardanome | ZincSearch – lightweight alternative to Elasticsearch | Sept 2022 |
| 94 | "ES would eat tons of memory and quite often crash due to OOM." | tomohawk | ZincSearch – lightweight alternative to Elasticsearch | Sept 2022 |
| 95 | "The distributed system will start re-replicating each partition after node failure. With more, smaller partitions, you can re-replicate quicker." | colin_mccabe | The probability of data loss in large clusters | Jan 2017 |
| 96 | "You don't need 3 complete failures to lose data: perhaps 1 complete failure plus 2 partial HDD failures." | woliveirajr | The probability of data loss in large clusters | Jan 2017 |
| 97 | "Rebuilding a RAID5 with 4 TB drives has a significant chance of failure." | dom0 | The probability of data loss in large clusters | Jan 2017 |
| 98 | "More partitions per node mean faster recovery but increase overall failure scenarios across the cluster." | Retric | The probability of data loss in large clusters | Jan 2017 |
| 99 | "Elasticsearch is not a pain in the ass to scale, it is one of the easiest databases to scale. It has built-in horizontal scaling without user intervention, using peer discovery and automatic shard distribution." | jturpin | Elasticsearch is not a pain in the ass to scale | Mar 2022 |
| 100 | "Their Elasticsearch cluster required constant scaling (just double the size of the server), costing more than their entire AWS bill. PostgreSQL required minimal maintenance beyond occasional index tuning." | atonse | Elasticsearch is not a pain in the ass to scale | Mar 2022 |
| 101 | "While Elasticsearch offers easier horizontal scaling than SQL databases, it's more expensive operationally due to high memory and compute demands. At scale, involved a lot of operational support." | thinkharderdev | Elasticsearch is not a pain in the ass to scale | Mar 2022 |
| 102 | "Replacing Elasticsearch with modern alternatives like Loki for log management. Elasticsearch is one order of magnitude worse to scale and operate than misused NoSQL databases." | fishpen0 | Elasticsearch is not a pain in the ass to scale | Mar 2022 |
| 103 | "Elastic is 'shooting themselves in the foot' by restricting clients to only their instances, giving AWS OpenSearch an opportunity to enhance SDKs supporting both platforms." | ram_rar | The Elasticsearch Saga Continues | Aug 2021 |
| 104 | "AWS-hosted Elasticsearch is subpar and signals AWS doesn't genuinely care about search capabilities, suggesting OpenSearch will drift further from ES quality over time." | kureikain | The Elasticsearch Saga Continues | Aug 2021 |
| 105 | "AWS faces major issues with execution due to internal culture problems, with the Elasticsearch team swamped managing operational pressure rather than development." | reducesuffering | The Elasticsearch Saga Continues | Aug 2021 |
| 106 | "Frustrated with Elastic's all-or-nothing license model requiring expensive premium tiers for basic needs like access control and SSO, considering OpenSearch despite fewer features." | vladvasiliu | The Elasticsearch Saga Continues | Aug 2021 |
| 107 | "We are for a few projects. We've been using them for over a year and have been impressed. We have 10s millions of items in there with lots of daily inserts/deletions etc." | crucio | Qdrant raised $28M Series A | Jan 2024 |
| 108 | "I do, and it's very very rough around the edges to be honest. Lots of things broken, things are even breaking between releases suddenly in unexpected places." | inertiatic | Qdrant raised $28M Series A | Jan 2024 |
| 109 | "Chroma has a big following by virtue of being plugged into the AI ecosystem in SF. Qdrant seems to be doing great work but their location in Europe is probably not helping." | gk1 | Qdrant winning horse? | Dec 2023 |
| 110 | "We've been using qdrant in production for over a year. It's excellent and the team are very responsive to the few issues we've had." | crucio | Qdrant winning horse? | Dec 2023 |
| 111 | "What matters is that Qdrant is the most performant, and it's an open-source vectordb, not a closed-source vectordb like Pinecone." | smurda | Qdrant winning horse? | Dec 2023 |
| 112 | "I've been using pgvector, it has worked as expected...it will still probably exist in its current form after the dust settles." | clwg | Qdrant winning horse? | Dec 2023 |
| 113 | "Weaviate and Qdrant have similar offerings in terms of features, open-sourceness, flexible deployment, and integrations." | victorialslocum (Weaviate) | Qdrant winning horse? | Dec 2023 |
| 114 | "Heaven forbid you make a configuration change that triggers a blue-green deployment and during the deploy one of the AZs runs out of that instance SKU. This required AWS support intervention lasting days." | lflux | AWS Elasticsearch: a fundamentally-flawed offering | Oct 2019 |
| 115 | "The main usability problem is that they don't tell you when that will trigger a blue-green deployment." | outworlder | AWS Elasticsearch: a fundamentally-flawed offering | Oct 2019 |
| 116 | "Shard rebalancing appears forcibly disabled on AWS clusters, resulting in imbalanced node usage—one full node while others have 300GB+ free." | DominoTree | AWS Elasticsearch: a fundamentally-flawed offering | Oct 2019 |
| 117 | "We ran a minor upgrade to our cluster earlier this week and it knocked out the entire cluster for over two hours." | tedivm | AWS Elasticsearch: a fundamentally-flawed offering | Oct 2019 |
| 118 | "AWS managed services generally cost significantly more than EC2 alternatives. Self-management could have halved infrastructure costs." | bifrost | AWS Elasticsearch: a fundamentally-flawed offering | Oct 2019 |
| 119 | "Migrating to a different hosting platform than AWS for ES also mitigates this problem too, which is more likely in our case." | lflux | AWS Elasticsearch: a fundamentally-flawed offering | Oct 2019 |
| 120 | "S3 performance fluctuates significantly—what benchmarks at 400ms could spike to 4000ms, potentially exhausting application resources and frustrating users." | spotman | Ask HN: Using AWS S3 as a database | May 2015 |
| 121 | "S3 functions as merely a key-value store. Complex queries require full bucket traversal. S3 doesn't have a good way to even estimate the amount of data in it." | spotman | Ask HN: Using AWS S3 as a database | May 2015 |
| 122 | "Retrieving multiple objects requires individual requests—fetching 30 items means 30 separate calls. Querying MySQL from Node has got to be easier than building your own database on top of S3." | pjungwir | Ask HN: Using AWS S3 as a database | May 2015 |
| 123 | "Re-indexing 10M embeddings took ~20-30 minutes. Changing embedding models requires full re-indexing, creating operational friction." | (article author cited) | Do you need a vector database? | Apr 2023 |
| 124 | "Using pgvector with default IVF settings yields only ~50% recall. Improving this increases latency significantly—the classic speed-versus-accuracy compromise." | (commenter) | Do you need a vector database? | Apr 2023 |
| 125 | "For typical enterprise chatbot projects (100-500 documents), brute-force similarity search works fine. The overhead of specialized infrastructure becomes counterproductive." | (commenter) | Do you need a vector database? | Apr 2023 |
| 126 | "I'm using sqlite-vec along with FTS5 in SQLite and it's pretty cool." | markusw | Vector databases are the wrong abstraction | Oct 2024 |
| 127 | "A/B testing across multiple embedding models without affecting the source—advocate for separated databases for plaintext and vectors." | _bramses | Vector databases are the wrong abstraction | Oct 2024 |
| 128 | "Scales fine if you know what you are doing." | bdangubic | Databases in 2024: A Year in Review | Jan 2025 |
| 129 | "AOF with fsync configured correctly provides some of the transactional guarantees regarding atomicity and durability." | antirez | Databases in 2025: A Year in Review | Jan 2026 |

---

*Collected: April 2026. 129 data points from 29 HN threads spanning 2015–2026.*
