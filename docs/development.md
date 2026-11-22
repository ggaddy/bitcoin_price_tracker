# Development, testing, and releases

Run all commands from the repository root unless noted otherwise.

## Rust checks

[rust-toolchain.toml](../rust-toolchain.toml) pins Rust `1.100.0`, `rustfmt`,
and `clippy`, matching `rust-version` in [Cargo.toml](../Cargo.toml). Always use
locked dependencies:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Ordinary Rust tests use local mock providers, injected clocks, and temporary
SQLite files; they do not contact public upstream APIs.

## Browser tests

The deterministic Chromium suite requires Node.js 20+ (CI uses `26.13.0`) and
pinned Playwright (`1.65.1` in [tests/browser/package.json](../tests/browser/package.json)):

```bash
npm ci --prefix tests/browser
npm exec --prefix tests/browser -- playwright install chromium
npm test --prefix tests/browser
```

On Linux hosts missing browser system libraries, run:

```bash
npm exec --prefix tests/browser -- playwright install --with-deps chromium
```

The suite serves actual dashboard assets locally, mocks API responses, rejects
external network requests, and covers freshness, recovery, deadlines, tab
visibility, identity fallbacks, safe rendering, reduced motion, RSS, and narrow
layouts.

Optional test environment variables:

- `BTC_APP_URL=http://127.0.0.1:3000`: test embedded assets served by a running
  Rust process (APIs remain mocked).
- `BTC_SCREENSHOT_DIR=/existing/directory`: save `desktop.png` and `narrow.png`
  captures.

## Load-harness tests

CI runs the bounded Python unit tests for the local origin load harness:

```bash
python3 -m unittest discover -s scripts -p test_load_test.py
```

For full local capacity and contention runs, see
[docs/load-testing.md](load-testing.md).

## Container verification and Selenium

### Runtime image verification

With Docker or Podman, Python 3, and host `readelf` (from `binutils`), verify a
built image (such as `btc-matrix` from [docs/operations.md](operations.md#named-volume-recommended)):

```bash
python3 scripts/verify_runtime.py --engine docker --image btc-matrix
```

[scripts/verify_runtime.py](../scripts/verify_runtime.py) creates disposable
containers/volumes with `--network none`, checks the complete five-ELF inventory
(application plus four native libraries), retired-package absence, native symbol
imports, built-in NSS configuration, `LC_ALL=C`, non-root UID/GID `10001:10001`,
`--healthcheck`, seeded quotes, restart persistence, storage failure/recovery,
bind mounts, and graceful shutdown, then cleans up its own resources.

Also verify the trimmed image's resolver and TLS behavior (requires host Python 3
and OpenSSL; no host Rust toolchain required):

```bash
python3 scripts/verify_runtime_network.py --engine docker --image btc-matrix
```

[scripts/verify_runtime_network.py](../scripts/verify_runtime_network.py)
compiles a disposable validation helper with the pinned build stage and locked
dependencies and runs it inside the final image against loopback host-file, DNS
(A records and AAAA no-data fallback), and private-CA HTTPS fixtures with no
external runtime network. It verifies certificate-name/untrusted-CA rejection
and the libraries actually loaded after those operations. New native
dependencies, non-C locales, extra NSS backends, or native plugins require a new
library review.

Also verify that runtime advisory exceptions have not expired:

```bash
python3 scripts/check_image_exceptions.py
```

### Selenium smoke test

The separate Rust Selenium smoke test needs Docker or Podman and Python 3 (no
host Cargo required):

```bash
scripts/run_selenium_smoke_test.sh
```

[scripts/run_selenium_smoke_test.sh](../scripts/run_selenium_smoke_test.sh)
runs version-tagged Selenium (`4.52.0-20261101`) and Rust (`1.100.0-bookworm`)
containers sharing one network namespace on an ephemeral loopback port. It
requires bind mounts and `--network container:<id>` support (Linux engines,
including WSL).

Overrides: `CONTAINER_ENGINE`, `SELENIUM_IMAGE`, `SELENIUM_CONTAINER_NAME`,
`SELENIUM_WAIT_SECONDS` (`1`–`300`, default `60`), and `RUST_TEST_IMAGE`. An
existing conflicting container name is rejected rather than removed; cleanup
only removes IDs created by the script.

With a manually managed WebDriver and host Cargo, set `WEBDRIVER_URL` (default
`http://127.0.0.1:4444`) and `SELENIUM_APP_HOST` (default
`host.containers.internal`), then run:

```bash
cargo test --locked selenium_dashboard_smoke_test -- --ignored
```

## Optional compiler cache

[Kache](https://github.com/kunobi-ninja/kache) is optional for local builds.
Install a prebuilt release, or with the pinned Rust `1.100.0` toolchain (Rust
1.95+):

```bash
cargo install kache --locked --version 2.0.0
kache init
```

Use `kache doctor` and `kache stats` to inspect setup and cache hits.

## CI and advisory checks

[.github/workflows/ci.yml](../.github/workflows/ci.yml) runs on pull requests,
pushes to `master`, weekly (`23 8 * * 1`), and manual dispatch with pinned
action SHAs and read-only `contents` permissions:

| Job | Checks |
| --- | --- |
| `rust` | Rust `1.100.0` (with Kache `2.0.0`): `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked` |
| `browser` | Node `26.13.0` + Playwright Chromium: `npm test --prefix tests/browser` |
| `advisories` | Rust `1.100.0` + `cargo-audit` `0.23.0`: `cargo +1.100.0 audit --deny warnings` |
| `npm-advisories` | Node `26.13.0`: `npm audit --prefix tests/browser --audit-level=low` |
| `runtime` | `test_load_test.py`, `docker build`, `verify_runtime.py`, `verify_runtime_network.py`, `check_image_exceptions.py`, Grype `v0.121.3` (`severity-cutoff: high`, `.grype.yaml`) |
| `published-image` | Weekly schedule only: `check_image_exceptions.py` + Grype scan of resolved `agaddy/bitcoin_price_tracker:latest` digest |

There are no accepted Rust or npm advisories. Runtime-image applicability
exceptions are governed by [SECURITY.md](../SECURITY.md) and
[.grype.yaml](../.grype.yaml).

## Releases

Publishing requires the `DOCKERHUB_TOKEN` GitHub Actions secret with push
access to `agaddy/bitcoin_price_tracker`.

1. Merge changes through a passing pull request and check out clean, up-to-date
   `master`.
2. Choose an unused stable `vMAJOR.MINOR.PATCH` tag, then tag and push:

```bash
: "${release_tag:?Set release_tag to the new stable release tag}"
git tag "$release_tag"
git push origin "$release_tag"
```

[.github/workflows/container.yml](../.github/workflows/container.yml) verifies
the tagged commit equals `origin/master`, reruns exact-commit CI validation,
builds a local candidate image, runs `verify_runtime.py`,
`verify_runtime_network.py`, Grype `v0.121.3`, and `check_image_exceptions.py`,
and pushes that exact candidate without rebuilding as `<version>` (without
leading `v`) and `latest`. Branch pushes and prerelease tags do not publish
images.

Checked-in [GitHub ruleset snapshots](../.github/rulesets) enforce PR checks on
`master` and restrict `v*` tag creation to admins while blocking tag moves and
deletions; see [SECURITY.md](../SECURITY.md#runtime-images-and-release-checks).
Git tagging, Docker Hub image publication, GitHub Release creation, and
production deployment (`ggaddy/kube-lan-rke2`) are separate steps.
