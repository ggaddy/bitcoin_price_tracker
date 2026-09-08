#!/usr/bin/env python3
"""Verify the final image using an isolated volume and network, without providers."""
import argparse
import json
import subprocess
import time
import tempfile
from pathlib import Path
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--engine', default='docker')
parser.add_argument('--image', required=True)
args = parser.parse_args()
volume = 'btc-runtime-test-' + uuid.uuid4().hex
container = None
volume_created = False
helper = 'docker.io/library/python:3.12.3-slim-bookworm'


def command(*parts, check=True):
    return subprocess.run([args.engine, *parts], check=check, capture_output=True, text=True)


def probe(code, mount=False):
    options = ['run', '--rm', '--network', 'container:' + container, '--user', '10001:10001']
    if mount:
        options += ['-v', volume + ':/app/data']
    result = command(*options, helper, 'python3', '-c', code)
    if result.stdout:
        print(result.stdout.strip())
    return result.stdout


http = '''
import json, urllib.request, urllib.error
base = 'http://127.0.0.1:3100'
def request(path, data=None):
    req = urllib.request.Request(base+path, data=None if data is None else json.dumps(data).encode(), headers={'Content-Type':'application/json'})
    try: response = urllib.request.urlopen(req, timeout=5)
    except urllib.error.HTTPError as error: response = error
    with response:
        return response.status, json.load(response)
'''

