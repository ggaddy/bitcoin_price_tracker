"""Bounded regression checks for load measurement, independent of real providers."""
import asyncio
import unittest

from load_test import cadence, exchange, percentiles


def event(provider, at, mode="healthy"):
    return {"provider": provider, "at": at, "mode": mode}


class CadenceTests(unittest.TestCase):
    def test_initial_parallel_refresh_and_rotating_providers(self):
        events = [event(name, 0) for name in ("coingecko", "coinbase", "kraken", "gemini")]
        events += [event("coingecko", 10), event("coinbase", 20), event("news", 1)]
        result = cadence(events, 0)
        self.assertEqual(result["violations"], [])
        self.assertEqual(result["refresh_groups"], 3)
        self.assertEqual(result["minimum_refresh_gap_seconds"], 10)

    def test_global_gate_cannot_be_hidden_by_switching_providers(self):
        result = cadence([event("coingecko", 0), event("coinbase", 5)], 0)
        self.assertIn("global ten-second gate", result["violations"])

    def test_timeout_backoff_retry_after_and_exponential_backoff(self):
        for name, valid, invalid in (
            ("coingecko", [0, 15, 40], [0, 15, 35]),
            ("coinbase", [0, 30, 60], [0, 30, 50]),
            ("gemini", [0, 10, 30, 70], [0, 10, 30, 60]),
        ):
            with self.subTest(provider=name):
                self.assertEqual(cadence([event(name, at, "providers") for at in valid], 0)["violations"], [])
                self.assertTrue(cadence([event(name, at, "providers") for at in invalid], 0)["violations"])

    def test_news_cannot_refetch_during_failure_backoff(self):
        self.assertTrue(cadence([event("news", 0, "news"), event("news", 15, "news")], 0)["violations"])

    def test_percentiles_include_the_tail_and_empty_is_explicit(self):
        self.assertEqual(percentiles(list(range(1, 101))), {"p50": 50, "p95": 95, "p99": 99})
        self.assertEqual(percentiles([]), {"p50": None, "p95": None, "p99": None})


class HttpTests(unittest.IsolatedAsyncioTestCase):
    async def fixture(self, response, stall=False):
        self.tasks = set()

        async def handle(reader, writer):
            task = asyncio.current_task()
            self.tasks.add(task)
            try:
                await reader.readuntil(b"\r\n\r\n")
                writer.write(response)
                await writer.drain()
                if stall:
                    await reader.read()
            finally:
                writer.close()
                await writer.wait_closed()
                self.tasks.discard(task)

        self.server = await asyncio.start_server(handle, "127.0.0.1", 0)
        return self.server.sockets[0].getsockname()[1]

    async def asyncTearDown(self):
        if hasattr(self, "server"):
            self.server.close()
            await self.server.wait_closed()
            for task in list(self.tasks):
                task.cancel()
            await asyncio.gather(*self.tasks, return_exceptions=True)

    async def test_reads_chunked_error_body_and_retry_hint(self):
        port = await self.fixture(b'HTTP/1.1 503 Unavailable\r\nTransfer-Encoding: chunked\r\n'
                                  b'Retry-After: 2\r\n\r\n2\r\n{}\r\n0\r\n\r\n')
        status, headers, data = await exchange(port, "/api/news")
        self.assertEqual((status, headers["retry-after"], data), (503, "2", {}))

    async def test_deadline_includes_body_and_releases_connection(self):
        port = await self.fixture(b'HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{', stall=True)
        with self.assertRaises(asyncio.TimeoutError):
            await exchange(port, "/api/price", timeout=0.05)


if __name__ == "__main__":
    unittest.main()
