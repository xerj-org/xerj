# Agent memory benchmark

Same data, same questions and one scoring script for every system.
Results are published at https://xerj.org/compare/ai-agent-memory-benchmark.

Run date: 2026-10-31. Machine: shared Linux x86_64 box, 8 vCPU Intel Xeon, 15 GB RAM, no GPU.
Other jobs ran on the box at the same time (load average about 8), so treat latency as a rough guide.

## Files

| File | What it is |
| --- | --- |
| `make_dataset.py`, `dataset.json` | 85 memories in 6 sessions and 75 questions (25 exact, 25 paraphrase, 15 update, 10 distractor). |
| `common.py` | Shared loop: write every memory, then ask every question, k=5. Records ranked texts and latency. |
| `run_<system>.py` | One adapter per system. Each one only calls the system's own API. |
| `score.py` | Scores every `results/*.json` the same way. Writes `out/summary.csv`, `out/summary.json`, `out/per_question.csv`. |
| `results/` | Raw output per run: ranked texts per question, per call latency, versions, commits. |
| `probes/memvid_and_probe.json` | memvid lexical search on full questions vs key words. |
| `stars.json` | GitHub stars and licenses from `gh api repos/<owner>/<repo>`, 2026-10-31. |
| `deps.json` | Package counts and how they were measured. |
| `llm_proxy.py` | Counting proxy in front of llama.cpp, so adapters can report LLM calls per write. |
| `emb_server.py` | Small OpenAI compatible `/v1/embeddings` server with fastembed all-MiniLM-L6-v2. Used only for ReMe hybrid. |
| `reme_hybrid_minilm.yaml` | ReMe's default.yaml with the embedding components enabled as its README describes, pointed at `emb_server.py`. |
| `make_visuals.py`, `md_table.py` | Build the site's tables and charts from `out/summary.json`. |

## Scoring

- hit@k: a returned memory in the top k contains the answer as a whole word, case insensitive.
- Newest first (update questions): the first returned memory that holds either the new or the old value holds the new one.
- Distractor correct first: same rule with the near miss value.
- Latency: median and p95 per call, measured in the client. HTTP systems include one local round trip.

## Systems and versions

| System | Version | Repo commit at test time | Config |
| --- | --- | --- | --- |
| XERJ | v1.0.0-rc.100 release binary | n/a | `bm25`, `semantic`, `hybrid`, `hybrid_recency` (recency_weight 0.3), all with `--embed-mode neural` |
| mem0 | mem0ai 2.2.1 | 753496cf | `raw`: infer=False, fastembed all-MiniLM-L6-v2, Qdrant local. `infer`: infer=True with the local LLM below |
| MemPalace | mempalace 3.11.0 | 4f7745af | defaults: Chroma, all-MiniLM-L6-v2 ONNX |
| ReMe | reme-ai[as] 0.4.2.1 | 1e51e0dc | `bm25_default`: default.yaml. `hybrid_minilm`: reme_hybrid_minilm.yaml |
| agentmemory | @agentmemory/agentmemory 0.9.31 | c6c476d2 | keyless default, no embeddings, read through smart-search, iii-engine v0.25.0 |
| mcp-memory-service | 12.4.0 with [sqlite] | 0071ab2a | defaults: sqlite_vec, all-MiniLM-L6-v2 ONNX, semantic dedup on |
| memvid | memvid-sdk 2.0.160 | e6bd9f7b | lexical default |

ReMe ownership: `pyproject.toml` lists "EconML team of Alibaba Tongyi Lab" as authors.

## Commands

Telemetry and feedback were off for every run:

```
export XERJ_DISABLE_FEEDBACK=true MEM0_TELEMETRY=False III_TELEMETRY_ENABLED=false MEMVID_TELEMETRY=0
# Chroma (MemPalace) anonymized telemetry is off by MemPalace's own setting.
```

XERJ (data dir on disk; the disk watermark was raised to 99% on this test node because the shared disk was nearly full):

