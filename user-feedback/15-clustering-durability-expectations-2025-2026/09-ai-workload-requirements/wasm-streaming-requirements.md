# WASM & Streaming Data Processing Requirements: Research Findings 2025–2026

**Research Date:** April 2026  
**Searches Conducted:** 10 targeted queries  
**Sources:** 20+ articles, documentation pages, and technical references  
**Data Points Collected:** 95+

---

## Overview

WebAssembly (WASM) has crossed the threshold from experimental to production-ready for database extensibility and streaming data pipelines. The data below synthesizes findings from database vendors, streaming platform builders, analyst commentary, and developer-facing documentation into concrete, numbered data points. Points are grouped by theme to support product planning and feature prioritization.

---

## Section 1: WASM UDFs in Databases — Adoption & Implementations

1. **ClickHouse** ships WASM UDF support as an experimental feature using `CREATE FUNCTION ... LANGUAGE WASM` syntax, backed by Wasmtime as the default engine (with WasmEdge as an alternative).

2. **ScyllaDB** supports WASM-based User-Defined Functions (UDFs) and User-Defined Aggregates (UDAs) in experimental mode, using Wasmtime compiled natively in Rust.

3. **ScyllaDB chose Wasmtime over Google V8** specifically because Wasmtime is "lighter than V8 and its potential for being async-friendly" — demonstrating that runtime selection is driven by database-internal architecture constraints, not just performance alone.

4. **SingleStore Code Engine** supports both UDFs and Table-Valued Functions (TVFs) via WASM compilation, making it one of the more complete WASM extensibility platforms among commercial databases.

5. **TiDB** built a WASM UDF engine on the Wasmer runtime during TiDB Hackathon 2020, with LLVM integration for fast bytecode compilation.

6. **libSQL** (SQLite fork) integrates WasmEdge to support UDFs, requiring the `wasm32-wasip1` compilation target due to WasmEdge's WASI function support.

7. **DuckDB** supports WASM-based UDFs, enabling data engineers to write custom code that runs directly within the in-memory analytical database.

8. **Seafowl** (now part of EnterpriseDB) supported custom WASM UDFs as a first-class feature, reflecting that even newer analytical databases adopted WASM extensibility early.

9. **MySQL HeatWave** and standard **SQLite** allow creation of WebAssembly-based UDFs callable directly from SQL.

10. **RedPanda** and **InfinyOn (Fluvio)** both support WASM for data stream manipulation — extending the WASM UDF pattern beyond relational databases into streaming platforms.

11. **WASI SQL Embedding** is a proposed standard to define how WASM modules can be embedded in SQL databases as extensions — a sign that the ecosystem is pushing toward interoperability, not just vendor-specific implementations.

12. Multiple databases are exploring WASM beyond scalar UDFs: stored procedures, table-valued functions (TVFs), user-defined aggregates (UDAs), custom types, and custom index types are all in active discussion or development.

---

## Section 2: Language Support & Developer Expectations

13. **Over ten programming languages** currently compile to WebAssembly, with more anticipated — meaning database platforms adopting WASM UDFs gain multi-language support with minimal incremental engineering effort.

14. **ClickHouse WASM UDFs** officially support Rust, C, and C++, with the requirement that code targets `wasm32-unknown-unknown` (freestanding, no OS or standard library dependencies).

15. **ScyllaDB UDFs** support WAT (WebAssembly Text format), C/C++ (via clang), Rust (via cargo wasm32 targets), and AssemblyScript (TypeScript-like language for WASM).

16. **TiDB's WASM UDF engine** supports C, C++, Go, and Rust natively, with Emscripten used for source-to-WASM conversion.

17. A critical developer expectation: **Python support with NumPy** for WASM UDFs is an active ask. Host interface standardization remains a challenge for libraries with native dependencies (like NumPy for data science users).

18. **JavaScript/TypeScript** can be compiled to WASM components using ComponentizeJS, which embeds the SpiderMonkey engine — useful for plugin systems but adds overhead.

19. Developers expect a **write-once, deploy-anywhere** model: TiDB's pitch is "You submit your bytecode once, and your custom function can be executed across all TiDB nodes" regardless of whether nodes are x86 or ARM.

20. **Rust is the dominant development language** for writing WASM UDFs in performance-critical database settings, but user demand for Go, Python, and TypeScript remains high.

---

## Section 3: Performance Characteristics & Benchmarks

