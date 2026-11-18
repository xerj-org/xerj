#!/usr/bin/env python3
"""Synthetic Perl code-cap boundaries on a private node; no existing data is contacted."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
from urllib.request import build_opener, ProxyHandler, Request

CAP = 2 << 20


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--estimate-only', action='store_true')
    args = parser.parse_args()
    original = args.binary.resolve(strict=True)
    env = {k: v for k, v in os.environ.items() if not k.startswith('XERJ_')
           and k.lower() not in ('http_proxy', 'https_proxy', 'all_proxy', 'no_proxy')}
    env.update(XERJ_DISABLE_FEEDBACK='true', XERJ_NUM_CPUS='4')
    opener = build_opener(ProxyHandler({}))
    with tempfile.TemporaryDirectory(prefix='xerj-code-cap-') as temp:
        root = Path(temp)
        binary = root / ('xerj.exe' if os.name == 'nt' else 'xerj')
        shutil.copyfile(original, binary)
        binary.chmod(0o700)
        print('Executable SHA-256:', hashlib.sha256(binary.read_bytes()).hexdigest(), flush=True)
        source = root / 'source'
        source.mkdir()
        for name, size in [('below', CAP - 1), ('at', CAP), ('over', CAP + 1)]:
            head = f'sub {name}_probe {{ return 1; }}\n#'
            tail = f'\nsub {name}_tail {{ return 2; }}\n'
            body = head + 'x' * (size - len(head) - len(tail)) + tail
            assert len(body.encode()) == size
            (source / f'{name}.pl').write_bytes(body.encode())
        sockets = [socket.socket(), socket.socket()]
        for sock in sockets:
            sock.bind(('127.0.0.1', 0))
        port, rest = [sock.getsockname()[1] for sock in sockets]
        for sock in sockets:
            sock.close()
        url = f'http://127.0.0.1:{port}'
        config = root / 'node.toml'
        config.write_text(f'[server]\nes_compat_port={port}\nrest_port={rest}\ngrpc_port=0\n'
                          f'data_dir={json.dumps(str(root / "data"))}\n'
                          '[limits]\ndisk_flood_stage_percent=99\n')

        def run(command, expected=0):
            result = subprocess.run([str(binary), *command], cwd=root, env=env,
                                    capture_output=True, text=True, timeout=180)
            if result.returncode != expected:
                raise AssertionError(f'{command[0]} exit {result.returncode}: {result.stdout}\n{result.stderr}')
            return result

        def request(path, body=None):
            req = Request(url + path, None if body is None else json.dumps(body).encode(),
                          headers={'Content-Type': 'application/json'})
            with opener.open(req, timeout=15) as response:
                return json.load(response)

        def verify():
            query = {'size': 10, 'query': {'bool': {'filter': [
                {'term': {'prefix': 'code-cap-test'}}, {'term': {'doc_kind': 'file'}}]}}}
            files = [hit['_source'] for hit in request('/autoindex-catalog/_search', query)['hits']['hits']]
            assert len(files) == 3, files
            skipped = next(item for item in files if item['path'].endswith('over.pl'))
            assert skipped['status'] == 'skipped' and skipped['records'] == 0, skipped
            assert skipped['bytes'] == CAP + 1, skipped
            reason = skipped['reason']
            assert f'{CAP + 1} bytes' in reason and f'{CAP} bytes' in reason, reason
            assert '--max-file-mb' in reason and 'does not raise' in reason, reason
            assert 'too large to be hand-written' not in reason, reason
            for name in ('below', 'at'):
                for suffix in ('probe', 'tail'):
                    result = json.loads(run(['def', f'{name}_{suffix}', '--prefix', 'code-cap-test',
                                             '--url', url, '--lang', 'perl', '--json']).stdout)
                    hits = result['hits']['hits']
                    assert hits and hits[0]['_source']['name'] == f'{name}_{suffix}', result
            absent = json.loads(run(['def', 'over_probe', '--prefix', 'code-cap-test',
                                     '--url', url, '--lang', 'perl', '--json']).stdout)
            assert not absent['hits']['hits'], absent
            count = request('/code-cap-test-*/_count')['count']
            assert count > 0
            print(f'PASS catalog: over.pl skipped, 0 records, exact bytes; 4 accepted definitions; {count} live records', flush=True)
            return count

        command = ['autoindex', str(source), '--url', url, '--prefix', 'code-cap-test',
                   '--state-dir', str(root / 'state'), '--no-graph', '--no-semantic',
                   '--max-file-mb', '6', '--workers', '2', '--max-minutes', '0']
        initial_count = None
        for phase in ('first', 'restart'):
            with (root / f'{phase}.log').open('w') as log:
                node = subprocess.Popen([str(binary), '--insecure', '-c', str(config)],
                                        cwd=root, env=env, stdout=log, stderr=log)
                try:
                    deadline = time.monotonic() + 60
                    while True:
                        assert node.poll() is None, 'private node exited before readiness'
                        try:
                            request('/')
                            break
                        except OSError:
                            if time.monotonic() >= deadline:
                                raise TimeoutError('private-node readiness timeout')
                            time.sleep(.1)
                    if phase == 'first':
                        dry = run(command + ['--dry-run'])
                        for line in (dry.stdout + dry.stderr).splitlines():
                            if 'autoindex:' in line and 'files (' in line:
                                print(line, flush=True)
                        if args.estimate_only:
                            return
                        for generation in range(2):
                            result = run(command + ['--progress', 'json'], expected=3)
                            done = []
                            for line in (result.stdout + result.stderr).splitlines():
                                try:
                                    event = json.loads(line)
                                except json.JSONDecodeError:
                                    continue
                                if isinstance(event, dict) and event.get('bar'):
                                    print(event['bar'], flush=True)
                                if isinstance(event, dict) and event.get('event') == 'done':
                                    done.append(event)
                                    print(line, flush=True)
                            assert len(done) == 1 and done[0]['ok'] is True and done[0]['exit'] == 3, done
                            assert done[0]['code_files'] == 3 and done[0]['code_files_indexed'] == 2 and done[0]['code_files_junked'] == 1, done
                            count = verify()
                            if initial_count is None:
                                initial_count = count
                            assert count == initial_count
                            print(f'PASS index/rerun {generation + 1}: exit 3 is completed-with-junk, skipped source stays explicit', flush=True)
                    else:
                        assert verify() == initial_count
                        print('PASS durable restart: unchanged catalog and record count', flush=True)
                finally:
                    if node.poll() is None:
                        node.terminate()
                        try:
                            node.wait(timeout=30)
                        except subprocess.TimeoutExpired:
                            node.kill()
                            node.wait()
                            raise
                    assert node.returncode == 0, node.returncode
                    print('private node exit 0', flush=True)


if __name__ == '__main__':
    main()
