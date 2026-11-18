#!/usr/bin/env python3
"""Check #2580 on a private node, holding first-stage candidates fixed.

Accept --expect-zero for an original lexical-judge binary, or --runner for a
fresh-node compatibility gate before the fixture. No model or personal data.
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
from urllib.request import build_opener, ProxyHandler, Request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--expect-zero', action='store_true')
    parser.add_argument('--runner', type=Path)
    args = parser.parse_args()
    original = args.binary.resolve(strict=True)
    runner = args.runner.resolve(strict=True) if args.runner else None
    repo = Path(__file__).resolve().parents[2]
    env = {k: v for k, v in os.environ.items() if not k.startswith('XERJ_')
           and k.lower() not in ('http_proxy', 'https_proxy', 'all_proxy', 'no_proxy')}
    env.update(XERJ_DISABLE_FEEDBACK='true', XERJ_NUM_CPUS='4')
    opener = build_opener(ProxyHandler({}))
    with tempfile.TemporaryDirectory(prefix='xerj-cjk-judge-') as temp:
        root = Path(temp)
        binary = root / ('xerj.exe' if os.name == 'nt' else 'xerj')
        shutil.copyfile(original, binary)
        binary.chmod(0o700)
        print('Executable snapshot SHA-256:', hashlib.sha256(binary.read_bytes()).hexdigest(), flush=True)
        config = root / 'node.toml'
        # Only this tiny temporary fixture changes its volume reserve.
        config.write_text('[server]\nauto_port = false\n\n[limits]\ndisk_flood_stage_percent = 99\n\n[rerank]\nenabled = false\n')
        env['XERJ_CONFIG'] = str(config)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        assert port <= 65533
        url = f'http://127.0.0.1:{port}'

        def run(command, stdin=None, timeout=120):
            result = subprocess.run([str(x) for x in command], input=stdin, capture_output=True,
                                    text=True, cwd=root, env=env, timeout=timeout)
            assert result.returncode == 0, (command, result.returncode, result.stdout, result.stderr)
            return result.stdout

        def request(method, path, body=None):
            data = json.dumps(body).encode() if body is not None else None
            req = Request(url + path, data=data, method=method, headers={'Content-Type': 'application/json'})
            with opener.open(req, timeout=30) as response:
                return json.load(response)

        def query(judge=None):
            body = {'query': {'match_all': {}}, 'size': 3}
            if judge is not None:
                body['judge'] = judge
            return request('POST', '/cjkjudge-docs/_search', body)

        def check():
            base = query()
            assert 'judged' not in base
            assert all('_p_relevant' not in h for h in base['hits']['hits'])
            judged = query({'query': '雷同'})
            hits = judged['hits']['hits']
            assert len(hits) == 3 and judged['judged']['scorer'] == 'lexical', judged
            scores = {h['_id']: h['_p_relevant'] for h in hits}
            print('Observed lexical scores:', json.dumps(scores, ensure_ascii=False), flush=True)
            assert {h['_id']: h['_score'] for h in hits} == {h['_id']: h['_score'] for h in base['hits']['hits']}
            if args.expect_zero:
                assert all(p == 0.0 for p in scores.values()), scores
                assert not query({'query': '雷同', 'min_p': 0.01})['hits']['hits']
                return
            assert scores['exact'] > scores['partial'] > 0.0 == scores['unrelated'], judged
            assert hits[0]['_id'] == 'exact', judged
            cut = (scores['exact'] + scores['partial']) / 2
            kept = query({'query': '雷同', 'min_p': cut})
            assert [h['_id'] for h in kept['hits']['hits']] == ['exact'], kept
            assert kept['hits']['total'] == base['hits']['total']
            assert [kept['judged'][k] for k in ['judged', 'kept', 'dropped']] == [3, 1, 2], kept
            messages = [
                {'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {}},
                {'jsonrpc': '2.0', 'id': 2, 'method': 'tools/call', 'params': {
                    'name': 'xerj_search', 'arguments': {'index': 'cjkjudge-docs',
                    'query_dsl': {'match_all': {}}, 'size': 3, 'judge': {'query': '雷同'}}}},
            ]
            raw = run([binary, 'mcp', '--url', url], ''.join(json.dumps(m) + '\n' for m in messages))
            reply = next(json.loads(line)['result'] for line in raw.splitlines() if json.loads(line).get('id') == 2)
            assert not reply.get('isError'), reply
            mcp = json.loads(reply['content'][0]['text'])
            assert [(h['_id'], h['_p_relevant']) for h in mcp['hits']['hits']] == [(h['_id'], h['_p_relevant']) for h in hits], mcp
            print('PASS HTTP/MCP order, CJK evidence, threshold counts, total and original _score', flush=True)

        print(run([binary, '--version']), end='', flush=True)
        for phase in ['first-run', 'restart']:
            with (root / f'{phase}.log').open('w') as log:
                node = subprocess.Popen([str(binary), '--insecure', '--port', str(port),
                    '--data-dir', str(root / 'data'), '--embed-mode', 'lexical'],
                    cwd=root, env=env, stdout=log, stderr=log)
                try:
                    deadline = time.monotonic() + 60
                    while True:
                        assert node.poll() is None, (root / f'{phase}.log').read_text()
                        try:
                            request('GET', '/')
                            break
                        except OSError:
                            if time.monotonic() >= deadline:
                                raise TimeoutError('node readiness')
                            time.sleep(0.1)
                    if phase == 'first-run':
                        if runner:
                            print(run([runner, '--url', url, '--dir', repo / 'engine/tests/es-compat-yaml/yaml'], timeout=900), flush=True)
                        request('PUT', '/cjkjudge-docs', {'mappings': {'properties': {'body': {'type': 'text'}}}})
                        for id_, body in [('exact', '这里发现雷同内容雷同内容'), ('partial', '部分雷声同时出现'), ('unrelated', '今天阳光明媚风景宜人')]:
                            request('PUT', f'/cjkjudge-docs/_doc/{id_}?refresh=true', {'body': body})
                        check()
                        request('POST', '/cjkjudge-docs/_flush', {})
                    check()
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
        print('PASS buffered, flushed and restarted fixture' + (' (original zero-score control)' if args.expect_zero else ''), flush=True)


if __name__ == '__main__':
    main()