21. **WASM UDF n-body benchmark** execution speed approximates Rust performance and exceeds Go implementation speeds — near-native is not marketing copy; it holds in synthetic benchmarks.

22. **WASM cold start times**: "often instantiating in microseconds to single-digit milliseconds" — compared to traditional container-based serverless functions requiring "hundreds of milliseconds or even seconds."

23. **AWS Lambda WASM benchmarks** show 10–40x improvements in cold start times compared to container-based functions (cited in server-side WASM guide, 2025).

24. **WebAssembly binary sizes**: A 'Hello, World' binary is a few kilobytes. Container images are often hundreds of megabytes. For plugin distribution, this difference is material.

25. **WebAssembly module cold start** is typically 1–5 milliseconds vs. 50–500 milliseconds for containers. This matters for UDFs executed per-query or per-row.

26. **InfinyOn claims 50x less memory** than traditional JVM-based streaming solutions for analytics workloads, attributed to Rust + WASM architecture.

27. InfinyOn benchmarks claim **"100X faster" pipeline development** (developer ergonomics claim, not throughput claim) vs. traditional approaches.

28. **JIT-based WASM runtimes** (Wasmtime, Wasmer) compile WASM to native code at runtime for high performance; **interpreter-based runtimes** (wasm3) are slower but easier to embed and "very predictable" — relevant tradeoff for embedded database scenarios.

29. **ClickHouse provides per-query performance controls**: `webassembly_udf_max_fuel` (instruction limits), `webassembly_udf_max_memory`, `webassembly_udf_max_input_block_size`, and `webassembly_udf_max_instances` — operators expect fine-grained resource controls.

30. **ClickHouse WASM UDF ABI modes**: ROW_DIRECT (row-by-row, primitive types only, minimal overhead, no serialization) vs. BUFFERED_V1 (block-based, all ClickHouse types, serialization overhead) — users must choose between ergonomics and raw throughput.

31. **Fermyon handles 75 million requests per second** on their WASM-based edge platform, demonstrating that WASM execution density at scale is production-proven (2025 data).

32. **Cloudflare 2025 developer survey**: 34% of Workers deployments now include WASM components. Primary use cases: data transformation (28%), cryptography (22%), image processing (19%).

---

## Section 4: Security & Sandbox Properties

33. **WASM linear memory model**: bounds-checked, no direct pointers into host memory — this is enforced by the specification, not convention.

34. **Capability-based access control**: WASM modules cannot access files, sockets, or syscalls unless explicitly granted by the host runtime. This is the core security primitive.

35. **WASI defines a minimal, capability-based API** rather than exposing arbitrary system calls — this is intentional and directly relevant to embedding WASM in multi-tenant databases.

36. **ScyllaDB's rationale for sandboxing**: "vastly reduced risk of somebody running malicious code" within embedded language contexts — this is the explicit user promise for multi-tenant UDF execution.

37. **TiDB's UDF engine security promise**: "the UDF engine can only execute limited instructions, without the risk of executing malicious code or invoking system calls."

38. **ClickHouse WASM UDF execution**: "guest code executes in a sandboxed environment having access only to a dedicated memory space" — with SHA256 hash verification supported for cross-replica consistency.

39. **ClickHouse host API surface** exposed to guest WASM is minimal: `clickhouse_server_version()`, `clickhouse_throw()`, `clickhouse_log()`, `clickhouse_random()` — no arbitrary host calls.

40. **Google V8 "sandbox-within-a-sandbox"**: Even if an attacker achieves memory corruption inside V8, they find themselves trapped in a secondary "virtual" heap containing no raw pointers to the host process. Defense-in-depth is a real production concern.

41. **Wasmtime CVE-2023-6699**: A regression in handling of externref could cause runtime confusion between host-managed objects and raw integers, leading to host-side panic or potential memory disclosure. Production runtimes have real vulnerability surface.

42. **Wasmer filesystem escape vulnerability**: Malicious WASM modules could bypass WASI filesystem restrictions by exploiting how the runtime translated virtual paths to host paths. This demonstrates that the sandbox is not theoretical — attackers are probing it.

43. **WAVEN (2025 NDSS paper)**: WebAssembly memory virtualization for enclaves — academic research into strengthening WASM isolation even further, showing active security community investment.

44. **Wasmtime is implementing control-flow-integrity (CFI)** mechanisms to leverage hardware state and further guarantee that WebAssembly stays within its sandbox, mitigating potential Cranelift (JIT compiler) bugs.

