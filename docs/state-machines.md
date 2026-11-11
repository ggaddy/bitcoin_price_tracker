# Application state machines

Reviewed 2026-11-11, starting at `b4be784` (`origin/master`), with the corrections
described below. This is the single reference for the application's explicit
enums and implicit state carried by locks, timestamps, timers, transactions and
browser flags. Beads task `bitcoin_price_tracker-jrk` owns the audit; Beads, not
this document, owns ongoing work tracking. Wire-field definitions remain in
[the source contract](source-contract.md).

States below are predicates over existing fields, not proposed new enums.
Independent machines compose: a fresh quote can coexist with failed provider
health, a completed HTTP response with an active background refresh, and a hidden
page with an abort still settling. Treating these combinations as one boolean
was the source of several defects. Unless specified otherwise, an event that
fails a guard leaves the machine unchanged.

## Ownership and time

| Resource / fact | Owner / synchronization | Lifetime / clock |
| --- | --- | --- |
| TCP connection | Connection-pool semaphore permit in its `JoinSet` task | Accept through completion, header/lifetime timeout or forced drain |
| HTTP admission | Route-group semaphore permit, shared through `REQUEST_LEASE` with blocking storage jobs | Handler and any outstanding storage work, including after caller cancellation |
| Viewer membership | `Presence.viewers` mutex | Last heartbeat through age **15s inclusive**, monotonic time |
| Activation epoch | `Presence.generation` atomic, incremented under viewer mutex | Process-local; empty-to-active admission only |
| Price refresh | Owned coordinator mutex guard | Reservation before database read through provider work, commit and report publication |
| Provider retries | Coordinator fields under that same guard | Monotonic attempt/eligibility deadlines; HTTP dates converted once at completion |
| Quotes and provider health | SQLite immediate write / deferred read transactions | Durable; observation, attempt-start and persistence Unix times have separate meanings |
| News cache | Short-held `FeedCache` mutex and `refreshing` reservation | Process-local; monotonic completion time and a 12s task deadline |
| Price browser workers | `busy`, controller, timer, visibility generation | One unsettled request per kind; 3s presence / 8s price deadlines |
| Browser report publication | Request sequence shared by startup presence and price GET | Earlier-issued responses cannot replace a later-issued accepted report |
| Browser age / news retry / scroll dwell | `performance.now()` and bounded timers | Monotonic elapsed time; wall clock is used for displaying dates only |

Only one application instance may own a database. These locks, rate budgets,
viewer IDs and cooldowns are not coordination between processes. Restart resets
process-local state and preserves committed SQLite state. There is no durable
retry queue and no autonomous price scheduler: admitted readers trigger work.

## 1. Process startup and shutdown

Implementation: [`main.rs`](../src/main.rs), [`config.rs`](../src/config.rs),
[`lifecycle.rs`](../src/lifecycle.rs), [`refresh.rs`](../src/refresh.rs).

States: `Configuring`, `Probing`, `Initializing`, `Serving`, `Stopping`,
`Draining`, `Exited(success|failure)`.

| From | Event / guard | To / effects |
| --- | --- | --- |
| Configuring | Invalid address, zero/out-of-range limit, invalid numeric setting | Exited(failure), before bind or migration |
| Configuring | Valid config and `--healthcheck` | Probing; bounded local `/health` request with proxies disabled |
| Probing | Success status / request or status failure | Exited(success) / Exited(failure) |
| Configuring | Normal invocation | Initializing: client setup, SQLite initialization, then bind |
| Initializing | Client, migration or bind error | Exited(failure); a failed migration rolls back |
| Initializing | Listener bound | Serving; create fresh process-local state |
| Serving | SIGINT/SIGTERM or injected shutdown future | Stopping: set price stop flag, capture one shutdown deadline, stop accepting sockets |
| Stopping | Notify existing connection tasks | Draining: graceful HTTP shutdown, then acquire price ownership before the same deadline |
| Draining | HTTP and background price work finish | Exited(success) |
| Draining | HTTP or price deadline expires | Exited(failure); abort HTTP tasks; runtime cleanup has a separate final 5s bound |

Invariants: a spawned price job that observes stopping returns before provider
I/O; already-dispatched work may finish its commit. `drain_until` waits
through report publication, not just network completion. The HTTP and price
drains do not each receive a new grace period. News is disposable in-memory
work and is not included in the durable price drain. Detached blocking jobs are
not forcibly cancelled by dropping an HTTP future.

