#!/usr/bin/env python3
"""Synthetic session -> redacted JSONL -> autoindex -> cited decision, on a private node."""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[2]


def run(binary, arguments, env, work, expected=0):
    result = subprocess.run([str(binary), *arguments], env=env, cwd=work,
                            capture_output=True, text=True, timeout=90)
    if result.returncode != expected:
        raise RuntimeError(f'{arguments[0]} exit {result.returncode}: {result.stderr}')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--format', choices=('claude-code', 'codex'), default='claude-code')
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    env = {k: v for k, v in os.environ.items() if not k.startswith('XERJ_')
           and k.lower() not in ('all_proxy', 'http_proxy', 'https_proxy', 'no_proxy')}
    env.update(XERJ_DISABLE_FEEDBACK='true', XERJ_AUTH='', XERJ_NUM_CPUS='4')
    with tempfile.TemporaryDirectory(prefix='xerj-sessions-smoke-') as temp:
        work = Path(temp)
        source = work / 'session.jsonl'
        fixture = 'codex-session.jsonl' if args.format == 'codex' else 'claude-session.jsonl'
        source.write_bytes((ROOT / 'engine/crates/xerj-autoindex/tests/fixtures' / fixture).read_bytes())
        exports = work / 'export'
        exports.mkdir()
        output = exports / 'records.jsonl'
        command = ['sessions', 'import', '--format', args.format, '--input', str(source), '--output', str(output)]
        imported = json.loads(run(binary, command, env, work).stdout)
        assert imported['records'] == 2, imported
        safe = output.read_bytes()
        assert b'owner@example.com' not in safe and b'DO_NOT_EXPORT' not in safe
        run(binary, command, env, work, expected=1)
        assert output.read_bytes() == safe
        print('offline import: 2 redacted text turns; overwrite refused', flush=True)

        ports = []
        sockets = []
        for _ in range(2):
            sock = socket.socket()
            sock.bind(('127.0.0.1', 0))
            sockets.append(sock)
            ports.append(sock.getsockname()[1])
        for sock in sockets:
            sock.close()
        config = work / 'node.toml'
        config.write_text(f'[server]\nbind_address="127.0.0.1"\nes_compat_port={ports[0]}\n'
                          f'rest_port={ports[1]}\ngrpc_port=0\ndata_dir={json.dumps(str(work / "data"))}\n'
                          '[limits]\ndisk_flood_stage_percent=0\n')
        url = f'http://127.0.0.1:{ports[0]}'
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

        def request(path, body=None):
            req = urllib.request.Request(url + path,
                                         None if body is None else json.dumps(body).encode(),
                                         headers={'Content-Type': 'application/json'})
            with opener.open(req, timeout=10) as response:
                return json.load(response)

        def start(log):
            child = subprocess.Popen([str(binary), '--insecure', '-c', str(config)], cwd=work,
                                     env=env, stdout=log, stderr=log)
            deadline = time.monotonic() + 60
            try:
                while time.monotonic() < deadline:
                    if child.poll() is not None:
                        raise RuntimeError('private node exited before readiness')
                    try:
                        request('/')
                        return child
                    except OSError:
                        time.sleep(.1)
                raise RuntimeError('private node readiness timed out')
            except BaseException:
                stop(child)
                raise

        def stop(child):
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)
                    raise RuntimeError('private node required SIGKILL')
            assert child.returncode == 0, child.returncode

        with (work / 'node.log').open('w') as log:
            child = start(log)
            try:
                index = ['autoindex', str(exports), '--prefix', 'session-smoke', '--url', url,
                         '--state-dir', str(work / 'state'), '--no-graph', '--no-semantic',
                         '--workers', '2', '--max-minutes', '0']
                dry = run(binary, index + ['--dry-run'], env, work)
                for line in (dry.stdout + dry.stderr).splitlines():
                    if 'autoindex:' in line and 'files (' in line:
                        print(line, flush=True)
                for generation in range(2):
                    before = time.monotonic()
                    indexed = run(binary, index + ['--progress', 'json'], env, work)
                    done = []
                    for line in (indexed.stdout + indexed.stderr).splitlines():
                        if line.startswith('xerj-done'):
                            print(line, flush=True)
                        try:
                            event = json.loads(line)
                        except json.JSONDecodeError:
                            continue
                        if isinstance(event, dict) and event.get('bar'):
                            print(event['bar'], flush=True)
                        if isinstance(event, dict) and event.get('event') == 'done':
                            done.append(event)
                            print(line, flush=True)
                    assert len(done) == 1 and done[0]['ok'] is True and done[0]['exit'] == 0, done
                    assert not done[0].get('datasets_refused') and not done[0].get('files_refused'), done
                    count = request('/session-smoke-*/_count')['count']
                    assert count == 2, count
                    print(f'autoindex pass {generation + 1}: {time.monotonic() - before:.3f}s; {count} live documents', flush=True)
                result = request('/session-smoke-*/_search', {'size': 10, 'query': {'match_all': {}}})
                stored = result['hits']['hits']
                assert len(stored) == 2, result
                assert 'owner@example.com' not in json.dumps(stored)
                assert 'DO_NOT_EXPORT' not in json.dumps(stored)
                decision = next(h['_source'] for h in stored if 'durable journal' in h['_source']['body'])
                expected_line = 9 if args.format == 'codex' else 3
                expected_files = [] if args.format == 'codex' else ['src/watch.rs']
                assert decision['source_file'] == 'session.jsonl' and decision['source_line'] == expected_line, decision
                assert decision['files'] == expected_files, decision
                assert decision['session_format'] == args.format, decision
                search = run(binary, ['search', '--prefix', 'session-smoke', '--url', url,
                                      '--json', 'why did we drop polling'], env, work)
                assert 'durable journal' in search.stdout, search.stdout
                print(f'search: decision text found; original session.jsonl:{expected_line}; files={expected_files}', flush=True)
            finally:
                stop(child)
            child = start(log)
            try:
                assert request('/session-smoke-*/_count')['count'] == 2
                print('graceful restart: 2 documents preserved', flush=True)
            finally:
                stop(child)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
