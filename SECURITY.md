# Dependency advisories

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

The P5 dependency update removes the unmaintained `paste` dependency by upgrading
the Selenium client to thirtyfour 0.36.3 with its automatic driver downloader
disabled. The runner supplies a pinned Selenium container. Patched locked
versions include quinn-proto 0.11.15, rustls-webpki 0.103.13, and rand 0.9.3.
See the RustSec database for [quinn-proto](https://rustsec.org/advisories/RUSTSEC-2026-0185.html),
[rustls-webpki](https://rustsec.org/advisories/RUSTSEC-2026-0104.html),
[rand](https://rustsec.org/advisories/RUSTSEC-2026-0097.html), and
[paste](https://rustsec.org/advisories/RUSTSEC-2024-0436.html).

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

The active GitHub rulesets are recorded in `.github/rulesets/`: changes to
`master` require a pull request, resolved review threads, and the five CI checks
from the GitHub Actions app against an up-to-date branch. There is no admin bypass.
Zero external approvals are required to support a sole maintainer, so this is
validation enforcement rather than independent two-person review. Separate tag
rules permit creation only by repository admins and prohibit moving/deleting
`v*` tags, including for admins. Repository settings can still be changed by an
administrator; secure the maintainer's GitHub account and Docker Hub token.

Rustls is locked to 0.23.45 (with rustls-webpki 0.103.15), fixing
[RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285.html).
The audit still fails on every unaccepted advisory.

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

#### Debian 13 digest update review

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

#### October 2026 release review

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
`kube_lan` repository. Keep Cloudflare Access restricted until the new image is
released/deployed and the final public path and per-client abuse controls have
been verified. Node NTP maintains the clock used by containers; the tracker
requires no NTP network access or capability to change the clock.
