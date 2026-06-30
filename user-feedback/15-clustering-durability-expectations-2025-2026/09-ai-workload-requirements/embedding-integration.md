# Embedding Integration & AI/ML Model Integration with Databases — User Expectations 2025-2026

Research covering user expectations, analyst findings, vendor announcements, and developer feedback on inline embedding generation, built-in ML inference, and AI co-processing within database and search systems.

| # | Quote/Summary | Source | Date | Category |
|---|---|---|---|---|
| 1 | "Automated Embedding in MongoDB Vector Search — a groundbreaking feature designed to make building sophisticated, AI-powered applications easier than ever. Solves a major point of friction: one-click automatic embedding directly inside MongoDB, which eliminates the need to sync data and manage external models." | MongoDB Blog | 2025 | Auto-Embedding at Ingest |
| 2 | "Before this release, developers faced: manual embedding generation required choosing an embedding model from over 100 available options and figuring out how to integrate external API services." | MongoDB Blog | 2025 | Auto-Embedding at Ingest |
| 3 | "Synchronization overhead meant every insert, update, or delete required keeping source data and vector embeddings in sync, and developers had to set up custom pipelines, monitor change streams, and write logic to trigger re-embedding." | MongoDB Blog | 2025 | Auto-Embedding at Ingest |
| 4 | "According to a 2025 IDC survey, more than 74% of organizations plan to use integrated vector databases to store and query vector embeddings within their agentic AI workflows." | MongoDB / IDC Survey | 2025 | Market Expectation |
| 5 | "Embedding search and vector search directly into the database gives developers one less complexity to manage, and allows them to stay focused on building intelligent applications." | MongoDB Blog | 2025 | Auto-Embedding at Ingest |
| 6 | "Automated text embedding allows MongoDB Community users to create vector search indexes that automatically generate, store, and query text embeddings using Voyage AI models, eliminating the need for manual embedding pipelines." | MongoDB Press Release | 2025 | Auto-Embedding at Ingest |
| 7 | "BigQuery manages embedding generation on behalf of the user and keeps generated embeddings in sync with the source data." | Google Cloud Blog | 2025 | Auto-Embedding at Ingest |
| 8 | "You can now define an embedding column in your DDL using GENERATED ALWAYS AS AI.EMBED, which automatically handles vector generation in the background whenever you insert data." | Google Cloud BigQuery Docs | 2025 | Inline Embedding |
| 9 | "AI.SEARCH automatically uses the embedding model associated with the generated embedding column from the base table, so you don't need to interact with the embedding configuration when using AI.SEARCH or VECTOR_SEARCH." | Google Cloud BigQuery Docs | 2025 | Inline Embedding |
| 10 | "No more manual pipelines, no more synchronization issues, just easy, AI-ready data." | Google Cloud Blog — BigQuery | 2025 | Auto-Embedding at Ingest |
| 11 | "Goodbye, Pipelines: Building AI Search in Seconds with BigQuery Autonomous Embeddings." | Medium / Google Cloud Community | 2025 | Auto-Embedding at Ingest |
| 12 | "By treating embeddings as just another generated column, similar to a calculated field, developers who know SQL can build sophisticated semantic search engines." | Google Cloud Blog | 2025 | Inline Embedding |
| 13 | "SQL Server 2025 allows AI workflows to run directly in the database, without needing an external app to orchestrate calls." | TrustedTech / Microsoft | 2025 | Model Serving Inside DB |
| 14 | "Once registered, these models become first-class objects in the database, and you can invoke them via T-SQL functions to perform tasks like generating text, analyzing sentiment, or calculating an embedding vector for a piece of data." | TrustedTech — SQL Server 2025 | 2025 | Model Serving Inside DB |
| 15 | "SQL Server 2025 introduces a new VECTOR data type to store high-dimensional embeddings (for example, a 768-dimension vector from a language model) efficiently." | Microsoft SQL Server Blog | 2025 | Model Serving Inside DB |
| 16 | "SQL Server 2025 provides built-in, extensible AI capabilities and seamless integration using familiar T-SQL language." | Red Gate / Simple Talk | 2025 | Model Serving Inside DB |
| 17 | "The database can take a piece of text from a table, send it to an external NLP model for analysis, get the result, and use it in a query, all within a single T-SQL script." | TrustedTech | 2025 | Model Serving Inside DB |
| 18 | "CREATE EXTERNAL MODEL and AI_GENERATE_EMBEDDINGS commands are available in SQL Server 2025. It enables integration with Azure AI Foundry, Azure OpenAI Service, Ollama, and OpenAI." | Red Gate | 2025 | Embedding API Integration |
| 19 | "This reduces latency and keeps sensitive data secure on the database server while still leveraging AI insights." | Pure Storage — SQL Server 2025 | 2025 | Model Serving Inside DB |
| 20 | "SQL Server 2025 eliminates the need for separate search, analytics, and AI pipelines, simplifying architecture while enabling more responsive, insight-driven decision-making." | TrustedTech | 2025 | Semantic Search Without External Pipeline |
| 21 | "Oracle AI Database implements an ONNX runtime directly within the database. This allows you to generate vector embeddings directly within the Oracle AI Database using SQL." | Oracle Database Docs | 2025 | Model Serving Inside DB |
| 22 | "Document load, transformation, chunking, embedding, similarity search, and RAG with LLMs is available natively or through APIs within the database." | Oracle AI Vector Search | 2025 | Inline Embedding |
| 23 | "Oracle is aligning with this trend as one of the first cloud database providers to bring AI to data, offering built-in vector embedding and search across LLMs and internal data." | Oracle Analyst Blog | 2025 | Model Serving Inside DB |
| 24 | "Oracle AI Database 26ai incorporates AI into the core of data management, unifying search across traditional internal data, real-time operational data, graph, JSON, and AI vectors." | Oracle AI World 2025 | 2025 | AI Co-Processor Expectations |
| 25 | "With HeatWave AutoML including everything users need to build, train, and explain ML models within HeatWave." | Oracle HeatWave | 2025 | Database Built-in ML Inference |
| 26 | "Oracle AI Database supports importing text transformer, classification, regression, and clustering models in ONNX format to use from SQL, and can deploy ONNX format models to Oracle Machine Learning Services for real-time inferencing." | Oracle Machine Learning Docs | 2025 | Database Built-in ML Inference |
| 27 | "BigQuery ML has built-in support for batch prediction, without the need to use Vertex AI. You can run ML inferences across imported custom models in ONNX, XGBoost and TensorFlow, all available right within BigQuery where your data resides." | Google Cloud BigQuery Docs | 2025 | Database Built-in ML Inference |
| 28 | "After AutoML training, Redshift ML compiles the best model and registers it as a prediction SQL function in your Redshift cluster, which you can then invoke by calling the prediction function inside a SELECT statement." | AWS Redshift ML Docs | 2025 | Database Built-in ML Inference |
| 29 | "Snowflake announced general availability of Cortex AI Functions in November 2025, delivering production-ready AI within the Snowflake SQL engine." | Snowflake Release Notes | Nov 2025 | Model Serving Inside DB |
| 30 | "AI_EMBED generates an embedding vector for a text or image input, which can be used for similarity search, clustering, and classification tasks." | Snowflake Documentation | 2025 | Inline Embedding |
| 31 | "AI_SIMILARITY calculates the embedding similarity between two inputs without needing to explicitly create the embedding vectors." | Snowflake Documentation | 2025 | Inline Embedding |
| 32 | "With AI_EMBED, you can build comprehensive multimodal search infrastructure using just SQL, making RAG applications and similarity search systems incredibly powerful and simplifying the entire development process." | DEV Community | 2025 | Inline Embedding |
| 33 | "Cortex Search services now support both multi-indexing and customized vector embeddings in preview, allowing for more refined results by enabling searches over multiple columns of data." | Snowflake Release Notes | Dec 2025 | Embedding API Integration |
| 34 | "Elastic announced the Elastic Inference Service (EIS), a GPU-accelerated inference-as-a-service for Elasticsearch semantic search, vector search, and generative AI workflows." | Elastic Press Release | 2025 | AI Co-Processor Expectations |
| 35 | "Elastic acquired Jina AI in October 2025, and the practical effect is that three Jina models are now GA through Elastic Inference Service (EIS), removing the need for an external JinaAI API key or self-hosted ML nodes." | Elasticsearch Labs | Oct 2025 | Embedding API Integration |
| 36 | "Elasticsearch's approach requires creating an index mapping to start ingesting, embedding, and querying data, with no need to define model-related settings and parameters or to create inference ingest pipelines." | Elastic Docs | 2025 | Semantic Search Without External Pipeline |
| 37 | "Generating embeddings automatically requires configuring a language model that will convert text to embeddings both at ingestion time and query time." | OpenSearch Documentation | 2025 | Auto-Embedding at Ingest |
| 38 | "StarTree (Apache Pinot) announced native vector auto embedding support, with Model Context Protocol (MCP) integration, following Fall 2025." | StarTree Press Release | 2025 | Auto-Embedding at Ingest |
| 39 | "pgai Vectorizer makes it possible to automate embedding creation, keep them up-to-date as the source data changes, and manage it all within PostgreSQL without the complexity of building custom data workflows." | Timescale / TigerData Blog | 2025 | Auto-Embedding at Ingest |
| 40 | "After the initial launch of pgai Vectorizer, the team received consistent feedback from developers who wanted to use it with their existing managed Postgres databases." | DEV Community / Timescale | 2025 | Auto-Embedding at Ingest |
| 41 | "pgai Vectorizer solved embedding management problems with a declarative approach that automated the entire embedding lifecycle with a single SQL command, similar to how you'd create an index in Postgres." | TigerData Blog | 2025 | Auto-Embedding at Ingest |
| 42 | "Supabase Edge Functions, pgmq, pg_net, and pg_cron are required to bridge the gap — semantic search requires asynchronous API calls to a provider like OpenAI to generate vector embeddings." | Supabase Docs | 2025 | Embedding API Integration |
| 43 | "With AlloyDB, you can call custom Vertex AI models directly from the database, for high-throughput, low-latency augmented transactions." | Google Cloud Blog | 2025 | Model Serving Inside DB |
| 44 | "Weaviate allows users to upload raw text or images directly, and automatically invokes configured embedding models (OpenAI, Cohere, Hugging Face, etc.) to generate vectors, eliminating the need for an external vectorization pipeline." | Vector DB Comparison | 2025 | Auto-Embedding at Ingest |
| 45 | "Weaviate includes modules for text/vectorization (e.g., transformer models), reducing dependency on external APIs, whereas Milvus and Pinecone require you to handle embeddings separately." | Vector DB Comparison | 2025 | Embedding API Integration |
| 46 | "Pinecone requires you to handle embeddings separately rather than providing built-in embedding generation." | Pinecone vs Weaviate comparison | 2025 | Embedding API Integration |
| 47 | "Milvus offers a TextEmbedding Function interface that directly integrates popular embedding models from OpenAI, Cohere, AWS Bedrock, Google Vertex AI, Voyage AI, and other providers into your data pipeline." | Milvus Blog | 2025 | Embedding API Integration |
| 48 | "The line between embedding models and vector databases is blurring, with researchers co-designing end-to-end neural retrieval systems where the embedding space, quantization, and approximate nearest neighbor (ANN) structure are learned jointly." | Artsmart.ai / DeployBase | 2025 | AI Co-Processor Expectations |
| 49 | "Embedding quality will make or break performance, not your database choice, with most companies spending weeks comparing databases when their embedding model barely works." | Xenoss / Sparkco — Vector DB Comparison | 2025 | Market Expectation |
| 50 | "RAG tools come with steep trade-offs including high complexity and steep learning curves, code-heavy implementations limiting experimentation, fragmented workflows requiring context-switching between embedding models and vector databases, and tooling sprawl." | Morphik Blog | 2025 | Semantic Search Without External Pipeline |
| 51 | "77% of engineering leaders identify building AI capabilities into applications to improve features and functionality as a significant or moderate pain point." | Gartner Survey | May 2025 | Market Expectation |
| 52 | "60% of organizations will adopt AI-driven data integration tools by 2026, up from just 20% in 2022." | Gartner | 2025 | Market Expectation |
| 53 | "Standalone vector silos often introduce more operational friction than they offer in performance gains for many workloads." | Actian Blog — Vector DB Benchmarks | 2025 | AI Co-Processor Expectations |
| 54 | "One system built on Pinecone and OpenAI saw a 14 percent drop in retrieval precision after a silent model upgrade, and without versioned embeddings, side-by-side evaluation or clean rollbacks were impossible." | DevX / DevCommunity | 2025 | Embedding API Integration |
| 55 | "Without guardrails, the LLM might generate queries accessing restricted tables or leaking sensitive data — a key concern when integrating LLMs directly with databases." | Meilisearch Blog | 2025 | LLM Integration Expectations |
| 56 | "Best systems combine LLM-to-SQL with other methods like vector search for unstructured data, MCP for tool integration, and RAG for context-rich answers." | Meilisearch Blog | 2025 | LLM Integration Expectations |
| 57 | "MCP (joining the Linux Foundation) has already become the standard for tool and data access in agent-style LLM systems." | Meilisearch Blog | 2025 | LLM Integration Expectations |
| 58 | "By 2030, LLM-powered search is projected to eclipse traditional search engines in global usage, with the crossover expected around 2028-2030." | TTMS — LLM Search Forecast | 2025 | LLM Integration Expectations |
| 59 | "Foundational models become more deeply embedded within data infrastructure — whether through big data projects delivering high-quality data for ML, or through models providing intelligent support for data governance." | VLDB/CIDR 2025 Paper | 2025 | AI Co-Processor Expectations |
| 60 | "As foundational models become more deeply embedded within data infrastructure, there may be tighter integration between these domains — a bidirectional synergy that could fundamentally reshape how we approach data management." | VLDB/CIDR 2025 Paper | 2025 | AI Co-Processor Expectations |
| 61 | "Semantic search works entirely within private environments without external data sharing — you can deploy vector databases and embedding models on your own infrastructure, keeping all data and queries internal." | Parallel.ai | 2025 | Semantic Search Without External Pipeline |
| 62 | "The 2025 landscape increasingly emphasizes reducing external dependencies and building semantic search capabilities directly into databases and platforms rather than requiring separate specialized infrastructure." | TrustedTech | 2025 | Semantic Search Without External Pipeline |
| 63 | "Graft offers production-grade semantic search on your data without a single line of code, with complexity abstracted away — no need to worry about embedding generation, model hosting, index management, or deployment intricacies." | Graft Blog | 2025 | Semantic Search Without External Pipeline |
| 64 | "SQL Server 2025 can perform searches based on semantic similarity using vector data types and indexes, allowing queries for 'items similar to X' ranked by cosine similarity or other distance metrics." | TrustedTech | 2025 | Semantic Search Without External Pipeline |
| 65 | "Through the AWS SageMaker integration, you can easily use any dense, single-vector embedding model deployed as a SageMaker endpoint with your Weaviate vector database instance." | Weaviate Blog — BYOV | 2025 | Embedding API Integration |
| 66 | "Instead of treating inference as 'something outside the database,' you can import an ONNX model into the database as a schema-native object, and run inference directly in SQL — right next to your data." | Oracle Developers Blog | 2026 | Model Serving Inside DB |
| 67 | "The AI inference market is expected to grow from USD 106.15 billion in 2025 to USD 254.98 billion by 2030 at a CAGR of 19.2%." | MarketsAndMarkets | 2025 | Market Expectation |
| 68 | "Inference workloads will account for roughly two-thirds of all compute in 2026 (up from a third in 2023 and half in 2025), with the market for inference-optimized chips expected to grow to over $50 billion in 2026." | Deloitte AI Compute Report | 2025 | AI Co-Processor Expectations |
| 69 | "88 percent of organizations report regular AI use in at least one business function, compared with 78 percent a year ago." | McKinsey State of AI | 2025 | Market Expectation |
| 70 | "87 percent of companies identify AI as a top priority in their business plans, 76 percent of organizations use AI, and 69 percent use generative AI in at least one business function." | IDCA Q1 2025 Survey | 2025 | Market Expectation |
| 71 | "The embedding market in 2025 has significantly evolved: compact, well-distilled small models are competitive with multi-billion-parameter systems, shifting focus from raw size to smart architecture and training quality." | DeployBase | 2025 | Embedding API Integration |
| 72 | "Matryoshka Representation Learning (MRL) allows a 3072-dimension vector to be truncated to 256 dimensions while retaining semantic quality — a 12x storage reduction." | Encord / DeployBase | 2025 | Inline Embedding |
| 73 | "In 2025, managed vendors introduced complex 'read unit' pricing that created a 'Growth penalty': if your index grows from 10GB to 100GB, you may pay 10x as much for the same query result, primarily driving the market's shift toward 'Vector as a Feature'." | Actian Blog | 2025 | Market Expectation |
| 74 | "Reddit's engineering team, managing 340M+ vectors, identified metadata filtering as the primary performance bottleneck in their 2025 deployment." | DEV Community — RAG Pipelines | 2025 | AI Co-Processor Expectations |
| 75 | "84% of developers are using or planning to use AI tools in their development process, up from 76% in 2024." | Stack Overflow Developer Survey | 2025 | Market Expectation |
| 76 | "Only 29% of respondents say they trust AI outputs to be accurate, down from 40% in 2024. More developers actively distrust the accuracy of AI tools (46%) than trust it (33%)." | Stack Overflow Developer Survey | 2025 | LLM Integration Expectations |
| 77 | "The biggest single frustration, cited by 66% of developers, is dealing with 'AI solutions that are almost right, but not quite'." | Stack Overflow Developer Survey | 2025 | LLM Integration Expectations |
| 78 | "In 2024-2025, enterprise implementations increasingly use multiple embedding models specialized for different document types within the same pipeline." | DEV Community — RAG Production | 2025 | Embedding API Integration |
| 79 | "Organizations achieve 3.7x average ROI from AI-powered data integration, with IDC research showing top performers reaching 10.3x ROI through mature integration capabilities." | IDC / Bizdata360 | 2025 | Market Expectation |
| 80 | "The 10 Best Semantic Search APIs in 2025 — top providers include OpenAI, Cohere, Vertex AI, Jina, and Voyage AI, all offering REST endpoints consumable from within database trigger functions or ingest processors." | Shaped.ai | 2025 | Embedding API Integration |

