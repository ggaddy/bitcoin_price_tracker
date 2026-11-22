# Security policy

This document describes the repository's active security controls, automated
advisory checks, and runtime exception policy. Dated review logs and edge
observations live in [docs/security/review-history.md](docs/security/review-history.md);
the archived September 2026 audit is in
[docs/archive/security-audit-2026-09.md](docs/archive/security-audit-2026-09.md).
Setup and runtime operations are in [docs/operations.md](docs/operations.md);
current work is tracked in Beads.

## Dependency advisories

[CI](.github/workflows/ci.yml) runs `cargo audit --deny warnings` and
`npm audit --audit-level=low` on pull requests, pushes to `master`, and weekly.
Both jobs fail visibly without `continue-on-error`. RustSec uses `cargo-audit`
`0.23.0` with the application's pinned Rust `1.100.0` toolchain. Browser test
packages in `tests/browser` are included in the npm scan.

There are currently no accepted Rust or npm advisories. Before adding an
advisory ID to `.cargo/audit.toml`, record its affected dependency/path, why
upgrading or removing it is not possible, actual exposure, mitigations, owner,
expiry date, and removal issue here and in
[review history](docs/security/review-history.md). Remove the exception once a
fix is available. Never suppress a scanner failure merely to pass CI.

## Runtime images and release checks

The [Dockerfile](Dockerfile) builds on a digest-pinned Rust `1.100.0` image,
prunes a digest-pinned Debian 13 distroless `cc` filesystem with
[scripts/prune_runtime.py](scripts/prune_runtime.py) to retain only the
executable's four-library native dependency chain and reviewed built-in
files/DNS NSS configuration, and assembles the final `FROM scratch` image
running as UID/GID `10001:10001` with `LC_ALL=C` and no shell or package
manager. [Dependabot](.github/dependabot.yml) checks Docker digests, GitHub
Actions, Cargo, and browser npm dependencies weekly. Deployments must still
enforce a read-only root filesystem, drop capabilities, disable privilege
escalation and service-account token mounts, and restrict network access; an
image alone does not provide workload isolation.

Release tags (`vMAJOR.MINOR.PATCH`) trigger
[.github/workflows/container.yml](.github/workflows/container.yml), which reruns
exact-commit CI, verifies the tagged commit matches current `master`, builds a
local candidate, runs [scripts/verify_runtime.py](scripts/verify_runtime.py)
and [scripts/verify_runtime_network.py](scripts/verify_runtime_network.py),
scans that exact candidate with Grype `v0.121.3`, checks exception expiry via
[scripts/check_image_exceptions.py](scripts/check_image_exceptions.py), and
publishes that same candidate without rebuilding. PR/master CI scans its built
image; weekly CI also resolves and scans the published `latest` digest.
Unreviewed High/Critical findings fail these jobs, including unfixed findings;
lower severities remain visible in logs.

Checked-in [GitHub ruleset snapshots](.github/rulesets) require a pull request,
resolved review threads, and all five CI checks (`rust`, `browser`,
`advisories`, `npm-advisories`, `runtime`) against an up-to-date `master`
branch without admin bypass (zero external approvals for a sole maintainer).
Separate rulesets restrict `v*` tag creation to admins and forbid tag moves or
deletion. Verify live repository settings directly in GitHub when needed, and
protect the maintainer account and `DOCKERHUB_TOKEN` secret.

## Runtime advisory policy

Authoritative exception matchers and the enforced review deadline live in
[.grype.yaml](.grype.yaml).

- **Current review deadline:** before **2026-12-07** (enforced by
  [scripts/check_image_exceptions.py](scripts/check_image_exceptions.py) in CI
  and release workflows).
