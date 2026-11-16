import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { chromium } from 'playwright';

let browser, server, base;
const now = 1700000000;
const providers = ['CoinGecko', 'Coinbase', 'Kraken', 'Gemini'];
function report(overrides = {}) {
  return {
    status: 'LIVE', stale: false, source_max_age_seconds: 90, evaluated_at_unix: now,
    fetched_at_unix: now, fetched_age_seconds: 0, active_viewers: 1, warnings: [],
    average_price: 100150, spread: 300, refresh_succeeded: true, refreshing: false,
    coverage: { configured_source_count: 4, fresh_source_count: 4, contributing_sources: [...providers] },
    sources: providers.map((source, index) => ({ source, price_usd: 100000 + index * 100,
      last_success_at_unix: now, quote_kind: ['aggregate', 'spot', 'last_trade', 'bid'][index], age_seconds: 0, freshness: 'fresh' })),
    provider_health: providers.map(source => ({ source, last_attempt: { outcome: 'success', attempted_at_unix: now } })),
    ...overrides,
  };
}
const unavailable = () => report({ status: 'UNAVAILABLE', stale: true, sources: [], average_price: null, spread: null,
  coverage: { configured_source_count: 4, fresh_source_count: 0, contributing_sources: [] } });
const json = (route, data, status = 200, headers = {}) => route.fulfill({ status, headers, contentType: 'application/json', body: JSON.stringify(data) });

before(async () => {
  browser = await chromium.launch({ headless: true });
  if (process.env.BTC_APP_URL) { base = process.env.BTC_APP_URL.replace(/\/$/, ''); return; }
  const files = new Map(await Promise.all([
    ['/', '../../src/ui.html', 'text/html'],
    ['/assets/dashboard.js', '../../src/dashboard.js', 'text/javascript'],
    ['/assets/dashboard.css', '../../src/dashboard.css', 'text/css'],
    ['/assets/cybercore-0.3.0.min.css', '../../src/vendor/cybercore-0.3.0.min.css', 'text/css'],
  ].map(async ([path, file, type]) => [path, { body: await readFile(new URL(file, import.meta.url)), type }])));
  const ui = await readFile(new URL('../../src/ui.rs', import.meta.url), 'utf8');
  const policy = ui.match(/"(default-src [^"]+)"/)[1];
  server = createServer((req, res) => {
    const file = files.get(new URL(req.url, "http://localhost").pathname);
    res.writeHead(file ? 200 : 404, { 'Content-Type': file?.type ?? 'text/plain', 'Content-Security-Policy': policy });
    res.end(file?.body ?? 'Not found');
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  base = `http://127.0.0.1:${server.address().port}`;
});
after(async () => {
  await browser?.close();
  if (server) await new Promise(resolve => server.close(resolve));
});

async function setup(t, options = {}) {
  const origin = options.origin ?? base;
  const context = await browser.newContext({ viewport: options.viewport ?? { width: 1200, height: 900 }, reducedMotion: options.reducedMotion ?? 'no-preference' });
  context.setDefaultTimeout(4000);
  const errors = [], presence = [], pending = new Set();
  let prices = 0, maxPrices = 0, maxPresence = 0, maxNews = 0, maxInactive = 0;
  context.on('page', page => {
    page.on('pageerror', error => errors.push(error.message));
    page.on('request', req => {
      if (['/api/price', '/api/presence', '/api/news'].some(path => req.url().endsWith(path))) {
        pending.add(req);
        maxPrices = Math.max(maxPrices, [...pending].filter(req => req.url().endsWith('/api/price')).length);
        maxPresence = Math.max(maxPresence, [...pending].filter(req => req.url().endsWith('/api/presence') && req.postDataJSON()?.active).length);
        maxInactive = Math.max(maxInactive, [...pending].filter(req => req.url().endsWith('/api/presence') && !req.postDataJSON()?.active).length);
        maxNews = Math.max(maxNews, [...pending].filter(req => req.url().endsWith('/api/news')).length);
      }
    });
    for (const event of ['requestfinished', 'requestfailed']) page.on(event, req => pending.delete(req));
  });
  await context.addInitScript(() => {
    window.testHidden = false;
    Object.defineProperty(document, 'hidden', { get: () => window.testHidden });
    Object.defineProperty(document, 'visibilityState', { get: () => window.testHidden ? 'hidden' : 'visible' });
    window.changeVisibility = hidden => { window.testHidden = hidden; document.dispatchEvent(new Event('visibilitychange')); };
    window.unhandled = [];
    window.cspViolations = [];
    window.addEventListener('securitypolicyviolation', event => window.cspViolations.push(event.violatedDirective));
    window.addEventListener('unhandledrejection', event => window.unhandled.push(String(event.reason)));
  });
  if (options.init) await context.addInitScript(options.init, options.initArg);
  await context.route('**/*', async route => {
    const url = route.request().url();
    if (!url.startsWith(origin + '/')) { errors.push(`Unexpected external request: ${url}`); return route.abort(); }
    if (url.endsWith('/api/presence')) {
      const body = route.request().postDataJSON();
      presence.push(body);
      if (options.presence) return options.presence(route, body);
      return json(route, { active_viewers: body.active ? 1 : 0 });
    }
    if (url.endsWith('/api/news')) return options.news ? options.news(route) : json(route, { xml: null, stale: true }, 503);
    if (url.endsWith('/api/price')) {
      prices++;
      return options.price ? options.price(route, prices) : json(route, report());
    }
    return route.continue();
  });
  const page = await context.newPage();
  await page.clock.install();
  t.after(async () => {
    try {
      assert.deepEqual(errors, []);
      for (const tab of context.pages()) assert.deepEqual(await tab.evaluate(() => window.unhandled ?? []), []);
      for (const tab of context.pages()) assert.deepEqual(await tab.evaluate(() => window.cspViolations ?? []), []);
    } finally { await context.close(); }
  });
  await page.goto(origin);
  return { page, context, presence, prices: () => prices, maxPrices: () => maxPrices, maxPresence: () => maxPresence,
    maxNews: () => maxNews, maxInactive: () => maxInactive };
}
async function text(page, id, expected) {
  try {
    await page.waitForFunction(({ id, expected }) => document.getElementById(id).textContent.includes(expected), { id, expected });
  } catch (error) {
    throw new Error(`${id}: expected ${expected}, got ${await page.locator('#' + id).textContent()}`, { cause: error });
  }
}
async function tick(page, ms) { await page.clock.runFor(ms); }

// Real HTTP headers with an unfinished body exercise transport ownership;
// route.fulfill() would finish the body and hide the leak under test.
async function streamingFixture(t, path, status) {
  const files = new Map(await Promise.all([
    ['/', '../../src/ui.html', 'text/html'],
    ['/assets/dashboard.js', '../../src/dashboard.js', 'text/javascript'],
    ['/assets/dashboard.css', '../../src/dashboard.css', 'text/css'],
    ['/assets/cybercore-0.3.0.min.css', '../../src/vendor/cybercore-0.3.0.min.css', 'text/css'],
  ].map(async ([path, file, type]) => [path, { body: await readFile(new URL(file, import.meta.url)), type }])));
  const ui = await readFile(new URL('../../src/ui.rs', import.meta.url), 'utf8');
  const policy = ui.match(/"(default-src [^"]+)"/)[1];
  let active = 0, close;
  const closed = new Promise(resolve => { close = resolve; });
  const fixture = createServer((req, res) => {
    const pathname = new URL(req.url, 'http://localhost').pathname;
    if (pathname === path) {
      active++;
      res.on('close', () => { active--; close(); });
      res.writeHead(status, { 'Content-Type': 'application/json', 'Retry-After': '1' });
      res.write('{"held":');
      return;
    }
    const file = files.get(pathname);
    res.writeHead(file ? 200 : 404, { 'Content-Type': file?.type ?? 'text/plain', 'Content-Security-Policy': policy });
    res.end(file?.body ?? 'Not found');
  });
  await new Promise(resolve => fixture.listen(0, '127.0.0.1', resolve));
  t.after(async () => {
    fixture.closeAllConnections();
    await new Promise(resolve => fixture.close(resolve));
  });
  return { origin: `http://127.0.0.1:${fixture.address().port}`, closed, active: () => active };
}