Evidence: `tracker_shutdown_drains_a_background_refresh_after_its_http_response_finishes`,
`shutdown_drains_an_in_flight_request`, `shutdown_has_a_deadline_for_stalled_requests`,
and `tracker_background_drain_uses_the_existing_shutdown_deadline`.

## 2. Connection admission and lifetime

Implementation: `lifecycle::serve_until`.

States: `Accepted`, `Rejected`, `ReadingHeaders`, `ServingRequest`, `Graceful`,
`Closed`. Remote and loopback connections use separate fixed semaphore pools;
loopback has `min(MAX_CONNECTIONS, 4)` slots.

| From | Event / guard | To / effects |
| --- | --- | --- |
| Accepted | Its pool has no permit | Rejected → Closed; drop socket without queuing a waiter |
| Accepted | Permit acquired | ReadingHeaders; start header and whole-connection deadlines |
| ReadingHeaders | Valid complete headers within limits | ServingRequest |
| ReadingHeaders | Header timeout (default 5s), parse/size/count error, disconnect | Closed; release permit |
| ReadingHeaders / ServingRequest | Shutdown notification | Graceful; disable further admission on that connection, retain original lifetime deadline |
| ServingRequest / Graceful | Response done or disconnect | Closed; HTTP/1 keep-alive disabled |
| Any admitted connection | Lifetime (default 25s) or forced shutdown deadline | Closed; task/connection dropped, permit released |

Invariants: one request per connection; no forwarding header can choose the
loopback pool; connection-task count is bounded by both pools, with completed
tasks reaped by the accept loop. Application cancellation still has to respect
the independent storage-work lifetime in machine 3.

Evidence: `incomplete_headers_release_connection_capacity_before_application_deadline`
and `connection_lifetime_bounds_a_stalled_handler_and_frees_its_slot` use local
sockets and verify subsequent admission.

## 3. Request admission and blocking-work leases

Implementation: [`limits.rs`](../src/limits.rs), `storage::run_blocking`.

Each fixed route group (price, presence, health, news, assets/other) has a token
bucket and a separate concurrency semaphore. State is the product of
`tokens ∈ [0, rate]`, the nondecreasing accounting timestamp, and occupied slots.
Health reserves rate 10 / concurrency 4; news concurrency is capped at 8.

| From | Event / guard | To / effects |
| --- | --- | --- |
| Incoming | Refill using `max(sampled_now, updated)`; tokens below 1 | Rejected 429, `Retry-After: 1` |
| Incoming | Token available | Consume one token, then attempt nonwaiting concurrency admission |
| Token consumed | No slot available | Rejected 503, `Retry-After: 1`; token is deliberately not refunded |
| Token consumed | Slot acquired | Running under shared request lease, with a 20s handler deadline |
| Running | Direct awaited SQLite job queued/running | Running + storage lease clone; blocking work stays off async workers |
| Running | Handler completes, caller disconnects, or deadline expires | Handler finished/cancelled (408 on deadline); keep slot while any storage clone remains |
| Handler finished/cancelled | Last associated storage job completes/unwinds | Release final lease and slot |

Invariants: accounting time never moves backward, including when older sampled
times acquire the mutex later. A queued/running SQLite job cannot become
unaccounted work merely because its caller was dropped. Route pools remain
independent. Spawned price work intentionally does not inherit the task-local
HTTP lease: it has its own single-refresh owner, so it does not block cached
requests. New detached request work must explicitly preserve its owner; Tokio
task locals do not automatically propagate into spawned tasks.

Confirmed violations and corrections: **F1**, **F2** below. Existing checks also
cover independent health/presence/news capacity and release after a normal
handler timeout. The new storage regressions verify cancellation, timeout,
independent health capacity and admission after the held job finishes.

## 4. Viewer membership and activation epochs

Implementation: [`presence.rs`](../src/presence.rs), `handlers::update_presence`.

Per viewer states: `Absent`, `Present(last_seen)`, `Expired` (computed until
pruned). Global states: `Empty`, `Occupied`, `AtCapacity`; these overlap per
viewer state. Generation is independent of count.

| From | Event / guard | To / effects |
| --- | --- | --- |
| Any | Invalid ID (not 1–128 ASCII alphanumeric/`-`/`_`) | 400; no mutation or startup price |
| Any valid request / count read | Acquire viewer mutex, then sample monotonic time | Remove entries with age **greater than** 15s |
| Absent, occupied or empty | `active=true`, room available | Present(now); if pruned map was empty, increment generation once |
| Absent, at capacity | `active=true` | 429 + `Retry-After: 15`; no startup price or generation change |
| Present, including at capacity | `active=true` | Present(now); renew TTL without incrementing generation |
| Present / Absent | `active=false` | Absent; removal is idempotent |
| Admitted active | `include_price=true` | Release viewer mutex, then cached-price path; nested failure does not undo admission |

