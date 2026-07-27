#!/usr/bin/env python3
"""Run bounded ingest diagnostics through XERJ's real ES-compatible HTTP path."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import signal
import socket
import subprocess
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

SCHEMA = "xerj.bounded_run.v1"
PROC_SCHEMA = "xerj.process_sample.v1"
HERE = Path(__file__).resolve().parent


def hash_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def toml_string(value: str) -> str:
    """Encode a TOML basic string without interpolating path syntax."""
    return json.dumps(value)


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def request(url: str, method: str = "GET", value=None, raw: bytes | None = None) -> dict:
    if raw is None and value is not None:
        raw = json.dumps(value, separators=(",", ":")).encode()
    req = urllib.request.Request(
        url, data=raw, method=method, headers={"content-type": "application/json"}
    )
    try:
        with urllib.request.urlopen(req, timeout=300) as response:
            payload = response.read()
    except urllib.error.HTTPError as error:
        body = error.read().decode(errors="replace")
        raise RuntimeError(f"{method} {url}: HTTP {error.code}: {body[:2000]}") from error
    return json.loads(payload or b"{}")


def read_proc(pid: int) -> dict:
    root = Path("/proc") / str(pid)
    result: dict[str, object] = {}
    try:
        for line in (root / "status").read_text().splitlines():
            key, _, value = line.partition(":")
            if key in {"VmRSS", "VmHWM", "VmSize", "RssAnon", "RssFile", "RssShmem"}:
                result[f"{key.lower()}_bytes"] = int(value.split()[0]) * 1024
        for line in (root / "smaps_rollup").read_text().splitlines():
            key, _, value = line.partition(":")
            if key in {"Rss", "Pss", "Private_Clean", "Private_Dirty", "Swap"}:
                result[f"smaps_{key.lower()}_bytes"] = int(value.split()[0]) * 1024
        for line in (root / "io").read_text().splitlines():
            key, _, value = line.partition(":")
            if key in {"read_bytes", "write_bytes", "rchar", "wchar"}:
                result[f"io_{key}"] = int(value.strip())
        stat = (root / "stat").read_text().split()
        result["cpu_user_ticks"] = int(stat[13])
        result["cpu_system_ticks"] = int(stat[14])
        result["threads"] = len(list((root / "task").iterdir()))
        result["fds"] = len(list((root / "fd").iterdir()))
    except (FileNotFoundError, ProcessLookupError, PermissionError, ValueError):
        result["sample_error"] = True
    return result


class ProcSampler:
    def __init__(self, pid: int, path: Path, phase, interval: float):
        self.pid = pid
        self.path = path
        self.phase = phase
        self.interval = interval
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self._run, name="xerj-proc-sampler", daemon=True)

    def _run(self):
        with self.path.open("w", encoding="utf-8", buffering=1) as target:
            os.fchmod(target.fileno(), 0o600)
            while not self.stop.is_set():
                process = read_proc(self.pid)
                if process.get("sample_error"):
                    break
                sample = {
                    "schema": PROC_SCHEMA,
                    "ts_unix_ns": time.time_ns(),
                    "pid": self.pid,
                    "phase": self.phase(),
                    **process,
                }
                target.write(json.dumps(sample, sort_keys=True) + "\n")
                self.stop.wait(self.interval)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.stop.set()
        self.thread.join(timeout=max(2.0, self.interval * 2))


def start_server(
    binary: Path, cell: Path, data: Path, suffix: str, sample_ms: int, embed_mode: str
):
    es_port, rest_port, grpc_port = free_port(), free_port(), free_port()
    config = cell / f"server-{suffix}.toml"
    trace = cell / f"ingest-memory-{suffix}.ndjson"
    log = cell / f"server-{suffix}.log"
    config.write_text(
        "[server]\n"
        f'rest_port = {rest_port}\n'
        f'grpc_port = {grpc_port}\n'
        f'es_compat_port = {es_port}\n'
        'bind_address = "127.0.0.1"\n'
        f"data_dir = {toml_string(str(data))}\n\n"
        "[auth]\nenabled = false\n",
        encoding="utf-8",
    )
    config.chmod(0o600)
    env = os.environ.copy()
    env.update(
        {
            "XERJ_INGEST_MEMORY_TRACE": "summary",
            "XERJ_INGEST_MEMORY_SAMPLE_MS": str(sample_ms),
            "XERJ_INGEST_MEMORY_OUTPUT": str(trace),
            "XERJ_EMBED_MODE": embed_mode,
            "XERJ_LOG": "info",
        }
    )
    stream = log.open("wb")
    os.fchmod(stream.fileno(), 0o600)
    try:
        process = subprocess.Popen(
            [str(binary), "--insecure", "--config", str(config), "--data-dir", str(data)],
            stdout=stream,
            stderr=subprocess.STDOUT,
            env=env,
        )
    except BaseException:
        stream.close()
        raise
    url = f"http://127.0.0.1:{es_port}"
    for _ in range(240):
        if process.poll() is not None:
            stream.close()
            raise RuntimeError(f"server exited {process.returncode}; inspect {log}")
        try:
            request(url)
            return process, stream, url, trace, config
        except Exception:
            time.sleep(0.05)
    process.kill()
    process.wait()
    stream.close()
    raise RuntimeError(f"server did not become ready; inspect {log}")


def stop_server(process: subprocess.Popen, stream) -> int:
    process.send_signal(signal.SIGINT)
    try:
        code = process.wait(timeout=60)
    except subprocess.TimeoutExpired:
        process.kill()
        code = process.wait()
        raise RuntimeError("server did not stop gracefully within 60 seconds")
    finally:
        stream.close()
    if code != 0:
        raise RuntimeError(f"server exited with status {code}")
    return code


def cleanup_server(process: subprocess.Popen, stream) -> None:
    """Best-effort no-throw cleanup for an interrupted or failed cell.

    This is deliberately safe to call after ``stop_server``. It never replaces
    the request/verification exception that caused a cell to abort.
    """
    try:
        if process.poll() is None:
            try:
                process.send_signal(signal.SIGINT)
                process.wait(timeout=10)
            except (ProcessLookupError, subprocess.TimeoutExpired):
                if process.poll() is None:
                    process.kill()
                    process.wait()
    except BaseException:
        # Cleanup is a last-resort guard. The original cell failure is more
        # actionable and must retain its traceback.
        pass
    finally:
        try:
            stream.close()
        except BaseException:
            pass


def verify(url: str, index: str, count: int, ids: list[str], phase: str) -> dict:
    counted = request(f"{url}/{index}/_count")
    actual = int(counted.get("count", -1))
    if actual != count:
        raise RuntimeError(f"{phase}: expected {count} documents, got {actual}")
    observed = {}
    for doc_id in ids:
        result = request(
            f"{url}/{index}/_search",
            "POST",
            {"size": 2, "query": {"term": {"doc_key": doc_id}}, "track_total_hits": True},
        )
        total = int(result["hits"]["total"]["value"])
        hits = [hit["_id"] for hit in result["hits"]["hits"]]
        if total != 1 or hits != [doc_id]:
            raise RuntimeError(f"{phase}: sentinel {doc_id} returned total={total}, ids={hits}")
        observed[doc_id] = hits
    return {"phase": phase, "count": actual, "sentinels": observed}


def bulk_load(url: str, index: str, docs: Path, count: int, batch_size: int, set_phase):
    batch: list[bytes] = []
    sent = 0
    with docs.open("rb") as source:
        for number, raw in enumerate(source):
            if number >= count:
                break
            action = json.dumps(
                {"index": {"_index": index, "_id": f"doc-{number:08d}"}},
                separators=(",", ":"),
            ).encode()
            batch.extend((action, raw.rstrip(b"\n")))
            if number % batch_size == batch_size - 1 or number + 1 == count:
                set_phase(f"bulk:{sent}-{number + 1}")
                response = request(f"{url}/_bulk", "POST", raw=b"\n".join(batch) + b"\n")
                if response.get("errors"):
                    errors = [
                        item for item in response.get("items", []) if next(iter(item.values())).get("error")
                    ]
                    raise RuntimeError(f"bulk errors: {json.dumps(errors[:3])}")
                sent = number + 1
                batch.clear()
    if sent != count:
        raise RuntimeError(f"corpus ended at {sent}, expected {count}")


def run_cell(
    binary: Path,
    corpus: Path,
    root: Path,
    count: int,
    batch: int,
    sample_ms: int,
    embed_mode: str,
):
    cell = root / f"docs-{count}"
    cell.mkdir(mode=0o700)
    data = cell / "data"
    data.mkdir(mode=0o700)
    index = "bounded-diagnostics"
    phase_lock = threading.Lock()
    phase_name = "boot"

    def phase(value=None):
        nonlocal phase_name
        with phase_lock:
            if value is not None:
                phase_name = value
            return phase_name

    timeline = []
    process, stream, url, trace, config = start_server(
        binary, cell, data, "ingest", sample_ms, embed_mode
    )
    try:
        with ProcSampler(process.pid, cell / "proc-ingest.ndjson", phase, sample_ms / 1000):
            phase("create")
            request(
                f"{url}/{index}",
                "PUT",
                {
                    "mappings": {
                        "properties": {
                            "doc_key": {"type": "keyword"},
                            "company": {"type": "keyword"},
                            "year": {"type": "long"},
                            "quarter": {"type": "keyword"},
                            "amount": {"type": "double"},
                            "body": {"type": "semantic_text"},
                            "labels": {"type": "keyword"},
                        }
                    }
                },
            )
            started = time.monotonic_ns()
            bulk_load(url, index, corpus / "documents.ndjson", count, batch, phase)
            timeline.append({"phase": "bulk", "elapsed_ns": time.monotonic_ns() - started})
            ids = ["doc-00000000", f"doc-{count // 2:08d}", f"doc-{count - 1:08d}"]
            phase("refresh")
            started = time.monotonic_ns()
            request(f"{url}/{index}/_refresh", "POST")
            checks = [verify(url, index, count, ids, "after_refresh")]
            timeline.append(
                {"phase": "refresh_and_verify", "elapsed_ns": time.monotonic_ns() - started}
            )
            phase("flush")
            started = time.monotonic_ns()
            flush = request(f"{url}/{index}/_flush", "POST")
            checks.append(verify(url, index, count, ids, "after_flush"))
            timeline.append(
                {"phase": "flush_and_verify", "elapsed_ns": time.monotonic_ns() - started}
            )
            phase("merge")
            started = time.monotonic_ns()
            merge = request(f"{url}/{index}/_forcemerge?max_num_segments=1", "POST")
            checks.append(verify(url, index, count, ids, "after_merge"))
            timeline.append(
                {"phase": "merge_and_verify", "elapsed_ns": time.monotonic_ns() - started}
            )
            phase("shutdown")
            started = time.monotonic_ns()
            stop_server(process, stream)
            timeline.append({"phase": "shutdown", "elapsed_ns": time.monotonic_ns() - started})
    finally:
        cleanup_server(process, stream)

    phase("restart")
    started = time.monotonic_ns()
    process2, stream2, url2, trace2, config2 = start_server(
        binary, cell, data, "restart", sample_ms, embed_mode
    )
    try:
        with ProcSampler(process2.pid, cell / "proc-restart.ndjson", phase, sample_ms / 1000):
            checks.append(verify(url2, index, count, ids, "after_restart"))
            timeline.append(
                {"phase": "restart_and_verify", "elapsed_ns": time.monotonic_ns() - started}
            )
            phase("restart_shutdown")
            started = time.monotonic_ns()
            stop_server(process2, stream2)
            timeline.append(
                {"phase": "restart_shutdown", "elapsed_ns": time.monotonic_ns() - started}
            )
    finally:
        cleanup_server(process2, stream2)
    for path in cell.iterdir():
        if path.is_file():
            path.chmod(0o600)
    return {
        "documents": count,
        "batch_size": batch,
        "checks": checks,
        "flush_response": flush,
        "merge_response": merge,
        "timeline": timeline,
        "artifacts": {
            path.name: {"bytes": path.stat().st_size, "sha256": hash_file(path)}
            for path in sorted(cell.iterdir())
            if path.is_file()
        },
    }


def git_identity(binary: Path) -> dict:
    try:
        root = subprocess.check_output(
            ["git", "-C", str(HERE), "rev-parse", "--show-toplevel"], text=True
        ).strip()
        return {
            "root": root,
            "commit": subprocess.check_output(
                ["git", "-C", root, "rev-parse", "HEAD"], text=True
            ).strip(),
            "dirty": bool(
                subprocess.check_output(["git", "-C", root, "status", "--porcelain"], text=True)
            ),
        }
    except (subprocess.CalledProcessError, FileNotFoundError):
        return {"root": None, "commit": None, "dirty": None}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--profile", choices=("ci", "nightly"), default="nightly")
    parser.add_argument("--documents", help="override profile with increasing comma-separated cells")
    parser.add_argument("--batch-size", type=int, default=128)
    parser.add_argument("--sample-ms", type=int, default=100)
    parser.add_argument("--embed-mode", default="lexical")
    args = parser.parse_args()
    profile_counts = {"ci": "64,256", "nightly": "256,1024,4096"}
    counts = [int(value) for value in (args.documents or profile_counts[args.profile]).split(",")]
    if sorted(set(counts)) != counts or len(counts) < 2 or counts[0] < 1:
        parser.error("--documents must contain at least two strictly increasing positive counts")
    if args.batch_size < 1:
        parser.error("--batch-size must be positive")
    if args.sample_ms < 1:
        parser.error("--sample-ms must be positive")
    args.binary = args.binary.resolve()
    args.corpus = args.corpus.resolve()
    if not os.access(args.binary, os.X_OK):
        parser.error(f"not executable: {args.binary}")
    corpus_manifest = json.loads((args.corpus / "corpus_manifest.json").read_text())
    if corpus_manifest["documents"] < counts[-1]:
        parser.error("corpus contains fewer documents than the largest cell")
    if hash_file(args.corpus / corpus_manifest["file"]) != corpus_manifest["sha256"]:
        parser.error("corpus hash does not match corpus_manifest.json")
    if args.output.exists():
        parser.error(f"output already exists: {args.output}")
    args.output.mkdir(parents=True, mode=0o700)
    args.output.chmod(0o700)
    started = time.time_ns()
    cells = [
        run_cell(
            args.binary,
            args.corpus,
            args.output,
            count,
            args.batch_size,
            args.sample_ms,
            args.embed_mode,
        )
        for count in counts
    ]
    manifest = {
        "schema": SCHEMA,
        "started_unix_ns": started,
        "finished_unix_ns": time.time_ns(),
        "binary": {
            "path": str(args.binary),
            "bytes": args.binary.stat().st_size,
            "sha256": hash_file(args.binary),
            "version": subprocess.check_output([str(args.binary), "--version"], text=True).strip(),
        },
        "suite_git": git_identity(args.binary),
        "host": {
            "platform": platform.platform(),
            "machine": platform.machine(),
            "python": platform.python_version(),
            "cpu_count": os.cpu_count(),
            "page_size": os.sysconf("SC_PAGE_SIZE"),
        },
        "configuration": {
            "documents": counts,
            "profile": args.profile,
            "batch_size": args.batch_size,
            "sample_ms": args.sample_ms,
            "embedding_mode": args.embed_mode,
            "instrumentation": "summary",
        },
        "corpus": corpus_manifest,
        "cells": cells,
    }
    (args.output / "run_manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    (args.output / "run_manifest.json").chmod(0o600)
    print(args.output / "run_manifest.json")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