for (const kind of ['price', 'presence', 'news']) {
  test(`rejected ${kind} response cancels its stalled body before retry`, { timeout: 10000 }, async t => {
    const fixture = await streamingFixture(t, `/api/${kind}`, kind === 'price' ? 500 : 503);
    let attempts = 0;
    const options = { origin: fixture.origin };
    if (kind === 'price') options.price = route => ++attempts === 1 ? route.continue() : json(route, report());
    if (kind === 'presence') options.presence = (route, body) => body.active && ++attempts === 1 ? route.continue() : json(route, {});
    if (kind === 'news') options.news = route => ++attempts === 1 ? route.continue() : json(route, { xml: newsFeed('Recovered headline'), stale: false });
    const app = await setup(t, options);
    await text(app.page, kind === 'news' ? 'feed-status' : 'status', kind === 'news' ? 'retrying shortly' : kind === 'price' ? 'OFFLINE' : 'DEGRADED');
    await tick(app.page, kind === 'news' ? 1100 : 5100);
    await text(app.page, kind === 'news' ? 'feed-list' : 'status', kind === 'news' ? 'Recovered headline' : 'LIVE');
    const maximum = kind === 'price' ? app.maxPrices() : kind === 'presence' ? app.maxPresence() : app.maxNews();
    assert.equal(maximum, 1, 'retry must not overlap an abandoned response body');
    await fixture.closed;
    assert.equal(fixture.active(), 0, 'native server transfer was cancelled');
  });
}

test('inactive presence cancels an unread successful response body', { timeout: 10000 }, async t => {
  const fixture = await streamingFixture(t, '/api/presence', 200);
  const app = await setup(t, { origin: fixture.origin,
    init: () => { navigator.sendBeacon = () => false; },
    presence: (route, body) => body.active ? json(route, {}) : route.continue(),
  });
  await text(app.page, 'status', 'LIVE');
  const response = app.page.waitForResponse(res => res.url().endsWith('/api/presence') && !res.request().postDataJSON()?.active);
  await app.page.evaluate(() => window.changeVisibility(true));
  await response;
  await tick(app.page, 3100);
  assert.equal(fixture.active(), 0, 'completion must close the unread keepalive body');
});

for (const beacon of ['rejected', 'missing', 'throwing']) {
  test(`inactive presence stays single flight with ${beacon} beacon and recovers after timeout`, { timeout: 10000 }, async t => {
    const held = [];
    const app = await setup(t, {
      init: mode => {
        navigator.sendBeacon = mode === 'missing' ? undefined : () => {
          if (mode === 'throwing') throw new Error('Beacon refused');
          return false;
        };
      }, initArg: beacon,
      presence: (route, body) => body.active ? json(route, {}) : held.push(route),
    });
    await text(app.page, 'status', 'LIVE');
    await app.page.evaluate(() => {
      for (let i = 0; i < 16; i++) { window.changeVisibility(true); window.changeVisibility(false); }
      window.changeVisibility(true);
    });
    await tick(app.page, 100);
    assert.equal(app.maxInactive(), 1, 'all stops must share one unsettled inactive fallback');
    assert.equal(held.length, 1);
    await tick(app.page, 3100);
    await app.page.evaluate(() => { window.changeVisibility(false); window.changeVisibility(true); });
    await tick(app.page, 100);
    assert.equal(held.length, 2, 'timeout releases ownership for a later stop');
    for (const route of held) await json(route, {}).catch(() => {});
  });
}

test('registration renders its cached price without an initial price GET and keeps later heartbeats small', async t => {
  const app = await setup(t, { presence: (route, body) => json(route, {
    active_viewers: body.active ? 1 : 0, ...(body.include_price ? { price: report() } : {}),
  }) });
  await text(app.page, 'status', 'LIVE');
  await text(app.page, 'sources', 'Coinbase');
  assert.equal(app.prices(), 0);
  assert.equal(app.presence[0].include_price, true);
  // Clock advancement fires fetch; wait for Chromium to deliver the routed events.
  const price = app.page.waitForResponse(response => response.url().endsWith('/api/price'));
  const heartbeat = app.page.waitForResponse(response => response.url().endsWith('/api/presence')
    && response.request().postDataJSON()?.active && response.request().postDataJSON()?.include_price === undefined);
  await tick(app.page, 5100);
  await Promise.all([price, heartbeat]);
  assert.equal(app.prices(), 1);
  assert.ok(app.presence.some(body => body.active && body.include_price === undefined));
  assert.equal(app.maxPrices(), 1);
  assert.equal(app.maxPresence(), 1);
});

