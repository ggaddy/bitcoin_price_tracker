(() => {
  "use strict";
  const el = Object.fromEntries(["avg", "spread", "updated", "viewers", "status", "note", "sources", "warnings", "coverage"]
    .map(id => [id, document.getElementById(id)]));
  const fmt = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", maximumFractionDigits: 2 });
  const priceText = value => fmt.format(value).replace(/,/g, "\u2009");
  const workers = {
    presence: { busy: false, timer: null, controller: null, timeout: 3000 },
    price: { busy: false, timer: null, controller: null, timeout: 8000 },
  };
  let sessionId, running = false, generation = 0, ageTimer = null;
  let lastData = null, receivedAt = 0, priceError = "", presenceError = "";

  function status(label) {
    // Only changes are announced by role=status, never routine heartbeats.
    if (el.status.textContent !== label) el.status.textContent = label;
    el.status.classList.toggle("status-bad", ["OFFLINE", "UNAVAILABLE", "UNSUPPORTED"].includes(label));
    el.status.classList.toggle("status-warn", ["IDLE", "DEGRADED", "STALE"].includes(label));
  }

  function node(tag, className, text) {
    const element = document.createElement(tag);
    element.className = className;
    element.textContent = text;
    return element;
  }

  function createSession() {
    // A new identity per document avoids copied sessionStorage identities in
    // duplicated/opener tabs. No storage capability is needed or accessed.
    const crypto = globalThis.crypto;
    if (typeof crypto?.randomUUID === "function") {
      try { return crypto.randomUUID(); } catch { /* Try random bytes below. */ }
    }
    if (typeof crypto?.getRandomValues === "function") {
      return Array.from(crypto.getRandomValues(new Uint8Array(16)), byte => byte.toString(16).padStart(2, "0")).join("");
    }
    throw new Error("Secure random identifiers are unavailable.");
  }

  function validate(data) {
    const finite = value => typeof value === "number" && Number.isFinite(value);
    if (!data || !["LIVE", "DEGRADED", "STALE", "UNAVAILABLE"].includes(data.status)
      || !Array.isArray(data.sources) || !Array.isArray(data.provider_health)
      || !Array.isArray(data.warnings) || !Array.isArray(data.coverage?.contributing_sources)
      || !Number.isInteger(data.coverage.configured_source_count) || data.coverage.configured_source_count < 1
      || !finite(data.source_max_age_seconds) || data.source_max_age_seconds <= 0
      || !finite(data.evaluated_at_unix)
      || new Set(data.coverage.contributing_sources).size !== data.coverage.contributing_sources.length
      || !data.coverage.contributing_sources.every(name => typeof name === "string")
      || data.coverage.contributing_sources.length > data.coverage.configured_source_count
      || !data.sources.every(source => typeof source.source === "string" && finite(source.price_usd) && source.price_usd > 0
        && ["fresh", "stale", "future", "unknown"].includes(source.freshness)
        && (source.age_seconds === null || (finite(source.age_seconds) && source.age_seconds >= 0))
        && (source.freshness !== "fresh" || source.age_seconds !== null))
      || !data.provider_health.every(health => typeof health.source === "string" && health.last_attempt
        && ["unknown", "success", "failure"].includes(health.last_attempt.outcome))) {
      throw new Error("Invalid price response");
    }
    return data;
  }

  function render() {
    if (!running) return;
    const warnings = [];
    if (priceError) warnings.push(priceError);
    if (presenceError) warnings.push(presenceError);
    if (!lastData) {
      status(priceError ? "OFFLINE" : presenceError ? "DEGRADED" : "Connecting");
      if (priceError) el.avg.textContent = "Unavailable";
      el.note.textContent = warnings.length ? "Connection interrupted. Retrying automatically." : "Connecting to the price feed.";
    } else {
      const data = lastData;
      // Advance server-provided ages with elapsed monotonic time. Unknown/future
      // observations never become fresh locally; only a new response can do that.
      const elapsed = Math.max(0, Math.floor((performance.now() - receivedAt) / 1000));
      const sources = data.sources.map(source => ({ ...source,
        age: source.age_seconds === null ? null : source.age_seconds + elapsed,
        currentFreshness: source.freshness === "fresh" && source.age_seconds + elapsed >= data.source_max_age_seconds
          ? "stale" : source.freshness,
      }));
      const contributors = data.coverage.contributing_sources.map(name => sources
        .filter(source => source.source === name && source.currentFreshness === "fresh")
        .sort((a, b) => a.age - b.age)[0]).filter(Boolean);
      const count = contributors.length;
      let label = data.status;
      if (sources.length && !count) label = "STALE";
      else if (label === "LIVE" && count < data.coverage.configured_source_count) label = "DEGRADED";
      if (priceError) label = "OFFLINE";
      else if (presenceError && label === "LIVE") label = "DEGRADED";
      status(label);
      let mean = null;
      contributors.forEach((source, index) => { mean = index ? mean + (source.price_usd - mean) / (index + 1) : source.price_usd; });
      el.avg.textContent = mean === null ? "Unavailable" : priceText(mean);
      el.spread.textContent = count ? priceText(Math.max(...contributors.map(s => s.price_usd)) - Math.min(...contributors.map(s => s.price_usd))) : "-";
      el.coverage.textContent = `${count} of ${data.coverage.configured_source_count} sources contribute · Fresh for less than ${data.source_max_age_seconds}s`;
      el.viewers.textContent = String(data.active_viewers ?? 0);
      const times = sources.map(source => source.last_success_at_unix).filter(time => Number.isFinite(time) && time <= data.evaluated_at_unix);
      el.updated.textContent = times.length ? new Date(Math.max(...times) * 1000).toLocaleString() : "Unknown";
      el.note.textContent = priceError
        ? `Connection interrupted. Retaining last received quotes; report received ${elapsed}s ago. Retrying automatically.`
        : presenceError ? "Viewer heartbeat failed. Displaying available quotes while retrying."
        : !count ? "No fresh quotes qualify for the average. Retained quotes are shown with their ages."
        : `Showing ${count} contributing sources. Ages advance between server updates.`;
      el.sources.replaceChildren();
      const names = [...new Set([...sources.map(source => source.source), ...data.provider_health.map(health => health.source)])];
      for (const name of names) {
        const source = sources.filter(source => source.source === name).sort((a, b) => (a.age ?? Infinity) - (b.age ?? Infinity))[0];
        const health = data.provider_health.find(health => health.source === name)?.last_attempt;
        const article = node("article", "source-card", "");
        const surface = node("div", "cyber-card source-card__surface", "");
        surface.append(node("h3", "label", name), node("p", "value", source ? priceText(source.price_usd) : "No quote"));
        surface.append(node("p", "source-unit", source ? `USD / BTC · ${String(source.quote_kind ?? "unknown").replace(/_/g, " ")}` : "Waiting for a successful quote"));
        surface.append(node("p", "source-detail", source
          ? (source.age === null ? "Age unknown" : `${source.age}s`) : "Age unknown"));
        if (health?.outcome === "failure") surface.append(node("p", "source-error", `Latest attempt failed: ${health.error?.message ?? "Provider request failed"}`));
        else if (health?.outcome !== "success") surface.append(node("p", "source-detail", "Latest attempt unknown"));
        article.append(surface);
        el.sources.append(article);
      }
      if (!names.length) el.sources.append(node("p", "empty-state", "No provider quotes available. Retrying automatically."));
      warnings.push(...data.warnings.map(String));
    }
    el.avg.dataset.text = el.avg.textContent;
    // Avoid repeated live-region announcements; only the badge is live.
    el.warnings.replaceChildren(...(warnings.length ? warnings : ["None"]).map(text => node("li", "", text)));
  }

  async function poll(kind) {
    const worker = workers[kind];
    if (!running || worker.busy) return;
    clearTimeout(worker.timer);
    worker.timer = null;
    worker.busy = true;
    const owner = generation;
    const controller = new AbortController();
    worker.controller = controller;
    const deadline = setTimeout(() => controller.abort(), worker.timeout);
    try {
      const response = await fetch(kind === "price" ? "/api/price" : "/api/presence", kind === "price"
        ? { signal: controller.signal, cache: "no-store" }
        : { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ session_id: sessionId, active: true }), signal: controller.signal });
      if (kind === "presence") {
        if (!response.ok) throw new Error("Heartbeat rejected");
        // Consume the body within the same deadline so slow bodies cannot leak requests.
        await response.text();
        if (running && owner === generation) presenceError = "";
      } else {
        if (!response.ok && response.status !== 503) throw new Error("Price request rejected");
        const data = validate(await response.json());
        if (response.status === 503 && data.status !== "UNAVAILABLE") throw new Error("Invalid unavailable response");
        if (running && owner === generation) {
          // An unavailable response cannot erase previously received quotes.
          if (data.status === "UNAVAILABLE" && lastData?.sources.length) {
            priceError = "Price service unavailable; retained quotes may be out of date.";
          } else {
            lastData = data;
            receivedAt = performance.now();
            priceError = "";
          }
        }
      }
    } catch {
      if (running && owner === generation) {
        if (kind === "presence") presenceError = "Viewer heartbeat unavailable. Retrying automatically.";
        else priceError = "Price update unavailable; retained quotes may be out of date. Retrying automatically.";
      }
    } finally {
      clearTimeout(deadline);
      worker.controller = null;
      worker.busy = false;
      if (running) {
        render();
        worker.timer = setTimeout(() => poll(kind), owner === generation ? 5000 : 0);
      }
    }
  }

  function inactivePresence() {
    if (!sessionId) return;
    const payload = JSON.stringify({ session_id: sessionId, active: false });
    try {
      if (typeof navigator.sendBeacon === "function" && navigator.sendBeacon("/api/presence", new Blob([payload], { type: "application/json" }))) return;
    } catch { /* A rejected beacon uses a bounded keepalive request. */ }
    const controller = new AbortController();
    const deadline = setTimeout(() => controller.abort(), 3000);
    fetch("/api/presence", { method: "POST", headers: { "Content-Type": "application/json" }, body: payload, keepalive: true, signal: controller.signal })
      .catch(() => {}).finally(() => clearTimeout(deadline));
  }

  function start() {
    document.documentElement.classList.toggle("page-hidden", document.hidden);
    if (running || document.hidden || !sessionId) return;
    running = true;
    generation++;
    render();
    // The first heartbeat registers this viewer before its initial price request.
    // Subsequent heartbeats and prices own independent, nonoverlapping loops.
    poll("presence").then(() => { if (running && workers.price.timer === null) poll("price"); });
    ageTimer = setInterval(render, 1000);
  }

  function stop() {
    document.documentElement.classList.add("page-hidden");
    if (!running) return;
    running = false;
    generation++;
    clearInterval(ageTimer);
    ageTimer = null;
    for (const worker of Object.values(workers)) {
      clearTimeout(worker.timer);
      worker.timer = null;
      worker.controller?.abort();
    }
    inactivePresence();
    status("IDLE");
    el.note.textContent = "Tracking paused while this page is hidden. Quotes will be checked when you return.";
  }

  try {
    if (typeof fetch !== "function" || typeof AbortController !== "function") throw new Error("Bounded requests unavailable");
    sessionId = createSession();
    document.addEventListener("visibilitychange", () => document.hidden ? stop() : start());
    window.addEventListener("pagehide", stop);
    window.addEventListener("pageshow", start);
    if (document.hidden) {
      document.documentElement.classList.add("page-hidden");
      status("IDLE");
      el.note.textContent = "Open this tab to start price tracking.";
    } else start();
  } catch {
    status("UNSUPPORTED");
    el.avg.textContent = "Unavailable";
    el.avg.dataset.text = "Unavailable";
    el.note.textContent = "This browser cannot safely track a viewer session. Use a browser with secure random IDs, fetch, and AbortController support.";
  }
})();
