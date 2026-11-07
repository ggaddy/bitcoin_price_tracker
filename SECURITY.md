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

The runtime copies a digest-pinned Debian 13 distroless `cc` filesystem after
retaining only the executable's native dependency chain, runs as UID/GID 10001,
and has no shell or package manager. Dependabot checks Docker digests, Actions,
Cargo, and browser dependencies weekly. Deployments must still enforce a read-only root,
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
[.grype.yaml](.grype.yaml). The reviewed deadline is **2026-12-07**; review must
finish before that date. [The deadline checker](scripts/check_image_exceptions.py)
fails CI/release checks on or after expiry. Owner: repository maintainer `ggaddy`.
The [completed November 7 review](#november-7-2026-post-release-exception-renewal)
reassesses all seven retained package/version matches after the v2.9.8 release.
Beads issue `bitcoin_price_tracker-12z` tracks the next full review;
`bitcoin_price_tracker-wbt` and [GitHub #40](https://github.com/ggaddy/bitcoin_price_tracker/issues/40)
track removal of the CVE-2026-84243 exception. Remaining removal tasks are
`bitcoin_price_tracker-w9n` (other libc) and `bitcoin_price_tracker-hcw` (GCC/C++).
The [library audit](#november-7-2026-complete-runtime-library-audit) removed the
zlib and four unused GCC-package matchers. The replacement was published and
independently verified as [v2.9.8](https://github.com/ggaddy/bitcoin_price_tracker/releases/tag/v2.9.8)
(`bitcoin_price_tracker-31u`); both `2.9.8` and `latest` resolved to registry digest
`sha256:14aa8127739beb85f91fdd01b7ffb6a6268efee22c87e3d377de9392fb2a8665`
at this review. This does not establish a production deployment inventory.

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
images. The [November 7 assessment](#november-7-2026-runtime-advisory-assessment)
records the original failing scan; the [subsequent remediation](#november-7-2026-zlib-remediation)
passes the local candidate scan. The [complete library audit](#november-7-2026-complete-runtime-library-audit)
further reduces native modules and exceptions. Those earlier steps did not renew
the deadline; the subsequent full review below justifies the new review period.

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

### November 7, 2026 runtime advisory assessment

Reviewed on **2026-11-07 UTC** for `bitcoin_price_tracker-o6i`. Owner: `ggaddy`.
The [machine-readable evidence](docs/security/runtime-review-2026-11-07.json)
records exact image identities, build-input and executable hashes, all findings,
vendor assessments, runtime verification, advisory checks, and commands. The
candidate was built from clean `origin/master` revision
`bcce713da4065b16b87a8676110d43aaea078e9f` in an isolated worktree; existing
edits in the original checkout were preserved. No production state was inspected.

**Strict image scanning fails.** Grype **0.118.0** (Syft **1.51.1**) refreshed
successfully to schema `v6.1.10`, built **2026-11-07T06:18:48Z**, archive SHA-256
`f90467f0e784f8f6dd04b9a53588851e4b6fb874a4171d9edc95e91502feda5a`.
Database age validation remained enabled. Each Linux/amd64 image reports the
same **45 package/advisory matches**: 13 High, 22 Medium, 3 Low, and 7 Negligible.
Twelve High matches use the existing exceptions; **CVE-2026-76844** is an
unaccepted High match. All three `--fail-on high` scans exit **2**, with
`discovered vulnerabilities at or above the severity threshold`. Both the
committed policy and the pre-existing local draft produce this result.

| Image | Exact identity |
| --- | --- |
| Local candidate, built with locked dependencies and the pinned Dockerfile | Podman manifest `sha256:1f691fa66f1c8262a35739d0d520be2a5bb63c073ba86f1bfa83fb2d5cf4a684`; configuration/image ID `sha256:213fc88222ea87536ac3fd7056f6150f02bdb370c5bca25dd3b79138b4f9d43d` |
| Published `latest`, still v2.9.7 at resolution | Registry manifest `sha256:e43323e73d6371a6c9b63781a76a61f4f90f97c24275611a644618dd10d120bc` |
| Latest supported distroless `cc-debian13:nonroot` | Index `sha256:08476c244888bfb425bae1ae4dd7c17f093e972aab8426883d97f3c6f280f7c1`; amd64 manifest `sha256:65af2e9e44e71de2df28245f6814301775d3ef04436b7d563f240d86fedf24aa` |

`podman save --format docker-archive` preserves the candidate's configuration
ID and filesystem. Grype's archive manifest identity is
`sha256:55dc26ba4cac500627eaaafe71bc13f45ea29e90f8b4f78b8500f807b34bd3c6`;
the evidence distinguishes it from the local Podman manifest.

The six existing advisory exceptions were reassessed individually:

| Advisory | Exact binary package/version | Assessment and removal tracking |
| --- | --- | --- |
| [CVE-2026-84243](https://security-tracker.debian.org/tracker/CVE-2026-84243) | `libc6` `2.41-12+deb13u4` | Trixie and unstable remain unfixed. Both application images use `LC_ALL=C`, `LANG=C`; their executables have no gettext/text-domain imports. HTTP cannot set process environment or catalog paths. Retain the existing matcher; `bitcoin_price_tracker-wbt`. |
| [CVE-2026-5435](https://security-tracker.debian.org/tracker/CVE-2026-5435) | `libc6` `2.41-12+deb13u4` | Trixie remains vulnerable with `<no-dsa>`; the unstable fix is `2.43-3`. Neither executable imports the affected deprecated DNS-printing functions. Retain; `bitcoin_price_tracker-w9n`. |
| [CVE-2026-19499](https://security-tracker.debian.org/tracker/CVE-2026-19499) | `libc6` `2.41-12+deb13u4` | Trixie remains vulnerable with `<no-dsa>`; the unstable fix is `2.43-5`. Neither executable imports `strfmon`/`strfmon_l`; currency display uses browser formatting. Retain; `bitcoin_price_tracker-w9n`. |
| [CVE-2026-85091](https://security-tracker.debian.org/tracker/CVE-2026-85091) | `zlib1g` `1:1.3.dfsg+really1.3.1-1+b1` | **A supported trixie-security fix now exists:** `1:1.3.dfsg+really1.3.1-1+deb13u1`, recorded in [DSA-6613-1](https://security-tracker.debian.org/tracker/DSA-6613-1). The latest distroless tag still contains the old package. Neither executable links zlib or exposes gzip writes. Retain only for the existing affected images pending fix/removal; `bitcoin_price_tracker-dd1`, `bitcoin_price_tracker-he8`. |
| [CVE-2026-95619](https://security-tracker.debian.org/tracker/CVE-2026-95619) | Each of `gcc-14-base`, `libgcc-s1`, `libgomp1`, `libstdc++6` at `14.2.0-19` (four matches) | Trixie GCC 14 remains vulnerable with `<no-dsa>`; unstable fixes it in `14.4.0-3`. Neither executable imports C++ symbols or links libstdc++/libgomp; the linked unwinder does not implement the affected C++ allocation operator. Retain; `bitcoin_price_tracker-hcw`. |
| [CVE-2026-102010](https://security-tracker.debian.org/tracker/CVE-2026-102010) | The same four GCC binary packages at `14.2.0-19` (four matches) | Trixie remains vulnerable with `<no-dsa>`; unstable fixes it in `14.4.0-3`. No C++ priority-queue `erase_if` path, C++ imports, or libstdc++/libgomp linkage is used. Retain; `bitcoin_price_tracker-hcw`. |

The new [CVE-2026-76844](https://security-tracker.debian.org/tracker/CVE-2026-76844)
also affects the old zlib package. Its stale-buffer overflow follows failed gzip
writes, including blocking descriptor failures; versions before 1.3.1.2 require
a subsequent `gzclearerr()` call. The earlier nonblocking-write rationale must
not be used to automatically waive this different advisory. Debian lists the
same supported security update as its fix. Grype's captured database still
reports no fixed version, so vendor evidence and image contents must both be
checked. Updating only to the currently resolved distroless digest removes
neither zlib finding. `bitcoin_price_tracker-he8` tracks a supported fix/removal
and a fresh exact-image scan; no new exception is added.

Full `verify_runtime.py` checks pass on both exact application images, including
non-root/read-only operation, quote serving, persistence, health failure/recovery,
bind mounts, shutdown, and native/locale guards. Their executables are identical
(SHA-256 `7ecaaffb67799c11360bfa27e5436f44e5d19e48e7dca7f28e6f8748356d0b21`)
and link only libc, libm, libgcc_s, and the loader. Bundled C SQLite/ring and
Rustls remain the native/TLS dependencies; SQLite extension loading is not
enabled. `dlopen`/`dlsym` imports remain, so these guards do not establish the
safety of every indirect native path or patch the affected libraries.

No matcher, image pin, threshold, or review deadline changes in this assessment.
The committed policy still expires **2026-11-10**. The original checkout contains
a pre-existing uncommitted **2026-12-02** draft, preserved separately; that draft
does not change the committed deadline. Boundary checks confirm each policy
fails on and after its own deadline. The outstanding full review
`bitcoin_price_tracker-fmq` remains open pending the zlib remediation; this
assessment is not a renewal. `cargo-audit` **0.22.2** with `--deny warnings`
(verified official binary, 207 dependencies) and `npm audit --audit-level=low`
pass with no accepted advisories. CI gates and suppressed-finding visibility
remain unchanged. No image was published and no production rollout occurred.

### November 7, 2026 zlib remediation

Local remediation for `bitcoin_price_tracker-he8` and `bitcoin_price_tracker-dd1`.
Owner: `ggaddy`. The [remediation evidence](docs/security/zlib-remediation-2026-11-07.json)
records the exact candidate, scan results, package-file comparison and controls.
This subsection supersedes the earlier assessment's local-candidate failure;
that historical image and its evidence are preserved.

The latest supported distroless digest still includes the affected zlib package.
Debian's trixie-security backport `1:1.3.dfsg+really1.3.1-1+deb13u1` is available
and its archive checksum was verified through signed Debian security indexes.
However, Grype's current database still reports both zlib High advisories against
that backport. No exception or scanner-database modification is used to bypass
this discrepancy.

The service uses Rustls and does not need OpenSSL or zlib. `libcrypto.so.3` from
the base's unused `libssl3t64` package is its only retained ELF consumer of
`libz.so.1`. The Dockerfile therefore removes both complete packages from an
intermediate root filesystem and copies that filesystem into `scratch`. It
checks the reviewed original package versions, verifies their owned files using
the base's checksum manifests, and removes the actual libraries, engines,
symlink, documentation and package metadata. A `readelf` check rejects any
remaining shared-library dependency on libz, libssl or libcrypto. A base package
change fails the recipe and requires review; removal is not applied blindly to
a future base version. The final image contains no earlier untrimmed base layer.

The filesystem comparison confirms **22 package entries removed**, **1,737 entries
unchanged**, and no added or changed entries, excluding container-generated
hosts/hostname/resolver files. The executable and CA bundle remain byte-identical;
the reviewed imports, linked libraries, user, environment, health check, command
and shutdown signal remain intact. Runtime verification now independently
exports the exact image filesystem and rejects leftover OpenSSL/zlib files.

The Linux/amd64 candidate `localhost/btc-zlib-remediation:20261107` has image ID
`sha256:699e495afd0b19d039bbb6fd010a8e7f3952976a1b79fbf827e02f1ac5e6880f`
and Podman manifest
`sha256:e3da2f2695e8503987ae512237a84a7b06badff2741d267bb769908b84ff60d5`.
Its Docker-archive scan manifest is
`sha256:8d690d5197d00a8d08df42110613f89703a4f2b21925075232e909785345bb49`.
Grype 0.118.0 with the unchanged November 7 database passes `--fail-on high`
with **exit 0**: 42 total matches, comprising **11 existing suppressed High**,
21 Medium, 3 Low and 7 Negligible. Neither `zlib1g` nor either zlib advisory
appears. The CVE-2026-85091 matcher is removed; no matcher is added or broadened.
The remaining five advisories retain their earlier exact package/version
assessments and removal tracking. Suppressed findings remain visible, the
High/Critical threshold stays strict, and the deadline remains **2026-11-10**.

Full exact-image runtime verification passes. The original image fails the new
filesystem guard. A temporary control policy omitting only CVE-2026-84243 makes
the remediated image's strict scan fail with exit 2 on that real High finding.
Pinned Rust 1.85.1 formatting and Clippy pass; Rust tests report 109 passed and
one optional Selenium test ignored. Strict RustSec and npm audits pass without
exceptions. No dashboard code changed.

Published `latest` still resolves to v2.9.7 manifest
`sha256:e43323e73d6371a6c9b63781a76a61f4f90f97c24275611a644618dd10d120bc`.
It still contains the old packages and fails the reduced policy with exit 2 on
both CVE-2026-76844 and CVE-2026-85091. The weekly published-image check must
continue reporting that failure until an authorized release replaces the image;
`bitcoin_price_tracker-31u` tracks that follow-up. The broader review
`bitcoin_price_tracker-fmq` remains open. No exception deadline is renewed, and
this local remediation does not publish an image or establish a production rollout.

### November 7, 2026 complete runtime library audit

Follow-up for `bitcoin_price_tracker-yhq`, based on merged PR #44 revision
`64ce88c`. Owner: `ggaddy`. The [machine-readable library audit](docs/security/runtime-libraries-2026-11-07.json)
records the complete before/after ELF inventory, package removal, retained
dependency graph, image identities, hashes, verification and scanner controls.

The PR #44 image still had **277 shared-library ELF files**. Three entire unused
packages are now removed: `libstdc++6` and `libgomp1` `14.2.0-19`, and `libzstd1`
`1.5.7+dfsg-1`. Unused libc compatibility, debugger/profiler, legacy NIS/NSS,
vector-math and charset-conversion modules/tables are also removed. The new
image contains the application and exactly these **four native libraries**:

| Library | Why retained |
| --- | --- |
| `libc.so.6` | C runtime, allocator/threading and built-in host-file/DNS lookup |
| `libm.so.6` | Directly linked numerical operations, including bundled SQLite |
| `libgcc_s.so.1` | Directly linked Rust/GCC stack unwinding support |
| `ld-linux-x86-64.so.2` | ELF interpreter and transitive dependency of all three libraries |

This is a reviewed direct/transitive dependency closure, with additional review
of indirect loading. In the pinned glibc 2.41 implementation,
[files and DNS NSS backends are built into libc](https://github.com/bminor/glibc/blob/glibc-2.41/nss/nss_module.c).
The image now explicitly configures `hosts: files dns` and `files` for other NSS
databases, eliminating configurations referring to removed compat/NIS/database
plugins. The release imports no iconv, gettext or locale-selection interfaces;
`LC_ALL=C` remains required. Rust handles UTF-8 and SQLite uses its bundled
encoding code. SQLite extension loading is not enabled or exposed. Future
localization, NSS backends, native plugins or dependency changes require review;
the release still imports `dlopen`/`dlsym`, so no general claim about arbitrary
dynamic-loading configurations is made.

`scripts/prune_runtime.py` checks reviewed package versions and owned-file
checksums, removes complete unused packages, and derives the retained ELF
dependency graph from the release executable/interpreter. Unexpected native
dependencies fail the build. It removes other ELF and gconv files, keeps the
`libc6` package identity/version and updates its owned-file manifest to describe
the trimmed package accurately. `libgcc-s1` and its `gcc-14-base` dependency and
licensing remain. The final image has no untrimmed earlier layer.

Against the PR #44 image, **273 shared libraries** and **308 filesystem entries**
are removed. Only NSS configuration and the libc ownership manifest change;
**1,427 entries remain unchanged**, including the executable and CA bundle.
The uncompressed image falls from **30,288,199 to 16,589,125 bytes (45.2%)**.
The Linux/amd64 candidate is `localhost/btc-runtime-minimal:20261107`, image ID
`sha256:d7fdd901cfadcfe85536ec167ad86839ab4c5e3f6e36fd9f54e633ea967ff2a0`,
Podman manifest `sha256:a3cdda5cb59fda10affd2488f13b0e3c0b72ff4d11f0cf4918ee2fe37e68e4ea`.
Other architectures were not runtime-tested in this review.

Full exact-image application/runtime verification passes and independently
requires the five-ELF inventory and built-in NSS configuration. The older PR #44
image fails that guard on a gconv module. A separate helper using the production
client builder and locked dependencies runs inside the immutable candidate
image against isolated loopback fixtures: host-file lookup, DNS A/AAAA queries, trusted
Rustls HTTPS, certificate-name rejection and untrusted-CA rejection all pass.
After these operations `/proc/self/maps` contains only the four retained native
libraries. The helper and private CA are not included in the release image.
Both ordinary CI and release checks now enforce this network verification.

The four exact CVE-2026-95619/CVE-2026-102010 matchers for removed `libstdc++6`
and `libgomp1` are deleted. Grype 0.118.0 with the November 7 database passes
`--fail-on high` with exit 0: **32 total matches**, comprising **7 retained High**,
15 Medium, 3 Low and 7 Negligible. Remaining High matches are the three libc
interfaces and two GCC-source advisories attributed to each of the retained
`libgcc-s1`/`gcc-14-base` packages; their existing applicability assessments and
removal tracking remain. No matcher is added or broadened. A temporary control
omitting only CVE-2026-84243 fails with exit 2 on that real High finding.
Suppressed findings stay visible and the **2026-11-10** deadline is unchanged.

Pinned formatting/Clippy and 109 Rust tests pass (one optional Selenium test
ignored), as do strict RustSec/npm audits. Published `latest` still resolves to
v2.9.7 digest `sha256:e43323e73d6371a6c9b63781a76a61f4f90f97c24275611a644618dd10d120bc`;
it remains affected and fails the reduced strict policy until an authorized
release replaces it. `bitcoin_price_tracker-31u` tracks that release and
`bitcoin_price_tracker-fmq` remains open. No image was published or deployed.

### November 7, 2026 post-release exception renewal

Completed review: `bitcoin_price_tracker-fmq`. Owner for **every retained
matcher**: `ggaddy`. Reviewed **2026-11-07**; review again before **2026-12-07**,
tracked by `bitcoin_price_tracker-12z`. The [review evidence](docs/security/exception-renewal-2026-11-07.json)
records the individual decisions, vendor-page hashes, exact image identities,
native inventories, scanner results and controls. This renewal follows the
completed zlib/library remediation and v2.9.8 publication; it supersedes the
earlier assessment's outstanding release blocker and November 10 review date.

Freshly resolved `latest` is v2.9.8, registry manifest
`sha256:14aa8127739beb85f91fdd01b7ffb6a6268efee22c87e3d377de9392fb2a8665`,
configuration `sha256:573c15a1a2a998044f5dc2206d3aaf79352c1e306acb39d03f12dc1d8ecde01c`.
The locked candidate built from release source `420acaf` has configuration
`sha256:d7fdd901cfadcfe85536ec167ad86839ab4c5e3f6e36fd9f54e633ea967ff2a0`.
All five ELF files and retained package versions match between the images;
the executable SHA-256 is
`7ecaaffb67799c11360bfa27e5436f44e5d19e48e7dca7f28e6f8748356d0b21`.

Debian's current tracker still lists no supported trixie fix for these five
advisories. The latest supported distroless index remains
`sha256:08476c244888bfb425bae1ae4dd7c17f093e972aab8426883d97f3c6f280f7c1`
(amd64 manifest `sha256:65af2e9e44e71de2df28245f6814301775d3ef04436b7d563f240d86fedf24aa`).
Inspection confirms the same affected libc/GCC package versions; changing only
the base digest cannot retire these matches. Unstable glibc/GCC fixes are not
supported Debian 13 updates and are not mixed into this runtime.

All seven exact binary-package matchers are retained with these decisions:

| Advisory | Package/version | Current exposure and removal tracking |
| --- | --- | --- |
| [CVE-2026-84243](https://security-tracker.debian.org/tracker/CVE-2026-84243) | `libc6` `2.41-12+deb13u4` | No supported or unstable fix. Gettext remains in libc, but the executable imports no gettext/text-domain or locale-selection functions. `LC_ALL=C` prevents non-C message catalogs; HTTP cannot set process environment/catalog paths. Retain; `bitcoin_price_tracker-wbt` / GitHub #40. |
| [CVE-2026-5435](https://security-tracker.debian.org/tracker/CVE-2026-5435) | `libc6` `2.41-12+deb13u4` | Trixie remains `<no-dsa>`; unstable fix `2.43-3`. No affected DNS-printing imports or exports exist in the five shipped ELF files; `libresolv` is absent. Normal files/DNS resolution passes. Retain the source-attributed match; `bitcoin_price_tracker-w9n`. |
| [CVE-2026-19499](https://security-tracker.debian.org/tracker/CVE-2026-19499) | `libc6` `2.41-12+deb13u4` | Trixie remains `<no-dsa>`; unstable fix `2.43-5`. `strfmon` code remains in libc, but the executable imports none of its interfaces and exposes no native monetary-format strings. Browser currency formatting uses JavaScript. Retain; `bitcoin_price_tracker-w9n`. |
| [CVE-2026-95619](https://security-tracker.debian.org/tracker/CVE-2026-95619) | `gcc-14-base` `14.2.0-19` | Trixie remains `<no-dsa>`; unstable fix `14.4.0-3`. This package owns documentation/licensing, not executable code. The affected aligned C++ allocation implementation is not shipped. Retain the source-attributed match; `bitcoin_price_tracker-hcw`. |
| [CVE-2026-95619](https://security-tracker.debian.org/tracker/CVE-2026-95619) | `libgcc-s1` `14.2.0-19` | Same vendor status. The retained library implements unwinding, not the affected C++ allocation operator; no C++ imports/exports or libstdc++ are present. Retain; `bitcoin_price_tracker-hcw`. |
| [CVE-2026-102010](https://security-tracker.debian.org/tracker/CVE-2026-102010) | `gcc-14-base` `14.2.0-19` | Trixie remains `<no-dsa>`; unstable fix `14.4.0-3`. Documentation-only package; the affected libstdc++ binary-heap `erase_if` implementation is not shipped or used by this Rust/C dependency graph. Retain the source-attributed match; `bitcoin_price_tracker-hcw`. |
| [CVE-2026-102010](https://security-tracker.debian.org/tracker/CVE-2026-102010) | `libgcc-s1` `14.2.0-19` | Same vendor status. The unwinder contains no affected C++ container implementation; no C++ runtime/imports/exports or application container path are present. Retain; `bitcoin_price_tracker-hcw`. |

Upstream fixes locate the GCC issues in
[libstdc++ aligned allocation](https://github.com/gcc-mirror/gcc/commit/200183d4ab5d57575f3dfebc6af3baf649910b9e)
and the [libstdc++ binary-heap header](https://github.com/gcc-mirror/gcc/commit/aaa8351f4d2e636f9680a1f0a8ebc2f0a60611e6),
separate from the retained libgcc unwinder. Package identities and licensing
remain truthful; no package metadata is removed to hide a finding.

Grype **0.118.0**, with the freshly checked schema `v6.1.10` database built
**2026-11-07T06:18:48Z**, scans both candidate and published images under the
original November 10 policy with **exit 0**: **32 matches** each, comprising
**7 reviewed High**, 15 Medium, 3 Low and 7 Negligible. There are no unaccepted
High/Critical findings and no retired-package findings. The untrimmed latest
base remains a deliberately failing control: **exit 2**, with six additional
High matches in removed zlib/C++/OpenMP packages. No exceptions are restored for
that base. Each of seven controls omitting one retained matcher independently
fails with exit 2 on exactly that package/advisory. Suppressed findings remain
visible; matcher bodies and the strict threshold are unchanged.

Full application/runtime and isolated host-file/DNS/Rustls TLS checks pass on
both immutable images, including certificate rejection and inspection of the
four loaded native libraries. C locale and built-in files/DNS NSS remain
required. SQLite extension loading is not enabled or exposed. `dlopen`/`dlsym`
remain imported, and monetary/gettext code remains in libc: this is a bounded
applicability assessment, not patched packages or proof about arbitrary native
plugins/configuration. Positive IPv6 transport and other architectures were
not runtime-tested. Strict RustSec and npm advisory checks also pass.

The **30-day** next-review interval is justified by the completed vendor and
exact-image reassessment, absence of a supported fix, and enforced exposure
guards; CI already passed before changing the date. Review sooner when a
supported fix appears, a new advisory is reported, or native dependencies,
locale/NSS configuration or extension-loading exposure changes. Remove each
matcher as soon as a supported image fixes or removes the affected package.
Deadline controls pass before, and fail on/after, both the old and new
expiry dates; a missing review date still fails. This review updates
policy/documentation only and does not publish an image or deploy production.

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