Invariants: count never exceeds the configured cap; existing heartbeat/removal
works at capacity; invalid/rejected/inactive requests cannot request startup
prices. Epoch increments only for observed empty-to-occupied admissions. Count
and nested price count are separate samples, not one atomic response snapshot.
Heartbeats have arrival-order semantics: a delayed inactive beacon can remove a
renewed ID; a later active heartbeat repairs membership. Hidden/closed viewers
can remain until TTL expiry because beacon delivery is best effort. These are
bounded presence semantics, not a guarantee of instantaneous browser visibility.

Evidence: `invalid_presence_cannot_mutate_admission_or_request_a_startup_price`,
`presence_samples_time_after_acquiring_the_viewer_lock`,
`viewer_cap_preserves_existing_heartbeats_removal_and_expiry`,
`presence_ignores_wall_clock_jumps_and_expires_just_after_ttl`, and
`startup_snapshot_cannot_bypass_admission_or_activate_an_inactive_viewer`.

## 5. Cached-price dispatch and refresh ownership

Implementation: `refresh::cached_prices`, `PriceRefresh`, `RefreshCoordinator`.

States: `Unowned`, `Inspecting` (guard reserved before SQLite read), `Planned`,
`Dispatched`, `Persisting`, `Reporting`, plus orthogonal `Stopping`. Report state
is `(succeeded, safe_errors)` from the latest completed process-local attempt.

| From | Event / guard | To / effects |
| --- | --- | --- |
| Unowned | Cached read wins `try_lock_owned` | Inspecting; other readers never wait for the coordinator |
| Any owned state | Another cached read | Read and serve its own stored snapshot; no second dispatch |
| Inspecting | Storage read failed | Return 500/UNAVAILABLE with safe warning; no provider dispatch |
| Inspecting | Stopping, no viewers, or persisted snapshot age below 10s | Serve stored response, release guard; pending generation survives |
| Inspecting | Coordinator gate refuses plan | Serve stored response, release guard |
| Inspecting | Coordinator accepts plan | Planned → Dispatched; move guard and state into independent Tokio task, return cached data immediately |
| Dispatched | Stop flag or zero viewers on final admission recheck | Release guard without I/O or report completion; already-recorded attempt gate remains |
| Dispatched | Still admitted | Concurrent selected-provider I/O, then Persisting |
| Persisting | Atomic store success or error | Reporting; complete cooldown bookkeeping using every typed provider failure |
| Reporting | Publish latest completed report | Unowned; release guard only after report update |

Invariants: at most one price job spans I/O, merging, persistence and report
publication. HTTP cancellation cannot cancel dispatched work. A viewer expiring
before final dispatch prevents I/O; expiration after dispatch does not cancel
the batch. The store merges the latest committed observations inside its write
transaction rather than a stale copy from the cached read.

`refreshing` is a sample of reservation/in-progress ownership, not a promise that
the response's data includes that work. `refresh_succeeded` means a new quote
was committed by the latest completed attempt, not by the current GET. Reads,
reports, viewer counts and ownership can be sampled on different sides of a
concurrent completion; only persisted quote/health reads are transactional.
Failure-only batches and write failures report false; cached reads preserve the
report; read errors explicitly report false. Report state starts empty on restart.

Evidence: concurrent warm/cold cached-read tests, `activation_during_batch_survives_completion_and_freshness_gate`,
`presence_expiring_during_database_inspection_prevents_dispatch`,
`dispatched_batch_can_finish_after_viewers_expire`,
`completed_client_request_does_not_cancel_background_work_or_its_attempt_gate`,
and `refresh_merges_the_latest_stored_observations_at_commit_time`.

## 6. Rotation, activation and provider cooldowns

Implementation: `RefreshCoordinator::{begin, complete}` in `refresh.rs`.

State: rotation cursor, optional `last_attempt`, four optional `next_eligible`
deadlines, four failure counters capped at 6, and `completed_generation`.
`PendingFull` means requested generation differs from completed generation;
`Rotating` means they match. Each provider is `Eligible` or `Cooling`.

