# Local origin load testing

`scripts/load_test.py` measures this application's HTTP origin with disposable
local processes, mock providers, and temporary SQLite data. It has no target URL
option and cannot be pointed at production. It does not contact public price or
news providers. No container engine or third-party Python packages are required.

Use Linux, Python 3.11 or newer, and the pinned Rust toolchain. Run from the repo
root, with a working Cargo/Rust PATH and an unused output filename:

```bash
python3 scripts/load_test.py --seconds 90 --output target/load-test/capacity.jsonl
python3 scripts/load_test.py --scenarios burst --cold-start --seconds 90 \
  --output target/load-test/cold-burst.jsonl
```

The first command runs seven cases, taking approximately fourteen minutes:
healthy providers at 100, 500, 1,000, and 1,250 viewers, followed by a simultaneous
1,000-viewer admission burst, provider contention, and stalled RSS. Each case has
a fresh app process and database, a 20-second linear ramp (zero for a burst), ten
seconds to settle, and 90 seconds of steady measurement. Every case uses runtime
defaults. No capacity settings are changed by the harness. The second command
also measures a burst before the database contains any quotes.

For a short harness smoke check (not capacity or cooldown evidence):

```bash
python3 -m unittest discover -s scripts -p test_load_test.py
python3 scripts/load_test.py --viewers 8 --contention-viewers 8 \
  --seconds 2 --ramp-seconds 1 --output target/load-test/smoke.jsonl
```

The fixture is an ignored Rust test compiled with `cargo test --locked --release
--no-run`. It runs the real router, connection lifecycle, admission, refresh
coordinator, SQLite storage, and news handler with the system clock. Cleartext
and mocked endpoints are injected only into this test process; production keeps
its HTTPS-only client and public endpoints. RSS now uses the same internal
endpoint dependency injection as the four price providers. The fixture is absent
from production builds. A bounded Python regression suite runs in CI; capacity
tests remain opt-in and `cargo test --locked` skips the serving fixture.

The app and mock listeners bind only to `127.0.0.1` on OS-assigned ports. Linux
clients bind their source to an IPv4 address on a local non-loopback interface;
the harness verifies the accepted peer before testing. This exercises the
normal 128-connection pool while keeping the listener inaccessible remotely.
Plain loopback load would incorrectly measure the four reserved probe slots.
An available local non-loopback IPv4 interface is required; no packets are sent
to a remote host to discover it. Separate health requests test each pool.

Each output file contains one environment JSON record and one record per case.
Records are flushed after each completed case. Files are created exclusively,
so an existing report is never overwritten. `--fixture-binary` can reuse a
previously built release test executable. `--viewers`, `--contention-viewers`,
`--seconds`, `--ramp-seconds`, and `--scenarios` select smaller or longer runs;
arguments have bounds. Cold starts apply only to healthy and burst cases.

## Workload and accounting

Each synthetic viewer starts a news loop alongside one initial presence POST
with `include_price: true`. The next presence request waits five seconds after
completion, using the dashboard's three-second deadline. The first price GET
waits one second if the bundled price is refreshing, five seconds otherwise, or
runs immediately when no bundled price arrived. Subsequent independent price
GETs wait five seconds after completion and use an eight-second deadline.
News uses its 15-second deadline, the dashboard's bounded `Retry-After` handling,
two-second refresh retries, and five-minute default retry interval. Each loop
has at most one outstanding request. Rejected admissions keep retrying with the
same session ID and keep reading prices, as the dashboard does.

By default, a disposable warm-up viewer seeds real quotes through the mock
providers, then is removed before measurement. The news cache begins cold.
Provider contention starts after this seeding: CoinGecko takes six seconds,
exceeding its five-second client deadline; Coinbase returns 429 with
`Retry-After: 30`; Gemini returns 500; Kraken remains healthy. The RSS case stalls
the feed for 15 seconds, exceeding its 12-second deadline. Cached-price and both
health probes run independently once a second during every case. Provider and
RSS contention are separate cases so their effects can be inspected separately.

