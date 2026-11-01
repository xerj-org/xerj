#!/usr/bin/env python3
"""Exercise installed session hooks against a private node and synthetic transcript only."""
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
from urllib.request import Request, build_opener, ProxyHandler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--authenticated', action='store_true', help='use the node default API-key authentication')
    args = parser.parse_args()
    original = args.binary.resolve(strict=True)
    env = {k: v for k, v in os.environ.items() if not k.startswith('XERJ_')
           and k.lower() not in ('http_proxy', 'https_proxy', 'all_proxy', 'no_proxy')}
    env.update(XERJ_DISABLE_FEEDBACK='true', XERJ_NUM_CPUS='2')
    opener = build_opener(ProxyHandler({}))
    with tempfile.TemporaryDirectory(prefix='xerj-session-memory-') as temporary:
        work = Path(temporary).resolve()
        binary = work / "xerj's binary"
        shutil.copyfile(original, binary)
        binary.chmod(0o700)
        print('Executable SHA-256:', hashlib.sha256(binary.read_bytes()).hexdigest(), flush=True)
        project = work / "project's work"
        project.mkdir()
        claude = project / '.claude'
        claude.mkdir()
        settings = claude / 'settings.local.json'
        foreign = {'permissions': {'allow': ['Read']}, 'hooks': {'Stop': [
            {'hooks': [{'type': 'command', 'command': 'echo foreign-hook'}]}]}}
        settings.write_text(json.dumps(foreign))
        state = claude / 'xerj-memory'
        sockets = [socket.socket(), socket.socket()]
        for sock in sockets:
            sock.bind(('127.0.0.1', 0))
        port, rest = [sock.getsockname()[1] for sock in sockets]
        for sock in sockets:
            sock.close()
        url = f'http://127.0.0.1:{port}'
        node_config = work / 'node.toml'
        node_config.write_text(f'[server]\nes_compat_port={port}\nrest_port={rest}\ngrpc_port=0\n'
                               f'data_dir={json.dumps(str(work / "data"))}\n'
                               '[limits]\ndisk_flood_stage_percent=99\n')
        sid = '550e8400-e29b-41d4-a716-446655440000'
        transcript = work / 'session.jsonl'
        rows = [
            {'type': 'user', 'sessionId': sid, 'uuid': '550e8400-e29b-41d4-a716-446655440001',
             'message': {'role': 'user', 'content': 'How do we recover? Contact owner@example.com.'}},
            {'type': 'assistant', 'sessionId': sid, 'uuid': '550e8400-e29b-41d4-a716-446655440002',
             'message': {'role': 'assistant', 'content': [
                 {'type': 'thinking', 'thinking': 'DO_NOT_EXPORT_THINKING'},
                 {'type': 'text', 'text': 'We chose snapshots because the journal preserves recovery.'},
                 {'type': 'tool_use', 'name': 'Edit', 'input': {'file_path': 'src/state.rs', 'new_string': 'DO_NOT_EXPORT_TOOL_INPUT'}}]}}
        ]

        def write_rows():
            transcript.write_text(''.join(json.dumps(row) + '\n' for row in rows))

        def cli(*args, expected=0):
            result = subprocess.run([str(binary), *args], cwd=project, env=env,
                                    text=True, capture_output=True, timeout=10)
            assert result.returncode == expected, (args, result.stdout, result.stderr)
            return result

        def request(path, body=None):
            headers = {'Content-Type': 'application/json'}
            if 'XERJ_API_KEY' in env:
                headers['Authorization'] = 'ApiKey ' + env['XERJ_API_KEY']
            req = Request(url + path, None if body is None else json.dumps(body).encode(), headers=headers)
            with opener.open(req, timeout=5) as response:
                return json.load(response)

        def wait_for(predicate, label, seconds=75):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if predicate():
                    return
                time.sleep(.1)
            status = (state / 'status.json').read_text() if (state / 'status.json').exists() else 'no status'
            raise AssertionError(f'{label}: {status}')

        def hook(event, command=None):
            payload = {'hook_event_name': event, 'cwd': str(project), 'session_id': sid,
                       'transcript_path': str(transcript), 'source': 'startup', 'reason': 'other'}
            if command is None:
                command = json.loads(settings.read_text())['hooks'][event][-1]['hooks'][0]['command']
            started = time.monotonic()
            result = subprocess.run(command, shell=True, cwd=project, env=env,
                                    input=json.dumps(payload), text=True, capture_output=True, timeout=4)
            elapsed = time.monotonic() - started
            assert result.returncode == 0 and not result.stderr, (result.stdout, result.stderr)
            if event == 'SessionEnd':
                assert not result.stdout
            print(f'{event}: {elapsed:.3f}s, exit 0, stderr empty', flush=True)
            return result.stdout

        def complete():
            return (state / 'receipts').exists() and any((state / 'receipts').iterdir()) and not any((state / 'pending').iterdir())

        node = None
        logs = []

        def start():
            nonlocal node
            log = (work / f'node-{len(logs)}.log').open('w')
            logs.append(log)
            flags = [] if args.authenticated else ['--insecure']
            node = subprocess.Popen([str(binary), *flags, '-c', str(node_config)],
                                    cwd=work, env=env, stdout=log, stderr=log)
            def ready():
                assert node.poll() is None, 'node exited early'
                if args.authenticated:
                    key = work / 'data' / 'admin.key'
                    if not key.exists():
                        return False
                    env['XERJ_API_KEY'] = key.read_text().strip()
                try:
                    request('/')
                    return True
                except OSError:
                    return False
            wait_for(ready, 'readiness', 60)

        def stop():
            nonlocal node
            if node is not None:
                node.terminate()
                node.wait(timeout=30)
                assert node.returncode == 0, node.returncode
                print('private node exit 0', flush=True)
                node = None

        try:
            write_rows()
            original_settings = settings.read_bytes()
            cli('init', '--memory', '--url', url, '--dry-run')
            assert settings.read_bytes() == original_settings and not state.exists()
            cli('init', '--memory', '--url', url)
            installed = settings.read_bytes()
            cli('init', '--memory', '--url', url)
            assert settings.read_bytes() == installed
            config = json.loads((state / 'config.json').read_text())
            prefix, namespace = config['prefix'], config['namespace']
            print('PASS dry-run, quoted paths, install and unchanged re-install', flush=True)
            start()
            hook('SessionEnd')
            wait_for(complete, 'first import')
            for line in (state / 'last-estimate.txt').read_text().splitlines():
                if 'autoindex:' in line and ('files (' in line or 'estimated index' in line):
                    print(line, flush=True)
            assert (state / 'last-estimate.txt').stat().st_size > 0
            print(f'Synthetic input: {transcript.stat().st_size} bytes; dry-run output captured', flush=True)
            archives = list((state / 'archives').glob('*.jsonl'))
            assert len(archives) == 1
            exported = archives[0].read_text()
            assert '[REDACTED_EMAIL]' in exported and 'owner@example.com' not in exported
            assert 'DO_NOT_EXPORT' not in exported
            assert len(exported.splitlines()) == 2
            count = request('/' + prefix + '-*/_count')['count']
            assert count == 2, count
            memories = request('/_memory/' + namespace + '?size=10')
            assert memories['count'] == 1, memories
            text = hook('SessionStart')
            assert 'We chose snapshots' in text and 'archives/' in text and '[memory:' in text, text
            assert len(text.encode()) <= 4096 and len(text.splitlines()) <= 15
            print('PASS redaction, 2 searchable records, one memory and cited SessionStart brief', flush=True)
            hook('SessionEnd')
            wait_for(complete, 'duplicate import')
            assert request('/' + prefix + '-*/_count')['count'] == count
            assert request('/_memory/' + namespace + '?size=10')['count'] == 1
            assert len(list((state / 'archives').glob('*.jsonl'))) == 1
            rows.append({'type': 'assistant', 'sessionId': sid, 'uuid': '550e8400-e29b-41d4-a716-446655440003',
                         'message': {'role': 'assistant', 'content': 'Open item: validate the watcher after restart.'}})
            write_rows()
            hook('SessionEnd')
            wait_for(complete, 'changed transcript')
            assert request('/' + prefix + '-*/_count')['count'] == 3
            assert request('/_memory/' + namespace + '?size=10')['count'] == 1
            assert len(list((state / 'archives').glob('*.jsonl'))) == 2
            assert 'Open item: validate' in hook('SessionStart')
            print('PASS duplicate event idempotence and changed-transcript replacement (3 records, one memory)', flush=True)
            stop()
            cached = hook('SessionStart')
            assert cached.startswith('Cached') and 'Open item: validate' in cached
            before = next((state / 'exports').glob('*.jsonl')).read_bytes()
            transcript.write_text(transcript.read_text() + '{bad-private-tail')
            hook('SessionEnd')
            wait_for(lambda: (state / 'status.json').exists() and json.loads((state / 'status.json').read_text())['state'] == 'pending', 'malformed pending', 10)
            assert next((state / 'exports').glob('*.jsonl')).read_bytes() == before
            assert len(list((state / 'archives').glob('*.jsonl'))) == 2
            write_rows()
            start()
            hook('SessionStart')
            wait_for(complete, 'retry after node restart')
            assert request('/' + prefix + '-*/_count')['count'] == 3
            assert request('/_memory/' + namespace + '?size=10')['count'] == 1
            print('PASS offline cached brief, malformed-tail atomicity and retained-job recovery after restart', flush=True)
            old_command = json.loads(settings.read_text())['hooks']['SessionEnd'][-1]['hooks'][0]['command']
            cli('init', '--memory', '--remove', '--dry-run')
            assert settings.read_bytes() == installed
            # A completed receipt can precede lease release by a few instructions.
            def remove():
                result = subprocess.run([str(binary), 'init', '--memory', '--remove'], cwd=project,
                                        env=env, text=True, capture_output=True, timeout=4)
                if result.returncode == 0:
                    return True
                assert 'busy' in result.stderr, result.stderr
                return False
            wait_for(remove, 'remove', 10)
            assert json.loads(settings.read_text()) == foreign
            assert not json.loads((state / 'config.json').read_text())['enabled']
            assert len(list((state / 'archives').glob('*.jsonl'))) == 2
            assert request('/' + prefix + '-*/_count')['count'] == 3
            assert request('/_memory/' + namespace + '?size=10')['count'] == 1
            hook('SessionEnd', old_command)
            assert not any((state / 'pending').iterdir())
            print('PASS uninstall keeps foreign hooks and data; stale hook is a silent no-op', flush=True)
            # Explicit cleanup only after the retention assertions, on this script's own node.
            plan = cli('autoindex', 'remove', '--prefix', prefix, '--root', str(state / 'exports'), '--url', url, expected=4)
            assert prefix in plan.stdout + plan.stderr
            cli('autoindex', 'remove', '--prefix', prefix, '--root', str(state / 'exports'), '--url', url, '--yes')
            headers = {'Authorization': 'ApiKey ' + env['XERJ_API_KEY']} if 'XERJ_API_KEY' in env else {}
            req = Request(url + '/_memory/' + namespace, method='DELETE', headers=headers)
            with opener.open(req, timeout=5) as response:
                assert response.status == 200
            assert request('/_memory/' + namespace + '?size=10')['count'] == 0
            print('PASS documented ownership-plan cleanup and dedicated memory deletion', flush=True)
        finally:
            stop()
            for log in logs:
                log.close()


if __name__ == '__main__':
    main()
