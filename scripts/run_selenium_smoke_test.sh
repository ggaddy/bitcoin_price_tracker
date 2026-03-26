#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SELENIUM_CONTAINER_NAME="${SELENIUM_CONTAINER_NAME:-btc-selenium}"
SELENIUM_IMAGE="${SELENIUM_IMAGE:-docker.io/selenium/standalone-chrome:latest}"
WEBDRIVER_URL="${WEBDRIVER_URL:-http://127.0.0.1:4444}"
SELENIUM_APP_HOST="${SELENIUM_APP_HOST:-127.0.0.1}"
SELENIUM_WAIT_SECONDS="${SELENIUM_WAIT_SECONDS:-60}"

if [[ -n "${CONTAINER_ENGINE:-}" ]]; then
  ENGINE="$CONTAINER_ENGINE"
elif command -v podman >/dev/null 2>&1; then
  ENGINE="podman"
elif command -v docker >/dev/null 2>&1; then
  ENGINE="docker"
else
  echo "No supported container engine found. Set CONTAINER_ENGINE or install podman/docker." >&2
  exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is required to run the Selenium smoke test." >&2
  exit 1
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required to wait for WebDriver readiness." >&2
  exit 1
fi

STARTED_CONTAINER=0
cleanup() {
  if [[ "$STARTED_CONTAINER" == "1" ]]; then
    "$ENGINE" rm -f "$SELENIUM_CONTAINER_NAME" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

if "$ENGINE" ps -a --format '{{.Names}}' | grep -Fxq "$SELENIUM_CONTAINER_NAME"; then
  echo "Removing existing container: $SELENIUM_CONTAINER_NAME"
  "$ENGINE" rm -f "$SELENIUM_CONTAINER_NAME" >/dev/null
fi

echo "Starting Selenium container with $ENGINE: $SELENIUM_IMAGE"
"$ENGINE" run --rm -d --name "$SELENIUM_CONTAINER_NAME" --network host "$SELENIUM_IMAGE" >/dev/null
STARTED_CONTAINER=1

export WEBDRIVER_URL
export SELENIUM_APP_HOST
export SELENIUM_WAIT_SECONDS

python3 - <<'PY'
import json
import os
import time
import urllib.request

url = os.environ["WEBDRIVER_URL"].rstrip("/") + "/status"
timeout = int(os.environ["SELENIUM_WAIT_SECONDS"])
last_error = None
for _ in range(timeout):
    try:
        with urllib.request.urlopen(url, timeout=2) as response:
            data = json.load(response)
        if data.get("value", {}).get("ready") is True:
            print(f"WebDriver ready at {url}")
            raise SystemExit(0)
        last_error = data
    except Exception as exc:
        last_error = str(exc)
    time.sleep(1)
print(f"WebDriver did not become ready within {timeout}s: {last_error}")
raise SystemExit(1)
PY

cd "$ROOT_DIR"
echo "Running Selenium smoke test"
cargo test selenium_dashboard_smoke_test -- --ignored "$@"
