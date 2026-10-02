#!/usr/bin/env python3
"""Fixture-only cache-header acceptance; no cache is enabled or installed."""
import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
from http.server import ThreadingHTTPServer

import probe as p


class HeaderFixture(p.Fixture):
    def end_headers(self):
        self.send_header('Cache-Control', 'private')
        self.send_header('Cache-Control', 'No-Store')
        self.send_header('Pragma', 'no-cache')
        self.send_header('Expires', '0')
        self.send_header('Vary', '*')
        self.send_header('Age', '0')
        self.send_header('Set-Cookie', 'fixture-sensitive=blocked')
        self.send_header('X-Fixture-Sensitive', 'blocked')
        super().end_headers()


def call(address, path, data):
    connection = http.client.HTTPConnection(address, timeout=10)
    connection.request('POST', path, json.dumps(data), {
        'Content-Type': 'application/json', 'Authorization': 'Bearer fake-client-key',
        'x-api-key': 'fake-client-key', 'anthropic-version': '2023-06-01',
    })
    reply = connection.getresponse()
    headers = reply.getheaders()
    body = reply.read()
    connection.close()
    return reply.status, headers, body


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--steve', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    binary = args.steve.resolve()
    env = {'PATH': os.environ['PATH'], 'TMPDIR': tempfile.gettempdir(),
           'STEVE_SPIKE_KEY': p.KEY, 'STEVE_OPERATOR': 'fixture-operator'}
    children, handles = [], []
    server = None
    result = {'fixture_only': True, 'cache_enabled': False, 'real_provider_calls': 0,
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'script_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              'cases': [], 'failures': []}
    with tempfile.TemporaryDirectory(prefix='steve-cache-headers-') as temporary:
        root = Path(temporary)
        def launch(command, name):
            log = root / (name + '.log')
            handle = log.open('wb')
            handles.append(handle)
            child = subprocess.Popen(command, env=env, stdout=handle, stderr=subprocess.STDOUT)
            children.append(child)
            return child, log
        try:
            config = root / 'fixture.toml'
            config.write_text('[logging]\njson=true\n')
            child, log = launch([str(binary), '--config', str(config), 'test-upstream',
                                 '--listen', '127.0.0.1:0'], 'template')
            address = p.ready_address(child, log, 'test_upstream_ready', 'local_addr')
            protocols = [('chat', '/v1/chat/completions'), ('responses', '/v1/responses'),
                         ('messages', '/v1/messages')]
            for protocol, path in protocols:
                for streaming in (False, True):
                    p.Fixture.templates[path, streaming] = p.request(
                        address, path, p.payload(protocol, streaming))['body']
            p.stop(child)
            server = ThreadingHTTPServer(('127.0.0.1', 0), HeaderFixture)
            server.daemon_threads = True
            threading.Thread(target=server.serve_forever, daemon=True).start()
            upstream = '127.0.0.1:' + str(server.server_port)
            journal = root / 'accounting'
            subprocess.run([str(binary), 'accounting', 'provision', '--root', str(journal)],
                           env=env, check=True, capture_output=True, timeout=15)
            text = ('[server]\ninference_bind="127.0.0.1:0"\nmanagement_bind="127.0.0.1:0"\n'
                    '[database]\nurl=' + json.dumps('sqlite://' + str(root / 'data.db') + '?mode=rwc') +
                    '\n[object_storage]\nroot=' + json.dumps(str(root / 'objects')) +
                    '\n[queues]\naccounting_journal=' + json.dumps(str(journal)) +
                    '\n[logging]\njson=true\n[native]\n')
            for name, protocol in [('open', 'openai'), ('anth', 'anthropic')]:
                text += (f'[[native.providers]]\nid="{name}"\nprotocol="{protocol}"\n'
                         f'base_url="http://{upstream}"\ncredential_env="STEVE_SPIKE_KEY"\n')
            for model, provider, protocols_ in [('up-chat', 'open', ['chat', 'responses']),
                                              ('up-anth', 'anth', ['messages'])]:
                text += (f'[[native.models]]\nid="{model}"\nprovider="{provider}"\n'
                         f'upstream_model="{model}"\nprotocols={json.dumps(protocols_)}\n'
                         'input_micro_usd_per_million=1\noutput_micro_usd_per_million=1\n')
            config = root / 'steve.toml'
            config.write_text(text)
            child, log = launch([str(binary), '--config', str(config), 'serve'], 'steve')
            address = p.ready_address(child, log, 'listeners_ready', 'inference')
            for protocol, path in protocols:
                for streaming, scenario in [(False, 'normal'), (True, 'normal'),
                                             (False, 'error'), (True, 'error')]:
                    before = len(p.Fixture.records)
                    status, headers, body = call(address, path, p.payload(protocol, streaming, scenario))
                    lowered = [(name.lower(), value) for name, value in headers]
                    controls = [value for name, value in lowered if name == 'cache-control']
                    ok = (status == (200 if scenario == 'normal' else 502)
                          and controls == ['private', 'No-Store']
                          and ('pragma', 'no-cache') in lowered and ('vary', '*') in lowered
                          and not any(name in ('set-cookie', 'x-fixture-sensitive') for name, _ in lowered)
                          and (scenario != 'normal' or b'steve-test-' in body))
                    case = {'protocol': protocol, 'stream': streaming, 'scenario': scenario,
                            'status': status, 'cache_control': controls,
                            'upstream_attempts': len(p.Fixture.records) - before, 'pass': ok}
                    result['cases'].append(case)
                    if not ok:
                        result['failures'].append(case)
            result['credentials_ok'] = all(record['auth_ok'] for record in p.Fixture.records)
            result['log_canary_leak'] = any(p.KEY in log.read_text(errors='replace')
                                           for log in root.glob('*.log'))
        except Exception as error:
            result['error'] = str(error)
        finally:
            for child in reversed(children):
                p.stop(child)
            for handle in handles:
                handle.close()
            if server is not None:
                server.shutdown()
                server.server_close()
            result['cleanup'] = all(child.poll() is not None for child in children)
            args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({'cases': len(result['cases']), 'failures': len(result['failures']),
                      'error': result.get('error'), 'cleanup': result['cleanup']}))
    p.require(not result.get('error') and not result['failures'] and result.get('credentials_ok')
              and not result.get('log_canary_leak') and result['cleanup'], 'header acceptance failed')


if __name__ == '__main__':
    main()