- **Owner:** repository maintainer `ggaddy`.
- **Next review tracking:** Beads `bitcoin_price_tracker-12z`.
- **Latest review evidence:**
  [November 10, 2026 full exception renewal](docs/security/review-history.md#november-10-2026-full-exception-renewal)
  and [exception-renewal-2026-11-10.json](docs/security/exception-renewal-2026-11-10.json).

Retained exceptions are **applicability decisions**, not patched packages or a
clean scan. Every matcher in `.grype.yaml` must pin the exact CVE, Debian binary
package name, exact version, and `deb` type while keeping `show-suppressed: true`
and the `high` failure cutoff. Remove each exception as soon as a supported base
image fixes or removes the package (as done when zlib and unused C++/OpenMP
libraries were pruned in the
[November 7 library audit](docs/security/review-history.md#november-7-2026-complete-runtime-library-audit)),
and reassess immediately on any runtime, native-dependency, locale, NSS, or
environment change.

### Active runtime image exceptions

All 7 exact-version matchers in [.grype.yaml](.grype.yaml) cover five Debian 13
advisories with no supported trixie fix as of the 2026-11-10 review:

| Advisory | Exact Debian package(s) / version | Mitigating guard & applicability | Removal tracking |
| --- | --- | --- | --- |
| [CVE-2026-84243](https://security-tracker.debian.org/tracker/CVE-2026-84243) | `libc6` `2.41-12+deb13u4` | Image sets `LANG=C` and `LC_ALL=C`; `verify_runtime.py` enforces `LC_ALL=C` and rejects gettext/text-domain and locale-selection imports. No client-controlled env/locale or SSH. | `bitcoin_price_tracker-wbt`, [GitHub #40](https://github.com/ggaddy/bitcoin_price_tracker/issues/40) |
| [CVE-2026-5435](https://security-tracker.debian.org/tracker/CVE-2026-5435) | `libc6` `2.41-12+deb13u4` | `verify_runtime.py` rejects DNS-printing imports/exports and `libresolv` is absent; built-in files/DNS lookup verified by `verify_runtime_network.py`. | `bitcoin_price_tracker-w9n` |
| [CVE-2026-19499](https://security-tracker.debian.org/tracker/CVE-2026-19499) | `libc6` `2.41-12+deb13u4` | `verify_runtime.py` rejects `strfmon` / `strfmon_l` imports; currencies format in the browser with `Intl.NumberFormat`. | `bitcoin_price_tracker-w9n` |
| [CVE-2026-95619](https://security-tracker.debian.org/tracker/CVE-2026-95619) | `gcc-14-base`, `libgcc-s1` `14.2.0-19` | `gcc-14-base` is docs/licensing only; `libgcc-s1` is unwinding only. `libstdc++`/`libgomp` are absent and `verify_runtime.py` rejects C++ (`_Z*`) imports. | `bitcoin_price_tracker-hcw` |
| [CVE-2026-102010](https://security-tracker.debian.org/tracker/CVE-2026-102010) | `gcc-14-base`, `libgcc-s1` `14.2.0-19` | No C++ runtime or `libstdc++` container is shipped or linked; enforced by `verify_runtime.py`. | `bitcoin_price_tracker-hcw` |

[scripts/verify_runtime.py](scripts/verify_runtime.py) inspects the exact image
binary and filesystem to enforce the five-ELF inventory (executable plus
`libc`, `libm`, `libgcc_s`, and dynamic loader), retired-package absence,
symbol-import guards, built-in NSS configuration, and `LC_ALL=C`. Bundled SQLite
still imports `dlopen`/`dlsym` (`unixDlOpen`); SQLite extension loading is not
enabled and SQL queries are fixed and parameterized. These guards support
applicability reviews; they do not prove every indirect shared-library path safe.

## Application containment

- **Upstream requests:** Fixed endpoints in [src/config.rs](src/config.rs);
  HTTPS-only via Rustls, redirects disabled, 2s connect / 5s total timeout
  (12s for RSS), 64 KiB cap for price responses, 2 MiB cap for RSS feeds.
- **Origin admission & HTTP/1 bounds:** Connection admission happens before HTTP
  parsing (`MAX_CONNECTIONS` remote slots plus up to 4 separate loopback slots
  for exec health probes), with 5s header timeout, 64 max headers, 16 KiB read
  buffer, 25s max connection lifetime, one request per connection, and a 1,024-byte
  request body limit ([src/lifecycle.rs](src/lifecycle.rs),
  [src/handlers.rs](src/handlers.rs)). See
  [docs/operations.md](docs/operations.md#configuration-and-limits).
- **Browser security headers:** Responses send strict CSP (`default-src 'none'`,
  same-origin scripts/styles/fonts/connections, no inline scripts,
  `frame-ancestors 'none'`), `X-Content-Type-Options: nosniff`,
  `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, and `Cache-Control: no-store`
  on `/api/*` and `/health` ([src/ui.rs](src/ui.rs)).
- **Edge and infrastructure boundary:** No limiter trusts forwarded IP headers
  or tracks per-client keys; enforce TLS and per-client abuse controls at the
  reverse proxy/edge. Treat the tracker as public for abuse controls. Kubernetes
  manifests and rollout evidence live in the separate `ggaddy/kube-lan-rke2`
  repository. Containers use the node clock and require no NTP egress or
  clock-setting capability.
