// Capture the documentation screenshots under docs/images/.
//
//   node scripts/capture_screenshots.mjs [name ...] [--out DIR]
//
// Serves examples/ on a free port with a temporary home, gives it some history
// (completed, failed, and paused runs, a skipped fire, artifacts, variables, a rule),
// and captures each page at 1440x900 with headless Chrome over the DevTools protocol.
// The worker image starts its own server, with a token, and a worker.
// Names restrict the capture to those images. Needs a built UI (`just ui`), the
// extension (`just dev`), Node 22, and Chrome; set CHROME to its path if it is not
// found. A server already listening on 4200 is not touched.

import { execFileSync, spawn } from "node:child_process";
import { cpSync, existsSync, mkdtempSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..");
const WIDTH = 1440;
const HEIGHT = 900;
const MAX_BYTES = 300_000;
const DAY = "2026-09-14";

const args = process.argv.slice(2);
const outIndex = args.indexOf("--out");
const OUT = outIndex >= 0 ? path.resolve(args.splice(outIndex, 2)[1]) : path.join(ROOT, "docs", "images");
const only = new Set(args);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function freePort() {
  return new Promise((resolve) => {
    const srv = createServer().listen(0, "127.0.0.1", () => {
      const { port } = srv.address();
      srv.close(() => resolve(port));
    });
  });
}

function findChrome() {
  const candidates = [
    process.env.CHROME,
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
  ];
  for (const c of candidates) if (c && existsSync(c)) return c;
  for (const name of ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"]) {
    try {
      return execFileSync("which", [name], { encoding: "utf8" }).trim();
    } catch {}
  }
  throw new Error("Chrome not found; set CHROME to its path");
}

// ---------------------------------------------------------------- the demo server

class Demo {
  constructor(port) {
    this.base = `http://127.0.0.1:${port}`;
    this.home = mkdtempSync(path.join(tmpdir(), "cereyan-shots-home-"));
    // Run from a scratch directory: examples write their targets relative to it.
    this.cwd = mkdtempSync(path.join(tmpdir(), "cereyan-shots-cwd-"));
    this.proc = spawn(
      "uv",
      ["run", "--project", ROOT, "cereyan", "serve", path.join(ROOT, "examples"), "--port", String(port), "--max-engines", "4"],
      { cwd: this.cwd, env: { ...process.env, CEREYAN_HOME: this.home }, stdio: "ignore", detached: true },
    );
  }

  async request(method, route, body) {
    const r = await fetch(this.base + route, {
      method,
      headers: body === undefined ? {} : { "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!r.ok) throw new Error(`${method} ${route}: ${r.status} ${await r.text()}`);
    return r.status === 204 ? null : r.json();
  }

  get(route) {
    return this.request("GET", route);
  }

  async ready() {
    for (let i = 0; i < 300; i++) {
      try {
        await this.get("/api/health");
        return;
      } catch {
        await sleep(200);
      }
    }
    throw new Error("the demo server did not start");
  }

  stop() {
    // The server, its interpreter, and its engines share one process group.
    try {
      process.kill(-this.proc.pid, "SIGTERM");
    } catch {}
    for (const dir of [this.home, this.cwd]) rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
}

// A second, short-lived server for the worker's status page: workers need a
// token, which the main demo must not have, and one processor, so runs spill
// over to the worker. Both serve a copy of examples/, because a worker's
// engines start in its checkout and the examples write files relative to it.
class WorkerDemo {
  constructor(port, statusPort) {
    this.token = "screenshots";
    this.base = `http://127.0.0.1:${port}`;
    this.status = `http://127.0.0.1:${statusPort}`;
    this.statusPort = statusPort;
    this.home = mkdtempSync(path.join(tmpdir(), "cereyan-shots-home-"));
    this.workerHome = mkdtempSync(path.join(tmpdir(), "cereyan-shots-worker-home-"));
    this.checkout = mkdtempSync(path.join(tmpdir(), "cereyan-shots-checkout-"));
    cpSync(path.join(ROOT, "examples"), this.checkout, { recursive: true, filter: (p) => !p.includes("__pycache__") });
    this.server = spawn(
      "uv",
      ["run", "--project", ROOT, "cereyan", "serve", this.checkout, "--port", String(port), "--max-engines", "1", "--token", this.token],
      { cwd: this.checkout, env: { ...process.env, CEREYAN_HOME: this.home }, stdio: "ignore", detached: true },
    );
  }

  async request(method, route, body) {
    const r = await fetch(this.base + route, {
      method,
      headers: { authorization: `Bearer ${this.token}`, ...(body === undefined ? {} : { "content-type": "application/json" }) },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!r.ok) throw new Error(`${method} ${route}: ${r.status} ${await r.text()}`);
    return r.json();
  }

  async statusJson() {
    return (await fetch(`${this.status}/status.json`)).json();
  }

  async until(check, what, timeout = 120_000) {
    const end = Date.now() + timeout;
    while (Date.now() < end) {
      try {
        if (await check()) return;
      } catch {}
      await sleep(500);
    }
    throw new Error(`timed out waiting for ${what}`);
  }

  async start() {
    await this.until(() => this.request("GET", "/api/server"), "the worker demo server");
    this.worker = spawn(
      "uv",
      ["run", "--project", ROOT, "cereyan", "worker", this.checkout, "--host", this.base, "--token", this.token,
        "--name", "build-02", "--processors", "2", "--labels", "zone=eu,disk=ssd", "--status-port", String(this.statusPort)],
      { cwd: this.checkout, env: { ...process.env, CEREYAN_HOME: this.workerHome }, stdio: "ignore", detached: true },
    );
    await this.until(async () => (await this.statusJson()).state === "online", "the worker to come online");
  }

  // Hold the server's one processor with a long run, so the rest go to the
  // worker. A worker keeps an engine per module, so its two processors serve two
  // modules: `flaky_load` (one fails) first, then the `load_*` flows.
  async seed() {
    const flows = Object.fromEntries((await this.request("GET", "/api/flows")).map((f) => [f.name, f]));
    const run = async (name, parameters = {}) => (await this.request("POST", `/api/flows/${flows[name].id}/runs`, { parameters })).id;
    const state = async (id) => (await this.request("GET", `/api/runs/${id}`)).state.type;
    const hold = await run("usually_quick", { seconds: 45 });
    await this.until(async () => (await state(hold)) === "Running", "the server to be busy");
    const first = await run("flaky_load", { rows: 2 });
    await this.until(async () => ["Completed", "Failed"].includes(await state(first)), "the first worker run");
    const started = [await run("flaky_load", { rows: 5 })];
    for (const day of ["2026-09-12", "2026-09-13", DAY]) {
      started.push(await run("load_orders", { day }));
      started.push(await run("load_customers", { day }));
    }
    const settled = new Set(["Completed", "Failed", "Crashed", "Cancelled"]);
    for (const id of [...started, hold]) await this.until(async () => settled.has(await state(id)), `run ${id}`);
    await this.until(async () => {
      const s = await this.statusJson();
      return (s.stats?.by_flow ?? []).filter((f) => f.completed + f.failed > 0).length >= 2;
    }, "runs on the worker in its stats");
  }

  stop() {
    for (const p of [this.worker, this.server]) {
      try {
        if (p) process.kill(-p.pid, "SIGTERM");
      } catch {}
    }
    for (const dir of [this.home, this.workerHome, this.checkout]) rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
}

async function seed(demo) {
  const flows = Object.fromEntries((await demo.get("/api/flows")).map((f) => [f.name, f]));
  const started = [];
  const run = async (name, parameters = {}) => {
    const r = await demo.request("POST", `/api/flows/${flows[name].id}/runs`, { parameters });
    started.push(r.id);
    return r;
  };
  // History worth a sparkline, one failure, one run paused on a question.
  for (const day of ["2026-09-12", "2026-09-13", DAY]) {
    await run("etl", { day });
    await run("daily_etl", { day });
    await run("sales", { day });
    await run("inventory", { day });
    await run("load_orders", { day });
    await run("load_customers", { day });
  }
  await run("flaky_load", { rows: 2 });
  await run("flaky_load", { rows: 5 });
  await run("check_orders", { count: 0 });
  await run("check_orders", { count: 12 });
  await run("reprocess_day", { day: DAY });
  await run("compact_tables");
  await run("publish", { day: DAY });

  const settled = new Set(["Completed", "Failed", "Crashed", "Cancelled", "Paused"]);
  const deadline = Date.now() + 180_000;
  while (Date.now() < deadline) {
    const state = new Map((await demo.get("/api/runs?limit=500")).items.map((r) => [r.id, r.state.type]));
    if (started.every((id) => settled.has(state.get(id)))) break;
    await sleep(1000);
  }

  const etl = (await demo.get(`/api/runs?flow=etl&limit=1`)).items[0];
  await demo.request("POST", "/api/artifacts", {
    run_id: etl.id,
    kind: "table",
    key: "daily-totals",
    data: { columns: ["region", "orders", "revenue"], rows: [["north", 412, 18230.5], ["south", 377, 16102.0]] },
  });
  await demo.request("POST", "/api/artifacts", {
    run_id: etl.id,
    kind: "markdown",
    key: "etl-summary",
    data: { text: "## Load summary\n\n789 orders loaded, no rows rejected." },
  });
  await demo.request("POST", "/api/artifacts", {
    run_id: etl.id,
    kind: "progress",
    key: "backfill-progress",
    data: { value: 60, label: "18 of 30 days" },
  });
  await demo.request("POST", "/api/artifacts", {
    run_id: etl.id,
    kind: "link",
    key: "report",
    data: { url: "https://example.com/reports/daily", text: "Daily report" },
  });

  await demo.request("POST", "/api/variables", { name: "warehouse/api_token", value: { token: "s3cr3t" }, secret: true, tags: ["warehouse"] });
  await demo.request("POST", "/api/variables", { name: "report_recipients", value: { to: ["data@example.com"] }, tags: ["reporting"] });
  await demo.request("POST", "/api/rules", {
    name: "rerun a failed load",
    when: { events: ["run.failed"], flows: ["flaky_load"] },
    do: [{ kind: "run_flow", flow: "flaky_load", parameters: { rows: 1 } }],
  });

  const schedule = flows.daily_etl.schedules[0];
  await demo.request("POST", `/api/schedules/${schedule.id}/skips`, { next: 1, by: "ui" });
  return { flows, etl };
}

// ---------------------------------------------------------------- the browser

class Browser {
  async open() {
    const port = await freePort();
    this.profile = mkdtempSync(path.join(tmpdir(), "cereyan-shots-chrome-"));
    this.proc = spawn(
      findChrome(),
      [
        "--headless=new",
        `--remote-debugging-port=${port}`,
        `--user-data-dir=${this.profile}`,
        `--window-size=${WIDTH},${HEIGHT}`,
        "--hide-scrollbars",
        "--force-device-scale-factor=1",
        "about:blank",
      ],
      { stdio: "ignore" },
    );
    let socketUrl;
    for (let i = 0; i < 150 && !socketUrl; i++) {
      try {
        const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
        socketUrl = targets.find((t) => t.type === "page")?.webSocketDebuggerUrl;
      } catch {}
      if (!socketUrl) await sleep(100);
    }
    if (!socketUrl) throw new Error("Chrome did not expose a page");
    this.ws = new WebSocket(socketUrl);
    await new Promise((r) => this.ws.addEventListener("open", r, { once: true }));
    this.seq = 0;
    this.pending = new Map();
    this.ws.addEventListener("message", (e) => {
      const m = JSON.parse(e.data);
      const done = this.pending.get(m.id);
      if (!done) return;
      this.pending.delete(m.id);
      done(m);
    });
    await this.send("Page.enable");
    await this.send("Runtime.enable");
    await this.send("Emulation.setDeviceMetricsOverride", {
      width: WIDTH,
      height: HEIGHT,
      deviceScaleFactor: 1,
      mobile: false,
    });
  }

  send(method, params = {}) {
    return new Promise((resolve, reject) => {
      const id = ++this.seq;
      this.pending.set(id, (m) => (m.error ? reject(new Error(`${method}: ${m.error.message}`)) : resolve(m.result)));
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  async eval(expression) {
    const { result, exceptionDetails } = await this.send("Runtime.evaluate", {
      expression,
      awaitPromise: true,
      returnByValue: true,
    });
    if (exceptionDetails) throw new Error(`${expression}: ${exceptionDetails.text}`);
    return result.value;
  }

  async until(expression, what = expression, timeout = 20_000) {
    const end = Date.now() + timeout;
    while (Date.now() < end) {
      if (await this.eval(`(() => { try { return !!(${expression}); } catch { return false; } })()`)) return;
      await sleep(100);
    }
    throw new Error(`timed out waiting for ${what}`);
  }

  async go(url, theme) {
    // The theme lives in localStorage, so it is set on the origin before the app reads it.
    if (theme !== this.theme) {
      await this.send("Page.navigate", { url: `${new URL(url).origin}/api/health` });
      await this.until("document.readyState === 'complete'");
      await this.eval(`localStorage.setItem('cereyan-theme', '${theme}'); true`);
      this.theme = theme;
    }
    await this.send("Page.navigate", { url });
    await this.until("document.querySelector('[data-testid=shell]')", "the app shell");
  }

  async capture(file) {
    const name = path.basename(file);
    await sleep(900); // let charts, fonts, and popovers settle
    const { data } = await this.send("Page.captureScreenshot", {
      format: "png",
      clip: { x: 0, y: 0, width: WIDTH, height: HEIGHT, scale: 1 },
    });
    writeFileSync(file, Buffer.from(data, "base64"));
    const size = statSync(file).size;
    const note = size > MAX_BYTES ? `  (over ${MAX_BYTES / 1000} KB)` : "";
    console.log(`wrote ${name} ${Math.round(size / 1000)} KB${note}`);
  }

  async close() {
    try {
      this.ws?.close();
    } catch {}
    if (this.proc && this.proc.exitCode === null) {
      const exited = new Promise((r) => this.proc.once("exit", r));
      this.proc.kill();
      await Promise.race([exited, sleep(5000)]);
    }
    if (this.profile) rmSync(this.profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
}

// ---------------------------------------------------------------- the images

const tab = (label) => `[...document.querySelectorAll('[role=tab]')].find((e) => e.textContent.trim().startsWith(${JSON.stringify(label)}))`;
const button = (label) => `[...document.querySelectorAll('button')].find((b) => b.textContent.trim().startsWith(${JSON.stringify(label)}))`;
const rows = (n) => `document.querySelectorAll('main tbody tr').length >= ${n}`;
const press = (el) =>
  `(${el}).dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, button: 0, pointerType: 'mouse' }))`;

function images({ base, flows, etl }) {
  const runPage = `${base}/runs/${etl.id}`;
  const flowPage = `${base}/flows/${flows.daily_etl.id}`;
  return {
    dashboard: async (b) => {
      await b.go(`${base}/`, "light");
      await b.until("document.querySelector('[data-testid=recently-completed] tbody tr')", "recently completed runs");
    },
    "dashboard-dark": async (b) => {
      await b.go(`${base}/`, "dark");
      await b.until("document.querySelector('[data-testid=recently-completed] tbody tr')", "recently completed runs");
    },
    runs: async (b) => {
      await b.go(`${base}/runs`, "light");
      const header = (key) => `document.querySelector('[data-testid="project-${key}"] button[aria-expanded]')`;
      await b.until(header("examples"), "the examples section");
      // Open the example project's section, whose groups show the nesting, and fold the rest.
      await b.eval(`[...document.querySelectorAll('[data-testid^="project-"] button[aria-expanded="true"]')].forEach((e) => e.click()); true`);
      await b.eval(`${header("examples")}.click(); true`);
      await b.until(`${header("examples")}.getAttribute('aria-expanded') === 'true'`, "the open section");
    },
    "run-detail": async (b) => {
      await b.go(runPage, "light");
      await b.until("document.querySelector('[data-testid=log-list] tr')", "log lines");
    },
    "run-detail-dark": async (b) => {
      await b.go(runPage, "dark");
      await b.until("document.querySelector('[data-testid=log-list] tr')", "log lines");
    },
    "run-timeline": async (b) => {
      await b.go(runPage, "light");
      await b.until(tab("Timeline"), "the Timeline tab");
      await b.eval(`${press(tab("Timeline"))}; ${tab("Timeline")}.click(); true`);
      await b.until("document.querySelector('main svg rect')", "the timeline");
    },
    flows: async (b) => {
      await b.go(`${base}/flows`, "light");
      const trigger = `document.querySelector('[aria-label="More actions for daily_etl"]')`;
      await b.until(trigger, "the daily_etl row menu");
      await b.eval(`${trigger}.scrollIntoView({ block: 'center' }); true`);
      await sleep(300);
      await b.eval(`${press(trigger)}; true`);
      await b.until(`[...document.querySelectorAll('[role=menuitem]')].some((e) => e.textContent.includes('Skip next run'))`, "the row menu");
    },
    "flow-detail": async (b) => {
      await b.go(flowPage, "light");
      await b.until(tab("Upcoming"), "the Upcoming tab");
      await b.eval(`${press(tab("Upcoming"))}; ${tab("Upcoming")}.click(); true`);
      await b.until("document.querySelector('[data-testid=look-ahead-divider]')", "projected fires");
    },
    "skip-dialog": async (b) => {
      await b.go(flowPage, "light");
      await b.until(button("Skip next"), "Skip next");
      await b.eval(`${button("Skip next")}.click(); true`);
      await b.until("document.querySelectorAll('[data-testid=skip-fire]').length > 3", "skippable fires");
      for (let i = 0; i < 2; i++) {
        await b.eval(`document.querySelector('[aria-label="More"]').click(); true`);
        await sleep(150);
      }
    },
    "reschedule-dialog": async (b) => {
      await b.go(flowPage, "light");
      await b.until(button("Reschedule"), "Reschedule");
      await b.eval(`${button("Reschedule")}.click(); true`);
      await b.until("document.querySelector('[data-testid=reschedule-week]')", "the week preview");
      await b.eval(`(() => {
        const input = document.querySelector('input[aria-label="Time"]');
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, '07:30');
        input.dispatchEvent(new Event('input', { bubbles: true }));
        return true;
      })()`);
    },
    events: async (b) => {
      await b.go(`${base}/events`, "light");
      const event = `[...document.querySelectorAll('[data-event-name]')].find((e) => e.dataset.eventName === 'run.failed') ?? document.querySelector('[data-event-name]')`;
      await b.until(event, "events");
      await b.eval(`(${event}).click(); true`);
      await b.until("document.querySelector('[data-testid=event-drawer]')", "the event panel");
    },
    artifacts: async (b) => {
      await b.go(`${base}/artifacts`, "light");
      await b.until("document.querySelectorAll('[data-testid^=artifact-item-]').length >= 4", "artifacts");
    },
    rules: async (b) => {
      await b.go(`${base}/rules`, "light");
      await b.until(rows(3), "rule rows");
    },
    variables: async (b) => {
      await b.go(`${base}/variables`, "light");
      await b.until(rows(2), "variable rows");
    },
    settings: async (b) => {
      await b.go(`${base}/settings`, "light");
      await b.until("document.querySelector('main').innerText.includes('Engines')", "the engine pool");
    },
    queue: async (b) => {
      // Last, so its runs for later appear on no other page: three runs join the line soon.
      for (const [name, minutes] of [["etl", 4], ["daily_etl", 18], ["sales", 42]]) {
        await fetch(`${base}/api/flows/${flows[name].id}/runs`, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ parameters: { day: DAY }, delay: minutes * 60 }),
        });
      }
      await b.go(`${base}/queue`, "light");
      await b.until("document.querySelectorAll('[data-testid=processor-tile]').length >= 4", "the processors");
      await b.until("document.querySelector('main').innerText.includes('sales')", "the runs joining the line");
    },
    worker: async (b) => {
      // Its own server and worker; stopped once every image is written.
      const wd = new WorkerDemo(await freePort(), await freePort());
      cleanups.push(() => wd.stop());
      await wd.start();
      await wd.seed();
      // The worker is its own origin: set the theme there, as `go` does for the server.
      await b.send("Page.navigate", { url: `${wd.status}/status.json` });
      await b.until("document.readyState === 'complete'");
      await b.eval(`localStorage.setItem('cereyan-theme', 'light'); true`);
      b.theme = undefined;
      await b.send("Page.navigate", { url: `${wd.status}/` });
      await b.until("document.querySelectorAll('[data-testid=flow-row]').length >= 2", "the worker's flows");
      await b.until("document.querySelector('[data-testid=connection]').innerText.includes('connected')", "the connection");
    },
  };
}

const cleanups = [];

// ---------------------------------------------------------------- main

const demo = new Demo(await freePort());
const browser = new Browser();
let failed = false;
try {
  await demo.ready();
  console.log(`seeding ${demo.base}`);
  const seeded = await seed(demo);
  const all = images({ base: demo.base, ...seeded });
  const unknown = [...only].filter((n) => !(n in all));
  if (unknown.length) throw new Error(`unknown image: ${unknown.join(", ")}; known: ${Object.keys(all).join(", ")}`);
  await browser.open();
  for (const [name, prepare] of Object.entries(all)) {
    if (only.size && !only.has(name)) continue;
    try {
      await prepare(browser);
      await browser.capture(path.join(OUT, `${name}.png`));
    } catch (err) {
      failed = true;
      console.error(`${name}: ${err.message}`);
    }
  }
} finally {
  try {
    await browser.close();
  } finally {
    for (const stop of cleanups) stop();
    demo.stop();
  }
}
process.exit(failed ? 1 : 0);
