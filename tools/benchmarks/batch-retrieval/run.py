#!/usr/bin/env python3
"""Pinned rust-vulns retrieval comparison; no model calls or answer generation."""
import argparse
import contextlib
import hashlib
import importlib.metadata
import io
import json
import os
from pathlib import Path
import platform
import queue
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
import zipfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
INDEX = 'advisories'
# Hold evidence policy fixed across all four arms, independent of adapter defaults.
MAX_TOKENS = 1_000_000
PACK_SHA256 = '54f0419e6d944635eeaf9199923064278c36fd578213db60ca77cc85d2a69e3d'
PACK_TAG = 'pack-rust-vulns-2026-10-24'


def wire(value):
    return json.dumps(value, ensure_ascii=False, separators=(',', ':')) + '\n'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def freeze_binary(data, work):
    binary = work / 'xerj'
    binary.write_bytes(data)
    binary.chmod(0o700)
    return binary


def read_pack(path):
    # Read once: both the digest and records refer to these exact bytes. No extraction.
    data = path.read_bytes()
    if sha(data) != PACK_SHA256:
        raise ValueError('pack SHA256 mismatch; use the pinned release in README.md')
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        manifest = json.loads(z.read('manifest.json'))
        if manifest['format_version'] != 1 or manifest['pack'] != 'rust-vulns':
            raise ValueError('unsupported pack manifest')
        records = []
        for name, entry in sorted(manifest['files'].items()):
            blob = z.read(name)
            if sha(blob) != entry['sha256']:
                raise ValueError('manifest checksum mismatch: ' + name)
            if name.startswith('records-') and name.endswith('.jsonl'):
                records.extend(json.loads(line) for line in blob.splitlines())
    if len(records) != manifest['counts']['records']:
        raise ValueError('pack record count mismatch')
    ids = [r['id'] for r in records]
    if len(set(ids)) != len(ids) or not all(ids):
        raise ValueError('pack has empty or duplicate canonical IDs')
    return records, manifest


def validate_cases(cases, records):
    if len(cases) != 50:
        raise ValueError('expected the committed 50-item workload')
    ids = {r['id'] for r in records}
    for case in cases:
        expected = set(case['expected_ids'])
        if not expected <= ids:
            raise ValueError('label names an absent canonical ID')
        if case['kind'] in ('canonical', 'alias', 'missing', 'duplicate'):
            item = case['input']
            actual = {r['id'] for r in records
                      if item['identifier'] in ([r[item['field']]] if item['field'] == 'id'
                                                else r[item['field']])}
            if expected != actual:
                raise ValueError('exact-ID gold differs from pack: ' + item['key'])


def ordered_rows(response, items):
    if '_token_budget' in response:
        raise ValueError('benchmark requires full evidence; response budget bound')
    rows = response.get('results')
    if not isinstance(rows, list) or len(rows) != len(items):
        raise ValueError('response count mismatch')
    for position, (row, item) in enumerate(zip(rows, items)):
        if row.get('position') != position or row.get('key') != item['key']:
            raise ValueError('response position/key mismatch')
    return rows


def score(case, row):
    status = row.get('status')
    retrieved, citations = [], []
    if status == 'hits':
        if row.get('abstain') is not False or not row.get('hits'):
            raise ValueError('malformed hits outcome')
        for entry in row['hits']:
            citation, hit = entry['citation'], entry['hit']
            if (citation.get('_index') != INDEX or citation.get('_id') != hit['_id']
                    or hit['_index'] != INDEX or hit['_source']['id'] != hit['_id']):
                raise ValueError('citation does not identify the stored advisory')
            retrieved.append(hit['_id'])
            citations.append(citation)
    elif status == 'abstain':
        if row.get('abstain') is not True or row.get('reason') != 'no_hits' or row.get('hits'):
            raise ValueError('malformed abstention')
    elif status == 'error':
        if 'error' not in row:
            raise ValueError('missing item error')
    else:
        raise ValueError('unknown outcome')
    expected = set(case['expected_ids'])
    if case['kind'] == 'invalid':
        correct = status == 'error'
    elif not expected:
        correct = status == 'abstain'
    elif case['kind'] == 'text':
        correct = status == 'hits' and bool(expected.intersection(retrieved))
    else:
        correct = status == 'hits' and set(retrieved) == expected
    return {'key': case['input']['key'], 'kind': case['kind'], 'status': status,
            'expected_ids': sorted(expected), 'retrieved_ids': retrieved,
            'citations': citations, 'correct': correct,
            **({'error': row['error']} if status == 'error' else {})}