| From | Event / guard | To / effects |
| --- | --- | --- |
| Any | Last attempt less than 10s ago | No plan, no state mutation |
| PendingFull | Gate open | Select every currently eligible provider in configured order |
| Rotating | Gate open | Select first eligible provider from rotation cursor, wrapping once |
| Either | No eligible providers | No plan; do not consume attempt or pending generation |
| Either | Nonempty selection | Record last attempt; install start+10s eligibility for selected providers; advance cursor past each selection |
| Attempted provider | Completed success, even if storage fails | Reset its failure counter; retain preinstalled minimum deadline |
| Attempted provider | Completed failure | Increment capped counter; cooldown from batch completion is 10/20/40/80/160/300s, then 300s |
| Failed provider | Valid representable `Retry-After` longer than backoff | Extend cooldown to that delay; absolute date converted using completion Unix time |
| Failed provider | Invalid, past or unrepresentable hint | Use normal backoff (past/zero hints cannot shorten it) |
| Full plan | Completion | Record the plan's generation, never a newer concurrently requested generation |
| Any | Process restart | Reset retry/cursor/report state; retained snapshot's 10s age gate still applies |

Invariants: activation cannot bypass global cadence or provider cooldowns. A
full plan can be partial when other providers are cooling; finishing it consumes
that epoch, not a promise that all providers succeeded. Failure counters follow
provider results independently of SQLite success. No autonomous retry occurs
when viewers/readers disappear. Unknown/future snapshot age does not suppress
refresh indefinitely, but also does not bypass monotonic attempt gates.

Evidence: coordinator tests for exact eligibility, retry hints and overflow,
capped backoff/reset, rotation on failure, and all-providers-cooling; route tests
`all_rate_limited_providers_wait_without_consuming_pending_activation`,
`rate_limited_provider_does_not_block_healthy_rotation_or_lose_retry_on_write_failure`,
`restart_resets_retry_state_but_preserves_partial_snapshot_success_gate`, and
`failed_attempt_frequency_does_not_increase_with_viewer_count_or_request_bursts`.

## 7. Provider attempt and validation

Implementation: [`pricing.rs`](../src/pricing.rs), [`errors.rs`](../src/errors.rs).

States: `Unselected`, `Requesting(start_unix)`, `ReadingBody`, `Validating`,
`Success(quote, observation_unix)`, `Failure(typed_error)`.

| From | Event / guard | To / effects |
| --- | --- | --- |
| Unselected | Included in plan | Requesting; record request-start time independently for each provider |
| Requesting | Connect/total deadline or transport failure | Failure(timeout/transport) |
| Requesting | Non-success HTTP status | Failure(http); capture optional retry hint without reading error body |
| Requesting | Success status | ReadingBody; reject declared length above 64KiB |
| ReadingBody | Actual chunks exceed 64KiB, malformed JSON, or body transfer fails | Failure(invalid_payload/transport/timeout) |
| ReadingBody | Bounded complete JSON | Validating required market, currency, symbol, selected field and finite positive price |
| Validating | Invalid structure or numeric value | Failure(invalid_payload/invalid_price) |
| Validating | Valid quote | Success; capture provider observation time now, not at batch completion |
| Success / Failure | All selected futures finish | Produce quote successes and health outcomes for one atomic store |

Invariants: default production client is HTTPS-only, redirects disabled, connect
deadline 2s and total deadline 5s including the body. Selected providers run
concurrently; one failed provider does not discard other successes. Last good
quote and latest attempt outcome are independent. Public errors are sanitized;
raw transport details remain diagnostic only. `LastAttempt` is unknown, success
with start time, or failure with start time and typed error; no in-progress
health record is persisted.

Evidence: provider fixture, market-validation, numeric-validation, redirect,
declared/chunked-body boundary, stalled headers/body/TLS, truncated transport,
and per-provider observation-time tests in `pricing.rs`; partial/all-failure
route tests verify retained observations and independent health.

## 8. SQLite schema, merge and commit

Implementation: [`storage.rs`](../src/storage.rs).

Schema states: `Unversioned(0)`, `Migrating`, `Ready(1)`, `Unsupported`.
Data states: `Empty`, `HealthOnly`, `Snapshot(quotes, metadata)`, each with latest
provider health. A write has `Reserved`, `Merged`, `Committed` or `RolledBack`.

