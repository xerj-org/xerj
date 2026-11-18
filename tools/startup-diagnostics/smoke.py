#!/usr/bin/env python3
"""Private-node creation, durable write, restart and data-directory diagnostics."""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    binary = parser.parse_args().binary.resolve(strict=True)
    env = {k: v for k, v in os.environ.items() if not k.startswith('XERJ_')
           and k.lower() not in ('all_proxy', 'http_proxy', 'https_proxy', 'no_proxy')}
    env.update(XERJ_DISABLE_FEEDBACK='true', XERJ_NUM_CPUS='2', XERJ_LOG='info')
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with tempfile.TemporaryDirectory(prefix='xerj-data-dir-smoke-') as temp:
        work = Path(temp)
        sockets = [socket.socket(), socket.socket()]
        for sock in sockets:
            sock.bind(('127.0.0.1', 0))
        port, rest = [sock.getsockname()[1] for sock in sockets]
        for sock in sockets:
            sock.close()
        url = f'http://127.0.0.1:{port}'
        cfg = work / 'node.toml'
        cfg.write_text(f'[server]\nes_compat_port={port}\nrest_port={rest}\ngrpc_port=0\ndata_dir="persistent"\n'
                       '[logging]\nformat="json"\n')

        def request(path, method='GET', body=None):
            data = None if body is None else json.dumps(body).encode()
            with opener.open(urllib.request.Request(url + path, data=data, method=method,
                             headers={'Content-Type': 'application/json'}), timeout=10) as response:
                return json.load(response)

        for generation in range(2):
            log_path = work / f'node-{generation}.log'
            with log_path.open('w') as log:
                node = subprocess.Popen([str(binary), '--insecure', '-c', str(cfg)], cwd=work,
                                        env=env, stdout=log, stderr=log)
                try:
                    deadline = time.monotonic() + 60
                    while True:
                        if node.poll() is not None:
                            raise RuntimeError(log_path.read_text())
                        try:
                            request('/')
                            break
                        except OSError:
                            if time.monotonic() >= deadline:
                                raise RuntimeError('private node readiness timed out')
                            time.sleep(.1)
                    if generation == 0:
                        request('/directory-smoke', 'PUT', {'mappings': {'properties': {'body': {'type': 'text'}}}})
                        request('/directory-smoke/_doc/decision?refresh=true', 'PUT', {'body': 'retain this decision'})
                    assert request('/directory-smoke/_count')['count'] == 1
                    assert request('/directory-smoke/_doc/decision')['_source']['body'] == 'retain this decision'
                finally:
                    if node.poll() is None:
                        node.terminate()
                        try:
                            node.wait(timeout=20)
                        except subprocess.TimeoutExpired:
                            node.kill()
                            node.wait(timeout=5)
                            raise RuntimeError('graceful stop required SIGKILL')
                    assert node.returncode == 0, node.returncode
            text = log_path.read_text()
            assert 'config file (--config)' in text
            assert str((work / 'persistent').resolve()) in text
            expected = 'created this launch' if generation == 0 else 'existing directory'
            assert expected in text
            structured = [json.loads(line) for line in text.splitlines() if line.startswith('{')]
            diagnostic = next(event['fields'] for event in structured if event.get('fields', {}).get('message') == 'server data directory')
            assert diagnostic['data_dir_state'].startswith(expected), diagnostic
            assert diagnostic['data_dir_source'].startswith('config file (--config)'), diagnostic
            print(f'PASS node {generation + 1}: {expected}; one durable document; JSON tracing; graceful exit 0', flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
