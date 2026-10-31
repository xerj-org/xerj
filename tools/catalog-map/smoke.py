#!/usr/bin/env python3
"""Verify CLI/MCP catalog scope and error classification against read-only loopback fixtures."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    env = {k: v for k, v in os.environ.items() if not k.startswith('XERJ_')
           and k.lower() not in ('all_proxy', 'http_proxy', 'https_proxy', 'no_proxy')}
    env.update(XERJ_DISABLE_FEEDBACK='true', XERJ_AUTH='')
    mode = 'empty'
    requests = []
    dataset = {'doc_kind': 'dataset', 'slug': 'ax-code', 'index_name': 'ax-code',
               'record_count': 2, 'fields_json': '[]'}

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def reply(self, status, body):
            payload = json.dumps(body).encode()
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def do_GET(self):
            requests.append(('GET', self.path))
            self.reply(200, {})

        def do_POST(self):
            requests.append(('POST', self.path))
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            if mode in ('missing', 'unauthorized', 'forbidden'):
                status = {'missing': 404, 'unauthorized': 401, 'forbidden': 403}[mode]
                kind = 'index_not_found_exception' if status == 404 else 'security_exception'
                self.reply(status, {'error': {'type': kind, 'reason': 'fixture refusal'}, 'status': status})
                return
            is_dataset = '"dataset"' in json.dumps(body.get('query', {}))
            hits = [{'_source': dataset}] if mode == 'positive' and is_dataset else []
            self.reply(200, {'hits': {'total': {'value': len(hits)}, 'hits': hits}})

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    url = f'http://127.0.0.1:{server.server_port}'
    failures = []
    try:
        with tempfile.TemporaryDirectory(prefix='xerj-catalog-map-') as temp:
            env.update(HOME=temp, USERPROFILE=temp, XDG_CONFIG_HOME=str(Path(temp) / 'config'), XDG_DATA_HOME=str(Path(temp) / 'data'))
            for mode in ('empty', 'missing', 'unauthorized', 'forbidden', 'positive'):
                for transport in ('cli', 'mcp'):
                    requests.clear()
                    try:
                        if transport == 'cli':
                            result = subprocess.run([str(binary), 'autoindex', 'map', '--url', url],
                                                    cwd=temp, env=env, capture_output=True, text=True, timeout=30)
                            assert result.returncode == (0 if mode == 'positive' else 1), result.stderr
                            text = result.stdout + result.stderr
                            json_result = subprocess.run([str(binary), 'autoindex', 'map', '--url', url, '--json'],
                                                         cwd=temp, env=env, capture_output=True, text=True, timeout=30)
                            assert json_result.returncode == result.returncode, json_result.stderr
                            if mode == 'positive':
                                assert json.loads(json_result.stdout)['datasets'][0]['index_name'] == 'ax-code'
                            else:
                                assert not json_result.stdout.strip(), json_result.stdout
                        else:
                            messages = [
                                {'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {'protocolVersion': '2024-11-05', 'capabilities': {}, 'clientInfo': {'name': 'catalog-fixture', 'version': '1'}}},
                                {'jsonrpc': '2.0', 'method': 'notifications/initialized'},
                                {'jsonrpc': '2.0', 'id': 2, 'method': 'tools/call', 'params': {'name': 'xerj_map', 'arguments': {'index': '*'}}},
                            ]
                            result = subprocess.run([str(binary), 'mcp', '--url', url], input=''.join(json.dumps(m) + '\n' for m in messages),
                                                    cwd=temp, env=env, capture_output=True, text=True, timeout=30)
                            assert result.returncode == 0, result.stderr
                            reply = next(json.loads(line)['result'] for line in result.stdout.splitlines() if json.loads(line).get('id') == 2)
                            assert reply.get('isError', False) == (mode in ('missing', 'unauthorized', 'forbidden')), reply
                            text = reply['content'][0]['text']
                            if mode == 'empty':
                                mapped = json.loads(text)
                                assert mapped['matched'] == 0 and mapped['indexes'] == [], mapped
                        if mode in ('empty', 'missing'):
                            assert 'managed corpus' in text, text
                            assert 'xerj corpus list' in text, text
                            assert 'not establish' in text, text
                        elif mode in ('unauthorized', 'forbidden'):
                            assert ('401' if mode == 'unauthorized' else '403') in text, text
                            assert 'managed corpus' not in text, text
                        else:
                            assert 'ax-code' in text, text
                            assert 'namespace_hint' not in text, text
                        assert all(method in ('GET', 'POST') and (path == '/' or path.endswith('/_search') or path.endswith('/_mapping'))
                                   for method, path in requests), requests
                        print(f'PASS {transport} {mode}: classification, scope and read-only requests', flush=True)
                    except (AssertionError, KeyError, StopIteration) as error:
                        failures.append(f'{transport} {mode}: {error}')
        if failures:
            raise AssertionError('\n'.join(failures))
    finally:
        server.shutdown()
        server.server_close()
        worker.join(timeout=5)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