| From | Event / guard | To / effects |
| --- | --- | --- |
| Unversioned | Startup immediate transaction | Migrating: create/alter schema, initialize health, prune older snapshots, publish version 1 in one transaction |
| Migrating | Any error / successful commit | Roll back schema+health+retention+version / Ready |
| Ready | Repeated startup | Preserve observation/health metadata; latest-only retention in transaction |
| Unsupported version | Startup | Reject before retention or schema mutation |
| Empty / HealthOnly / Snapshot | Invalid new numeric quote | Reject without mutating quotes or health |
| Any valid data state | Refresh write begins | Reserved: immediate transaction serializes writer before reading retained quotes |
| Reserved | At least one successful quote | Merged: replace successes by source, retain valid others, derive finite summaries and batch warnings, replace snapshot and source rows |
| Reserved | No successful quotes | Keep quote rows and snapshot timestamp untouched; update attempted-provider health only |
| Reserved / Merged | Any insert, health constraint or commit failure | RolledBack; prior snapshot and all prior health remain |
| Reserved / Merged | Commit succeeds | Committed snapshot+health, or health-only update; at most one snapshot remains |
| Any readable state | Deferred read transaction | Read snapshot metadata, quotes and health consistently, including absent snapshot |

Invariants: observation times are never inferred for migrated rows. New quote
prices are finite and positive; invalid retained prices are omitted on merge
and response evaluation. Snapshot summaries use all retained valid rows and
are persistence metadata, not fresh-only API aggregates. Their average must
remain finite for valid inputs (**F5** corrected overflow). Health constraints
exclude inconsistent outcome/error/status combinations. Deleting a snapshot
cascades quote rows but never provider health. A health-only failure can coexist
with an unchanged snapshot whose warnings describe an older batch; current
provider cards use current health, not those batch warnings.

Evidence: both legacy schema variants; unsupported-version and failed-migration
rollback; quote/health atomic failure; read/write interleaving even on an empty
database; restart preservation; invalid quote rejection; latest-only retention;
new `refresh_persists_finite_summaries_for_extreme_valid_quotes`.

## 9. Quote freshness and aggregate status

Implementation: [`models.rs`](../src/models.rs), [`freshness.rs`](../src/freshness.rs).
These enums are explicit; the transitions are derived on every response.

| Quote state | Guard at evaluation | Transition / consequence |
| --- | --- | --- |
| Invalid price | Nonfinite or nonpositive | Exclude row, regardless of timestamps |
| Unknown | Unconfigured source, missing time or unrepresentable age | Remain visible if price valid; never contribute |
| Future | Configured source, observation time after now | Visible with null age; never contribute |
| Fresh | Configured valid quote, checked age in [0, 90) seconds | Contribute once per provider, using newest qualifying duplicate |
| Fresh → Stale | Age reaches exactly 90s | Stop contributing; retained price/time unchanged |
| Any retained quote | Provider failure | Preserve quote/time; only health changes |
| Any | New valid provider observation committed | Replace quote and reevaluate at request time |
| Future / Unknown / Stale | New server evaluation or observation after clock change | May reclassify; never invent an observation time |

| Aggregate state | Exact predicate / permitted transitions |
| --- | --- |
| LIVE | Four fresh configured contributors and four successful health records; nonpersisted refresh error downgrades to DEGRADED |
| DEGRADED | At least one contributor with incomplete coverage or non-success health; can recover via new observations/health, or expire to STALE |
| STALE | Valid retained rows but zero contributors; new successful observation can restore contribution |
| UNAVAILABLE | Zero valid rows; new successful observation can restore contribution |

Invariants: zero contributors implies null average and spread; one implies its
price and zero spread; all aggregates are finite for valid positive inputs.
Coverage is unique, configured-order and at most four. `stale` is false only for
LIVE. Valid retained rows yield HTTP 200 even when STALE; no rows yield 503;
database read error yields 500 with empty untrusted state and safe warning.
Snapshot age never stands in for a provider's age. Clock corrections can change
server classifications, so Unix observation times are not a monotonic version.

Evidence: serialization tests, freshness edge/extreme/duplicate tests, exact
90s route expiry, unknown/future observations, retained fresh quotes after
failure, cold starts, migration/restart recovery and unreadable storage tests.

## 10. Dashboard session and request workers

Implementation: first closure in [`dashboard.js`](../src/dashboard.js).
States: `Unsupported`, `Hidden/Stopped`, `Visible/Running`; each worker separately
has `Idle`, `InFlight(owner_generation)`, `Waiting(timer)`.