def evidence_signature(rows):
    # Compare ranked identity + citation, not volatile timing or local positions.
    return [(r['status'], [(h['citation'], h['hit']['_source']) for h in r.get('hits', [])],
             r.get('error'), r.get('reason')) for r in rows]


def request(url, method, path, value=None, ndjson=False):
    data = (value.encode() if ndjson else wire(value).encode()) if value is not None else None
    req = urllib.request.Request(url + path, data, method=method,
                                 headers={'Content-Type': 'application/x-ndjson' if ndjson
                                          else 'application/json'})
    # This harness only talks to its own loopback node; never inherit proxy routing.
    with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(req, timeout=30) as r:
        return json.load(r)


def child_env():
    env = {k: v for k, v in os.environ.items()
           if k.lower() not in ('all_proxy', 'http_proxy', 'https_proxy', 'no_proxy')
           and not k.startswith('XERJ_')}
    env.update(XERJ_DISABLE_FEEDBACK='true', XERJ_AUTH='', XERJ_NUM_CPUS='4')
    return env


def hardware():
    if platform.system() == 'Darwin':
        return {'cpu_model': subprocess.check_output(['sysctl', '-n', 'machdep.cpu.brand_string'], text=True).strip(),
                'memory_bytes': int(subprocess.check_output(['sysctl', '-n', 'hw.memsize'], text=True))}
    if platform.system() == 'Linux':
        cpu = next((line.split(':', 1)[1].strip() for line in Path('/proc/cpuinfo').read_text().splitlines()
                    if line.startswith('model name')), platform.machine())
        return {'cpu_model': cpu,
                'memory_bytes': os.sysconf('SC_PHYS_PAGES') * os.sysconf('SC_PAGE_SIZE')}
    raise ValueError('this harness supports macOS and Linux')


def stop(child):
    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=20)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=5)
            raise RuntimeError('child required SIGKILL')
    if child.returncode != 0:
        raise RuntimeError('child exited with status ' + str(child.returncode))