---

## Category Summary

| Category | Count |
|---|---|
| Auto-Embedding at Ingest | 13 |
| Model Serving Inside DB | 12 |
| Embedding API Integration | 15 |
| Inline Embedding | 9 |
| Semantic Search Without External Pipeline | 7 |
| LLM Integration Expectations | 6 |
| Database Built-in ML Inference | 4 |
| AI Co-Processor Expectations | 8 |
| Market Expectation | 11 |
| **Total** | **85** |

---

## Key Themes

1. **Zero-pipeline embedding is an explicit product direction** — MongoDB, BigQuery, SQL Server 2025, Snowflake Cortex, Oracle 26ai, and OpenSearch all shipped or announced native auto-embedding at ingest in 2025.

2. **Developer friction from external pipelines is well-documented** — synchronization complexity, model versioning issues, operational overhead, and tooling sprawl are cited repeatedly as top pain points in 2025.

3. **ONNX as the universal import format** — Oracle, SQL Server, BigQuery, and Vertica all support importing ONNX models directly into the database engine for inference.

4. **Analysts confirm the demand** — IDC (74% plan integrated vector DBs), Gartner (77% cite AI app integration as a major challenge), McKinsey (88% regular AI use) all signal strong enterprise pull for unified AI+data systems.

5. **Inference is now the dominant AI workload** — inference is projected to account for ~66% of all AI compute by 2026, making in-database inference efficiency critical.

6. **Trust gap persists** — only 29% of developers trust AI output accuracy (Stack Overflow 2025), raising expectations for verifiable, auditable in-database AI pipelines with governance controls.