test('cold startup registration retries prices promptly while keeping viewer admission successful', async t => {
  const app = await setup(t, { presence: (route, body) => json(route, {
    active_viewers: 1, ...(body.include_price ? { price: unavailableWithRefresh() } : {}),
  }) });
  await text(app.page, 'status', 'UNAVAILABLE');
  assert.equal(app.prices(), 0);
  await tick(app.page, 1100);
  await text(app.page, 'status', 'LIVE');
  assert.equal(app.prices(), 1);
  assert.equal(await app.page.locator('#warnings').textContent(), 'None');
});

function unavailableWithRefresh() { return { ...unavailable(), refreshing: true }; }

test('invalid startup price falls back safely to the ordinary price endpoint', async t => {
  const app = await setup(t, { presence: (route, body) => json(route, {
    active_viewers: 1, ...(body.include_price ? { price: { sources: [] } } : {}),
  }) });
  await text(app.page, 'status', 'LIVE');
  await text(app.page, 'sources', 'CoinGecko');
  assert.equal(app.prices(), 1);
  assert.equal(await app.page.locator('#warnings').textContent(), 'None');
});

test('startup response body completed after hide cannot render quotes or start a price loop', async t => {
  const app = await setup(t, {
    presence: (route, body) => json(route, { active_viewers: 1, ...(body.include_price ? { price: report() } : {}) }),
    init: () => {
      const originalFetch = window.fetch;
      window.fetch = async (...args) => {
        const response = await originalFetch(...args);
        if (args[0] === '/api/presence' && JSON.parse(args[1]?.body ?? '{}').include_price) {
          const body = await response.text();
          response.text = () => new Promise(resolve => { window.releaseStartupBody = () => resolve(body); });
        }
        return response;
      };
    },
  });
  await app.page.waitForFunction(() => typeof window.releaseStartupBody === 'function');
  await app.page.evaluate(() => { window.changeVisibility(true); window.releaseStartupBody(); });
  await tick(app.page, 1000);
  assert.equal(await app.page.locator('#status').textContent(), 'IDLE');
  assert.equal(await app.page.locator('#avg').textContent(), 'Loading...');
  assert.equal(app.prices(), 0);
});

test('first load renders source semantics, coverage, and safe variable text', async t => {
  const data = report();
  data.sources[0].source = '<img src=x onerror=alert(1)>';
  data.coverage.contributing_sources[0] = data.sources[0].source;
  data.provider_health[0].source = data.sources[0].source;
  data.provider_health[0].last_attempt = { outcome: 'failure', error: { message: '<script>bad()</script>' } };
  data.status = 'DEGRADED';
  const { page } = await setup(t, { price: route => json(route, data) });
  await text(page, 'status', 'DEGRADED');
  await text(page, 'coverage', '4 of 4');
  await text(page, 'sources', 'last trade');
  await text(page, 'sources', '<script>bad()</script>');
  assert.equal(await page.locator('#sources img, #sources script').count(), 0);
  assert.match(await page.locator('#avg').textContent(), /100\u2009150\.00/);
  assert.equal(await page.locator('[role=status]').count(), 2);
  assert.equal(await page.locator('#warnings').getAttribute('aria-live'), null);
  if (process.env.BTC_SCREENSHOT_DIR) await page.screenshot({ path: `${process.env.BTC_SCREENSHOT_DIR}/desktop.png`, fullPage: true });
});

test('freshness expires between responses without upgrading unknown or future quotes', async t => {
  const data = report();
  data.sources[0].age_seconds = 89;
  data.sources[1].age_seconds = 89;
  data.sources[2].freshness = 'unknown'; data.sources[2].age_seconds = null;
  data.sources[3].freshness = 'future'; data.sources[3].age_seconds = null;
  data.status = 'DEGRADED'; data.coverage.contributing_sources = providers.slice(0, 2);
  const { page } = await setup(t, { price: route => json(route, data) });
  await text(page, 'coverage', '2 of 4');
  await tick(page, 1100);
  await text(page, 'status', 'STALE');
  await text(page, 'coverage', '0 of 4');
  await text(page, 'avg', 'Unavailable');
  assert.deepEqual(await page.locator('#sources .source-detail').allTextContents(), ['90s', '90s', 'Age unknown', 'Age unknown']);
});

for (const kind of ['startup', 'price', 'retry']) {
  test(`${kind} response transit cannot make expired quotes fresh on arrival`, async t => {
    let entered;
    const started = new Promise(resolve => { entered = resolve; });
    const data = report({ warnings: ['Delayed evaluated report'] });
    data.sources.forEach(source => { source.age_seconds = 89; source.last_success_at_unix = now - 89; });
    const initial = report();
    initial.sources.forEach(source => { source.age_seconds = 84; source.last_success_at_unix = now - 84; });
    if (kind === 'retry') {
      data.evaluated_at_unix = now + 5;
      data.sources.forEach(source => { source.last_success_at_unix = now - 84; });
    }
    const app = await setup(t, {
      presence: (route, body) => {
        if (kind === 'startup' && body.include_price) { entered(route); return; }
        return json(route, { active_viewers: 1 });
      },
      price: (route, count) => {
        if ((kind === 'price' && count === 1) || (kind === 'retry' && count === 2)) { entered(route); return; }
        return json(route, kind === 'retry' && count === 1 ? initial : report());
      },
    });
    if (kind === 'retry') {
      await text(app.page, 'status', 'LIVE');
      await tick(app.page, 5100);
    }
    const held = await started;
    // Stay within both real request deadlines; the report was evaluated before
    // this transfer delay. No mocked fetch or suppressed abort is needed.
    await tick(app.page, 2100);
    if (kind === 'retry') await text(app.page, 'status', 'STALE');
    await json(held, kind === 'startup' ? { active_viewers: 1, price: data } : data);
    // Especially on retry, prove this response was accepted before inspecting
    // freshness; the retained report may already be stale while it is held.
    await text(app.page, 'warnings', 'Delayed evaluated report');
    await text(app.page, 'sources', 'Coinbase');
    assert.equal(await app.page.locator('#status').textContent(), 'STALE');
    assert.equal(await app.page.locator('#avg').textContent(), 'Unavailable');
    await text(app.page, 'coverage', '0 of 4');
    await tick(app.page, 5100);
    await text(app.page, 'status', 'LIVE');
    assert.equal(app.maxPrices(), 1);
    assert.equal(app.maxPresence(), 1);
  });
}