45. **Key author caution**: WASM sandboxing is "not a silver bullet" — documented sandbox escapes exist via runtime bugs. Executing truly arbitrary, untrusted code remains inherently risky regardless of sandboxing technology.

46. **Traditional native plugins** have the same privileges as the host application — keyloggers, file exfiltration, and system compromise are documented risks. WASM's capability-based model is a genuine improvement, not just marketing.

---

## Section 5: Streaming Data Processing — WASM Integration

47. **Redpanda Connect, Fluvio SmartModules, and Vector WASM transforms** all allow developers to upload small WASM modules that execute directly within the data flow — eliminating the need for separate microservices.

48. **InfinyOn Fluvio SmartModules** are custom data processing functions written in Rust, compiled to WASM, executing on Streaming Processing Units. The aggregate function pattern: accumulator state + current record → output.

49. **Fluvio SmartModule aggregation example**: Maintaining running averages using the formula `new_average = average + (value - average) / count` in stateful WASM — demonstrating that UDFs can maintain per-stream state, not just transform individual records.

50. **Fraud detection use case**: "With WebAssembly, you can ship the logic that says 'yes, this is fraudulent or no, this is not fraudulent', allowing you to make real-time experiences actually feel real-time." — WASM-inside-pipeline as a latency solution.

51. **InfinyOn deployment package** is 37 MB deployable on edge devices — small enough for IoT-class hardware.

52. **SmartModule Hub**: Reusable WASM transformation packages published as a shared repository — this is an emerging user expectation: a marketplace/registry for pipeline plugins, not just a runtime API.

53. **WASM pipeline programmability**: Redpanda and Fluvio enable developers to "process, enrich, and filter events securely at runtime without the need to deploy separate microservices" — the pipeline itself becomes programmable.

54. **QCon London 2024 session** "Simplifying Streaming Data Transforms with WebAssembly" appeared in the program — indicating WASM in streaming is a practitioner-facing topic, not just research.

55. **Linux Foundation webinar**: "Simplifying Streaming Data Transforms with WASM" — open-source community endorsement of WASM for pipeline extensibility.

56. **InfinyOn user testimonial**: Practitioners emphasize desire for unified solutions "without memory limitations of JVM-based tools" and preference for "event-based approach without babysitting a bunch of point solutions."

---

## Section 6: Streaming Data Pipeline Requirements (General)

57. **86% of IT leaders** identify investments in data streaming as a top strategic priority (2025 data).

58. **25% of organizations** have reached advanced streaming maturity in 2025, up from just 8% in 2024 — a 3x increase in one year.

59. **44% of organizations** achieve a 500% return on investment from implementing real-time data streaming and analytics.

60. **Real-Time Analytics Market** projected to reach USD 193.71 billion by 2032 at 25.60% CAGR.

61. **Real-Time Data Streaming Tools market** projected to reach USD 35.3 billion by 2032.

62. **Core five-layer streaming pipeline architecture**: (1) Data Ingestion, (2) Stream Processing/Transformation, (3) Data Storage, (4) Real-Time Analytics, (5) Monitoring & Observability. Users expect all five layers to be addressable within a platform.

63. **Streaming systems must handle**: millions of events per second, latency in seconds or milliseconds, and continuous operation without scheduled downtime. These are baseline expectations, not advanced features.

64. **Apache Kafka** remains the foundational backbone for most enterprise streaming architectures, with Redpanda (Kafka-compatible) as the leading challenger (2025 landscape).

65. **Stream processing engines** in active use: Apache Flink, Apache Spark Streaming, Apache Storm, Kafka Streams, Redpanda Connect.

66. **User Defined Functions in Google Pub/Sub** (added in 2025): UDF support for transforming messages within managed streaming infrastructure — major cloud providers are following the same extensibility pattern as open-source tools.

67. **Multimodal data library support**: 2025 streaming platforms are adding support for multiple model types in a single pipeline — WASM is one mechanism to do this portably.

68. **Practitioners explicitly want** to "make ingestion smarter" — OpenSearch 3.1 introduced system ingest pipelines specifically to automate document processing without requiring manual user pipeline configuration.

---

## Section 7: Pre-Indexing Transformation Requirements

69. **Elasticsearch ingest pipelines** let users "perform common transformations on your data before indexing" including removing fields, extracting values from text, and enriching data — this is a well-established user expectation pattern.

70. **User expectation shift**: "Previously, if you built a custom ingest processor, users had to set up and manage the pipeline themselves. With system ingest pipelines, that burden shifts to the plugin" (OpenSearch 3.1) — users expect zero-config transformations.