@contextlib.contextmanager
def node(binary, work, env):
    # Reserve distinct ports during config construction, then release for startup.
    with contextlib.ExitStack() as stack:
        ports = []
        for _ in range(2):
            sock = stack.enter_context(socket.socket())
            sock.bind(('127.0.0.1', 0))
            ports.append(sock.getsockname()[1])
    url = 'http://127.0.0.1:' + str(ports[0])
    config = work / 'node.toml'
    config.write_text('[server]\nbind_address="127.0.0.1"\nes_compat_port=' + str(ports[0])
                      + '\nrest_port=' + str(ports[1]) + '\ngrpc_port=0\ndata_dir='
                      + json.dumps(str(work / 'data'))
                      + '\n[limits]\ndisk_flood_stage_percent=0\n')
    with (work / 'node.log').open('a') as log:
        child = subprocess.Popen([str(binary), '--insecure', '-c', str(config)],
                                 cwd=work, env=env, stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 60
            while True:
                if child.poll() is not None:
                    raise RuntimeError('node exited before readiness; see node.log')
                try:
                    request(url, 'GET', '/')
                    break
                except OSError:
                    if time.monotonic() >= deadline:
                        raise RuntimeError('node readiness timeout')
                    time.sleep(.1)
            yield url
        finally:
            stop(child)


def ingest(url, records):
    request(url, 'PUT', '/' + INDEX, {'mappings': {'dynamic': False, 'properties': {
        'id': {'type': 'keyword'}, 'aliases': {'type': 'keyword'},
        'body': {'type': 'text'}, 'title': {'type': 'text'},
        'ax_path': {'type': 'keyword'}, 'ax_locator': {'type': 'keyword'}}}})
    for start in range(0, len(records), 100):
        lines = []
        for r in records[start:start + 100]:
            lines.extend([wire({'index': {'_index': INDEX, '_id': r['id']}}),
                          wire(dict(r, ax_path='rust-vulns/records', ax_locator=r['id']))])
        result = request(url, 'POST', '/_bulk', ''.join(lines), ndjson=True)
        if result.get('errors') is not False:
            raise ValueError('bulk item errors: ' + wire(result))
    request(url, 'POST', '/' + INDEX + '/_refresh')
    if request(url, 'GET', '/' + INDEX + '/_count')['count'] != len(records):
        raise ValueError('indexed count differs from pack')


class MCP:
    def __init__(self, binary, url, work, env):
        self.log = (work / 'mcp.log').open('w')
        self.child = subprocess.Popen([str(binary), 'mcp', '--url', url], cwd=work,
                                      env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                      stderr=self.log, text=True)
        self.inbox = queue.Queue()
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()
        self.sequence = 0
        try:
            self.rpc('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {},
                                   'clientInfo': {'name': 'batch-retrieval-eval', 'version': '1'}})
        except BaseException:
            self.close()
            raise

    def read(self):
        for line in self.child.stdout:
            self.inbox.put(line)
        self.inbox.put(None)

    def rpc(self, method, params):
        self.sequence += 1
        sent = wire({'jsonrpc': '2.0', 'id': self.sequence, 'method': method, 'params': params})
        self.child.stdin.write(sent)
        self.child.stdin.flush()
        try:
            received = self.inbox.get(timeout=45)
        except queue.Empty:
            raise RuntimeError('MCP response deadline exceeded')
        if received is None:
            raise RuntimeError('MCP stdout closed unexpectedly')
        reply = json.loads(received)
        if reply.get('id') != self.sequence or 'error' in reply:
            raise ValueError('MCP response ID/error: ' + received)
        return sent, received, reply['result']

    def call(self, args):
        start = time.perf_counter_ns()
        sent, received, result = self.rpc('tools/call', {'name': 'xerj_batch_search', 'arguments': args})
        elapsed = (time.perf_counter_ns() - start) / 1e6
        if result.get('isError'):
            raise ValueError('batch-wide MCP error: ' + wire(result))
        return sent, received, json.loads(result['content'][0]['text']), elapsed

    def close(self):
        self.child.stdin.close()
        try:
            self.child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            stop(self.child)
        self.child.stdout.close()
        self.log.close()
        self.reader.join(timeout=1)
        if self.child.returncode != 0:
            raise RuntimeError('MCP exit ' + str(self.child.returncode))


def cli_call(binary, url, work, env, args):
    sent = wire(args)
    start = time.perf_counter_ns()
    result = subprocess.run([str(binary), 'batch-search', '--input', '-', '--url', url],
                            input=sent, capture_output=True, text=True, cwd=work, env=env, timeout=45)
    elapsed = (time.perf_counter_ns() - start) / 1e6
    response = json.loads(result.stdout) if result.stdout else {}
    rows = ordered_rows(response, args['queries'])
    expected_exit = 3 if any(r['status'] == 'error' for r in rows) else 0
    if result.returncode != expected_exit:
        raise ValueError('CLI failed: ' + result.stderr)
    return sent, result.stdout, response, elapsed