try:
    # Pull before creating resources so image downloads do not consume readiness time.
    command('pull', helper)
    command('volume', 'create', volume)
    volume_created = True
    container = command('create', '--network', 'none', '-v', volume + ':/app/data',
                        '-e', 'BIND_ADDRESS=0.0.0.0:3100', '-e', 'SHUTDOWN_SECONDS=5', args.image).stdout.strip()
    command('start', container)
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if command('exec', container, 'bitcoin_price_tracker', '--healthcheck', check=False).returncode == 0:
            break
        time.sleep(.25)
    else:
        raise RuntimeError('Image did not become healthy: ' + command('logs', container).stdout)
    config = json.loads(command('inspect', container).stdout)[0]['Config']
    assert config.get('Healthcheck', {}).get('Test'), 'Image lacks HEALTHCHECK metadata; use podman build --format docker'
    user = command('inspect', '--format', '{{.Config.User}}', container).stdout.strip()
    assert user == '10001:10001', user
    probe(http + '''
assert request('/health') == (200, {'status': 'ok'})
status, data = request('/api/price')
assert status == 503 and data['status'] == 'UNAVAILABLE'
assert data['active_viewers'] == 0
print('Non-root startup, configured bind address, empty health and price responses passed.')
''')
    probe(http + '''
import os, sqlite3, time
assert os.stat('/app/data/bitcoin_prices.db').st_uid == 10001
conn = sqlite3.connect('/app/data/bitcoin_prices.db')
now = int(time.time())
conn.execute("INSERT INTO price_snapshots(fetched_at_unix, average_price, spread, warnings_json, refreshed_source) VALUES (?, 100150, 300, '[]', 'all')", (now,))
snapshot = conn.execute('SELECT last_insert_rowid()').fetchone()[0]
for index, (name, kind) in enumerate([('CoinGecko', 'aggregate'), ('Coinbase', 'spot'), ('Kraken', 'last_trade'), ('Gemini', 'bid')]):
    conn.execute('INSERT INTO source_prices(snapshot_id, source, price_usd, last_success_at_unix, quote_kind) VALUES (?, ?, ?, ?, ?)', (snapshot, name, 100000 + index * 100, now, kind))
    conn.execute("UPDATE provider_health SET attempt_outcome='success', attempted_at_unix=?, error_category=NULL, error_message=NULL, http_status=NULL WHERE source=?", (now, name))
conn.commit()
status, data = request('/api/price')
assert status == 200 and data['status'] == 'LIVE' and data['active_viewers'] == 0
for active in [True, False, True, False]:
    assert request('/api/presence', {'session_id':'runtime-test', 'active':active})[0] == 200
    status, data = request('/api/price')
    assert status == 200 and data['active_viewers'] == int(active)
    assert data['average_price'] == 100150 and not data['refresh_succeeded']
    assert all(health['last_attempt']['outcome'] == 'success' for health in data['provider_health'])
assert request('/health') == (200, {'status':'ok'})
print('Seeded quote serving and pause/resume passed without upstream calls.')
''', mount=True)
    command('stop', '--time', '10', container)
    assert command('inspect', '--format', '{{.State.ExitCode}}', container).stdout.strip() == '0'
    assert 'HTTP shutdown completed' in command('logs', container).stdout
    command('start', container)
    probe(http + '''
import sqlite3
conn = sqlite3.connect('/app/data/bitcoin_prices.db')
assert conn.execute('PRAGMA user_version').fetchone()[0] == 1
assert conn.execute('SELECT COUNT(*) FROM price_snapshots').fetchone()[0] == 1
status, data = request('/api/price')
assert status == 200 and data['active_viewers'] == 0
assert len(data['sources']) == 4
assert [source['price_usd'] for source in data['sources']] == [100000, 100100, 100200, 100300]
assert all(health['last_attempt']['outcome'] == 'success' for health in data['provider_health'])
conn.execute('ALTER TABLE provider_health RENAME TO unavailable_health')
conn.commit()
assert request('/health') == (503, {'status':'unavailable'})
print('Graceful shutdown, restart persistence and unhealthy database detection passed.')
''', mount=True)
    assert command('exec', container, 'bitcoin_price_tracker', '--healthcheck', check=False).returncode != 0
    probe("import sqlite3; c=sqlite3.connect('/app/data/bitcoin_prices.db'); c.execute('ALTER TABLE unavailable_health RENAME TO provider_health'); c.commit()", mount=True)
    command('exec', container, 'bitcoin_price_tracker', '--healthcheck')
    invalid = command('run', '--rm', '--network', 'none', '-e', 'BIND_ADDRESS=invalid', args.image, check=False)
    assert invalid.returncode != 0 and 'BIND_ADDRESS' in invalid.stdout + invalid.stderr
    print('Health probe recovery and configuration failure passed.')
    # Exercise the documented bind-mount preparation as well as volume copy-up.
    with tempfile.TemporaryDirectory(prefix='btc-bind-test-') as directory:
        data_dir = Path(directory) / 'data'
        data_dir.mkdir()
        bind_container = None
        try:
            command('run', '--rm', '--network', 'none', '--user', '0:0', '-v', str(data_dir) + ':/app/data',
                    args.image, 'chown', '10001:10001', '/app/data')
            bind_container = command('run', '-d', '--network', 'none', '-v', str(data_dir) + ':/app/data', args.image).stdout.strip()
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                if command('exec', bind_container, 'bitcoin_price_tracker', '--healthcheck', check=False).returncode == 0:
                    break
                time.sleep(.25)
            else:
                raise RuntimeError('Owned bind mount did not become healthy')
            command('stop', '--time', '35', bind_container)
            assert command('inspect', '--format', '{{.State.ExitCode}}', bind_container).stdout.strip() == '0'
            print('Non-root bind-mount ownership and startup passed.')
        finally:
            if bind_container:
                command('rm', '-f', bind_container, check=False)
            # Clear only this temporary test directory from the engine namespace,
            # so rootless UID mappings cannot leave host-inaccessible test files.
            command('run', '--rm', '--network', 'none', '--user', '0:0', '-v', str(data_dir) + ':/mount',
                    helper, 'python3', '-c',
                    "from pathlib import Path; import shutil; [(shutil.rmtree(p) if p.is_dir() else p.unlink()) for p in Path('/mount').iterdir()]")

except subprocess.CalledProcessError as error:
    raise RuntimeError(error.stderr or error.stdout) from error
finally:
    if container:
        command('stop', '--time', '10', container, check=False)
        command('rm', '-f', container, check=False)
    if volume_created:
        command('volume', 'rm', volume, check=False)