test('LIVE expires locally as coverage drops', async t => {
  const data = report(); data.sources[0].age_seconds = 89;
  const { page } = await setup(t, { price: route => json(route, data) });
  await text(page, 'status', 'LIVE');
  await tick(page, 1100);
  await text(page, 'status', 'DEGRADED');
  await text(page, 'coverage', '3 of 4');
});

test('503 on first load recovers automatically', async t => {
  const { page } = await setup(t, { price: (route, count) => json(route, count === 1 ? unavailable() : report(), count === 1 ? 503 : 200) });
  await text(page, 'status', 'UNAVAILABLE');
  await tick(page, 5100);
  await text(page, 'status', 'LIVE');
});

for (const failure of ['network', 'non-json', '500', '503']) {
  test(`${failure} retains quotes, reports failure, and recovers`, async t => {
    const { page } = await setup(t, { price: (route, count) => {
      if (count !== 2) return json(route, report());
      if (failure === 'network') return route.abort();
      if (failure === 'non-json') return route.fulfill({ status: 200, body: '<html>proxy error</html>' });
      return json(route, unavailable(), Number(failure));
    } });
    await text(page, 'status', 'LIVE');
    const average = await page.locator('#avg').textContent();
    await tick(page, 5100);
    await text(page, 'status', 'OFFLINE');
    assert.equal(await page.locator('#avg').textContent(), average);
    await text(page, 'sources', 'CoinGecko');
    await text(page, 'note', 'Retaining');
    await tick(page, 5100);
    await text(page, 'status', 'LIVE');
  });
}

test('failed presence does not prevent prices and recovers without unhandled rejection', async t => {
  let fail = true;
  const { page } = await setup(t, { presence: (route, body) => json(route, {}, fail && body.active ? 503 : 200) });
  await text(page, 'status', 'DEGRADED');
  await text(page, 'sources', 'Coinbase');
  await text(page, 'warnings', 'heartbeat');
  fail = false;
  await tick(page, 5100);
  await text(page, 'status', 'LIVE');
});

test('slow prices time out with one request while heartbeats continue', async t => {
  const held = [];
  const app = await setup(t, { price: (route, count) => count === 1 ? held.push(route) : json(route, report()) });
  await app.page.waitForFunction(() => document.getElementById('status').textContent === 'Connecting');
  await tick(app.page, 8500);
  await text(app.page, 'status', 'OFFLINE');
  assert.ok(app.presence.filter(body => body.active).length >= 2);
  assert.equal(app.prices(), 1);
  await tick(app.page, 5100);
  await text(app.page, 'status', 'LIVE');
  assert.equal(app.maxPrices(), 1);
  assert.equal(app.maxPresence(), 1);
  for (const route of held) await json(route, report()).catch(() => {});
});

test('presence deadline permits price loading and heartbeat recovery', async t => {
  let first = true;
  const held = [];
  const { page } = await setup(t, { presence: (route, body) => {
    if (first && body.active) { first = false; held.push(route); return; }
    return json(route, {});
  } });
  await tick(page, 3200);
  await text(page, 'status', 'DEGRADED');
  await text(page, 'sources', 'CoinGecko');
  await tick(page, 5100);
  await text(page, 'status', 'LIVE');
  for (const route of held) await json(route, {}).catch(() => {});
});

test('hide aborts delayed prices, late responses cannot replace IDLE, show resumes once', async t => {
  const held = [];
  const app = await setup(t, { init: () => { navigator.sendBeacon = () => false; },
    price: (route, count) => count === 1 ? held.push(route) : json(route, report()) });
  await app.page.waitForFunction(() => document.getElementById('status').textContent === 'Connecting');
  // Wait for the first price request without relying on live provider timing.
  for (let attempt = 0; !held.length && attempt < 200; attempt++) await new Promise(resolve => setTimeout(resolve, 10));
  assert.equal(held.length, 1, 'first price request started');
  await app.page.evaluate(() => window.changeVisibility(true));
  await text(app.page, 'status', 'IDLE');
  await json(held[0], report()).catch(() => {});
  await tick(app.page, 20000);
  assert.equal(app.prices(), 1);
  assert.equal(await app.page.locator('#status').textContent(), 'IDLE');
  assert.ok(app.presence.some(body => !body.active));
  await app.page.evaluate(() => { window.changeVisibility(false); window.changeVisibility(false); });
  await text(app.page, 'status', 'LIVE');
  const price = app.page.waitForResponse(response => response.url().endsWith('/api/price'));
  await tick(app.page, 5100);
  await price;
  assert.equal(app.prices(), 3);
  assert.equal(app.maxPrices(), 1);
  assert.equal(app.maxPresence(), 1);
});

test('rapid hide/show and pagehide/pageshow preserve single request ownership', async t => {
  const app = await setup(t);
  await text(app.page, 'status', 'LIVE');
  await app.page.evaluate(() => {
    for (let i = 0; i < 4; i++) { window.changeVisibility(true); window.changeVisibility(false); }
    window.dispatchEvent(new Event('pagehide'));
    window.dispatchEvent(new Event('pageshow'));
  });
  await text(app.page, 'status', 'LIVE');
  await tick(app.page, 5500);
  assert.equal(app.maxPrices(), 1);
  assert.equal(app.maxPresence(), 1);
});

