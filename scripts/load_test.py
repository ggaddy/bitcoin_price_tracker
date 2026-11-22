#!/usr/bin/env python3
"""Linux-only, opt-in origin capacity measurement. Uses only Python's stdlib.

Starts an optimized Rust test fixture, loopback mocks, and temporary SQLite data.
There is deliberately no target URL option. See docs/load-testing.md.
"""
import argparse
import asyncio
from collections import Counter, defaultdict
import contextlib
import datetime
import fcntl
import hashlib
import ipaddress
import json
import math
import os
from pathlib import Path
import platform
import resource
import signal
import socket
import struct
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
PROVIDERS = ("coingecko", "coinbase", "kraken", "gemini")
BODIES = dict(zip(PROVIDERS, (
    '{"bitcoin":{"usd":100000}}',
    '{"data":{"base":"BTC","currency":"USD","amount":"100100"}}',
    '{"error":[],"result":{"XXBTZUSD":{"c":["100200","1"]}}}',
    '{"symbol":"BTCUSD","bid":"100300"}',
)))
BODIES["news"] = '<rss><channel><item><title>Local fixture</title><link>https://example.invalid/</link></item></channel></rss>'


def percentiles(values):
    ordered = sorted(values)
    return {f"p{p}": round(ordered[max(0, math.ceil(len(ordered) * p / 100) - 1)], 3)
            if ordered else None for p in (50, 95, 99)}