71. **Custom ingest processors in Elasticsearch**: Can add fields, obfuscate sensitive information, route documents, run ML models, and call external APIs — the scope of what users expect "pre-indexing transformation" to mean is broad.

72. **Splunk Ingest Processor** supports pipeline syntax for pre-ingest transformation at scale — demonstrating that even mature log management platforms are now built around programmable ingest pipelines.

73. **Schema evolution is a first-class requirement**: "Data is never static. Fields get added, formats change, and new use cases emerge. If your ingestion pipeline isn't designed to accommodate evolving schemas, even small changes can cause major disruptions downstream."

74. **Data quality and validation at ingestion**: "The pipeline algorithmically inspects and validates the raw data to confirm it meets expected standards for accuracy and consistency" — this is an expected feature, not an advanced add-on.

75. **Observability of ingest pipelines**: "transforms ingestion from a black box into a transparent, measurable system" — users expect per-pipeline metrics, not just overall throughput numbers.

76. **Dead Letter Queues (DLQs)** are a standard expectation for failed ingest events: route failures with metadata, separate transient from structural errors, monitor DLQ volume as a health signal.

77. **Schema registries and contracts** are expected in 2025 production pipelines: versioned schemas, central registries, backward-compatible change support, and version tracking in record metadata.

78. **Security at the ingest edge**: Encrypt in transit (TLS), apply access controls, mask/tokenize PII before storage, implement audit logging. These are table-stakes expectations.

79. **Timestamp normalization** is a documented pain point: all timestamps must be normalized to UTC with ISO 8601 format, capturing both event time and ingestion time.

80. **Idempotency and deduplication** using UUID/ULID event identifiers are standard engineering requirements for production ingest pipelines.

---

## Section 8: Edge Computing & Serverless WASM Processing

81. **WebAssembly 3.0 became a W3C standard in September 2025**, standardizing nine production features: WasmGC, exception handling, tail calls, 64-bit memory, 128-bit SIMD, and others.

82. **WASI 0.3** introduced native async capabilities in early 2025 — resolving a longstanding gap for database and streaming use cases that require asynchronous execution.

83. **WASI Sockets** reached standardization with full HTTP client and server support in 2025–2026 — enabling WASM modules in databases to initiate outbound network calls within a controlled capability model.

84. **Fastly Compute@Edge**: 10,000+ users on WASM-based edge computing platform (2025).

85. **American Express** adopted WASM for an internal Function-as-a-Service platform — described as "what may be the largest commercial deployment of WASM to date" — representing enterprise-scale validation.

86. **AWS Lambda** supports WASM functions as a first-class runtime, with benchmarks showing 10–40x improvements in cold start times vs. container-based functions.

87. **Edge WASM use cases** (2025): content transformation (28%), cryptography (22%), image processing (19%) — data transformation is the top production use case.

88. **Cloudflare Workers** handle millions of requests with sub-millisecond cold starts — demonstrating WASM execution density achievable in production at scale.

89. **WASM on IoT and embedded devices**: InfinyOn's 37 MB package fits on edge IoT devices — the WASM execution model is viable from cloud down to constrained hardware.

90. **Cost savings from edge processing**: Organizations avoid expensive cloud egress fees by processing WASM transforms locally or at edge nodes — particularly relevant for applications handling large volumes of user-generated data.

91. **Spin framework** (Fermyon) supports popular programming languages for WASM serverless functions and integrates with standard IDEs and version control — developer toolchain maturity is now a user expectation.

---

## Section 9: Plugin System Architecture Expectations

92. **Traditional native plugin risks**: Plugins have the same privileges as the host application — keyloggers, file exfiltration, and system compromise are real documented risks, making WASM sandboxing a genuine security upgrade.

93. **WIT (Wasm Interface Types) language** provides machine-readable interface definitions for WASM components — richer type system than C ABI: records, variants, enums, lists, options, results, tuples, resources.

94. **Extism framework**: Lightweight framework for building with WebAssembly that supports running WASM code on servers, edge, CLIs, IoT, browsers — a cross-platform plugin runtime abstraction gaining traction.

95. **Helm 4 WASM plugin system**: Feature-complete by November 2025 as part of Helm v4.0.0 — indicating WASM plugins are crossing into mainstream DevOps tooling, not just database and streaming platforms.

96. **wasmCloud wash CLI** features an extensible architecture with WebAssembly-based plugins — cloud-native tooling adopting the pattern.

