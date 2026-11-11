#!/usr/bin/env python3
"""Test final-image NSS and Rustls on isolated local DNS/HTTPS fixtures."""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile
import time
import uuid

FIXTURE = r'''
from collections import Counter
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import json, socket, ssl, struct, threading

queries = Counter()
requests = Counter()
def dns():
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.bind(('127.0.0.1', 53))
    while True:
        packet, address = udp.recvfrom(4096)
        offset, labels = 12, []
        while packet[offset]:
            length = packet[offset]
            labels.append(packet[offset+1:offset+1+length].decode('ascii'))
            offset += length + 1
        offset += 1
        kind, _ = struct.unpack('!HH', packet[offset:offset+4])
        name = '.'.join(labels)
        queries[f'{name}:{kind}'] += 1
        Path('/fixture/dns.json').write_text(json.dumps(dict(queries)))
        known = name in ('dns-only.runtime.test', 'wrong-name.runtime.test')
        answer = b''
        if known and kind == 1:
            answer = b'\xc0\x0c' + struct.pack('!HHIH', 1, 1, 0, 4) + socket.inet_aton('127.0.0.1')
        response = packet[:2] + struct.pack('!HHHHH', 0x8180 if known else 0x8183, 1, int(bool(answer)), 0, 0)
        udp.sendto(response + packet[12:offset+4] + answer, address)

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        requests[self.headers['Host']] += 1
        Path('/fixture/requests.json').write_text(json.dumps(dict(requests)))
        body = b'runtime TLS fixture'
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass

threading.Thread(target=dns, daemon=True).start()
server = ThreadingHTTPServer(('127.0.0.1', 8443), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain('/fixture/server.pem', '/fixture/server.key')
server.socket = context.wrap_socket(server.socket, server_side=True)
Path('/fixture/ready').touch()
server.serve_forever()
'''


def verify(engine, image):
    repo = Path(__file__).resolve().parents[1]
    builder = 'btc-network-probe-' + uuid.uuid4().hex
    fixture = None
    builder_created = False

    def command(*parts):
        result = subprocess.run([engine, *parts], capture_output=True, text=True)
        if result.returncode:
            raise RuntimeError(result.stderr[-6000:] or result.stdout[-6000:])
        return result.stdout

    candidate = command('image', 'inspect', '--format', '{{.Id}}', image).strip()
    try:
        # Reuse the pinned release toolchain/dependencies, without adding a probe to the image.
        command('build', '--target', 'builder', '--tag', builder, str(repo))
        builder_created = True
        with tempfile.TemporaryDirectory(prefix='btc-network-fixture-') as directory:
            path = Path(directory)
            path.chmod(0o755)
            command('run', '--rm', '-v', str(repo) + ':/workspace:ro',
                    '-v', directory + ':/fixture', '-w', '/workspace',
                    '-e', 'CARGO_TARGET_DIR=/app/target', builder, 'sh', '-c',
                    'cargo build --locked --release --example runtime_network_probe && '
                    'cp -f /app/target/release/examples/runtime_network_probe /fixture/probe')
            certificates = [
                ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
                 '-keyout', 'ca.key', '-out', 'ca.pem', '-subj', '/CN=Runtime Fixture CA',
                 '-addext', 'basicConstraints=critical,CA:TRUE'],
                ['req', '-new', '-newkey', 'rsa:2048', '-nodes', '-keyout', 'server.key',
                 '-out', 'server.csr', '-subj', '/CN=dns-only.runtime.test'],
                ['x509', '-req', '-in', 'server.csr', '-CA', 'ca.pem', '-CAkey', 'ca.key',
                 '-CAcreateserial', '-out', 'server.pem', '-days', '1', '-extfile', 'extensions.cnf'],
            ]
            (path / 'extensions.cnf').write_text(
                'subjectAltName=DNS:dns-only.runtime.test,DNS:hosts-only.runtime.test\n'
                'basicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\n')
            for arguments in certificates:
                subprocess.run(['openssl', *arguments], cwd=directory, check=True,
                               capture_output=True, text=True)
            (path / 'ca.pem').chmod(0o644)
            (path / 'resolv.conf').write_text('nameserver 127.0.0.1\noptions timeout:1 attempts:1\n')
            (path / 'hosts').write_text('127.0.0.1 localhost hosts-only.runtime.test\n::1 localhost\n')
            fixture = command('run', '-d', '--network', 'none', '-v', directory + ':/fixture',
                              'docker.io/library/python:3.15.0-slim-bookworm@sha256:0e019bbdfb5d9874cb2389f1c2884b64aa2ac97c99960b5bc521cef978c0617d', 'python3', '-c', FIXTURE).strip()
            deadline = time.monotonic() + 15
            while not (path / 'ready').exists():
                if time.monotonic() >= deadline:
                    raise RuntimeError('Network fixture did not start: ' + command('logs', fixture))
                time.sleep(0.1)
            output = command('run', '--rm', '--network', 'container:' + fixture,
                             '--read-only', '--cap-drop=ALL', '--security-opt=no-new-privileges',
                             '-v', directory + ':/fixture:ro',
                             '-v', str(path / 'resolv.conf') + ':/etc/resolv.conf:ro',
                             '-v', str(path / 'hosts') + ':/etc/hosts:ro',
                             '--entrypoint', '/fixture/probe', candidate, '--check-maps')
            queries = json.loads((path / 'dns.json').read_text())
            requests = json.loads((path / 'requests.json').read_text())
            assert queries.get('dns-only.runtime.test:1', 0) > 0, queries
            assert queries.get('dns-only.runtime.test:28', 0) > 0, queries
            assert not any(key.startswith('hosts-only.runtime.test:') for key in queries), queries
            assert requests == {'hosts-only.runtime.test:8443': 1, 'dns-only.runtime.test:8443': 1}, requests
            loaded = sorted({line.removeprefix('LOADED ') for line in output.splitlines() if line.startswith('LOADED ')})
            assert {Path(name).name for name in loaded} == {'libc.so.6', 'libm.so.6', 'libgcc_s.so.1', 'ld-linux-x86-64.so.2'}, loaded
            print('\n'.join(line for line in output.splitlines() if not line.startswith('LOADED ')))
            print('Verified image:', candidate)
            print('Loaded native libraries:', json.dumps(loaded))
            print('Isolated DNS queries:', json.dumps(queries, sort_keys=True))
            print('Verified TLS requests:', json.dumps(requests, sort_keys=True))
    finally:
        if fixture:
            subprocess.run([engine, 'rm', '-f', fixture], capture_output=True, check=False)
        if builder_created:
            subprocess.run([engine, 'image', 'rm', builder], capture_output=True, check=False)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--engine', default='docker')
    parser.add_argument('--image', required=True)
    args = parser.parse_args()
    verify(args.engine, args.image)