test('a delayed startup price cannot overwrite a newer resumed price response', async t => {
  let registrations = 0, priceStarted, startupStarted;
  const firstPrice = new Promise(resolve => { priceStarted = resolve; });
  const startup = new Promise(resolve => { startupStarted = resolve; });
  const newer = report();
  newer.sources.forEach(source => { source.price_usd = 200000; });
  const app = await setup(t, {
    presence: (route, body) => {
      if (body.include_price && ++registrations === 2) { startupStarted(route); return; }
      return json(route, { active_viewers: 1 });
    },
    price: (route, count) => {
      if (count === 1) { priceStarted(route); return; }
      return json(route, newer);
    },
  });
  const cancelledPrice = await firstPrice;
  await app.page.evaluate(() => { window.changeVisibility(true); window.changeVisibility(false); });
  const heldStartup = await startup;
  await tick(app.page, 100);
  await text(app.page, 'avg', '200\u2009000.00');
  const response = app.page.waitForResponse(res => res.url().endsWith('/api/presence') && res.request().postDataJSON()?.include_price);
  await json(heldStartup, { active_viewers: 1, price: report() });
  await response;
  await tick(app.page, 100);
  assert.match(await app.page.locator('#avg').textContent(), /200\u2009000\.00/);
  assert.equal(app.maxPrices(), 1);
  assert.equal(app.maxPresence(), 1);
  await json(cancelledPrice, report()).catch(() => {});
});

test('denied storage and missing UUID still produce distinct identities in opener tabs', async t => {
  const app = await setup(t, { init: () => {
    Object.defineProperty(window, 'sessionStorage', { get() { throw new Error('Storage denied'); } });
    Object.defineProperty(crypto, 'randomUUID', { value: undefined });
  } });
  await text(app.page, 'status', 'LIVE');
  const popupPromise = app.context.waitForEvent('page');
  await app.page.evaluate(() => window.open('/', '_blank'));
  const popup = await popupPromise;
  await popup.waitForLoadState();
  await text(popup, 'status', 'LIVE');
  const identities = new Set(app.presence.filter(body => body.active).map(body => body.session_id));
  assert.equal(identities.size, 2);
  for (const id of identities) assert.match(id, /^[a-f0-9]{32}$/);
});

test('copied sessionStorage cannot duplicate presence identity', async t => {
  const app = await setup(t, { init: () => { if (location.protocol === 'http:') sessionStorage.setItem('btc-matrix-session-id', 'copied-tab-id'); } });
  await text(app.page, 'status', 'LIVE');
  const other = await app.context.newPage();
  await other.goto(base);
  await text(other, 'status', 'LIVE');
  const ids = app.presence.filter(body => body.active).map(body => body.session_id);
  assert.equal(new Set(ids).size, 2);
  assert.ok(!ids.includes('copied-tab-id'));
});

test('missing secure randomness produces explicit unsupported state without requests', async t => {
  const app = await setup(t, { init: () => {
    Object.defineProperty(crypto, 'randomUUID', { value: undefined });
    Object.defineProperty(crypto, 'getRandomValues', { value: undefined });
  } });
  await text(app.page, 'status', 'UNSUPPORTED');
  assert.equal(app.prices(), 0);
  assert.equal(app.presence.length, 0);
});

test('reduced motion responds to preference changes and hidden pages pause effects', async t => {
  const { page } = await setup(t, { reducedMotion: 'reduce' });
  await text(page, 'status', 'LIVE');
  const animation = () => page.locator('h1').evaluate(el => getComputedStyle(el, '::before').animationName);
  assert.equal(await animation(), 'none');
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  await page.evaluate(() => window.changeVisibility(true));
  assert.equal(await page.locator('h1').evaluate(el => getComputedStyle(el, '::before').animationPlayState), 'paused');
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.evaluate(() => window.changeVisibility(false));
  assert.equal(await animation(), 'none');
});

test('narrow viewport and long provider errors do not overflow', async t => {
  const data = report();
  data.provider_health[0].last_attempt = { outcome: 'failure', error: { message: 'LongError'.repeat(80) } };
  data.sources[0].source = 'LongProvider'.repeat(30);
  const { page } = await setup(t, { viewport: { width: 320, height: 800 }, price: route => json(route, data) });
  await text(page, 'sources', 'LongError');
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
  if (process.env.BTC_SCREENSHOT_DIR) await page.screenshot({ path: `${process.env.BTC_SCREENSHOT_DIR}/narrow.png`, fullPage: true });
});

for (const kind of ['non-json', 'invalid-contract']) {
  test(`initial ${kind} response leaves Connecting and retries`, async t => {
    const { page } = await setup(t, { price: (route, count) => count > 1 ? json(route, report())
      : kind === 'non-json' ? route.fulfill({ status: 200, body: '<html>Unavailable</html>' }) : json(route, { sources: [] }) });
    await text(page, 'status', 'OFFLINE');
    await text(page, 'avg', 'Unavailable');
    await tick(page, 5100);
    await text(page, 'status', 'LIVE');
  });
}

test('initially hidden page makes no requests until visible', async t => {
  const app = await setup(t, { init: () => { window.testHidden = true; } });
  await text(app.page, 'status', 'IDLE');
  await tick(app.page, 20000);
  assert.equal(app.prices(), 0);
  assert.equal(app.presence.length, 0);
  await app.page.evaluate(() => window.changeVisibility(false));
  await text(app.page, 'status', 'LIVE');
});

test('a body completed after hide cannot replace IDLE', async t => {
  const app = await setup(t, { init: () => {
    const originalFetch = window.fetch;
    window.fetch = async (...args) => {
      const response = await originalFetch(...args);
      if (args[0] === '/api/price') {
        const data = await response.json();
        response.json = () => new Promise(resolve => { window.releasePriceBody = () => resolve(data); });
      }
      return response;
    };
  } });
  await app.page.waitForFunction(() => typeof window.releasePriceBody === 'function');
  await app.page.evaluate(() => { window.changeVisibility(true); window.releasePriceBody(); });
  await tick(app.page, 1000);
  assert.equal(await app.page.locator('#status').textContent(), 'IDLE');
  assert.equal(await app.page.locator('#avg').textContent(), 'Loading...');
});

test('network heartbeat failures remain recoverable', async t => {
  let fail = true;
  const { page } = await setup(t, { presence: route => fail ? route.abort() : json(route, {}) });
  await text(page, 'status', 'DEGRADED');
  await text(page, 'sources', 'CoinGecko');
  fail = false;
  await tick(page, 5100);
  await text(page, 'status', 'LIVE');
});