97. **Istio WASM plugin distribution**: Production-scale service mesh supports WASM module distribution for extensibility — proving the plugin distribution model works at Kubernetes scale.

98. **Data-intensive plugin limitation**: Shared memory buffers remain a challenge for WASM components in data-heavy scenarios. Ongoing work includes asynchronous streams and shared heaps for future iterations.

---

## Section 10: Gaps, Risks & Critical Observations

99. **"Three Years of Almost Ready"** (Java Code Geeks, April 2026): WebAssembly continues to receive "almost production-ready" characterizations in some quarters — adoption is real but uneven across ecosystems.

100. **ClickHouse WASM UDFs are marked experimental and not supported in ClickHouse Cloud** (as of research date) — vendor adoption is production on self-hosted but lagging on managed cloud offerings.

101. **ScyllaDB WASM UDFs remain in experimental mode** — the gap between "available" and "production-recommended" is significant and matters for enterprise adoption timelines.

102. **No universal WASM UDF standard across databases** exists yet. WASI SQL Embedding is a proposal, not a ratified specification — users writing WASM UDFs for one database cannot directly port them to another.

103. **Freestanding WASM requirement (ClickHouse)**: Modules must compile to `wasm32-unknown-unknown` with no OS or standard library dependencies — this rules out most existing Rust/C++ libraries that use the standard library, limiting practical language ecosystem access.

104. **JavaScript plugin overhead**: ComponentizeJS embeds the full SpiderMonkey engine in the WASM binary — enabling JS plugins but with significant size and startup overhead compared to native WASM.

105. **WASM module deletion is blocked** in ClickHouse if UDFs reference the module — operational lifecycle management of WASM modules is an unsolved area that users will hit in production.

106. **Binary serialization overhead** in WASM streaming pipelines (Fluvio): serde_json encode/decode operations add measurable latency for structured data — users need to understand serialization cost when designing WASM pipelines.

107. **The platform itself becomes programmable** is both a feature and a support burden — when users can inject arbitrary WASM into data pipelines, debugging becomes non-trivial. Observability tooling for WASM execution is an active gap.

---

## Summary: Key Takeaways for XERJ.ai Feature Planning

**What users already expect (table stakes):**
- WASM UDF support in analytical databases (ClickHouse, DuckDB, ScyllaDB all have it)
- Sandbox isolation for UDFs (security is the primary motivation)
- Multi-language support (Rust, C/C++ at minimum; Go and Python are demanded)
- Pre-indexing transformation hooks (Elasticsearch/OpenSearch have normalized this)
- Schema evolution support in ingest pipelines
- Dead letter queues for ingest failures
- Per-pipeline observability and metrics

**What users want next (differentiators in 2025–2026):**
- WASM UDFs in managed/cloud offerings (not just self-hosted)
- Plugin marketplaces/registries (SmartModule Hub model)
- Zero-config transformation pipelines (system ingest pipelines, not user-managed)
- Portable WASM modules across databases (standards not yet there)
- Async WASM execution with latency guarantees
- Edge-deployable WASM pipeline transforms
- Rich type interfaces (WIT) rather than raw C ABI for plugin APIs

**Where the ecosystem is still rough:**
- No cross-database WASM UDF portability standard
- Managed cloud offerings lag self-hosted in WASM support
- Standard library access limitations in freestanding WASM targets
- Operational lifecycle management (versioning, deletion, rollback) of WASM modules
- Debugging and tracing inside WASM execution
- Python/NumPy support for data science UDF use cases

---

## Sources

