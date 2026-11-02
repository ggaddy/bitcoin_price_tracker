# Agent instructions

## Work tracking

Use **Beads (`bd`) for all task tracking**, dependencies, blockers, and persistent
project memory. Read the [project Beads skill](.agents/skills/beads/SKILL.md) and
run `bd prime` for full workflow context; run it again after context recovery.
Do not create markdown task trackers or MEMORY.md files. Use `bd remember` for
persistent knowledge.

```bash
bd ready                 # Find available work
bd show ISSUE_ID         # Replace ISSUE_ID with the selected issue
bd update ISSUE_ID --claim # Claim atomically before implementation
bd create --title="..." --description="..." --type=task --priority=2
bd close ISSUE_ID        # Close only after the work is actually complete
```

Issues live in a local Dolt database managed by `bd`. `.beads/issues.jsonl` is a
passive export, not the source of truth; do not import it during normal work.
Cross-machine sync uses `bd dolt push/pull` and `refs/dolt/data` on the git remote,
separate from code branches under `refs/heads/*`. Use `bd where` to locate the
active workspace. See the [Beads sync concepts](https://github.com/gastownhall/beads/blob/main/docs/core-concepts/sync-concepts.md).

The active profile is **conservative**: do not create git commits, push code, or
run Dolt remote sync without explicit user authorization. Tool approval settings
do not change this project policy. Beads guidance is subordinate to the current
user, repository, and orchestrator instructions.

## Build and validation

Run from the repository root. Rust 1.85.1, rustfmt, and Clippy are pinned by
[rust-toolchain.toml](rust-toolchain.toml); use locked dependencies:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Dashboard changes also need deterministic Chromium tests (Node.js 20 or newer):

```bash
npm ci --prefix tests/browser
npm exec --prefix tests/browser -- playwright install chromium
npm test --prefix tests/browser
```

See [README.md](README.md#development-and-tests) for Linux browser dependencies,
embedded-asset checks, optional Kache, Selenium, and container verification.
Runtime-image/native-dependency changes require the documented final-image
verification and advisory review; the verifier needs host Python 3 and `readelf`.

## Architecture

The application is one Rust/Axum process with SQLite persistence and embedded
HTML/CSS/JavaScript. There is no frontend build step. One application instance
owns each database; presence, rate limits, refresh ownership, and backoff are
process-local. Kubernetes manifests and rollout evidence live in the separate
`ggaddy/kube-lan-rke2` repository.

| Area | Source |
| --- | --- |
| Startup, configuration, shared state | `src/main.rs`, `src/config.rs`, `src/state.rs` |
| Routes, cached price responses, viewer admission | `src/app.rs`, `src/handlers.rs`, `src/presence.rs` |
| Provider adapters and refresh coordination/backoff | `src/pricing.rs`, `src/refresh.rs` |
| SQLite migration/transactions, models, freshness/aggregates | `src/storage.rs`, `src/models.rs`, `src/models/source_contract.rs`, `src/freshness.rs` |
| Connection/request limits and shutdown | `src/lifecycle.rs`, `src/limits.rs` |
| Embedded dashboard and security/cache headers | `src/ui.rs`, `src/ui.html`, `src/dashboard.js`, `src/dashboard.css`, `src/vendor` |
| Independent in-memory RSS cache | `src/news.rs` |
| Local provider/clock fixtures and regressions | `src/test_support.rs`, `src/api_tests.rs`, `src/operations_tests.rs`, `tests/browser/dashboard.test.mjs` |

## Conventions

- Preserve [the price/source contract](docs/source-contract.md). Quote observation
  times, snapshot persistence time, and provider health have different meanings.
- Serve cached prices without waiting for provider I/O. Preserve single refresh
  ownership, ten-second gates, per-provider backoff, and presence admission.
- Keep blocking SQLite work off async workers and commit quote/health changes
  atomically. Migrations must roll back on failure; do not infer legacy quote ages.
- Use local provider fixtures, injected clocks, and temporary databases for tests;
  ordinary tests must not depend on public provider availability.
- Render variable browser content as text, preserve bounded requests/visibility
  ownership, and maintain the CSP. Keep raw viewer IDs out of logs and internal
  database paths or transport diagnostics out of public responses.
- Follow [SECURITY.md](SECURITY.md) for native-library exposure and advisory
  exceptions. Do not suppress findings or extend review deadlines to pass CI.
- Keep current work in Beads. [AUDIT.md](AUDIT.md), [SECURITY_AUDIT.md](SECURITY_AUDIT.md),
  and [project_plan](project_plan) preserve dated history, not current work queues.
- AGENTS.md and CLAUDE.md are independent files. Keep their substantive guidance
  identical when editing either one.

## Non-interactive commands

Use non-interactive flags so aliases or prompts cannot hang an agent. For
operations already authorized by the task, use `cp -f`, `mv -f`, and `rm -f`;
for recursive operations use `cp -rf` and `rm -rf`. These flags do not authorize
destructive changes. Use `ssh -o BatchMode=yes`, `scp -o BatchMode=yes`,
`apt-get -y`, and `HOMEBREW_NO_AUTO_UPDATE=1` with `brew` when applicable.

## Session completion

Before reporting completion:

1. Record remaining work/blockers in Beads and close only completed issues.
2. Run checks appropriate to the changes and report their results/limits.
3. Run `git status` and review the diff for unintended changes.
4. Follow the conservative profile: leave commits, code pushes, and Dolt remote
   sync for explicit authorization. Report changed files and suggested next steps.

Do not stop completed, authorized local work merely because a later commit or
publish step would need authorization. If an explicitly required sync/push fails,
report the exact command and error rather than claiming it succeeded.