def trial(call, cases, batch, encoding):
    groups = [cases] if batch else [[c] for c in cases]
    calls, rows = [], []
    # No scoring, hashing or tokenization inside the timed task loop.
    start = time.perf_counter_ns()
    captures = [call({'index': INDEX, 'size': 3, 'max_tokens': MAX_TOKENS, 'queries': [c['input'] for c in group]})
                for group in groups]
    elapsed = (time.perf_counter_ns() - start) / 1e6
    for group, (sent, received, response, call_ms) in zip(groups, captures):
        rows.extend(ordered_rows(response, [c['input'] for c in group]))
        calls.append({'elapsed_ms': call_ms, 'request_bytes': len(sent.encode()),
                      'response_bytes': len(received.encode()),
                      'input_bpe_tokens': len(encoding.encode(sent, disallowed_special=())),
                      'output_bpe_tokens': len(encoding.encode(received, disallowed_special=())),
                      'request_sha256': sha(sent.encode()), 'response_sha256': sha(received.encode())})
    scores = [score(c, r) for c, r in zip(cases, rows)]
    return {'task_elapsed_ms': elapsed, 'calls': calls, 'items': scores}, rows


def summarize(trials):
    first = trials[0]['items']
    by_kind = {kind: {'items': sum(i['kind'] == kind for i in first),
                      'correct': sum(i['correct'] for i in first if i['kind'] == kind)}
               for kind in sorted({i['kind'] for i in first})}
    return {'by_kind_first_trial': by_kind,
            'retrieval_items_per_trial': sum(i['kind'] != 'invalid' for i in first),
            'retrieval_correct_per_trial': [sum(i['correct'] for i in t['items']
                                               if i['kind'] != 'invalid') for t in trials],
            'task_ms_median': statistics.median(t['task_elapsed_ms'] for t in trials),
            'task_ms_min': min(t['task_elapsed_ms'] for t in trials),
            'task_ms_max': max(t['task_elapsed_ms'] for t in trials),
            'tool_calls_per_trial': len(trials[0]['calls']),
            **{key + '_per_trial': [sum(c[key] for c in t['calls']) for t in trials]
               for key in ('input_bpe_tokens', 'output_bpe_tokens', 'request_bytes', 'response_bytes')},
            'correct_items_per_trial': [sum(i['correct'] for i in t['items']) for t in trials],
            'abstain_items_per_trial': [sum(i['status'] == 'abstain' for i in t['items']) for t in trials],
            'error_items_per_trial': [sum(i['status'] == 'error' for i in t['items']) for t in trials]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pack', type=Path, required=True)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--engine-commit', required=True, help='source commit used to build --binary (operator declaration)')
    parser.add_argument('--output', type=Path, required=True, help='new directory; refuses existing paths')
    parser.add_argument('--repeats', type=int, default=5)
    args = parser.parse_args()
    if args.repeats < 2:
        parser.error('--repeats must be at least 2')
    if args.output.exists():
        parser.error('--output already exists; choose a new directory (nothing overwritten)')
    # All input and dependencies are validated before node startup or output publication.
    import tiktoken
    encoding = tiktoken.get_encoding('cl100k_base')
    records, manifest = read_pack(args.pack)
    case_bytes = (HERE / 'cases.json').read_bytes()
    cases = json.loads(case_bytes)
    validate_cases(cases, records)
    binary_data = args.binary.resolve(strict=True).read_bytes()
    binary_sha = sha(binary_data)
    env = child_env()
    host = hardware()
    args.output.mkdir(parents=True, exist_ok=False)
    report = {'schema_version': 1, 'date_utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
              'binary_sha256': binary_sha,
              'engine_source_commit_declared': args.engine_commit,
              'harness_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
              'harness_dirty': bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT)),
              'platform': platform.platform(), 'cpu': platform.processor(), 'logical_cpus': os.cpu_count(),
              'hardware': host,
              'python': platform.python_version(), 'xerj_num_cpus': 4,
              'tokenizer': {'package': 'tiktoken', 'version': importlib.metadata.version('tiktoken'),
                            'encoding': encoding.name, 'scope': 'exact CLI stdin/stdout or MCP tools/call JSON lines'},
              'pack_tag': PACK_TAG, 'pack_sha256': PACK_SHA256, 'manifest': manifest,
              'cases_sha256': sha(case_bytes), 'repeats': args.repeats,
              'protocol': {'concurrency': 1, 'warmups_per_arm': 1, 'cache': 'warm; no cache flush',
                           'order': 'alternate batch/single by repeat; CLI then MCP',
                           'baseline': 'same xerj_batch_search adapter, one-item envelope per call',
                           'indexing': 'full pack; explicit keyword id/aliases, text title/body; dynamic=false',
                           'node': 'fresh private loopback node; lexical default; no model or reranker',
                           'cli': 'process startup included per call',
                           'mcp': 'one persistent session; initialize excluded', 'size': 3,
                           'version': 2, 'max_tokens': MAX_TOKENS,
                           'budget_tokenizer': 'standard-uax29-words',
                           'budget_policy': 'fail if response budgeting binds; require full evidence'}, 'arms': {}}
    with tempfile.TemporaryDirectory(prefix='xerj-batch-retrieval-') as temp:
        work = Path(temp)
        try:
            # Execute the same private byte snapshot we hashed, even if Cargo
            # or another operator replaces the original executable during a run.
            binary = freeze_binary(binary_data, work)
            report['binary_version'] = subprocess.check_output(
                [str(binary), '--version'], text=True, cwd=work, env=env).strip()
            with node(binary, work, env) as url:
                ingest(url, records)
                reference = None
                for transport in ('cli', 'mcp'):
                    mcp = MCP(binary, url, work, env) if transport == 'mcp' else None
                    try:
                        call = mcp.call if mcp else lambda a: cli_call(binary, url, work, env, a)
                        trials = {'batch': [], 'single': []}
                        for repeat in range(-1, args.repeats):
                            for mode in (('batch', 'single') if repeat % 2 == 0 else ('single', 'batch')):
                                result, rows = trial(call, cases, mode == 'batch', encoding)
                                signature = evidence_signature(rows)
                                if reference is None:
                                    reference = signature
                                elif signature != reference:
                                    raise ValueError('ranked evidence/citation/outcome parity failure')
                                if repeat >= 0:
                                    trials[mode].append(result)
                                    print(transport, mode, repeat + 1, 'correct',
                                          sum(i['correct'] for i in result['items']), '/', len(cases), flush=True)
                        for mode, values in trials.items():
                            report['arms'][transport + '_' + mode] = {'summary': summarize(values), 'trials': values}
                    finally:
                        if mcp:
                            mcp.close()
                report['final_document_count'] = request(url, 'GET', '/' + INDEX + '/_count')['count']
                if report['final_document_count'] != len(records):
                    raise ValueError('benchmark changed document count')
            with node(binary, work, env) as url:
                report['restart_document_count'] = request(url, 'GET', '/' + INDEX + '/_count')['count']
                if report['restart_document_count'] != len(records):
                    raise ValueError('document count changed across graceful restart')
        finally:
            for name in ('node.log', 'mcp.log'):
                if (work / name).exists():
                    (args.output / name).write_bytes((work / name).read_bytes())
    report['quality_pass'] = all(i['correct'] for arm in report['arms'].values()
                                 for t in arm['trials'] for i in t['items'])
    report['parity_pass'] = True
    report['graceful_shutdown'] = True
    staging = args.output / 'results.json.tmp'
    staging.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    staging.replace(args.output / 'results.json')
    print(json.dumps({k: a['summary'] for k, a in report['arms'].items()}, indent=2))
    return 0 if report['quality_pass'] else 1


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (ValueError, RuntimeError, OSError, subprocess.SubprocessError) as error:
        print('error:', error, file=sys.stderr)
        sys.exit(1)
