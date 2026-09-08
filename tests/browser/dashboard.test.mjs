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
    average_price: 100150, spread: 300, refresh_succeeded: true,
    coverage: { configured_source_count: 4, fresh_source_count: 4, contributing_sources: [...providers] },
    sources: providers.map((source, index) => ({ source, price_usd: 100000 + index * 100,
      last_success_at_unix: now, quote_kind: ['aggregate', 'spot', 'last_trade', 'bid'][index], age_seconds: 0, freshness: 'fresh' })),
    provider_health: providers.map(source => ({ source, last_attempt: { outcome: 'success', attempted_at_unix: now } })),
    ...overrides,
  };
}
const unavailable = () => report({ status: 'UNAVAILABLE', stale: true, sources: [], average_price: null, spread: null,
  coverage: { configured_source_count: 4, fresh_source_count: 0, contributing_sources: [] } });
const json = (route, data, status = 200) => route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(data) });

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
    const file = files.get(req.url);
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
  const context = await browser.newContext({ viewport: options.viewport ?? { width: 1200, height: 900 }, reducedMotion: options.reducedMotion ?? 'no-preference' });
  context.setDefaultTimeout(4000);
  const errors = [], presence = [], pending = new Set();
  let prices = 0, maxPrices = 0, maxPresence = 0;
  context.on('page', page => {
    page.on('pageerror', error => errors.push(error.message));
    page.on('request', req => {
      if (req.url().endsWith('/api/price') || (req.url().endsWith('/api/presence') && req.postDataJSON()?.active)) {
        pending.add(req);
        maxPrices = Math.max(maxPrices, [...pending].filter(req => req.url().endsWith('/api/price')).length);
        maxPresence = Math.max(maxPresence, [...pending].filter(req => req.url().endsWith('/api/presence')).length);
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
  if (options.init) await context.addInitScript(options.init);
  await context.route('**/*', async route => {
    const url = route.request().url();
    if (!url.startsWith(base + '/')) { errors.push(`Unexpected external request: ${url}`); return route.abort(); }
    if (url.endsWith('/api/presence')) {
      const body = route.request().postDataJSON();
      presence.push(body);
      if (options.presence) return options.presence(route, body);
      return json(route, { active_viewers: body.active ? 1 : 0 });
    }
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
  await page.goto(base);
  return { page, context, presence, prices: () => prices, maxPrices: () => maxPrices, maxPresence: () => maxPresence };
}
async function text(page, id, expected) {
  try {
    await page.waitForFunction(({ id, expected }) => document.getElementById(id).textContent.includes(expected), { id, expected });
  } catch (error) {
    throw new Error(`${id}: expected ${expected}, got ${await page.locator('#' + id).textContent()}`, { cause: error });
  }
}
async function tick(page, ms) { await page.clock.runFor(ms); }

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
  assert.equal(await page.locator('[role=status]').count(), 1);
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
  await tick(app.page, 5100);
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
