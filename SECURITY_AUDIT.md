# Security audit and remediation

The September 2026 audit reviewed application commit
`87ddf1215fa4be1efc5fd9b004493006ba66c4c5`, its container, release automation,
and deployment controls. This public summary describes findings and remediation.
Deployment addresses, network topology, and detailed operational evidence are
maintained in the private infrastructure repository.

No remote code execution, SQL injection, or exploitable browser injection was
found in the reviewed application. That result does not establish that the
application or its deployment is invulnerable. The main deployment concern was
insufficient containment if the process were compromised.

## Findings addressed by v2.7.0

| ID | Priority | Finding and correction |
| --- | --- | --- |
| AUD-01 | High | Deployment networking allowed unrestricted cluster/LAN access. Infrastructure policy now limits ingress to the intended gateway/tunnel and egress to provider HTTPS plus restricted cluster DNS, with explicit private/node/API denies. |
| AUD-02 | Medium | The pod lacked containment controls and mounted an unnecessary API token. Infrastructure definitions now enforce non-root execution, dropped capabilities, seccomp, no privilege escalation, a read-only root, bounded writable storage, and health probes. |
| AUD-03 | Medium | Provider redirects could leave the fixed HTTPS destinations. Provider requests now require HTTPS and never follow redirects. |
| AUD-04 | Medium | Provider response bodies were unbounded. Declared and streamed responses are now capped at 64 KiB before JSON parsing; failures preserve stored quotes. |
| AUD-05 | Medium | Incomplete HTTP connections bypassed request middleware limits. The origin now caps connections before parsing, bounds header bytes/time and total lifetime, and reserves loopback capacity for health probes. |
| AUD-06 | Medium | Image publication did not depend on successful validation. Release tags now run exact-commit CI, verify and scan the candidate image, then publish it without rebuilding. Branch and release-tag protections are active. |
| AUD-07 | Low | Runtime OS packages were outside advisory checks. The runtime now uses a digest-pinned distroless base, with final-image and weekly published-image scans and documented, expiring advisory exceptions. |
| AUD-08 | Low | Browser security response headers were absent. Responses now enforce CSP, framing restrictions, content-type protection, and referrer policy; API and health responses are not cacheable. |

AUD-03 and AUD-04 require a faulty or compromised upstream; public clients cannot
supply provider URLs. AUD-05 was reproduced against a disposable direct origin,
not through a production public proxy. AUD-06 requires release/repository access.
The audit did not demonstrate exploitation of a runtime OS advisory through the
application. See [SECURITY.md](SECURITY.md) for advisory applicability and expiry.

## Validation

- Rust formatting and Clippy passed; 97 Rust tests passed, with one optional
  Selenium test ignored.
- All 23 deterministic Chromium tests passed against the final container under
  its actual Content Security Policy.
- Runtime verification covered non-root startup, read-only root, dropped
  capabilities, writable storage, quote persistence, health failure/recovery,
  bind-mount ownership, and graceful shutdown.
- Real socket checks verified header/connection deadlines, admission limits,
  and successful loopback health probes while remote connection capacity was full.
- Cargo-audit found no advisories among 219 dependencies; npm audit was clean.
- Grype reported 20 runtime package/advisory matches. Four High/Critical matches
  have exact package/version exceptions documented in SECURITY.md and expiring
  on 2026-10-08. Other High/Critical findings block publication. This is not a
  claim that the image has no vulnerable packages.
- Deployment verification used disposable canaries, a reachable control pod,
  actual Cilium policy-drop events, and post-rollout health/provider checks.
  The infrastructure repository contains the reproducible isolation check.

## Deployment requirements

Use the supported non-root UID/GID and a writable data directory. Enforce network
isolation and pod restrictions in the deployment; container defaults alone are
insufficient. Keep per-client abuse controls and HTTPS policy at the trusted
public edge. Presence counts are untrusted activity hints, not identities.

Access restrictions remain in place while the release and public-edge controls
are verified. A namespace and network policy do not create a separate kernel
boundary. Stronger protection from a container/kernel escape requires a separate
host/VM and network boundary. Containers read the node clock; node time
synchronization requires no NTP allowance in the application's egress policy.