| From | Event / guard | To / effects |
| --- | --- | --- |
| Initializing | Missing bounded-fetch or secure-ID capability | Unsupported; no presence/price requests |
| Initializing | Secure random per-document ID available | Hidden/Stopped or start visible generation; no browser storage needed |
| Stopped | Visible start/pageshow and ID available | Running; increment generation; request startup presence+cached price; start 1s age rendering |
| Running | Repeated start | No-op |
| Idle / Waiting worker | Poll, running and not busy | InFlight; own controller and body-inclusive deadline |
| InFlight | Another poll for same kind | No-op; never overlap same-kind requests |
| InFlight presence | Admitted startup object valid | Accept price subject to report sequence; first GET after 1s if refreshing, otherwise 5s |
| InFlight presence | Startup object absent/invalid or heartbeat failed | Bounded GET fallback; continue independent heartbeat loop |
| InFlight | Success/failure/abort settles | Release busy/controller, render only if running; schedule own next poll after 5s (0 if settling an old generation after restart) |
| Running | Hide/pagehide | Stopped; increment generation, clear age/poll timers, abort workers, best-effort inactive beacon or bounded 3s keepalive fallback; show IDLE |
| Old InFlight | Completes after stop/restart | Cannot publish into another generation; retain ownership until settlement, then resume at most one loop |

Invariants: no price/active-heartbeat polling while stopped; inactive beacons
are best effort and not part of the active-worker single-flight count. Abort
requests cancellation but ownership is released on settlement, not on the
abort call. Presence failure cannot block continued price polling. Requests
from different kinds may overlap on resume; price publication must also be
ordered across kinds (**F3**), not only protected by visibility generation.

Evidence: registration/legacy fallback, presence/price timeout, held startup
body, rapid visibility/page lifecycle, unsupported capabilities, denied storage,
distinct opener-tab IDs, and new cross-worker response-order regressions in
[`dashboard.test.mjs`](../tests/browser/dashboard.test.mjs).

## 11. Dashboard report and displayed freshness

State: optional last valid report, monotonic receipt time, latest accepted
price-request number, and independent price/presence errors. Display labels
include `Connecting`, `LIVE`, `DEGRADED`, `STALE`, `UNAVAILABLE`, `OFFLINE`,
`IDLE` and `UNSUPPORTED`.

| From | Event / guard | To / effects |
| --- | --- | --- |
| No report | Valid current-generation price response | Save report and receipt time; evaluate display |
| Any | Earlier-issued response than latest accepted report | Ignore, including its price error; do not reset ages or roll back quote/health state |
| Cached report with quotes | New UNAVAILABLE response or transport/body/contract failure | Retain report and age anchor; show OFFLINE / warning |
| Any | New accepted report with usable rows | Replace report, reset age anchor, clear price error |
| Cached report | 1s render tick | Add elapsed monotonic seconds to reported ages; drop expired contributors and recompute finite average/spread |
| LIVE | Some contribution expires / heartbeat error | DEGRADED; no local upgrade to LIVE |
| Report with retained rows | Last contribution expires | STALE (or OFFLINE if price transport failed) |
| Any running display | Price error / heartbeat recovery | OFFLINE / clear heartbeat warning; status still respects report and age |
| Any | Stop / unsupported | IDLE / UNSUPPORTED overrides running display |

Invariants: unknown/future/stale observations never become fresh solely from
local elapsed time. Unavailable/error responses cannot erase good quote cards.
Report acceptance is ordered by request issuance, not Unix timestamps (which
can move backward); it is not a server-side snapshot revision protocol. Source
names, warnings and provider messages render as text. Displayed observation
date comes from the latest nonfuture source observation, not snapshot save time.

Evidence: browser expiry, retained network/500/503/malformed responses,
recovery, safe-text/CSP checks, extreme values, and **F3** regression.

## 12. Server news cache

Implementation: [`news.rs`](../src/news.rs).
States are `(xml absent|present, checked_at absent|recent|expired,
stale flag, refreshing flag)`; useful names are `Cold`, `RefreshingCold`,
`Fresh`, `RefreshingRetained`, `FailedCold`, `FailedRetained`.

| From | Event / guard | To / effects |
| --- | --- | --- |
| Cold / Expired | Reader, no refresh reserved | Reserve refreshing under mutex; spawn one independent fetch; return immediately |
| RefreshingCold / RefreshingRetained | Concurrent reader | No second fetch; 503 if no XML, otherwise 200; `Retry-After: 2` |
| Refreshing | Valid bounded feed returned before 12s | Fresh; replace XML, clear stale, record monotonic completion, clear refreshing |
| Refreshing | HTTP/transfer/UTF-8/feed-marker/size/deadline failure | FailedCold / FailedRetained; preserve old XML, mark stale, record completion, clear refreshing |
| Fresh / Failed state | Reader less than 300s after completion | Return cached outcome; no retry |
| Fresh / Failed state | Reader at least 300s after completion | Expired → reserve refresh |