test('Bitcoin.com RSS renders safe links inside Feed activity and respects pause controls', async t => {
  const xml = `<rss><channel>
    <item><title><![CDATA[Bitcoin <img src=x onerror=alert(1)>]]></title><link>https://news.bitcoin.com/example/</link><pubDate>Mon, 21 Sep 2026 12:00:00 GMT</pubDate></item>
    <item><title>Unsafe link</title><link>javascript:alert(1)</link></item>
  </channel></rss>`;
  const { page } = await setup(t, { reducedMotion: 'reduce', news: route => json(route, { xml, stale: false }) });
  await text(page, 'feed-list', 'Bitcoin <img');
  assert.equal(await page.locator('.session #feed-list a').count(), 1);
  assert.equal(await page.locator('#feed-list img').count(), 0);
  assert.equal(await page.locator('#feed-list a').getAttribute('href'), 'https://news.bitcoin.com/example/');
  await text(page, 'feed-toggle', 'Resume scrolling');
  await page.locator('#feed-toggle').click();
  await text(page, 'feed-toggle', 'Pause scrolling');
  await page.locator('#feed-toggle').click();
  assert.equal(await page.locator('#feed-toggle').getAttribute('aria-pressed'), 'true');
});

test('RSS failure leaves price dashboard working', async t => {
  const { page } = await setup(t);
  await text(page, 'feed-status', 'temporarily unavailable');
  await text(page, 'status', 'LIVE');
});

// Holding headers or an already-read body deliberately ignores cancellation so
// the tests can exercise completions from a previous visibility period.
function holdFirstNewsResponse(stage) {
  const originalFetch = window.fetch;
  window.newsSignals = [];
  window.fetch = async (...args) => {
    if (args[0] !== '/api/news') return originalFetch(...args);
    const count = window.newsSignals.push(args[1].signal);
    const response = await originalFetch(...args);
    if (count === 1 && stage === 'headers') {
      await new Promise((resolve, reject) => {
        window.releaseNews = fail => fail ? reject(new Error('Late RSS failure')) : resolve();
      });
    } else if (count === 1 && stage === 'body') {
      const body = await response.json();
      response.json = () => new Promise((resolve, reject) => {
        window.releaseNews = fail => fail ? reject(new Error('Late RSS failure')) : resolve(body);
      });
    }
    return response;
  };
}

const newsFeed = title => `<rss><channel><item><title>${title}</title><link>https://news.bitcoin.com/story/</link></item></channel></rss>`;

for (const transition of ['hide', 'pagehide']) {
  test(`RSS ${transition} aborts held headers and resumes polling once`, async t => {
    let requests = 0, firstRoute;
    const started = new Promise(resolve => { firstRoute = resolve; });
    const app = await setup(t, { init: holdFirstNewsResponse, news: route => {
      if (++requests === 1) { firstRoute(route); return; }
      return json(route, { xml: newsFeed('Current headline'), stale: false });
    } });
    const held = await started;
    const status = await app.page.locator('#feed-status').textContent();
    const failed = app.page.waitForEvent('requestfailed', req => req.url().endsWith('/api/news'));
    await app.page.evaluate(transition => {
      if (transition === 'hide') window.changeVisibility(true);
      else window.dispatchEvent(new Event('pagehide'));
    }, transition);
    assert.equal(await app.page.evaluate(() => window.newsSignals[0].aborted), true);
    await failed;
    await json(held, { xml: newsFeed('Old headline'), stale: false }).catch(() => {});
    await tick(app.page, 20000);
    assert.equal(requests, 1, 'no news requests while stopped');
    assert.equal(await app.page.locator('#feed-status').textContent(), status);
    assert.doesNotMatch(await app.page.locator('#feed-list').textContent(), /Old headline/);
    await app.page.evaluate(transition => {
      for (let i = 0; i < 2; i++) {
        if (transition === 'hide') window.changeVisibility(false);
        else window.dispatchEvent(new Event('pageshow'));
      }
    }, transition);
    await text(app.page, 'feed-list', 'Current headline');
    await text(app.page, 'status', 'LIVE');
    await tick(app.page, 5100);
    assert.equal(requests, 2);
    assert.equal(app.maxPrices(), 1);
    assert.equal(app.maxPresence(), 1);
  });
}

for (const stage of ['headers', 'body']) {
  for (const resumeBeforeRelease of [false, true]) {
    test(`RSS late ${stage} cannot render or reschedule after ${resumeBeforeRelease ? 'rapid hide/show' : 'hide'}`, async t => {
      let requests = 0;
      const { page } = await setup(t, { init: holdFirstNewsResponse, initArg: stage,
        news: route => json(route, ++requests === 1
          ? { xml: newsFeed('Old headline'), stale: true, refreshing: true }
          : { xml: newsFeed('Current headline'), stale: false }) });
      await page.waitForFunction(() => typeof window.releaseNews === 'function');
      const status = await page.locator('#feed-status').textContent();
      await page.evaluate(resume => {
        window.changeVisibility(true);
        if (resume) {
          for (let i = 0; i < 4; i++) { window.changeVisibility(false); window.changeVisibility(true); }
          window.changeVisibility(false);
          window.dispatchEvent(new Event('pagehide'));
          window.dispatchEvent(new Event('pageshow'));
        }
      }, resumeBeforeRelease);
      assert.equal(await page.evaluate(() => window.newsSignals[0].aborted), true);
      await tick(page, 100);
      assert.equal(requests, 1, 'keep ownership until the previous request settles');
      await page.evaluate(() => window.releaseNews());
      await tick(page, 100);
      if (!resumeBeforeRelease) {
        assert.equal(await page.locator('#feed-status').textContent(), status);
        assert.doesNotMatch(await page.locator('#feed-list').textContent(), /Old headline/);
        await page.evaluate(() => window.changeVisibility(false));
      }
      await text(page, 'feed-list', 'Current headline');
      await text(page, 'feed-status', 'Updates every 5 minutes');
      await tick(page, 5100);
      assert.equal(requests, 2, 'discard the old refreshing response and its short retry cadence');
    });
  }

  test(`RSS late ${stage} failure cannot delay resumed polling`, async t => {
    let requests = 0;
    const { page } = await setup(t, { init: holdFirstNewsResponse, initArg: stage,
      news: route => { requests++; return json(route, { xml: newsFeed('Current headline'), stale: false }); } });
    await page.waitForFunction(() => typeof window.releaseNews === 'function');
    await page.evaluate(() => {
      window.dispatchEvent(new Event('pagehide'));
      window.dispatchEvent(new Event('pageshow'));
      window.releaseNews(true);
    });
    await tick(page, 100);
    await text(page, 'feed-list', 'Current headline');
    await text(page, 'feed-status', 'Updates every 5 minutes');
    assert.equal(requests, 2, 'cancelled failure must not set a five-minute delay');
  });
}