def source_address():
    """Discover a local interface without contacting any external address."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        for _, name in socket.if_nameindex():
            try:
                data = fcntl.ioctl(sock, 0x8915, struct.pack('256s', name.encode()[:15]))
                address = socket.inet_ntoa(data[20:24])
                if not ipaddress.ip_address(address).is_loopback:
                    return address
            except OSError:
                continue
    raise RuntimeError("A local non-loopback IPv4 interface is required for the remote connection pool")


async def check_source(address):
    """Keep listeners private while making the origin see a non-loopback peer."""
    seen = asyncio.get_running_loop().create_future()

    async def accept(reader, writer):
        seen.set_result(writer.get_extra_info("peername")[0])
        writer.close()
        await writer.wait_closed()

    server = await asyncio.start_server(accept, "127.0.0.1", 0)
    async with server:
        _, writer = await asyncio.open_connection("127.0.0.1", server.sockets[0].getsockname()[1],
                                                 local_addr=(address, 0))
        writer.close()
        await writer.wait_closed()
        peer = await asyncio.wait_for(seen, 2)
        if ipaddress.ip_address(peer).is_loopback:
            raise RuntimeError("Load traffic would use the four-slot loopback pool")


async def exchange(port, path, payload=None, source=None, timeout=8):
    writer = None
    try:
        async with asyncio.timeout(timeout):
            reader, writer = await asyncio.open_connection(
                "127.0.0.1", port, local_addr=(source, 0) if source else None)
            body = json.dumps(payload).encode() if payload is not None else b""
            method = "POST" if payload is not None else "GET"
            headers = (f"{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n"
                       f"Content-Type: application/json\r\nContent-Length: {len(body)}\r\n\r\n")
            writer.write(headers.encode() + body)
            await writer.drain()
            raw = await reader.readuntil(b"\r\n\r\n")
            lines = raw.decode().split("\r\n")
            status = int(lines[0].split()[1])
            response_headers = dict(line.lower().split(": ", 1) for line in lines[1:] if ": " in line)
            # Axum may use chunked encoding for bodies with no exact size hint.
            if response_headers.get("transfer-encoding") == "chunked":
                chunks = []
                while True:
                    size = int((await reader.readline()).split(b";", 1)[0], 16)
                    if not size:
                        await reader.readuntil(b"\r\n")
                        break
                    chunks.append(await reader.readexactly(size))
                    await reader.readexactly(2)
                data = b"".join(chunks)
            elif "content-length" in response_headers:
                data = await reader.readexactly(int(response_headers["content-length"]))
            else:
                data = await reader.read()
            return status, response_headers, json.loads(data)
    finally:
        if writer:
            writer.close()
            with contextlib.suppress(OSError):
                await writer.wait_closed()


class Mocks:
    def __init__(self):
        self.mode = "healthy"
        self.events = []
        self.tasks = set()

    async def handle(self, reader, writer):
        task = asyncio.current_task()
        self.tasks.add(task)
        try:
            raw = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), 2)
            name = raw.split(b" ")[1].decode().lstrip("/")
            if name not in BODIES:
                raise ValueError("Unexpected fixture endpoint")
            event = {"provider": name, "at": time.monotonic(), "mode": self.mode}
            self.events.append(event)
            status, extra = 200, ""
            if self.mode == "providers":
                if name == "coingecko":
                    await asyncio.sleep(6)  # longer than the production five-second deadline
                elif name == "coinbase":
                    status, extra = 429, "Retry-After: 30\r\n"
                elif name == "gemini":
                    status = 500
            if self.mode == "news" and name == "news":
                await asyncio.sleep(15)  # longer than the production RSS deadline
            body = BODIES[name].encode()
            writer.write((f"HTTP/1.1 {status} Fixture\r\n{extra}Content-Length: {len(body)}\r\n"
                          "Connection: close\r\n\r\n").encode() + body)
            await writer.drain()
        except (OSError, asyncio.IncompleteReadError, asyncio.TimeoutError):
            pass
        finally:
            self.tasks.discard(task)
            writer.close()
            with contextlib.suppress(OSError):
                await writer.wait_closed()

    async def stop(self):
        pending = list(self.tasks)
        for task in pending:
            task.cancel()
        await asyncio.gather(*pending, return_exceptions=True)


def process_sample(pid):
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return {
        "cpu_seconds": (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK"),
        "rss_mib": int(fields[21]) * os.sysconf("SC_PAGE_SIZE") / 1024**2,
        "threads": int(fields[17]),
        "fds": len(list(Path(f"/proc/{pid}/fd").iterdir())),
    }


class Measurement:
    def __init__(self, port, source, ramp, seconds):
        self.port, self.source = port, source
        self.started = time.monotonic()
        self.steady_at = self.started + ramp + 10  # allow retries/initial refresh to settle
        self.end = self.steady_at + seconds
        self.metrics = defaultdict(lambda: {"codes": Counter(), "latencies": [], "errors": Counter(),
                                            "unavailable": 0, "busy": 0})
        self.admitted = set()
        self.initial_failures = 0
        self.max_active = 0
        self.lags = []
        self.samples = []

    async def request(self, group, path, payload=None, timeout=8, local=False):
        started = time.monotonic()
        phase = "steady" if started >= self.steady_at else "startup"
        metric = self.metrics[(phase, group)]
        try:
            status, headers, data = await exchange(self.port, path, payload,
                                                   None if local else self.source, timeout)
            metric["codes"][str(status)] += 1
            if data.get("status") == "UNAVAILABLE" or (path == "/api/news" and not data.get("xml")):
                metric["unavailable"] += 1
            if (data.get("error") or "").startswith("Server busy"):
                metric["busy"] += 1
            if payload and status == 200:
                self.admitted.add(payload["session_id"])
            self.max_active = max(self.max_active, data.get("active_viewers", 0))
            if group == "admission" and status != 200:
                self.initial_failures += 1
            return status, headers, data
        except (OSError, ValueError, asyncio.IncompleteReadError, asyncio.LimitOverrunError,
                asyncio.TimeoutError) as error:
            metric["errors"][type(error).__name__] += 1
            if group == "admission":
                self.initial_failures += 1
            return 0, {}, {}
        finally:
            metric["latencies"].append((time.monotonic() - started) * 1000)

    async def sleep(self, seconds):
        await asyncio.sleep(min(seconds, max(0, self.end - time.monotonic())))

    async def loop(self, group, path, payload=None, delay=5, timeout=8, local=False):
        await self.sleep(delay)
        while time.monotonic() < self.end:
            await self.request(group, path, payload, timeout, local)
            await self.sleep(5 if group in ("price", "heartbeat") else 1)

    async def news(self):
        while time.monotonic() < self.end:
            status, headers, data = await self.request("news", "/api/news", timeout=15)
            delay = 300
            retry = float(headers.get("retry-after", 0))
            if (status in (429, 503) or 200 <= status < 300) and retry > 0:
                delay = min(30, max(1, retry))
            if status == 200 and data.get("refreshing"):
                delay = 2
            await self.sleep(delay)

    async def viewer(self, index, count, ramp):
        await self.sleep(ramp * index / count)
        payload = {"session_id": f"load-{index}", "active": True}
        # Feed and initial admission are independent, as in dashboard.js.
        feed = asyncio.create_task(self.news())
        try:
            _, _, data = await self.request("admission", "/api/presence",
                                           dict(payload, include_price=True), timeout=3)
            price = data.get("price")
            delay = (1 if price.get("refreshing") else 5) if price else 0
            await asyncio.gather(
                self.loop("heartbeat", "/api/presence", payload, timeout=3),
                self.loop("price", "/api/price", delay=delay), feed)
        finally:
            feed.cancel()
            await asyncio.gather(feed, return_exceptions=True)

    async def monitor(self, pid):
        while time.monotonic() < self.end:
            self.samples.append(process_sample(pid))
            before = time.monotonic()
            await self.sleep(0.2)
            self.lags.append(max(0, (time.monotonic() - before - 0.2) * 1000))

    def report(self):
        result = {}
        for (phase, group), metric in sorted(self.metrics.items()):
            count = len(metric["latencies"])
            duration = (self.end - self.steady_at if phase == "steady"
                        else self.steady_at - self.started)
            codes = metric["codes"]
            result[f"{phase}/{group}"] = {
                "attempts": count, "http_codes": dict(codes), "transport_errors": dict(metric["errors"]),
                "achieved_rps": round(sum(codes.values()) / duration, 3),
                "successful_rps": round(sum(n for code, n in codes.items() if code.startswith("2")) / duration, 3),
                "429_percent": round(100 * codes["429"] / count, 3),
                "503_percent": round(100 * codes["503"] / count, 3),
                "unavailable_bodies": metric["unavailable"], "busy_bodies": metric["busy"],
                "latency_ms_including_errors": percentiles(metric["latencies"]),
            }
        samples = self.samples
        return {"metrics": result, "initial_admission_failures": self.initial_failures,
                "unique_viewers_ever_admitted": len(self.admitted), "max_active_viewers_observed": self.max_active,
                "generator_event_loop_lag_ms": percentiles(self.lags),
                "app_resources": {
                    "cpu_percent_one_core": round(100 * (samples[-1]["cpu_seconds"] - samples[0]["cpu_seconds"])
                                                  / (self.end - self.started), 2),
                    **{f"max_{key}": round(max(s[key] for s in samples), 2)
                       for key in ("rss_mib", "threads", "fds")}}}


def cadence(events, started):
    """Validate observed dispatches, preserving the initial concurrent full refresh."""
    violations, groups = [], []
    prices = sorted((event for event in events if event["provider"] != "news"), key=lambda e: e["at"])
    for event in prices:
        if not groups or event["at"] - groups[-1][0]["at"] > 0.5:
            groups.append([event])
        else:
            groups[-1].append(event)
    gaps = [b[0]["at"] - a[0]["at"] for a, b in zip(groups, groups[1:])]
    if any(gap < 9.9 for gap in gaps):
        violations.append("global ten-second gate")
    summary = {}
    for name in (*PROVIDERS, "news"):
        attempts = [e for e in events if e["provider"] == name]
        failures = 0
        intervals = []
        for previous, current in zip(attempts, attempts[1:]):
            gap = current["at"] - previous["at"]
            intervals.append(gap)
            minimum = 10
            if previous["mode"] == "providers" and name != "kraken":
                failures += 1
                minimum = min(10 * 2**(failures - 1), 300)
                minimum += 5 if name == "coingecko" else 0
                if name == "coinbase":
                    minimum = max(minimum, 30)
            else:
                failures = 0
            if name == "news":
                minimum = 300
            if gap < minimum - 0.1:
                violations.append(f"{name} cooldown: {gap:.3f}s < {minimum}s")
        summary[name] = {"requests": len(attempts), "minimum_gap_seconds": round(min(intervals), 3) if intervals else None,
                         "dispatches": [{"seconds": round(e["at"] - started, 3), "mode": e["mode"]} for e in attempts]}
    return {"providers": summary, "refresh_groups": len(groups),
            "minimum_refresh_gap_seconds": round(min(gaps), 3) if gaps else None,
            "violations": violations}


def build_fixture():
    result = subprocess.run(["cargo", "test", "--locked", "--release", "--no-run", "--message-format=json"],
                            cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True)
    for line in result.stdout.splitlines():
        item = json.loads(line)
        if item.get("profile", {}).get("test") and item.get("executable") and item["target"]["name"] == "bitcoin_price_tracker":
            return Path(item["executable"])
    raise RuntimeError("Cargo did not report the test executable")


async def run_case(binary, source, scenario, count, ramp, seconds, cold_start=False):
    mocks = Mocks()
    server = await asyncio.start_server(mocks.handle, "127.0.0.1", 0)
    process = None
    try:
        async with server:
            with tempfile.TemporaryDirectory(prefix="btc-load-") as directory:
                env = dict(os.environ, BTC_LOAD_DIRECTORY=directory,
                           BTC_LOAD_UPSTREAM=f"127.0.0.1:{server.sockets[0].getsockname()[1]}")
                with open(Path(directory) / "fixture.log", "w+") as log:
                    process = subprocess.Popen([str(binary), "--ignored", "--exact", "load_fixture::serve", "--nocapture"],
                                               env=env, cwd=ROOT, stdout=log, stderr=log)
                    try:
                        for _ in range(150):
                            ready = Path(directory) / "ready.json"
                            if ready.exists():
                                try:
                                    config = json.loads(ready.read_text())
                                    break
                                except json.JSONDecodeError:
                                    pass
                            if process.poll() is not None:
                                log.seek(0)
                                raise RuntimeError(log.read())
                            await asyncio.sleep(0.1)
                        else:
                            raise RuntimeError("Fixture startup deadline exceeded")
                        port = int(config["address"].rsplit(":", 1)[1])
                        if not cold_start:
                            # Seed real quotes before contention; remove only our warm-up viewer.
                            seed = {"session_id": "warmup", "active": True, "include_price": True}
                            await exchange(port, "/api/presence", seed, timeout=3)
                            for _ in range(100):
                                status, _, _ = await exchange(port, "/api/price")
                                if status == 200:
                                    break
                                await asyncio.sleep(0.05)
                            else:
                                raise RuntimeError("Local providers did not seed a quote")
                            await exchange(port, "/api/presence", dict(seed, active=False), timeout=3)
                        mocks.mode = scenario
                        measurement = Measurement(port, source, ramp, seconds)
                        generator_cpu = time.process_time()
                        await asyncio.gather(
                            *(measurement.viewer(index, count, ramp) for index in range(count)),
                            measurement.loop("health_remote", "/health", delay=0),
                            measurement.loop("health_loopback", "/health", delay=0, local=True),
                            measurement.loop("cached_price_probe", "/api/price", delay=0),
                            measurement.monitor(process.pid))
                        result = {"type": "case", "scenario": scenario, "viewers": count,
                                  "cold_start": cold_start,
                                  "ramp_seconds": ramp, "settle_seconds": 10, "steady_seconds": seconds,
                                  "wall_seconds": round(time.monotonic() - measurement.started, 3),
                                  "config": config, **measurement.report(),
                                  "upstream_cadence": cadence(mocks.events, measurement.started),
                                  "generator_cpu_percent_one_core": round(100 * (time.process_time() - generator_cpu)
                                                                          / (time.monotonic() - measurement.started), 2),
                                  "generator_peak_rss_mib_since_start": round(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024, 2)}
                    finally:
                        if process.poll() is None:
                            process.send_signal(signal.SIGTERM)
                        try:
                            await asyncio.to_thread(process.wait, 32)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            await asyncio.to_thread(process.wait)
                        if process.returncode:
                            log.seek(0)
                            raise RuntimeError(f"Fixture exited {process.returncode}: {log.read()}")
                    return result
    finally:
        server.close()
        await server.wait_closed()
        await mocks.stop()


def environment(binary, args):
    def read(path):
        try:
            return Path(path).read_text().strip()
        except OSError:
            return None
    return {"type": "environment", "schema_version": 1,
            "utc": datetime.datetime.now(datetime.timezone.utc).isoformat(), "command": vars(args),
            "platform": platform.platform(), "python": platform.python_version(),
            "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
            "git_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
            "git_status": subprocess.check_output(["git", "status", "--short"], cwd=ROOT, text=True).splitlines(),
            "fixture_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
            "logical_cpus": os.cpu_count(), "cpu_model": next((line.split(":", 1)[1].strip() for line in
                  Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name")), None),
            "memory": read("/proc/meminfo"), "cgroup_cpu_max": read("/sys/fs/cgroup/cpu.max"),
            "cgroup_memory_max": read("/sys/fs/cgroup/memory.max"),
            "fd_limit": list(resource.getrlimit(resource.RLIMIT_NOFILE))}


async def main(args, binary, output):
    source = source_address()
    await check_source(source)
    output.write(json.dumps(environment(binary, args)) + "\n")
    output.flush()
    violations = []
    scenarios = args.scenarios.split(",")
    for scenario in scenarios:
        counts = args.viewers if scenario == "healthy" else [args.contention_viewers]
        for count in counts:
            ramp = 0 if scenario == "burst" else args.ramp_seconds
            print(f"{scenario}: {count} viewers, ramp {ramp}s + settle 10s + measure {args.seconds}s", flush=True)
            result = await run_case(binary, source, scenario, count, ramp, args.seconds, args.cold_start)
            output.write(json.dumps(result) + "\n")
            output.flush()
            violations.extend(result["upstream_cadence"]["violations"])
            price = result["metrics"].get("steady/price", {})
            print(json.dumps({"admitted": result["unique_viewers_ever_admitted"],
                              "initial_failures": result["initial_admission_failures"],
                              "price": price, "resources": result["app_resources"],
                              "cadence_violations": result["upstream_cadence"]["violations"]}), flush=True)
    if violations:
        raise RuntimeError(f"Upstream cadence violations: {violations}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--viewers", type=lambda value: [int(v) for v in value.split(",")], default=[100, 500, 1000, 1250])
    parser.add_argument("--contention-viewers", type=int, default=1000)
    parser.add_argument("--seconds", type=float, default=60)
    parser.add_argument("--ramp-seconds", type=float, default=20)
    parser.add_argument("--scenarios", default="healthy,burst,providers,news")
    parser.add_argument("--cold-start", action="store_true", help="Skip quote seeding (healthy/burst only)")
    parser.add_argument("--fixture-binary", help="Reuse a cargo test --release executable")
    parser.add_argument("--output", default=f"target/load-test/{datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')}.jsonl")
    args = parser.parse_args()
    if (not all(1 <= n <= 10000 for n in [*args.viewers, args.contention_viewers])
            or not 1 <= args.seconds <= 3600 or not 0 <= args.ramp_seconds <= 300
            or not args.scenarios.split(",") or any(s not in ("healthy", "burst", "providers", "news")
                                                  for s in args.scenarios.split(","))):
        parser.error("Invalid viewer count, timing, or scenario")
    if args.cold_start and any(s not in ("healthy", "burst") for s in args.scenarios.split(",")):
        parser.error("Contention scenarios require seeded cached quotes")
    if platform.system() != "Linux":
        parser.error("Linux is required for interface selection and /proc resource samples")
    binary = Path(args.fixture_binary).resolve() if args.fixture_binary else build_fixture()
    output_path = Path(args.output)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with output_path.open("x") as output:
        asyncio.run(main(args, binary, output))
    print(f"Report: {output_path}")