- [How WebAssembly is Eating the Database, One UDF At a Time](https://dylibso.com/blog/wasm-udf/)
- [Wasmtime: Supporting UDFs in ScyllaDB with WebAssembly](https://www.scylladb.com/2022/04/14/wasmtime/)
- [WebAssembly User Defined Functions | ClickHouse Docs](https://clickhouse.com/docs/sql-reference/functions/wasm_udf)
- [How to Use Wasm to Add UDFs to the Database](https://www.secondstate.io/articles/udf-saas-extension/)
- [UDF in the libSQL database | WasmEdge Developer Guides](https://wasmedge.org/docs/embed/use-case/libsql/)
- [The State of WebAssembly – 2025 and 2026](https://platform.uno/blog/the-state-of-webassembly-2025-2026/)
- [Beyond the Browser: The Developer's Guide to Server-Side WebAssembly in 2025](https://toolshelf.tech/blog/server-side-webassembly-wasm-guide-2025/)
- [How WebAssembly Powers Databases: Build a UDF Engine with WASM | TiDB](https://pingcap.medium.com/how-webassembly-powers-databases-build-a-udf-engine-with-wasm-1384d28342f0)
- [Create Wasm UDFs · SingleStore Helios Documentation](https://docs.singlestore.com/cloud/reference/code-engine-powered-by-wasm/create-wasm-udfs/)
- [Simplifying Streaming Data Transforms With WASM | Linux Foundation](https://www.linuxfoundation.org/webinars/simplifying-streaming-data-transforms-with-wasm)
- [WebAssembly Brings Inline Data Transformations to RedPanda | The New Stack](https://thenewstack.io/webassembly-brings-easy-inline-data-transformations-to-redpanda-kafka-streaming-platform/)
- [QCon London 2024: Simplifying Streaming Data Transforms with WebAssembly](https://qconlondon.com/presentation/apr2024/simplifying-streaming-data-transforms-webassembly)
- [Aggregate streaming data in real-time with WebAssembly | InfinyOn](https://www.infinyon.com/blog/2021/08/smartmodule-aggregates/)
- [Build end-to-end streaming analytics pipelines 100X faster | InfinyOn](https://infinyon.com/)
- [From Cloud to Edge computing – WASM I/O 2025](https://2025.wasm.io/sessions/from-cloud-to-edge-computing-unleashing-the-power-of-webassembly-at-the-edge/)
- [WebAssembly in 2026: Three Years of "Almost Ready" | Java Code Geeks](https://www.javacodegeeks.com/2026/04/webassembly-in-2026-three-years-of-almost-ready.html)
- [WebAssembly's Edge Revolution: Redefining Serverless Computing in 2025](https://kawaldeepsingh.medium.com/webassemblys-edge-revolution-how-wasm-is-redefining-serverless-computing-in-2025-638e21751386)
- [WASM as a Secure Sandbox: From the Browser to Distributed Runners](https://www.tspi.at/2025/10/02/wasmsandbox.html)
- [Security - WebAssembly](https://webassembly.org/docs/security/)
- [Security - Wasmtime](https://docs.wasmtime.dev/security.html)
- [Building Native Plugin Systems with WebAssembly Components | Sy Brand](https://tartanllama.xyz/posts/wasm-plugins/)
- [Introducing the wash CLI's new Wasm-powered plugin system | wasmCloud](https://wasmcloud.com/blog/introducing-wash-wasm-powered-plugin-system/)
- [GitHub - extism/extism: The framework for building with WebAssembly](https://github.com/extism/extism)
- [Making ingestion smarter: System ingest pipelines in OpenSearch](https://opensearch.org/blog/making-ingestion-smarter-system-ingest-pipelines-in-opensearch/)
- [Elasticsearch ingest pipelines | Elastic Docs](https://www.elastic.co/docs/manage-data/ingest/transform-enrich/ingest-pipelines)
- [10 Best Practices in Data Ingestion | Shaped](https://www.shaped.ai/blog/10-best-practices-in-data-ingestion)
- [Complete Overview of Streaming Data Pipeline Architecture | Acceldata](https://www.acceldata.io/blog/mastering-streaming-data-pipelines-for-real-time-data-processing)
- [The Data Streaming Landscape 2025 | Kai Waehner](https://kai-waehner.medium.com/the-data-streaming-landscape-2025-d3df73e5627d)
- [The Importance of Real-Time Data Processing in 2025 | Data Engineer Academy](https://dataengineeracademy.com/module/the-importance-of-real-time-data-processing-in-2025/)
- [Real-Time Data Integration Statistics 2026 | Integrate.io](https://www.integrate.io/blog/real-time-data-integration-growth-rates/)
- [Serverless 2.0: Unlocking Performance and Portability with WebAssembly | IJCESEN](https://ijcesen.com/index.php/ijcesen/article/view/4130)
- [Akamai and Fermyon Launch Edge-Native Serverless and AI Solutions Powered by WebAssembly](https://thecuberesearch.com/akamai-and-fermyon-launch-edge-native-serverless-and-ai-solutions-powered-by-webassembly/)
- [WASM Sandbox - IronClaw Security](https://www.mintlify.com/logicminds/ironclaw/security/wasm-sandbox)
- [UDFs in DuckDB: Unlocking Custom Functionality with Web Assembly | Orchestra](https://www.getorchestra.io/guides/udfs-in-duckdb-unlocking-custom-functionality-with-web-assembly)
