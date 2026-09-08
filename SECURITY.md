# Dependency advisories

CI runs `cargo audit --deny warnings` and `npm audit --audit-level=low` on pull
requests, pushes to master, and weekly. These jobs fail visibly; neither uses
`continue-on-error`. RustSec uses cargo-audit 0.22.2 with Rust 1.94.0, separately
from the application's pinned Rust 1.85.1 toolchain, to read current advisory
formats. Browser test packages are included in the npm scan.

There are currently no accepted advisories. Before adding any advisory ID to
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
