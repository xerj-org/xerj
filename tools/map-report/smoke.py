#!/usr/bin/env python3
"""Exercise #2643 on synthetic files and an isolated, temporary lexical node.

Optional --samples-dir copies the redacted graph reports there after validation.
No existing node/index or user corpus is read, indexed or deleted.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time
from urllib.request import build_opener, ProxyHandler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--samples-dir', type=Path)
    parser.add_argument('--runner', type=Path)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    source = args.binary.resolve(strict=True)
    runner = args.runner.resolve(strict=True) if args.runner else None
    samples = args.samples_dir.resolve() if args.samples_dir else None
    env = {k: v for k, v in os.environ.items() if not k.startswith('XERJ_')
           and k.lower() not in ('http_proxy', 'https_proxy', 'all_proxy', 'no_proxy')}
    env.update(XERJ_DISABLE_FEEDBACK='true', XERJ_NUM_CPUS='4')
    opener = build_opener(ProxyHandler({}))
    with tempfile.TemporaryDirectory(prefix='xerj-map-report-') as temp:
        root = Path(temp)
        binary = root / ('xerj.exe' if os.name == 'nt' else 'xerj')
        shutil.copyfile(source, binary)
        binary.chmod(0o700)
        print('Executable SHA-256:', hashlib.sha256(binary.read_bytes()).hexdigest(), flush=True)
        fixture = root / 'fixture'
        shutil.copytree(Path(__file__).parent / 'fixture', fixture)
        (fixture / 'unsupported.bin').write_bytes(b'\0\xff\0\xfe' * 32)
        config = root / 'node.toml'
        config.write_text('[server]\nauto_port = false\n\n[limits]\ndisk_flood_stage_percent = 99\n\n[rerank]\nenabled = false\n')
        env['XERJ_CONFIG'] = str(config)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        assert port <= 65533
        url = f'http://127.0.0.1:{port}'

        def run(command, accepted=(0,), timeout=180):
            result = subprocess.run([str(x) for x in command], cwd=root, env=env,
                                    capture_output=True, text=True, timeout=timeout)
            assert result.returncode in accepted, (command, result.returncode, result.stdout, result.stderr)
            return result

        def cli(*command, accepted=(0,)):
            return run([binary, *command, '--url', url], accepted=accepted)

        def map_command(prefix, *flags, accepted=(0,)):
            return cli('autoindex', 'map', '--prefix', prefix, *flags, accepted=accepted)

        print(run([binary, '--version']).stdout.strip(), flush=True)
        with (root / 'node.log').open('w') as log:
            node = subprocess.Popen([str(binary), '--insecure', '--port', str(port),
                                     '--data-dir', str(root / 'data'), '--embed-mode', 'lexical'],
                                    cwd=root, env=env, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 60
                while True:
                    assert node.poll() is None, (root / 'node.log').read_text()
                    try:
                        with opener.open(url + '/_cluster/health', timeout=2):
                            break
                    except OSError:
                        if time.monotonic() > deadline:
                            raise TimeoutError('node readiness')
                        time.sleep(0.1)
                if runner:
                    print(run([runner, '--url', url, '--dir', repo / 'engine/tests/es-compat-yaml/yaml'],
                              timeout=900).stdout, flush=True)
                preview = cli('autoindex', fixture, '--dry-run', '--no-semantic', '--no-graph',
                              '--state-dir', root / 'preview')
                print('Dry-run estimate:', preview.stdout + preview.stderr, flush=True)
                for prefix, no_graph in [('mapplain', True), ('mapfixture', False)]:
                    started = time.monotonic()
                    flags = ['--no-graph'] if no_graph else []
                    indexed = cli('autoindex', fixture, '--prefix', prefix, '--state-dir', root / prefix,
                                  '--no-semantic', '--workers', '2', '--yes', *flags, accepted=(0, 3))
                    lines = (indexed.stdout + indexed.stderr).splitlines()
                    for line in lines:
                        if line.startswith(('xerj-bar ', 'xerj-done ')):
                            print(line, flush=True)
                    assert any(line.startswith('xerj-done ok=true') for line in lines), lines
                    assert not any('datasets_refused=' in line for line in lines), lines
                    print(f'Indexed {prefix} in {time.monotonic() - started:.2f}s', flush=True)
                    before_json = json.loads(map_command(prefix, '--json').stdout)
                    before_text = map_command(prefix).stdout
                    paths = {}
                    for flag, ext in [('--html', 'html'), ('--md', 'md')]:
                        path = root / f'{prefix}.{ext}'
                        map_command(prefix, flag, path, '--redact-paths')
                        body = path.read_text()
                        assert 'ParcelStore' in body, body
                        assert 'Skipped and refused files' in body
                        assert 'path redacted' in body
                        assert str(root) not in body
                        assert 'unsupported.bin' not in body
                        assert 'xerj search --prefix' in body
                        assert body.count('Where is ') == 5
                        if no_graph:
                            assert 'Graph sections omitted' in body
                            assert 'Most-linked files' not in body
                        else:
                            assert 'Most-linked files' in body
                            assert 'Incoming live edges' in body
                            assert 'overview unavailable' not in body
                        if ext == 'html':
                            assert body.startswith('<!doctype html>')
                            assert '<script' not in body and 'src=' not in body and 'href=' not in body
                        else:
                            assert body.startswith('# XERJ data map\n')
                        original = path.read_bytes()
                        refused = map_command(prefix, flag, path, accepted=(1,))
                        assert 'already exists' in refused.stderr
                        assert path.read_bytes() == original
                        paths[ext] = path
                    raw_path = root / f'{prefix}-raw.html'
                    map_command(prefix, '--html', raw_path)
                    raw_body = raw_path.read_text()
                    assert 'unsupported.bin' in raw_body
                    assert '[reason omitted:' not in raw_body
                    selected = root / f'{prefix}-docs.html'
                    map_command(prefix, '--dataset', 'docs', '--html', selected, '--redact-paths')
                    assert f'{prefix}-docs' in selected.read_text()
                    assert f'{prefix}-data' not in selected.read_text()
                    assert json.loads(map_command(prefix, '--json').stdout) == before_json
                    assert map_command(prefix).stdout == before_text
                    missing = root / f'{prefix}-missing.html'
                    failure = map_command('missing-corpus', '--html', missing, accepted=(1,))
                    assert not missing.exists(), failure
                    print(f'PASS {prefix}: HTML/Markdown, raw/redacted paths, dataset scope, graph state, no clobber, unchanged map/JSON, empty-catalog refusal', flush=True)
                    if not no_graph and samples:
                        samples.mkdir(parents=True, exist_ok=True)
                        for ext, path in paths.items():
                            shutil.copyfile(path, samples / f'sample.{ext}')
                print('PASS isolated map report lifecycle', flush=True)
            finally:
                if node.poll() is None:
                    node.send_signal(signal.SIGTERM)
                try:
                    node.wait(timeout=30)
                except subprocess.TimeoutExpired:
                    node.kill()
                    node.wait()
                    raise
                assert node.returncode == 0, (root / 'node.log').read_text()


if __name__ == '__main__':
    main()
