#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SELENIUM_CONTAINER_NAME="${SELENIUM_CONTAINER_NAME:-btc-selenium-$$-$RANDOM}"
SELENIUM_IMAGE="${SELENIUM_IMAGE:-docker.io/selenium/standalone-chrome:4.48.0-20260905}"
RUST_TEST_IMAGE="${RUST_TEST_IMAGE:-docker.io/library/rust:1.85.1-bookworm}"
SELENIUM_WAIT_SECONDS="${SELENIUM_WAIT_SECONDS:-60}"
ENGINE="${CONTAINER_ENGINE:-}"
if [[ -z "$ENGINE" ]]; then
  if command -v podman >/dev/null 2>&1; then ENGINE=podman
  elif command -v docker >/dev/null 2>&1; then ENGINE=docker
  else echo 'Install Docker or Podman, or set CONTAINER_ENGINE.' >&2; exit 1; fi
fi
command -v python3 >/dev/null || { echo 'python3 is required.' >&2; exit 1; }
# Names are never used for cleanup: only IDs returned by our own creation calls.
SELENIUM_ID=""
RUST_ID=""
cleanup() {
  if [[ -n "$RUST_ID" ]]; then "$ENGINE" rm -f "$RUST_ID" >/dev/null 2>&1 || true; fi
  if [[ -n "$SELENIUM_ID" ]]; then "$ENGINE" rm -f "$SELENIUM_ID" >/dev/null 2>&1 || true; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
if "$ENGINE" container inspect "$SELENIUM_CONTAINER_NAME" >/dev/null 2>&1; then
  echo "Container name already exists: $SELENIUM_CONTAINER_NAME. Choose another name; nothing was removed." >&2
  exit 1
fi
SELENIUM_ID="$("$ENGINE" create --name "$SELENIUM_CONTAINER_NAME" --shm-size=2g -p 127.0.0.1::4444 "$SELENIUM_IMAGE")"
"$ENGINE" start "$SELENIUM_ID" >/dev/null
SELENIUM_PORT="$("$ENGINE" port "$SELENIUM_ID" 4444/tcp)"
export WEBDRIVER_URL="http://$SELENIUM_PORT"
export SELENIUM_WAIT_SECONDS
python3 - <<'PY'
import json, os, time, urllib.request
seconds = int(os.environ['SELENIUM_WAIT_SECONDS'])
if not 1 <= seconds <= 300: raise SystemExit('SELENIUM_WAIT_SECONDS must be between 1 and 300')
deadline = time.monotonic() + seconds
while time.monotonic() < deadline:
    try:
        with urllib.request.urlopen(os.environ['WEBDRIVER_URL'] + '/status', timeout=2) as response:
            if json.load(response).get('value', {}).get('ready'):
                break
    except Exception:
        pass
    time.sleep(.5)
else: raise SystemExit('WebDriver did not become ready before the deadline')
PY
# Share Selenium's network namespace, so both the fixture app and WebDriver use
# loopback on Docker/Podman, without host networking or a host Rust installation.
RUST_ID="$("$ENGINE" create --network "container:$SELENIUM_ID" \
  -v "$ROOT_DIR:/app" -w /app \
  -e WEBDRIVER_URL=http://127.0.0.1:4444 -e SELENIUM_APP_HOST=127.0.0.1 \
  "$RUST_TEST_IMAGE" cargo test --locked --release selenium_dashboard_smoke_test -- --ignored "$@")"
"$ENGINE" start --attach "$RUST_ID"
EXIT_CODE="$("$ENGINE" inspect --format '{{.State.ExitCode}}' "$RUST_ID")"
exit "$EXIT_CODE"
