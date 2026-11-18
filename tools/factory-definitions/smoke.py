#!/usr/bin/env python3
"""Real autoindex -> CLI/MCP def checks for separately exported factories.

python3 tools/factory-definitions/smoke.py --binary engine/target/ci-test/xerj
Only synthetic source and a temporary private node are used. No models needed.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import shutil
import socket
import subprocess
import tempfile
import time
from urllib.request import build_opener, ProxyHandler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--runner', type=Path)
    parser.add_argument('--estimate-only', action='store_true')
    args = parser.parse_args()
    source_binary = args.binary.resolve(strict=True)
    runner = str(args.runner.resolve(strict=True)) if args.runner else None
    repo = Path(__file__).resolve().parents[2]
    env = {k: v for k, v in os.environ.items() if not k.startswith('XERJ_')
           and k.lower() not in ('http_proxy', 'https_proxy', 'all_proxy', 'no_proxy')}
    env.update(XERJ_DISABLE_FEEDBACK='true', XERJ_NUM_CPUS='4')
    opener = build_opener(ProxyHandler({}))
    with tempfile.TemporaryDirectory(prefix='xerj-factory-defs-') as temp:
        root = Path(temp)
        snapshot = root / ('xerj.exe' if os.name == 'nt' else 'xerj')
        shutil.copyfile(source_binary, snapshot)
        snapshot.chmod(0o700)
        binary = str(snapshot)
        print(f'Executable snapshot SHA-256: {hashlib.sha256(snapshot.read_bytes()).hexdigest()}', flush=True)
        env['XERJ_CONFIG'] = str(root / 'node.toml')
        # Tiny isolated fixture: do not inherit a shared laptop's 95% reserve.
        # Leave 1% of the volume reserved; never alter another node's settings.
        (root / 'node.toml').write_text('[server]\nauto_port = false\n\n[limits]\ndisk_flood_stage_percent = 99\n')
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        url = f'http://127.0.0.1:{port}'
        source = root / 'source'
        source.mkdir()
        expected = []
        for ext, lang in [('ts', 'typescript'), ('tsx', 'tsx'), ('js', 'javascript')]:
            generic = '' if ext == 'js' else '<UserState>'
            names = [f'{ext}InlineStore', f'{ext}NamedStore', f'{ext}DefaultStore']
            (source / f'store.{ext}').write_text(
                f'export const {names[0]} = create{generic}()((set) => ({{ user: null }}));\n'
                f'const {names[1]} = createStore({{}});\n'
                f'export {{ {names[1]} as publicStore }};\n'
                f'const {names[2]} = styled.button`color: red;`;\n'
                f'export default {names[2]};\n'
                f'const {ext}PrivateStore = createStore({{}});\n')
            expected.extend((name, lang, f'store.{ext}', line)
                            for name, line in zip(names, [1, 2, 4]))

        def run(command, *, stdin=None, timeout=120):
            result = subprocess.run(command, input=stdin, text=True, capture_output=True,
                                    cwd=root, env=env, timeout=timeout)
            if result.returncode:
                raise AssertionError(f'{command}: exit {result.returncode}\n{result.stdout}\n{result.stderr}')
            if 'autoindex' in command:
                print(result.stderr, end='', flush=True)
            return result.stdout

        print(run([binary, '--version']), end='', flush=True)

        def check_definitions():
            for name, lang, filename, line in expected:
                cli = json.loads(run([binary, 'def', name, '--prefix', 'fx2579',
                                      '--url', url, '--kind', 'const', '--lang', lang, '--json']))
                messages = [
                    {'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {}},
                    {'jsonrpc': '2.0', 'id': 2, 'method': 'tools/call', 'params': {
                        'name': 'xerj_def', 'arguments': {'symbol': name, 'prefix': 'fx2579',
                                                        'kind': 'const', 'lang': lang}}},
                ]
                raw = run([binary, 'mcp', '--url', url],
                          stdin=''.join(json.dumps(m) + '\n' for m in messages))
                reply = next(json.loads(s)['result'] for s in raw.splitlines()
                             if json.loads(s).get('id') == 2)
                assert not reply.get('isError'), reply
                mcp = json.loads(reply['content'][0]['text'])
                assert cli['hits'] == mcp['hits'], (name, cli, mcp)
                hits = cli['hits']['hits']
                assert hits, (name, cli)
                hit = hits[0]['_source']
                assert hit['name'] == name and hit['kind'] == 'const', hit
                assert hit['line'] == line and hit['ax_path'].endswith(filename), hit
                assert name in hit['code'], hit
            print(f'PASS {len(expected)} definitions: CLI/MCP parity and declaration citations', flush=True)

        for phase in ['first-run', 'restart']:
            with (root / f'{phase}.log').open('w') as log:
                node = subprocess.Popen([binary, '--insecure', '--data-dir', str(root / 'data'),
                                         '--port', str(port), '--embed-mode', 'lexical'],
                                        cwd=root, env=env, stdout=log, stderr=log)
                try:
                    deadline = time.monotonic() + 60
                    while True:
                        assert node.poll() is None, (root / f'{phase}.log').read_text()
                        try:
                            with opener.open(url, timeout=1) as response:
                                json.load(response)
                            break
                        except OSError:
                            if time.monotonic() >= deadline:
                                raise TimeoutError('private node did not become ready')
                            time.sleep(0.1)
                    if phase == 'first-run':
                        if runner:
                            print(run([runner, '--url', url, '--dir', str(repo / 'engine/tests/es-compat-yaml/yaml')], timeout=900), flush=True)
                        command = [binary, 'autoindex', str(source), '--url', url, '--prefix', 'fx2579',
                                   '--state-dir', str(root / 'state'), '--no-graph', '--no-semantic', '--yes']
                        print(run([*command, '--dry-run']), flush=True)
                        if args.estimate_only:
                            return
                        print(run(command), flush=True)
                        check_definitions()
                        print(run(command), flush=True)
                    check_definitions()
                finally:
                    if node.poll() is None:
                        node.send_signal(signal.SIGTERM)
                    try:
                        node.wait(timeout=30)
                    except subprocess.TimeoutExpired:
                        node.kill()
                        node.wait()
                        raise
                    assert node.returncode == 0, (root / f'{phase}.log').read_text()
        print('PASS first index, unchanged rerun, graceful stop and durable restart', flush=True)


if __name__ == '__main__':
    main()