```
tar xzf xerj-1.0.0-rc.100-x86_64-unknown-linux-gnu.tar.gz
xerj --insecure --port 9411 -d tmp/xerjdata --embed-mode neural --disable-feedback
XERJ_BIN=tmp/xerj-1.0.0-rc.100-x86_64-unknown-linux-gnu/xerj python3 run_xerj.py bm25   # also semantic, hybrid, hybrid_recency
```

Python systems (one venv each, Python 3.12):

```
uv venv tmp/venv_mem0 && uv pip install -p tmp/venv_mem0 mem0ai==2.2.1 fastembed
tmp/venv_mem0/bin/python run_mem0.py raw
tmp/venv_mem0/bin/python run_mem0.py infer      # needs the LLM below
uv pip install mempalace==3.11.0 && python run_mempalace.py default
uv pip install 'reme-ai[as]==0.4.2.1' && python run_reme.py <venv>/bin/reme bm25_default
python emb_server.py 8097 &                      # for ReMe hybrid only
python run_reme.py <venv>/bin/reme hybrid_minilm
uv pip install 'mcp-memory-service[sqlite]==12.4.0' && python run_mms.py default
uv pip install memvid-sdk==2.0.160 && python run_memvid.py lex && python probe_memvid_and.py
```

agentmemory (Node):

```
npm install @agentmemory/agentmemory@0.9.31
HOME=tmp/am/home III_TELEMETRY_ENABLED=false AGENTMEMORY_SECRET=<any> npx agentmemory --port 3411 --data-dir tmp/am/data &
python3 run_agentmemory.py keyless_bm25          # POST /agentmemory/remember, then /agentmemory/smart-search
```

Score and build:

```
python3 score.py
python3 make_visuals.py <repo_root>
```

Adapters write temporary stores under `/dev/shm/memb`. ReMe, MemPalace, memvid and mcp-memory-service stores were on tmpfs; XERJ, agentmemory and mem0's Qdrant were on disk.

## LLM for systems that call a model on write

llama.cpp b12019, Qwen2.5-3B-Instruct Q4_K_M, CPU only, behind `llm_proxy.py` on :8098:

```
llama-server -m qwen2.5-3b-instruct-q4_k_m.gguf --host 127.0.0.1 --port 8099 -c 16384 -np 1 -t 4 -tb 6 --temp 0
python3 llm_proxy.py 8098 http://127.0.0.1:8099
```

For every run that makes no LLM call, the llama-server process was paused (`kill -STOP`) so it did not take CPU.

## Findings worth knowing

- memvid lexical search returned nothing for full questions. The same key words without question words found the memory. See `probes/memvid_and_probe.json`.
- memvid vector search was not tested. In the PyPI build, local embeddings returned MV015 and a custom embedder stored no vectors (MV011).
- mcp-memory-service rejected u02, u31, u44 and u45 as semantic duplicates (default threshold 0.85, 24 hour window). Those updates were never stored.
- ReMe indexes files in the background. After the last write, memories took 6.04 s (BM25) and 7.13 s (hybrid) to become searchable. Writes are fast because they only write a file.
- XERJ `hybrid_recency` ranks the new value first for 14 of 15 updates but lowers overall top 5 recall.
- The XERJ BM25 median query was 0.9 ms in an earlier run on a quieter box and 3.77 ms in the recorded run.

## Not tested

claude-mem, Hindsight, cognee, Graphiti, Supermemory, TencentDB Agent Memory, Letta, Memori, memU, EverOS and MemOS. Each one needs an LLM call on write, an extra service (graph database, Docker stack) or a hosted key, or is an agent harness rather than a memory API. With a 3B model on CPU, mem0 infer took a median of 16.5 s per write (p95 about 65 s) on this box, so a fair run needs a GPU.

## mem0 with its LLM (infer=True)

85 writes, 85 LLM calls (1 per write), median write 16.5 s, p95 64.9 s, total ingest 2394 s.
Qwen2.5 3B returned output that mem0 could not parse on 2 writes ("Error parsing extraction response").
The LLM server ran during this run, so its latency also reflects a busy shared box.