Invariants: the mutex is never held across network I/O; one refresh survives
client cancellation; both failures and successes start a 300s completion-based
gate. Feed transfer is bounded to 2MiB. Server validation checks RSS/channel
markers, not full XML correctness or useful headlines: browser parsing remains
an independent acceptance step. Returned `stale` includes expiration and can
be true while refreshing retained data. News does not activate price providers.

Evidence: cache/failure serving, held cold/expired concurrent reads, exact
failure-backoff recovery and refresh-deadline tests in `news.rs`.

## 13. Browser news acquisition and retries

Implementation: second closure in `dashboard.js`.
States: `Stopped`, `Waiting(next_check)`, `InFlight(owner_generation)`,
`Parsing`, with orthogonal `previous XML absent|present`.

| From | Event / guard | To / effects |
| --- | --- | --- |
| Stopped | Visible start | Increment generation; fetch only if no unsettled request and monotonic deadline reached; otherwise wait remaining delay |
| Waiting | Timer due and visible/current | InFlight; own controller with 15s header+body deadline |
| InFlight | HTTP success | Parse JSON and RSS XML; reject parser errors, missing XML, or no safe useful headlines |
| Parsing | New accepted XML | Replace up to 30 deduplicated HTTP(S) links as text; retain previous headlines on failure |
| InFlight / Parsing | Success with `refreshing` | Schedule next check after 2s |
| InFlight / Parsing | Positive numeric Retry-After on 429/503/OK | Retry in clamped 1–30s unless refreshing overrides to 2s |
| InFlight / Parsing | Ordinary success or failure without short hint | Retry after 300s; failure preserves prior headlines |
| Any running state | Hide/pagehide | Stopped; clear timer/deadline, abort, retain request ownership until settlement |
| Old request | Settles after stop/restart | Cannot publish or replace retry deadline; schedule current generation against remaining monotonic delay |

Invariants: one unsettled feed request across visibility generations; hidden
time counts toward the remaining retry delay. Only the current owner may
publish or update `next_check`. Wall-clock jumps cannot extend or bypass the
wait (**F4**). An RSS error cannot stop the independent price/presence workers.

Evidence: held headers/bodies, late success/failure, rapid hide/show,
initially-hidden, body deadline, cold-cache short retry, retained headlines,
remaining-delay, missing capability and new forward/backward-clock tests.

## 14. Feed motion

Motion states compose `running`, user/reduced-motion pause, hover/focus/manual
interaction and `scrollable|at_bottom`.

| From | Event / guard | To / effects |
| --- | --- | --- |
| Initializing | Read reduced-motion preference | Paused if requested, otherwise unpaused |
| Paused / Unpaused | Toggle button | Flip pause; update label and aria-pressed |
| Either | Wheel/touch/pointer/key interaction | Paused until explicit resume or preference change |
| Either | Reduced-motion preference changes | Set pause to new preference |
| Running motion | Hidden, hovered, focused, paused or not scrollable | No movement on 50ms tick |
| Running motion | Visible, unpaused, unhovered, unfocused and scrollable | Move one pixel per tick; reset bottom dwell when not at bottom |
| At bottom | Eligible ticks, dwell greater than 3 monotonic seconds | Wrap to top and reset dwell |
| Any | Accept replacement feed | Reset scroll position to top |

Pause affects movement, not feed acquisition. CSS also pauses page effects
when hidden or reduced motion is requested. Browser tests cover scrolling,
pause, reduced motion and hidden-page effects.

## 15. Embedded asset revalidation

Implementation: [`ui.rs`](../src/ui.rs). State: `Uninitialized` or
`Initialized(content_hash)` per embedded asset's `OnceLock`.

| From | Event / guard | To / effects |
| --- | --- | --- |
| Uninitialized | First request | Compute weak ETag once from embedded bytes; Initialized |
| Initialized | Matching weak/strong If-None-Match or `*` | 304, ETag retained, no body |
| Initialized | Missing or nonmatching validator | 200 with embedded bytes and ETag |
| Either | HEAD | Same status/headers as GET, no response body |
| Initialized | Process restart with different embedded bytes | New process computes content's new validator |

Validators remain unchanged for the process lifetime. Responses retain security
headers; assets/HTML use `no-cache`, APIs/health use `no-store`. Validators
describe content, not mutable server freshness. Browser tests cover motion,
pause, reduced motion and legacy deployment caches; Rust tests cover GET/HEAD,
304, differing content, CSP and cache headers.

## Confirmed reachable violations, ranked by impact

These are executable counterexamples, not hypothetical enum combinations.
Regressions were run against the original implementation before corrections.
All five failure modes below are corrected; four related Beads issues group
the fixes. Priority reflects effect and exposure, not just ease of reproduction.

