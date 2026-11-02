#!/usr/bin/env python3
"""Isolated real-node lifecycle and installed hook command verification.

Covers both hosts: Claude Code `PreToolUse` and Gemini CLI `AfterTool`.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import shutil
import shlex
import subprocess
import tempfile
import time
from urllib.request import Request, build_opener, ProxyHandler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--authenticated', action='store_true')
    parser.add_argument('--claude-sample', action='store_true', help='run a controlled two-session real Claude Code delivery sample')
    parser.add_argument('--receipt', type=Path, help='new JSON file for the controlled host sample summary')
    args = parser.parse_args()
    original = args.binary.resolve(strict=True)
    env = {k: v for k, v in os.environ.items() if not k.startswith('XERJ_')
           and k.lower() not in ('http_proxy', 'https_proxy', 'all_proxy', 'no_proxy')}
    env.update(XERJ_DISABLE_FEEDBACK='true', XERJ_NUM_CPUS='2')
    opener = build_opener(ProxyHandler({}))
    with tempfile.TemporaryDirectory(prefix='xerj-search-hints-') as temp:
        work = Path(temp).resolve()
        binary = work / "xerj's binary"
        shutil.copyfile(original, binary)
        binary.chmod(0o700)
        print('Executable SHA-256:', hashlib.sha256(binary.read_bytes()).hexdigest(), flush=True)
        project = work / "project's work"
        project.mkdir()
        (project / 'src').mkdir()
        (project / 'src/lib.rs').write_text('pub fn recover_snapshot() -> bool { true }\n')
        sockets = [socket.socket(), socket.socket()]
        for sock in sockets:
            sock.bind(('127.0.0.1', 0))
        port, rest = [sock.getsockname()[1] for sock in sockets]
        for sock in sockets:
            sock.close()
        url = f'http://127.0.0.1:{port}'
        env['XERJ_URL'] = url
        node_config = work / 'node.toml'
        node_config.write_text(f'[server]\nes_compat_port={port}\nrest_port={rest}\ngrpc_port=0\n'
                               f'data_dir={json.dumps(str(work / "data"))}\n'
                               '[limits]\ndisk_flood_stage_percent=99\n')
        settings = project / '.claude/settings.json'
        state = project / '.claude/xerj-hints'
        gemini_settings = project / '.gemini/settings.json'
        gemini_state = project / '.gemini/xerj-hints'
        node = None
        logs = []
        commands = 0

        def cli(*args, expected=0):
            result = subprocess.run([str(binary), *args], cwd=project, env=env,
                                    text=True, capture_output=True, timeout=90)
            assert result.returncode == expected, (args, result.stdout, result.stderr)
            return result

        def request(path):
            headers = {}
            if 'XERJ_AUTH' in env:
                headers['Authorization'] = env['XERJ_AUTH']
            with opener.open(Request(url + path, headers=headers), timeout=3) as response:
                return json.load(response)

        def start():
            nonlocal node
            log = open(work / f'node-{len(logs)}.log', 'w+')
            logs.append(log)
            command = [str(binary), '--config', str(node_config)]
            if not args.authenticated:
                command.append('--insecure')
            node = subprocess.Popen(command, cwd=work, env=env, stdout=log, stderr=log)
            deadline = time.monotonic() + 40
            while time.monotonic() < deadline:
                if node.poll() is not None:
                    log.seek(0)
                    raise AssertionError(log.read()[-3000:])
                key = work / 'data/admin.key'
                if args.authenticated and key.exists():
                    env['XERJ_API_KEY'] = key.read_text().strip()
                    env['XERJ_AUTH'] = 'ApiKey ' + env['XERJ_API_KEY']
                try:
                    request('/')
                    return
                except Exception:
                    time.sleep(.1)
            raise AssertionError('private node readiness timeout')

        def stop():
            nonlocal node
            if node is not None:
                node.terminate()
                try:
                    status = node.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    node.kill()
                    node.wait()
                    raise
                node = None
                assert status == 0, status
                print('Private node graceful exit 0', flush=True)

        def hook(tool='Grep', sid='sample', expect=False, payload=None, command=None):
            nonlocal commands
            if command is None:
                command = json.loads(settings.read_text())['hooks']['PreToolUse'][-1]['hooks'][0]['command']
            if payload is None:
                payload = json.dumps({'hook_event_name': 'PreToolUse', 'cwd': str(project),
                                      'session_id': sid, 'tool_name': tool,
                                      'tool_input': {'pattern': 'PRIVATE_QUERY_DO_NOT_STORE'}})
            result = subprocess.run(command, shell=True, cwd=project, env=env, input=payload,
                                    text=True, capture_output=True, timeout=4)
            commands += 1
            assert result.returncode == 0 and not result.stderr, result
            if expect:
                value = json.loads(result.stdout)
                assert list(value) == ['hookSpecificOutput'], value
                specific = value['hookSpecificOutput']
                assert set(specific) == {'hookEventName', 'additionalContext'}, value
                assert specific['hookEventName'] == 'PreToolUse', value
                assert '--prefix hint-smoke' in specific['additionalContext'] and url in specific['additionalContext'], value
            else:
                assert not result.stdout, result.stdout
            return result.stdout

        def gemini_hook(tool='grep_search', sid='sample', expect=False, payload=None, command=None):
            """Invoke the installed Gemini CLI command exactly as AfterTool would."""
            nonlocal commands
            if command is None:
                command = json.loads(gemini_settings.read_text())['hooks']['AfterTool'][-1]['hooks'][0]['command']
            if payload is None:
                payload = json.dumps({'hook_event_name': 'AfterTool', 'cwd': str(project),
                                      'session_id': sid, 'tool_name': tool,
                                      'tool_input': {'pattern': 'PRIVATE_QUERY_DO_NOT_STORE'},
                                      'tool_response': {'output': 'PRIVATE_RESULT_DO_NOT_STORE'}})
            result = subprocess.run(command, shell=True, cwd=project, env=env, input=payload,
                                    text=True, capture_output=True, timeout=4)
            commands += 1
            assert result.returncode == 0 and not result.stderr, result
            if expect:
                value = json.loads(result.stdout)
                assert list(value) == ['hookSpecificOutput'], value
                specific = value['hookSpecificOutput']
                assert set(specific) == {'hookEventName', 'additionalContext'}, value
                assert specific['hookEventName'] == 'AfterTool', value
                assert '--prefix hint-smoke' in specific['additionalContext'] and url in specific['additionalContext'], value
            else:
                assert not result.stdout, result.stdout
            return result.stdout

        try:
            assert not settings.exists()
            cli('init', '--hooks', '--dry-run', '--url', url)
            assert not settings.exists() and not state.exists()
            settings.parent.mkdir(parents=True, exist_ok=True)
            foreign = {'permissions': {'allow': ['Read']}, 'hooks': {'PreToolUse': [
                {'matcher': 'UnrelatedTool', 'hooks': [{'type': 'command', 'command': 'echo foreign'}]}]}}
            settings.write_text(json.dumps(foreign))
            cli('init', '--hooks', '--url', url)
            first = settings.read_bytes()
            cli('init', '--hooks', '--url', url)
            assert settings.read_bytes() == first
            hook()  # offline
            start()
            hook()  # reachable, not indexed
            if args.authenticated:
                for path in [settings, state / 'config.json']:
                    assert env['XERJ_API_KEY'].encode() not in path.read_bytes()
            # Demonstrate ordinary init and opt-in memory coexist without changing the hint group.
            cli('init', '--url', url)
            assert settings.read_bytes() == first
            dry = cli('autoindex', str(project), '--dry-run', '--prefix', 'hint-smoke', '--url', url)
            print(dry.stdout + dry.stderr, flush=True)
            indexed = cli('autoindex', str(project), '--prefix', 'hint-smoke', '--state-dir', str(work / 'journal'),
                          '--url', url, '--no-graph', '--yes')
            print(indexed.stdout + indexed.stderr, flush=True)
            assert 'xerj-done ok=true' in indexed.stdout + indexed.stderr
            if args.claude_sample:
                prompt = ("This is an isolated hook delivery test. First use Grep once to find "
                          "recover_snapshot in src/. If a hook supplies an XERJ search hint, "
                          "follow it with exactly one xerj search via Bash, using this executable: "
                          + shlex.quote(str(binary)) + ". Otherwise answer from Grep. Give the definition's "
                          "file and line. Do not edit files, index anything, or submit feedback.")
                summary = {'claude_version': subprocess.check_output(['claude', '--version'], text=True).strip(),
                           'sample_kind': 'engineered hint delivery/uptake check; not a relevance or adoption benchmark'}
                for enabled in [False, True]:
                    config = json.loads(first)
                    if not enabled:
                        config['hooks']['PreToolUse'] = config['hooks']['PreToolUse'][:-1]
                    settings.write_text(json.dumps(config))
                    host = subprocess.run(['claude', '-p', prompt, '--output-format', 'stream-json', '--verbose',
                                           '--max-turns', '4', '--no-session-persistence', '--setting-sources', 'project',
                                           '--strict-mcp-config', '--mcp-config', '{"mcpServers":{}}',
                                           '--tools', 'Grep,Glob,Read,Bash', '--allowedTools', 'Grep,Glob,Read,Bash'],
                                          cwd=project, env=env, text=True, capture_output=True, timeout=90)
                    events = [json.loads(line) for line in host.stdout.splitlines() if line.startswith('{')]
                    calls = {block['id']: block for event in events for block in event.get('message', {}).get('content', [])
                             if isinstance(block, dict) and block.get('type') == 'tool_use'}
                    names = [block['name'] for block in calls.values()]
                    searches = sum(block.get('name') == 'Bash' and 'search' in block.get('input', {}).get('command', '')
                                   and 'xerj' in block.get('input', {}).get('command', '') for block in calls.values())
                    result = {'exit_code': host.returncode, 'tool_names': names, 'xerj_search_calls': searches,
                              'result_error': any(event.get('is_error', False) for event in events)}
                    summary['enabled' if enabled else 'baseline'] = result
                    print('Claude Code controlled sample:', enabled, json.dumps(result), flush=True)
                    assert host.returncode == 0 and not result['result_error'] and 'Grep' in names, result
                    assert searches == int(enabled), result
                settings.write_bytes(first)
                if args.receipt:
                    with args.receipt.open('x') as out:
                        json.dump(summary, out, indent=2)
                        out.write('\n')
            cli('init', '--memory', '--url', url)
            memory = (project / '.claude/settings.local.json').read_bytes()
            mapped = cli('autoindex', 'map', '--prefix', 'hint-smoke', '--url', url)
            assert 'hint-smoke' in mapped.stdout, mapped
            # Gemini CLI host (#2642): AfterTool, .gemini/settings.json, millisecond timeout.
            assert not gemini_settings.exists()
            cli('init', '--hooks', '--host', 'gemini', '--dry-run', '--url', url)
            assert not gemini_settings.exists() and not gemini_state.exists()
            gemini_settings.parent.mkdir(parents=True, exist_ok=True)
            gemini_foreign = {'mcpServers': {}, 'hooks': {'BeforeTool': [
                {'matcher': 'run_shell_command', 'hooks': [{'type': 'command', 'command': 'echo foreign'}]}]}}
            gemini_settings.write_text(json.dumps(gemini_foreign))
            cli('init', '--hooks', '--host', 'gemini', '--url', url)
            gemini_first = gemini_settings.read_bytes()
            cli('init', '--hooks', '--host', 'gemini', '--url', url)
            assert gemini_settings.read_bytes() == gemini_first
            assert settings.read_bytes() == first, 'Gemini setup must not change Claude settings'
            gemini_group = json.loads(gemini_first)['hooks']['AfterTool'][-1]
            assert json.loads(gemini_first)['hooks']['BeforeTool'] == gemini_foreign['hooks']['BeforeTool']
            assert gemini_group['hooks'][0]['timeout'] == 2000, gemini_group
            assert gemini_group['hooks'][0]['command'].endswith(' --host gemini'), gemini_group
            matcher = re.compile(gemini_group['matcher'])
            assert all(matcher.search(name) for name in
                       ('grep_search', 'search_file_content', 'glob', 'read_file')), gemini_group
            assert not any(matcher.search(name) for name in
                           ('mcp_other_grep_search', 'read_many_files', 'run_shell_command')), gemini_group
            # Controlled replay on the Gemini tool names; no actual agent/model is simulated.
            gemini_hook('read_file', sid='g-reads')
            gemini_hook('read_file', sid='g-reads')
            gemini_hook('read_file', sid='g-reads', expect=True)
            for _ in range(9):
                gemini_hook('grep_search', sid='g-reads')
            gemini_hook('glob', sid='g-reads', expect=True)
            gemini_hook('search_file_content', sid='g-fresh', expect=True)
            # Per-host counters: the Gemini replay above consumed no Claude hint slot.
            hook(sid='g-fresh', expect=True)
            gemini_hook(payload='{broken')
            gemini_hook(payload='x' * 65537)
            for field, value in [('tool_name', 'mcp_other_grep_search'), ('tool_name', 'Grep'),
                                 ('hook_event_name', 'BeforeTool'), ('hook_event_name', 'PreToolUse'),
                                 ('cwd', str(work))]:
                event = {'hook_event_name': 'AfterTool', 'cwd': str(project),
                         'session_id': 'g-wrong', 'tool_name': 'grep_search'}
                event[field] = value
                gemini_hook(payload=json.dumps(event))
            gemini_counters = (gemini_state / 'counters.json').read_text()
            assert 'PRIVATE_QUERY_DO_NOT_STORE' not in gemini_counters, gemini_counters
            assert 'PRIVATE_RESULT_DO_NOT_STORE' not in gemini_counters, gemini_counters
            assert 'g-fresh' not in gemini_counters, gemini_counters
            cli('init', '--hooks', '--host', 'gemini', '--remove', '--url', url)
            assert json.loads(gemini_settings.read_text()) == gemini_foreign
            gemini_hook(command=gemini_group['hooks'][0]['command'])
            assert settings.read_bytes() == first, 'Gemini removal must not change Claude settings'
            # Hosts with hooks switched off entirely are refused without rewriting settings.
            disabled = dict(gemini_foreign, hooksConfig={'enabled': False})
            gemini_settings.write_text(json.dumps(disabled))
            cli('init', '--hooks', '--host', 'gemini', '--url', url, expected=1)
            assert json.loads(gemini_settings.read_text()) == disabled
            gemini_settings.write_text(json.dumps(gemini_foreign))
            cli('init', '--hooks', '--host', 'gemini', '--url', url)
            gemini_hook(sid='g-both', expect=True)
            hook(sid='g-both', expect=True)
            # Controlled replay: Read x3, Grep x9, Glob; no actual agent/model is simulated.
            hook('Read', sid='reads')
            hook('Read', sid='reads')
            hook('Read', sid='reads', expect=True)
            for _ in range(9):
                hook('Grep', sid='reads')
            hook('Glob', sid='reads', expect=True)
            hook(sid='independent', expect=True)
            hook(payload='{broken')
            hook(payload='x' * 65537)
            for field, value in [('tool_name', 'Bash'), ('hook_event_name', 'PostToolUse'), ('cwd', str(work))]:
                event = {'hook_event_name': 'PreToolUse', 'cwd': str(project), 'session_id': 'wrong', 'tool_name': 'Grep'}
                event[field] = value
                hook(payload=json.dumps(event))
            counters = (state / 'counters.json').read_text()
            assert 'PRIVATE_QUERY_DO_NOT_STORE' not in counters and 'independent' not in counters
            found = cli('def', 'recover_snapshot', '--prefix', 'hint-smoke', '--url', url)
            assert 'recover_snapshot' in found.stdout and 'lib.rs' in found.stdout, found
            stop()
            hook(sid='offline')
            start()
            hook(sid='reads')  # rate counter survives node restart
            hook(sid='restarted', expect=True)
            cli('autoindex', 'remove', '--prefix', 'hint-smoke', '--root', str(project), '--url', url, '--yes')
            hook(sid='removed-corpus')
            gemini_hook(sid='g-removed-corpus')
            saved = json.loads(settings.read_text())
            owned = saved['hooks']['PreToolUse'][-1]
            saved['hooks']['PreToolUse'][-1]['matcher'] = 'Bash'
            settings.write_text(json.dumps(saved))
            edited = settings.read_bytes()
            cli('init', '--hooks', '--remove', '--url', url, expected=1)
            assert settings.read_bytes() == edited
            # Restore uniquely owned group from the persisted config, then removal works.
            saved['hooks']['PreToolUse'][-1] = json.loads((state / 'config.json').read_text())['group']
            settings.write_text(json.dumps(saved))
            command = owned['hooks'][0]['command']
            cli('init', '--hooks', '--remove', '--url', url)
            assert json.loads(settings.read_text()) == foreign
            hook(command=command)
            assert (project / '.claude/settings.local.json').read_bytes() == memory
            cli('init', '--hooks', '--url', url)
            stop()
            print(f'PASS: {commands} installed hook invocations across both hosts; '
                  'lifecycle, limits, scope, refusal/recovery and removal', flush=True)
        finally:
            stop()
            for log in logs:
                log.close()


if __name__ == '__main__':
    main()