test('RSS stays stopped on an initially hidden page and starts once when shown', async t => {
  let requests = 0;
  const { page } = await setup(t, { init: () => { window.testHidden = true; },
    news: route => { requests++; return json(route, { xml: newsFeed('Current headline'), stale: false }); } });
  await tick(page, 20000);
  assert.equal(requests, 0);
  await page.evaluate(() => {
    window.dispatchEvent(new Event('pageshow'));
    window.changeVisibility(false);
    window.dispatchEvent(new Event('pageshow'));
    window.changeVisibility(false);
  });
  await text(page, 'feed-list', 'Current headline');
  await text(page, 'status', 'LIVE');
  await tick(page, 5100);
  assert.equal(requests, 1);
});

test('missing AbortController reports unavailable RSS without unhandled errors or requests', async t => {
  let requests = 0;
  const { page } = await setup(t, { init: () => { window.AbortController = undefined; },
    news: route => { requests++; return json(route, {}); } });
  await text(page, 'status', 'UNSUPPORTED');
  await text(page, 'feed-status', 'temporarily unavailable');
  assert.equal(requests, 0);
});

test('RSS response-body deadline preserves cached headlines and five-minute failure cadence', async t => {
  let requests = 0;
  const app = await setup(t, { news: route => json(route, { xml: newsFeed('Cached headline'), refreshing: ++requests === 1 }),
    init: () => {
      const originalFetch = window.fetch;
      let news = 0;
      window.fetch = async (...args) => {
        const response = await originalFetch(...args);
        if (args[0] === '/api/news' && ++news === 2) {
          response.json = () => new Promise((resolve, reject) => {
            window.newsBodySignal = args[1].signal;
            args[1].signal.addEventListener('abort', () => reject(new Error('Body aborted')), { once: true });
          });
        }
        return response;
      };
    } });
  await text(app.page, 'feed-list', 'Cached headline');
  await tick(app.page, 2100);
  await app.page.waitForFunction(() => window.newsBodySignal !== undefined);
  await tick(app.page, 15100);
  assert.equal(await app.page.evaluate(() => window.newsBodySignal.aborted), true);
  await text(app.page, 'feed-status', 'Showing previous headlines — retrying in 5 minutes');
  await text(app.page, 'feed-list', 'Cached headline');
  await app.page.clock.fastForward(299000);
  assert.equal(requests, 2);
  await tick(app.page, 1100);
  await text(app.page, 'feed-status', 'Updates every 5 minutes');
  assert.equal(requests, 3);
  assert.ok(app.presence.filter(body => body.active).length >= 3);
  assert.ok(app.prices() >= 3);
  assert.equal(app.maxPrices(), 1);
  assert.equal(app.maxPresence(), 1);
});


test('RSS cold start retries promptly while prices stay available', async t => {
  let requests = 0;
  const xml = '<rss><channel><item><title>Fresh headline</title><link>https://news.bitcoin.com/fresh/</link></item></channel></rss>';
  const { page } = await setup(t, { news: route => ++requests === 1
    ? json(route, { xml: null, stale: true, refreshing: true }, 503, { 'Retry-After': '2' })
    : json(route, { xml, stale: false, refreshing: false }) });
  await text(page, 'feed-status', 'retrying shortly');
  await text(page, 'status', 'LIVE');
  await tick(page, 2100);
  await text(page, 'feed-list', 'Fresh headline');
  assert.equal(requests, 2);
  await tick(page, 10000);
  assert.equal(requests, 2, 'successful refresh returns to five-minute cadence');
});

test('RSS serves old headlines during refresh and defers retries while hidden', async t => {
  let requests = 0;
  const feed = title => `<rss><channel><item><title>${title}</title><link>https://news.bitcoin.com/story/</link></item></channel></rss>`;
  const { page } = await setup(t, { news: route => ++requests === 1
    ? json(route, { xml: feed('Cached headline'), stale: true, refreshing: true })
    : json(route, { xml: feed('Updated headline'), stale: false, refreshing: false }) });
  await text(page, 'feed-list', 'Cached headline');
  await text(page, 'feed-status', 'Refreshing cached');
  await page.evaluate(() => window.changeVisibility(true));
  await tick(page, 3000);
  assert.equal(requests, 1);
  await page.evaluate(() => window.changeVisibility(false));
  await text(page, 'feed-list', 'Updated headline');
  assert.equal(requests, 2);
});

test('RSS resumes the remaining retry delay when shown before it expires', async t => {
  let requests = 0;
  const { page } = await setup(t, { news: route => json(route, {
    xml: newsFeed(++requests === 1 ? 'Cached headline' : 'Current headline'), refreshing: requests === 1,
  }) });
  await text(page, 'feed-status', 'Refreshing cached');
  await page.evaluate(() => window.changeVisibility(true));
  await tick(page, 1000);
  await page.evaluate(() => window.changeVisibility(false));
  await tick(page, 500);
  assert.equal(requests, 1);
  await tick(page, 600);
  await text(page, 'feed-list', 'Current headline');
  assert.equal(requests, 2);
});

for (const clockShift of [-3600000, 3600000]) {
  test(`RSS retry delay survives a ${clockShift < 0 ? 'backward' : 'forward'} wall-clock change`, async t => {
    let requests = 0;
    const { page } = await setup(t, { news: route => json(route, {
      xml: newsFeed(++requests === 1 ? 'Cached headline' : 'Current headline'), refreshing: requests === 1,
    }) });
    await text(page, 'feed-status', 'Refreshing cached');
    await page.evaluate(() => window.changeVisibility(true));
    await tick(page, 1000);
    await page.clock.setSystemTime(await page.evaluate(shift => Date.now() + shift, clockShift));
    await page.evaluate(() => window.changeVisibility(false));
    await tick(page, 500);
    assert.equal(requests, 1, 'a wall-clock jump must not bypass the remaining retry delay');
    await tick(page, 600);
    await text(page, 'feed-list', 'Current headline');
    assert.equal(requests, 2, 'a wall-clock jump must not postpone the monotonic deadline');
  });
}

