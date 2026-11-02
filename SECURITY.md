# Security policy and review history

The policy sections describe the repository's checked-in checks and requirements.
[Recorded reviews](#recorded-dependency-and-applicability-reviews) and
[edge observations](#tracker-edge-policy--verified-october-11-2026-utc) are dated
evidence, not fresh scans or a live deployment inventory. Current work is tracked
in Beads; setup and operations are in [README.md](README.md).

## Dependency advisories

CI runs `cargo audit --deny warnings` and `npm audit --audit-level=low` on pull
requests, pushes to master, and weekly. These jobs fail visibly; neither uses
`continue-on-error`. RustSec uses cargo-audit 0.22.2 with Rust 1.94.0, separately
from the application's pinned Rust 1.85.1 toolchain, to read current advisory
formats. Browser test packages are included in the npm scan.

There are currently no accepted Rust or npm advisories. Before adding any advisory ID to
`.cargo/audit.toml`, record its affected dependency/path, why upgrading or removing
it is not currently possible, actual exposure, mitigations, an owner, a review
or expiry date, and a tracked removal issue here. Remove the exception when the
fix is available. Do not suppress a scanner failure merely to make CI pass.

## Runtime images and release checks

The runtime uses digest-pinned Debian 13 distroless `cc`, UID/GID 10001, and no
shell or package manager. Dependabot checks Docker digests, Actions, Cargo, and
browser dependencies weekly. Deployments must still enforce a read-only root,
drop capabilities, disable privilege escalation and API token mounts, and restrict
network access; an image alone does not provide workload isolation.

Release tags run the reusable CI workflow for that exact commit. Publication
requires the commit to be current `master`, then builds a local candidate, runs
the runtime checks, and scans that same image with Grype 0.118.0 before logging
in and pushing it without a rebuild. PR/master CI also scans its final image;
weekly CI additionally resolves and scans the published `latest` digest.
Unreviewed High/Critical findings fail these jobs, including findings without
a vendor fix. All other severities remain visible for review.

The checked-in [GitHub ruleset snapshots](.github/rulesets) require a PR,
resolved review threads, and five GitHub Actions checks against an up-to-date
branch for `master`, without an admin bypass. They require zero external
approvals for a sole maintainer: validation enforcement, not two-person review.
Separate snapshots restrict `v*` tag creation to admins and prohibit tag moves or
deletion, including for admins. These files record the expected protections;
verify live settings in GitHub when needed. Repository settings remain externally
configurable; secure the maintainer's GitHub account and Docker Hub token.

## Runtime advisory policy

The authoritative exception matchers and review deadline are in
[.grype.yaml](.grype.yaml). The checked-in deadline is **2026-11-10**; review must
finish before that date. [The deadline checker](scripts/check_image_exceptions.py)
fails CI/release checks on or after expiry. Owner: repository maintainer `ggaddy`.
Beads issue `bitcoin_price_tracker-5wx` tracks the next review;
`bitcoin_price_tracker-wbt` and [GitHub #40](https://github.com/ggaddy/bitcoin_price_tracker/issues/40)
track removal of the CVE-2026-84243 exception.

Retained exceptions are applicability decisions, not patched packages or a clean
scan. Match only the reviewed advisory, Debian binary package, and exact version;
keep suppressed matches visible and retain the High/Critical failure threshold.
Record exposure, mitigations, owner, review date, and removal tracking for every
exception. Remove an exception when a supported image fixes/removes the affected
package, and reassess it when runtime/native dependencies or configuration change.

[Runtime verification](scripts/verify_runtime.py) inspects the exact image's
executable for reviewed native imports and linked libraries, rejects C++/gettext
imports, and requires `LC_ALL=C`. These guards support the exposure reviews; they
do not prove every indirect or dynamically loaded library path safe. A review
must also check fresh scan/vendor evidence for both the candidate and published
images. This documentation cleanup does not renew any exception or establish
current upstream fix availability.

## Recorded dependency and applicability reviews

Each subsection records conditions at its named review date. Package versions,
fix availability, scan counts, older deadlines, and deployment references below
are historical; the current matcher inventory/deadline comes from the policy
above and `.grype.yaml`. Preserve this evidence when a later review supersedes it.

### September 2026 dependency baseline

The P5 dependency update removes the unmaintained `paste` dependency by upgrading
the Selenium client to thirtyfour 0.36.3 with its automatic driver downloader
disabled. The runner supplies a pinned Selenium container. Patched locked
versions include quinn-proto 0.11.15, rustls-webpki 0.103.13, and rand 0.9.3.
See the RustSec database for [quinn-proto](https://rustsec.org/advisories/RUSTSEC-2026-0185.html),
[rustls-webpki](https://rustsec.org/advisories/RUSTSEC-2026-0104.html),
[rand](https://rustsec.org/advisories/RUSTSEC-2026-0097.html), and
[paste](https://rustsec.org/advisories/RUSTSEC-2024-0436.html).

The September update locked Rustls to 0.23.45 (with rustls-webpki 0.103.15), fixing
[RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285.html).
The audit still fails on every unaccepted advisory.

### November 2026 dependency refresh

The November 1 update locks quinn-proto to 0.11.18, Rustls to 0.23.46,
rustls-webpki to 0.103.16, and both webpki-roots and webpki-root-certs to
1.0.10. These versions address RUSTSEC-2026-0338 through RUSTSEC-2026-0342,
RUSTSEC-2026-0346 through RUSTSEC-2026-0349, and RUSTSEC-2026-0381 through
RUSTSEC-2026-0383. Tokio is locked to the patched 1.51.6 maintenance release
for [RUSTSEC-2026-0386](https://rustsec.org/advisories/RUSTSEC-2026-0386.html),
preserving the application's Rust 1.85.1 toolchain. No Rust advisory exceptions
are added; CI continues to deny all advisory warnings.

### AUD-07 runtime advisory review

Review: 2026-09-08. Owner: repository maintainer `ggaddy`. Tracking: AUD-07 in
[the audit](SECURITY_AUDIT.md). Review before **2026-10-08**, enforced in CI by
`scripts/check_image_exceptions.py`. Remove each exception as soon as a supported
base digest fixes/removes the package; re-evaluate it on any dependency/runtime
change. Exceptions match both the CVE and exact Debian binary package version.

The first hardened-image scan found 20 package/advisory matches, down from 225
in the deployed Debian image. PCRE2, its available security update, and the
general-purpose utilities are absent from the new image. Four High/Critical
matches remain in the supported distroless base, with no fixed version offered
by the scanner for Debian 13:

| Finding | Package/version | Exposure review |
| --- | --- | --- |
| [CVE-2026-5450](https://security-tracker.debian.org/tracker/CVE-2026-5450) | `libc6` `2.41-12+deb13u3` | Requires the scanf family with a particular malloc character format/width. The release binary imports no scanf-family function. JSON/numeric parsing uses Rust; no shell/utility exposes this interface. |
| [CVE-2026-5928](https://security-tracker.debian.org/tracker/CVE-2026-5928) | `libc6` `2.41-12+deb13u3` | Requires `ungetwc` and particular overlapping character encodings. The binary imports no wide-stream functions, and request/provider data is handled as UTF-8. |
| [CVE-2026-5435](https://security-tracker.debian.org/tracker/CVE-2026-5435) | `libc6` `2.41-12+deb13u3` | Concerns deprecated DNS printing functions `ns_printrr`, `ns_printrrf`, and `fp_nquery`. None is imported; normal hostname resolution does not print DNS records through them. |
| [CVE-2026-85091](https://security-tracker.debian.org/tracker/CVE-2026-85091) | `zlib1g` `1:1.3.dfsg+really1.3.1-1+b1` | Concerns nonblocking gzip writes. The binary links only libc/libm/libgcc/the loader, does not load zlib, and provides no gzip-writing interface. TLS uses Rustls. |

### Debian 13 digest update review

Re-reviewed on 2026-09-21 for distroless digest
`54df941ed0d06a1bd95ef5e0ce391fd8d9f94b64782dc9a60062727849ee3f97`
and `libc6` `2.41-12+deb13u4`. Owner and tracking remain `ggaddy` / AUD-07;
the **2026-10-08** review deadline is unchanged.

The updated image no longer reports CVE-2026-5450 or CVE-2026-5928. Its two
unsuppressed High findings concern these interfaces:

| Finding | Package/version | Exposure review |
| --- | --- | --- |
| [CVE-2026-5435](https://security-tracker.debian.org/tracker/CVE-2026-5435) | `libc6` `2.41-12+deb13u4` | Deprecated DNS-printing APIs `ns_printrr`, `ns_printrrf`, and `fp_nquery`. The release executable imports none of them. Normal hostname resolution is a different interface. |
| [CVE-2026-19499](https://security-tracker.debian.org/tracker/CVE-2026-19499) | `libc6` `2.41-12+deb13u4` | Buffer overflow in `strfmon` / `strfmon_l` monetary formatting with right-justified padding. The release executable imports neither interface; displayed currencies are formatted in the browser with `Intl.NumberFormat`. |

Debian lists both as vulnerable in trixie with `<no-dsa>` (minor issue); fixes
are listed for unstable, not the supported Debian 13 base. These are scoped
applicability exceptions, **not patched packages**. No severity threshold or
fix-state filter is relaxed. The existing exact-version `deb13u3` exceptions
remain for the weekly scan of the previously published image until that image
is replaced. The unchanged zlib exception still applies to the new image.

`scripts/verify_runtime.py` extracts the exact image's release executable and
uses `readelf` to reject imports of the above APIs
and linked libraries outside the reviewed libc/libm/libgcc/loader set. This
keeps future dependency changes from silently invalidating the import review;
it is not a general proof that a shared library is free of vulnerable code.
The executable does import `dlopen`/`dlsym` (bundled SQLite includes
`unixDlOpen`), so the import check does not prohibit all runtime loading.
Application code does not enable SQLite extension loading or expose arbitrary
SQL, plugin paths, monetary-format strings, or DNS-printing interfaces.
Runtime/release CI runs this check before the image scan. Maintainers also
must reassess exposure on runtime changes and remove exceptions when a
supported image fixes or removes the affected packages.

`.grype.yaml` keeps reviewed findings visible in scan output and matches the
CVE, exact binary package version, and package type. Other findings remain
unsuppressed; no blanket `only-fixed` or OS exclusion is used.

### October 2026 release review

Re-reviewed on 2026-10-10 for the UI release. Owner: `ggaddy`; tracking:
AUD-07 in [the audit](SECURITY_AUDIT.md). Review before **2026-11-10**;
`scripts/check_image_exceptions.py` enforces this deadline. Remove exceptions
when supported packages fix the findings, and reassess on runtime changes.

The previous pinned distroless image contains `libssl3t64`
`3.5.7-1~deb13u2`, with four High findings fixed in `3.5.7-1~deb13u3`.
The updated Debian 13 nonroot image index is pinned to
`e792ab3d241a468a4fd7519ddbbebe66b49b5f365771716ea688ad40b6c6f1c2`;
its amd64 manifest is
`20afe6a70f2565277b704cc17289cb557f353a3619f6350f52a2317106598df4`.
Grype 0.118.0 with its 2026-10-10 database no longer reports those four High
OpenSSL findings in this image. They are not excepted.

Debian still lists [CVE-2026-5435](https://security-tracker.debian.org/tracker/CVE-2026-5435)
and [CVE-2026-19499](https://security-tracker.debian.org/tracker/CVE-2026-19499)
against trixie's `libc6` `2.41-12+deb13u4`, and
[CVE-2026-85091](https://security-tracker.debian.org/tracker/CVE-2026-85091)
against the retained zlib version. The existing exact-version exceptions remain:
the release executable imports none of the affected DNS-printing or monetary
formatting functions and does not link zlib. Old `deb13u3` exceptions are removed;
the deployed v2.9.1 image and new candidate both use `deb13u4`.

Two new advisories are attributed to each of `gcc-14-base`, `libgcc-s1`,
`libgomp1`, and `libstdc++6`, all at version `14.2.0-19`:

| Finding | Exposure review |
| --- | --- |
| [CVE-2026-95619](https://security-tracker.debian.org/tracker/CVE-2026-95619) | Integer overflow in the C++ aligned allocation operator. The application uses Rust with C SQLite/ring dependencies, does not link `libstdc++` or `libgomp`, and imports no C++ symbols. The linked `libgcc_s` unwinding library does not implement the affected C++ allocation operator. |
| [CVE-2026-102010](https://security-tracker.debian.org/tracker/CVE-2026-102010) | Use-after-free in the libstdc++ binary-heap priority queue's `erase_if` implementation. No application or dependency code uses this C++ container; no C++ runtime is linked. |

Debian reports no supported trixie fix for these two advisories; newer fixed
GCC packages are in unstable. `.grype.yaml` scopes each new exception to the
specific CVE, Debian binary package, and `14.2.0-19` version. Findings remain
visible and the High/Critical failure threshold remains unchanged.

Runtime verification inspects the exact image executable's imports and linked
libraries. The verification now
also rejects C++ mangled imports, in addition to rejecting linked libraries
outside libc/libm/libgcc/the loader. These checks support the applicability
review; they do not prove every shared-library or dynamically loaded code path
safe. SQLite extension loading remains disabled and no arbitrary plugin/SQL
interface is exposed.

### November 2026 gettext applicability review

Reviewed on 2026-11-01. Owner: `ggaddy`; tracking: AUD-07 and
[removal issue #40](https://github.com/ggaddy/bitcoin_price_tracker/issues/40).
The existing **2026-11-10** review deadline remains enforced.

The current image scan additionally reports
[CVE-2026-84243](https://security-tracker.debian.org/tracker/CVE-2026-84243)
against `libc6` `2.41-12+deb13u4`. Debian lists no fixed version for trixie
or unstable. The latest distroless `cc-debian13:nonroot` index checked on
November 1, `08476c244888bfb425bae1ae4dd7c17f093e972aab8426883d97f3c6f280f7c1`,
still contains that libc version, so updating the base cannot currently remove
this finding.

The advisory concerns gettext loading attacker-controlled message catalogs via
the `LANGUAGE` environment variable, with a non-C message locale. This HTTP
service does not accept client-supplied process environment, locale settings,
or catalog paths. It does not convert HTTP language headers into process
environment variables, use gettext localization, or run an SSH server.
The image now sets `LANG=C` and `LC_ALL=C`; runtime verification requires
`LC_ALL=C` and rejects gettext-family and text-domain imports in the exact
release executable. The existing native-library checks remain enforced.

`.grype.yaml` adds one applicability exception limited to this CVE, Debian
`libc6`, and the exact affected version. This retains vulnerable library code;
the import and locale checks mitigate exposure rather than patching glibc or
proving that every indirect library path is safe. Remove the exception when a
supported base fixes/removes the package, and reassess it if localization,
native dependencies, process-environment handling, or runtime settings change.
Other findings remain visible and the High/Critical threshold is unchanged.

## Application containment

Provider requests require HTTPS, never follow redirects, and read at most 64 KiB
before parsing JSON. Fixed endpoints remain in source. Accepted origin connections
are capped before HTTP parsing, with five-second header and 25-second total
lifetimes, 64 headers, a 16 KiB read buffer, and one request per connection.
Loopback exec health probes have separate connection capacity. Keep TLS and
per-client rate controls at the trusted edge.

The dashboard uses a CSP permitting only its embedded same-origin scripts/styles
and API, with no inline script exception. Responses deny framing and content
type sniffing, suppress referrers, and disable caching for API/health responses.

Kubernetes definitions and their deployment verification belong in the separate
`kube-lan-rke2` repository. The October 11, 2026 dashboard inspection found
Bitcoin's Access policy `home-cluster-gaddy-it-public` is **Allow Everyone**;
anonymous CLI 403 responses do not establish an identity allowlist. The tracker
is treated as public for abuse controls. Node NTP maintains the clock used by
containers; the tracker requires no NTP network access or capability to change
the clock.

## Tracker edge policy — verified October 11, 2026 UTC

This is a historical snapshot, last verified October 11, 2026 UTC. Live edge
configuration and the deployed image were not rechecked during documentation
cleanup. The observations below concern host-scoped rules for `bitcoin.gaddy.it`,
not zone-wide settings or other applications. Supporting evidence is archived in
[AUDIT.md](AUDIT.md) and the `ggaddy/kube-lan-rke2` infrastructure repository.

- **Rate limit:** rule `7e89a32606894c9490a48470d9304473`, expression
  `http.host eq "bitcoin.gaddy.it"`, per IP, 100 requests per 10 seconds;
  block for 10 seconds. Verified a Cloudflare 429 with `Retry-After: 10` and
  recovery to 200. Shared NAT users share one budget (about 25 normally polling
  tabs before asset/news overhead); observe and tune before expanding traffic.
  [Cloudflare counters](https://developers.cloudflare.com/waf/rate-limiting-rules/)
  are edge-location scoped and can lag; this is an additional safeguard alongside
  bounded origin limits, not a distributed-attack or per-user fairness guarantee.
- **HSTS:** rule `235ef42287bd4136850326db027ec879`, expression
  `http.host eq "bitcoin.gaddy.it" and ssl`, sets
  `Strict-Transport-Security: max-age=86400`. The Kubernetes HTTPS route for
  `bitcoin.k8s.gaddy.it` adds the same header. Initial policy is one day without
  `includeSubDomains` or preload; confirm continued HTTPS availability before
  extending it. Verified on successful HTTPS responses; LAN HTTP has no HSTS.
- **Cache policy:** rule `b27d9d405aee4738a50b1e7b4d24bcb9` matches the public
  host, uses origin edge cache-control (bypass if absent) and respects origin
  Browser TTL. This removes the previous four-hour browser override. HTML and
  assets now return `no-cache`; APIs/health return `no-store` and edge BYPASS.
  Asset validators are weak, content-derived cache identifiers rather than
  integrity signatures. Conditional GET/HEAD return 304 when unchanged.

At that inspection, the edge changes were live while the RSS/ETag application
changes and increased polling budget awaited release/deployment. The later
[AUDIT.md](AUDIT.md) note records that follow-up as released and deployed in
v2.9.4, superseding the earlier “v2.9.3 remains deployed” checkpoint. Neither
historical observation establishes today's rollout or edge configuration.

For future rollouts, verify the actual deployed digest, public conditional asset
responses, rate-limit events, latency, memory, and shared-network false positives
in the infrastructure workflow. The nominal 1,000-viewer rate arithmetic is not
a tested throughput guarantee; capacity measurement is tracked in
`bitcoin_price_tracker-4mr`.
