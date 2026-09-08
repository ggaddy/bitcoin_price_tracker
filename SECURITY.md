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

This is an applicability assessment, not a claim that the packages are patched.
`.grype.yaml` keeps these four reviewed matches visible in table output. The
remaining 16 matches are unsuppressed (four Medium, one Low, seven Negligible,
four Unknown). Medium findings concern the same unused DNS printing functions,
shell word expansion, attacker-supplied `fopen` mode strings, and zlib CRC APIs.
The application imports none of those interfaces. Review unknown/new records as
advisory details become available. No blanket `only-fixed` or OS exclusion is used.

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