| Finding / issue | Reachable trace and observed evidence before correction | Impact / correction / regression |
| --- | --- | --- |
| **F1 — P1**, `bitcoin_price_tracker-jrk.4` | Admit request with concurrency=1 → `run_blocking` job enters held work → abort caller → HTTP permit drops but blocking work continues → retry returns **200**, expected **503**. Repeating this schedule can queue/run work beyond the route limit. A 20s handler timeout has the same lifetime split. | Availability: cancellation/retry load bypasses work bounds. Share the admitted permit through the task-local request scope and capture a clone in queued/running blocking jobs. `cancelled_request_retains_capacity_until_its_blocking_storage_job_finishes` and `timed_out_request_retains_capacity_until_its_blocking_storage_job_finishes` verify rejection while held, independent health and subsequent recovery. |
| **F2 — P2**, `bitcoin_price_tracker-jrk.1` | Rate=1: consume at t0; consume refill at t1; a delayed caller sampled at t0 acquires mutex after t1 and fails but rewinds `updated`; another t1 call incorrectly succeeds. Regression failed with “the same second must not refill twice.” Sampling outside the mutex makes this interleaving possible under concurrent admission. | Over-admission under load. Clamp accounting time to its prior value under the bucket lock. `delayed_admission_cannot_refill_an_already_accounted_interval` forces that ordering and checks the next legitimate refill. |
| **F3 — P2**, `bitcoin_price_tracker-jrk.2` | Hold first price GET → hide/show synchronously → resumed startup presence is held → cancelled old GET settles and starts a new GET → new GET renders **$200,000** → release earlier startup cache → display regresses to **$100,150** in the same generation. | Incorrect quote/health display and resetting ages to an older report. Order accepted price results across both workers by request sequence; discard older price results/errors. Chromium `a delayed startup price cannot overwrite a newer resumed price response` reproduces with real aborted fetch and held route response, without ignoring abort artificially. |
| **F4 — P2**, `bitcoin_price_tracker-jrk.2` | Cache requests short 2s retry → hide after response → advance 1s → shift wall clock ±1h → show. Backward shift retains old headline beyond the due time; forward shift fetches immediately instead of respecting the remaining second. Both regressions failed. | Delayed news recovery or premature retry. Use `performance.now()` for retry deadlines and scroll dwell. Chromium `RSS retry delay survives a backward/forward wall-clock change` verifies remaining delay and recovery in both directions. |
| **F5 — P3**, `bitcoin_price_tracker-jrk.3` | Pass four valid finite positive `f64::MAX` observations through real `store_refresh_sync` transaction → load persisted snapshot → average is **Some(inf)**, expected finite `f64::MAX`. | Durable summary corruption. Public price aggregation was already protected, limiting user impact. Use incremental bounded mean for persistence too. `refresh_persists_finite_summaries_for_extreme_valid_quotes` verifies both largest and smallest positive finite inputs across replacement transactions. |

## Review boundaries and validation

The inventory covers application-owned runtime state in `src/` and both
dashboard closures, including schema migration, cancellation, restart,
completion order, retries and shutdown. Local fixtures, injected clocks,
semaphores/channels, transaction hooks and Chromium virtual time exercise the
named traces; ordinary checks never rely on public provider availability.
This is an implementation audit, not model checking of every interleaving.

No additional confirmed invalid transition was found in the reviewed refresh,
presence, migration and atomic-write paths. Best-effort beacon ordering,
process-local restart resets, disposable RSS work on exit and independently
sampled response metadata are intentional limits recorded above. Existing
`bitcoin_price_tracker-p6w` tracks measured synchronized-admission burst recovery
separately; the corrections here do not claim to resolve that capacity study.

Required validation: `cargo fmt --all -- --check`,
`cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`, and
`npm test --prefix tests/browser` after locked npm installation and Chromium
preparation. Validation passed: formatting, Clippy with warnings denied, 116
Rust tests (one optional WebDriver test ignored), and all 48 Chromium tests.
The seven new regression cases cover the five failure modes above, with both
cancellation and handler-timeout storage lifetimes exercised.
Use the checked-out `rust-toolchain.toml`; this upstream baseline
pins Rust 1.99.1. Runtime image/dependencies are unchanged; the optional external
WebDriver smoke test is not part of deterministic validation.

When changing a state field, owner, guard, retry rule or observable status,
update its table and invariant here alongside the implementation and an
appropriate behavioral regression. New findings belong in Beads and should
include a concrete reachable trace before being described as confirmed.