test('RSS scrolls automatically and pauses when requested', async t => {
  const xml = '<rss><channel>' + Array.from({ length: 10 }, (_, i) => `<item><title>Bitcoin headline ${i}</title><link>https://news.bitcoin.com/story-${i}/</link></item>`).join('') + '</channel></rss>';
  const { page } = await setup(t, { news: route => json(route, { xml, stale: false }) });
  await text(page, 'feed-list', 'Bitcoin headline 9');
  await tick(page, 1000);
  const top = await page.locator('#feed-window').evaluate(el => el.scrollTop);
  assert.ok(top > 0);
  await page.locator('#feed-toggle').click();
  const pausedTop = await page.locator('#feed-window').evaluate(el => el.scrollTop);
  await tick(page, 1000);
  assert.equal(await page.locator('#feed-window').evaluate(el => el.scrollTop), pausedTop);
});

for (const interruption of ['pause', 'hidden', 'hover', 'focus', 'manual', 'reduced-motion']) {
  test(`RSS bottom dwell restarts after ${interruption}`, async t => {
    const xml = '<rss><channel>' + Array.from({ length: 10 }, (_, i) => `<item><title>Bitcoin headline ${i}</title><link>https://news.bitcoin.com/story-${i}/</link></item>`).join('') + '</channel></rss>';
    const { page } = await setup(t, { news: route => json(route, { xml, stale: false }) });
    await text(page, 'feed-list', 'Bitcoin headline 9');
    const feed = page.locator('#feed-window');
    await feed.evaluate(el => { el.scrollTop = el.scrollHeight; });
    await tick(page, 100);
    const bottom = await feed.evaluate(el => el.scrollTop);
    assert.ok(bottom > 0);
    if (interruption === 'hidden') await page.evaluate(() => window.changeVisibility(true));
    else if (interruption === 'hover') await feed.dispatchEvent('mouseenter');
    else if (interruption === 'focus') await feed.focus();
    else if (interruption === 'manual') await feed.dispatchEvent('wheel');
    else if (interruption === 'reduced-motion') {
      await page.emulateMedia({ reducedMotion: 'reduce' });
      await text(page, 'feed-toggle', 'Resume scrolling');
    } else await page.locator('#feed-toggle').click();
    await tick(page, 4000);
    assert.equal(await feed.evaluate(el => el.scrollTop), bottom);
    if (interruption === 'hidden') await page.evaluate(() => window.changeVisibility(false));
    else if (interruption === 'hover') await feed.dispatchEvent('mouseleave');
    else if (interruption === 'focus') await feed.evaluate(el => el.blur());
    else if (interruption === 'reduced-motion') {
      await page.emulateMedia({ reducedMotion: 'no-preference' });
      await text(page, 'feed-toggle', 'Pause scrolling');
    } else await page.locator('#feed-toggle').click();
    await tick(page, 100);
    assert.equal(await feed.evaluate(el => el.scrollTop), bottom, 'resuming starts a new bottom dwell');
    await tick(page, 2900);
    assert.equal(await feed.evaluate(el => el.scrollTop), bottom, 'retain a full three-second dwell');
    await tick(page, 200);
    assert.ok(await feed.evaluate(el => el.scrollTop) < bottom, 'wrap after the new dwell');
  });
}

test('a deployment bypasses legacy cached JavaScript and CSS', async t => {
  const html = await readFile(new URL('../../src/ui.html', import.meta.url));
  const js = await readFile(new URL('../../src/dashboard.js', import.meta.url));
  const css = await readFile(new URL('../../src/dashboard.css', import.meta.url));
  const vendor = await readFile(new URL('../../src/vendor/cybercore-0.3.0.min.css', import.meta.url));
  const fixture = createServer((req, res) => {
    const url = new URL(req.url, 'http://localhost');
    let body, type = 'text/html', cache = 'no-cache';
    if (url.pathname === '/warm') {
      body = '<link rel="stylesheet" href="/assets/dashboard.css?v=2.9.2"><script src="/assets/dashboard.js?v=2.9.2"></script>';
    } else if (url.pathname === '/assets/dashboard.js') {
      type = 'text/javascript';
      body = url.search && url.searchParams.get('v') !== '2.9.2' ? js : 'window.legacyDashboard = true;';
      cache = url.search && url.searchParams.get('v') !== '2.9.2' ? 'no-cache' : 'public, max-age=31536000';
    } else if (url.pathname === '/assets/dashboard.css') {
      type = 'text/css';
      body = url.search && url.searchParams.get('v') !== '2.9.2' ? css : '.feed-window { height: 0px; }';
      cache = url.search && url.searchParams.get('v') !== '2.9.2' ? 'no-cache' : 'public, max-age=31536000';
    } else if (url.pathname === '/assets/cybercore-0.3.0.min.css') {
      type = 'text/css'; body = vendor;
    } else if (url.pathname.startsWith('/api/')) {
      type = 'application/json';
      body = JSON.stringify(url.pathname === '/api/news'
        ? { stale: false, xml: '<rss><channel><item><title>Fresh RSS headline</title><link>https://news.bitcoin.com/example/</link></item></channel></rss>' }
        : url.pathname === '/api/price' ? report() : { active_viewers: 1 });
    } else body = html;
    res.writeHead(200, { 'Content-Type': type, 'Cache-Control': cache });
    res.end(body);
  });
  await new Promise(resolve => fixture.listen(0, '127.0.0.1', resolve));
  const context = await browser.newContext();
  t.after(async () => { await context.close(); await new Promise(resolve => fixture.close(resolve)); });
  const page = await context.newPage();
  const origin = `http://127.0.0.1:${fixture.address().port}`;
  await page.goto(origin + '/warm');
  assert.equal(await page.evaluate(() => window.legacyDashboard), true);
  await page.goto(origin);
  await text(page, 'feed-list', 'Fresh RSS headline');
  assert.equal(await page.evaluate(() => window.legacyDashboard), undefined);
  assert.equal(await page.locator('#feed-window').evaluate(el => getComputedStyle(el).height), '180px');
});