Startup and steady metrics are separated by request start time. Reports include
completed HTTP requests/sec, successful requests/sec, HTTP codes, percentages
of all attempts receiving 429 and 503, transport errors, and full-request
latency p50/p95/p99 including errors. A 503 with an `UNAVAILABLE` price or missing
RSS is distinguished from a busy response; transport rejection at connection
admission is reported separately from HTTP overload. Admission failures count
first attempts; unique viewers ever admitted and the largest returned active
count describe eventual recovery. The requested duration, wall duration, ramp,
and settling time are explicit. Requests started before the deadline can finish
after it; achieved rates use the configured phase duration.

The app process is sampled every 200 ms via `/proc`: CPU as a percentage of one
core over the whole case, maximum RSS, threads, and open file descriptors.
Generator CPU, peak generator RSS since launch, and event-loop lag p50/p95/p99
help identify a saturated generator. The environment includes CPU, memory,
cgroup limits, kernel, Python/Rust versions, Git revision and dirty paths,
file-descriptor limits, and the release fixture's SHA-256. This is a release test
binary running on the host, without production tracing initialization or
container limits. Mocks and the generator share a Python process on the host.

Upstream dispatch timestamps and counters are recorded for each provider and
RSS, including the seed. Concurrent initial requests are grouped within 500 ms;
later refresh groups must be at least 9.9 seconds apart. Per-provider checks
also enforce ten seconds, increasing failure backoff, Coinbase's 30-second hint,
and the slow provider's five-second request time plus backoff (100 ms timing
tolerance). RSS cannot refetch within five minutes. Violations fail the run.
Short runs with no repeated failed-provider attempt provide no cooldown
measurement; inspect dispatches rather than interpreting zero violations alone
as proof of that coverage.

Cleanup signals and waits for only the child process created by the harness,
then closes its mock listener/tasks and removes its own temporary directory.
Shutdown has a bounded wait and a final kill of that child if needed. No shared
database, existing service, container, volume, or unrelated file is removed.
Interrupting a run preserves already flushed case records. SIGKILL of the
harness cannot run Python cleanup.

## Interpretation

Use the measured successful price/presence rate, active admissions, tail latency,
errors, generator headroom, and health probes together. At 1,000 viewers the
nominal five-second cadence is about 200 requests/sec in each polling group;
default token buckets allow 250/sec per group. This arithmetic alone does not
establish throughput, burst tolerance, or service-level guarantees. A passing
steady interval does not establish indefinite endurance.

These are local origin measurements of visible-tab API traffic. They exclude
HTML/CSS/JavaScript downloads, browser rendering, TLS, reverse proxies, WAN
latency, actual provider variability, and public-edge/NAT fairness. The
connection lifetime and one-request-per-connection behavior remain enabled;
the generator does not pool or retry connections behind the measurements.
Keep public-edge and shared-NAT abuse/fairness observations separate from these
results. Production resource limits and slower disks require a separate local
repeat under equivalent constraints before treating the results as a capacity
commitment.

## Recorded run: 2026-11-02

Raw reports: [capacity matrix](performance/2026-11-02-capacity.jsonl) and
[cold-start burst](performance/2026-11-02-cold-burst.jsonl). Commands are the two
90-second examples above. The capacity matrix uses seeded quotes; its earlier
report schema omits the optional `cold_start` field, which means false. Each
steady measurement lasted 90 seconds; ramped cases lasted about 120 seconds in
total and burst cases about 100 seconds.

The host was Linux 7.2.6 x86_64, AMD Ryzen 7 3800X (8 physical/16 logical cores),
about 62.7 GiB RAM, Python 3.12.3, Rust 1.85.1, with a 4,096 soft file-descriptor
limit. The exposed cgroup root did not provide CPU/memory limit files, so those
fields are null rather than a claim about production resource allowances. The
host ran the optimized fixture directly. Its SHA-256 is
`171451fcf85bd2579b2044cf32aa2171f85c687ef15ea4f73c57e3abc8394acf`.
The raw revision records the original dirty checkout; the executable hash
identifies the measured build. All ordinary price/presence rates below exclude
the additional once-per-second cached-price probe.

| Scenario | Requested viewers | Unique admitted | Successful price/s | Successful heartbeat/s | Price p99, ms | App max RSS, MiB | App CPU, % of one core |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Healthy, 20s ramp | 100 | 100 | 20.00 | 20.00 | 2.25 | 12.46 | 1.66 |
| Healthy, 20s ramp | 500 | 500 | 99.98 | 99.96 | 2.11 | 12.58 | 6.86 |
| Healthy, 20s ramp | 1,000 | 1,000 | 199.97 | 199.91 | 2.05 | 13.35 | 12.62 |
| Healthy, 20s ramp | 1,250 | 1,000 | 249.91 | 199.97 | 2.50 | 15.80 | 13.31 |
| Simultaneous warm burst | 1,000 | 898 | 70.98 | 96.30 | 131.13 | 33.06 | 20.61 |
| Simultaneous cold burst | 1,000 | 977 | 83.94 | 111.33 | 119.32 | 34.46 | 22.35 |
| Slow/failing providers, 20s ramp | 1,000 | 1,000 | 199.94 | 199.91 | 2.36 | 15.35 | 12.55 |
| Stalled RSS, 20s ramp | 1,000 | 1,000 | 199.89 | 199.91 | 2.21 | 14.44 | 12.66 |

All ramped cases had zero steady price transport errors, 429s, or 503s.
At 1,250 requested viewers, 250 initial admissions received 429 and about 20% of
steady heartbeats received 429, consistent with the 1,000-viewer cap. Rejected
clients continued price polling, leaving that group close to its 250/sec budget;
this does not support raising the viewer default. At 1,000 ramped viewers, every
initial admission succeeded and every measured steady heartbeat succeeded.

The warm burst had 871 initial admission failures (808 transport failures and
63 busy 503s). In its steady phase, 58.85% of price attempts failed at transport,
3.55% received 429, and 2.11% received busy 503. Only 898 viewers were ever
admitted during the run; this is not a simultaneous active-viewer count. Fixed
five-second retries remained poorly distributed throughout the measured period.
Both health probe groups still returned 200 for every completed steady attempt,
but their p99 increased to about 47 ms for the ordinary connection pool and
65 ms for the loopback pool. The independent cached-price probe had one 429.
This is evidence of a burst/recovery limit despite low average CPU use.

With stalled RSS, startup news traffic produced 3,101 expected cold/unavailable
503s and 54 rate-limit 429s across 3,155 attempts; there were no transport errors
or busy responses in that group. Price and presence continued successfully in
both startup and steady phases. RSS was fetched exactly once and remained in
failure backoff. Both health probes and the independent cached-price probe
completed all 90 steady requests successfully in the provider and RSS cases,
with p99 below 2.7 ms. The news outage therefore did not make cached prices or
database health unavailable in these runs.

Every case passed the observed upstream cadence checks. Ramped healthy cases
had 12–13 refresh groups in about 120 seconds, with minimum group gaps of at
least 10 seconds. Raising viewers from 100 to 1,250 did not multiply upstream
requests. During provider contention, slow CoinGecko dispatches were at about
9.97, 44.44, and 84.49 seconds; Coinbase 429 dispatches were at about 9.97,
54.48, and 94.52 seconds, preserving its 30-second hint. Gemini failed at about
9.97, 34.44, 74.48, and 114.53 seconds, respecting increasing cooldowns; Kraken
continued successfully. The maximum 300-second cooldown is covered by existing
injected-clock Rust tests rather than this short wall-clock run.

Generator CPU stayed below 13% of one core and generator event-loop p99 below
2.5 ms in the ramped matrix. The warm burst's event-loop p99 was about 45 ms, so
burst latency includes some generator scheduling pressure. Burst behavior is
workload-sensitive and should be repeated on the intended origin resource
limits before selecting a retry or admission change.

These results support **1,000 staggered visible-tab API viewers for the tested
90-second interval on this host**, including the simulated upstream failures.
They do not support a simultaneous 1,000-tab startup guarantee or a higher
default. Keep the existing defaults. A 20-second ramp to 1,000 viewers is the
tested successful admission pattern, not a general minimum ramp requirement.
Follow-up `bitcoin_price_tracker-p6w` tracks burst recovery; longer endurance,
equivalent container constraints, and edge/NAT fairness remain separate from
these local measurements.
