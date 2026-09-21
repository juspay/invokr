#!/usr/bin/env node
// Demo host for the Invokr leadership demo.
//
// Three jobs, none of which a static page could do on its own:
//
//   1. Serve the demo page and the deck.
//   2. Proxy the Invokr API same-origin, injecting the API key and tenant
//      headers, so no credential is ever in the page.
//   3. Own the worker process, so the pick-up take can actually kill it
//      (SIGKILL, not a graceful stop — the point is that an aborted
//      transaction rolls back and the job still fires) and bring it back.
//
// Zero dependencies on purpose: `node demo/server.mjs` is the whole thing.

import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { readFile, writeFile, readdir, mkdir } from "node:fs/promises";
import { existsSync } from "node:fs";
import { connect } from "node:net";
import { join, dirname, extname, normalize } from "node:path";
import { fileURLToPath } from "node:url";
import { bootstrap } from "./bootstrap.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = join(HERE, "..");
const PUBLIC = join(HERE, "public");
const RECORDINGS = join(HERE, "recordings");

const PORT = Number(process.env.INVOKR_DEMO_PORT ?? 4173);
const API_URL = (process.env.INVOKR_URL ?? "http://localhost:8080").replace(/\/$/, "");
const MOCK_URL = (process.env.INVOKR_MOCK_URL ?? "http://localhost:9999").replace(/\/$/, "");
const API_KEY = process.env.INVOKR_API_KEY ?? "dev-api-key";
const DASHBOARD_URL = process.env.INVOKR_DEMO_DASHBOARD_URL ?? "";
const WORKER_FEATURES = process.env.INVOKR_DEMO_WORKER_FEATURES ?? "";
const AUTO_WORKER = process.env.INVOKR_DEMO_MANAGE_WORKER !== "0";

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".woff2": "font/woff2",
};

let provisioned = null; // filled by bootstrap

// A small pool, because one worker cannot show contention. Each entry is
// { child, pid, workerId, startedAt }. `workerId` is scraped from the worker's
// own startup log so the page can put a claimed job into the box belonging to
// the worker that actually claimed it — executions carry that same id.
const workers = [];
const MAX_WORKERS = 3;
let lastExit = null;

// ─── worker lifecycle ────────────────────────────────────────────────────────

function workerCommand() {
  if (process.env.INVOKR_DEMO_WORKER_CMD) {
    const parts = process.env.INVOKR_DEMO_WORKER_CMD.split(" ");
    return { cmd: parts[0], args: parts.slice(1) };
  }
  for (const profile of ["release", "debug"]) {
    const bin = join(REPO, "target", profile, "invokr-worker");
    if (existsSync(bin)) return { cmd: bin, args: [] };
  }
  const args = ["run", "-p", "invokr-worker"];
  if (WORKER_FEATURES) args.push("--features", WORKER_FEATURES);
  return { cmd: "cargo", args };
}

function startWorker() {
  if (workers.length >= MAX_WORKERS) {
    return { ok: false, error: `already running ${workers.length} workers` };
  }
  const { cmd, args } = workerCommand();
  // Each worker needs its own metrics port, or the second one dies on bind.
  const metricsPort = String(Number(process.env.INVOKR_METRICS_PORT ?? 9090) + workers.length);
  const child = spawn(cmd, args, {
    cwd: REPO,
    env: {
      // A worker's pool defaults to 50 connections. Three of those plus the API
      // is more than a stock PostgreSQL (max_connections = 100) will give out,
      // and the first thing to fail is pg_cron — which the poll take needs. The
      // demo needs a handful of connections, so ask for a handful.
      INVOKR_DB_POOL_SIZE: "8",
      INVOKR_WORKER_MAX_CONCURRENT: "4",
      ...process.env,
      INVOKR_METRICS_PORT: metricsPort,
    },
    stdio: ["ignore", "pipe", "pipe"],
  });

  const entry = { child, pid: child.pid, workerId: null, startedAt: Date.now() };
  workers.push(entry);

  const read = (buf) => {
    const text = buf.toString();
    // "Worker polling started" carries the id this process will stamp on every
    // execution it claims.
    const match = text.match(/"worker_id":"(worker_[0-9a-f]+)"/);
    if (match) {
      entry.workerId = match[1];
      console.log(`[worker ${entry.pid}] ${match[1]}`);
    }
  };
  child.stdout.on("data", read);
  child.stderr.on("data", read);
  child.on("exit", (code, signal) => {
    const i = workers.indexOf(entry);
    if (i >= 0) workers.splice(i, 1);
    lastExit = { pid: entry.pid, code, signal, at: Date.now() };
    console.log(`[worker ${entry.pid}] exited code=${code} signal=${signal}`);
  });

  return { ok: true, pid: child.pid };
}

// SIGKILL, deliberately. A graceful stop drains in-flight work and proves
// nothing; SIGKILL aborts the transaction holding the claim, which is the
// behaviour the pick-up take is about. With no pid it kills the newest worker.
function killWorker(pid) {
  const entry = pid ? workers.find((w) => String(w.pid) === String(pid)) : workers[workers.length - 1];
  if (!entry) return { ok: false, error: "no such worker is running" };
  entry.child.kill("SIGKILL");
  const i = workers.indexOf(entry);
  if (i >= 0) workers.splice(i, 1);
  return { ok: true, pid: entry.pid, workerId: entry.workerId, signal: "SIGKILL" };
}

const workerSnapshot = () =>
  workers.map((w) => ({ pid: w.pid, workerId: w.workerId, startedAt: w.startedAt }));

// ─── probes ──────────────────────────────────────────────────────────────────

function tcpProbe(host, port, timeout = 350) {
  return new Promise((resolve) => {
    const socket = connect({ host, port });
    const done = (result) => {
      socket.destroy();
      resolve(result);
    };
    socket.setTimeout(timeout);
    socket.once("connect", () => done(true));
    socket.once("timeout", () => done(false));
    socket.once("error", () => done(false));
  });
}

async function httpProbe(url, timeout = 700) {
  try {
    const res = await fetch(url, { signal: AbortSignal.timeout(timeout) });
    return res.ok;
  } catch {
    return false;
  }
}

// ─── http plumbing ───────────────────────────────────────────────────────────

function send(res, status, body, type = "application/json; charset=utf-8") {
  const payload = typeof body === "string" || Buffer.isBuffer(body) ? body : JSON.stringify(body);
  res.writeHead(status, {
    "Content-Type": type,
    "Cache-Control": "no-store",
  });
  res.end(payload);
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => resolve(Buffer.concat(chunks)));
    req.on("error", reject);
  });
}

async function serveStatic(res, urlPath) {
  const rel = urlPath === "/" ? "index.html" : urlPath.replace(/^\/+/, "");
  const target = normalize(join(PUBLIC, rel));
  if (!target.startsWith(PUBLIC)) return send(res, 403, { error: "forbidden" });
  try {
    const file = await readFile(target);
    send(res, 200, file, MIME[extname(target)] ?? "application/octet-stream");
  } catch {
    send(res, 404, { error: `not found: ${rel}` });
  }
}

// Forwards to the real Invokr API with credentials attached here rather than in
// the page. `?ws=a|b` picks the tenant; the two-teams take uses both.
async function proxyApi(req, res, url) {
  const wsKey = url.searchParams.get("ws") ?? "a";
  const ws = provisioned?.workspaces?.[wsKey];
  url.searchParams.delete("ws");

  const target = `${API_URL}${url.pathname.replace(/^\/api/, "")}${url.search}`;
  const headers = { Authorization: `Bearer ${API_KEY}` };
  if (req.headers["content-type"]) headers["Content-Type"] = req.headers["content-type"];
  if (provisioned?.org_id) headers["X-Org-Id"] = provisioned.org_id;
  if (ws) headers["X-Workspace-Id"] = ws.workspace_id;

  const body = ["GET", "HEAD"].includes(req.method) ? undefined : await readBody(req);

  try {
    const upstream = await fetch(target, {
      method: req.method,
      headers,
      body: body?.length ? body : undefined,
      signal: AbortSignal.timeout(15000),
    });
    const text = await upstream.text();
    send(res, upstream.status, text, upstream.headers.get("content-type") ?? "application/json");
  } catch (err) {
    // The page turns this into a visible "API unreachable" state rather than a
    // silent stall — better to say so than to spin in front of a room.
    send(res, 502, { error: "upstream unreachable", detail: String(err), target });
  }
}

async function handleControl(req, res, url) {
  const path = url.pathname.replace(/^\/control/, "");

  if (path === "/status") {
    const [api, mock, kafka, redis] = await Promise.all([
      httpProbe(`${API_URL}/health`),
      httpProbe(`${MOCK_URL}/health`),
      tcpProbe("127.0.0.1", 9092),
      tcpProbe("127.0.0.1", 6379),
    ]);
    let recordings = [];
    try {
      recordings = (await readdir(RECORDINGS)).filter((f) => f.endsWith(".json")).map((f) => f.slice(0, -5));
    } catch {}
    return send(res, 200, {
      api,
      mock,
      workers: workerSnapshot(),
      maxWorkers: MAX_WORKERS,
      lastExit,
      transports: { kafka, redis, features: WORKER_FEATURES },
      provisioned,
      longRunning: Boolean(provisioned?.longRunning),
      // One definition of the hero endpoint, shared with the page.
      mandateSpec: provisioned?.mandateSpec ?? null,
      setup: provisioned?.setup ?? null,
      dashboardUrl: DASHBOARD_URL,
      apiUrl: API_URL,
      mockUrl: MOCK_URL,
      recordings,
    });
  }

  if (path === "/worker/kill" && req.method === "POST") {
    return send(res, 200, killWorker(url.searchParams.get("pid")));
  }
  if (path === "/worker/start" && req.method === "POST") return send(res, 200, startWorker());

  if (path === "/mock/reset" && req.method === "POST") {
    try {
      await fetch(`${MOCK_URL}/flaky/reset`, { method: "POST", signal: AbortSignal.timeout(2000) });
      return send(res, 200, { ok: true });
    } catch (err) {
      return send(res, 502, { ok: false, error: String(err) });
    }
  }

  // The receiving end's own account of what it did. The page shows this next to
  // Invokr's account of what it delivered, so the room is not taking either
  // side's word for it.
  if (path === "/mock/log" && req.method === "GET") {
    try {
      const upstream = await fetch(`${MOCK_URL}/_log${url.search}`, {
        signal: AbortSignal.timeout(3000),
      });
      return send(res, upstream.status, await upstream.text());
    } catch (err) {
      return send(res, 502, { error: "target unreachable", detail: String(err) });
    }
  }

  // Aarokya remembers how many times each mandate has been asked about, so a
  // rehearsal of the same mandate would start halfway through its own story.
  if (path === "/mock/reset" && req.method === "POST") {
    try {
      await fetch(`${MOCK_URL}/_polls/reset`, { method: "POST", signal: AbortSignal.timeout(2000) });
      return send(res, 200, { ok: true });
    } catch (err) {
      return send(res, 502, { ok: false, error: String(err) });
    }
  }

  if (path === "/mock/log/clear" && req.method === "POST") {
    try {
      await fetch(`${MOCK_URL}/_log/clear`, { method: "POST", signal: AbortSignal.timeout(2000) });
      return send(res, 200, { ok: true });
    } catch (err) {
      return send(res, 502, { ok: false, error: String(err) });
    }
  }

  if (path === "/bootstrap" && req.method === "POST") {
    try {
      provisioned = await bootstrap({ baseUrl: API_URL, apiKey: API_KEY, mockUrl: MOCK_URL });
      return send(res, 200, { ok: true, provisioned });
    } catch (err) {
      return send(res, 500, { ok: false, error: String(err.message ?? err) });
    }
  }

  const rec = path.match(/^\/recordings\/([a-z0-9-]{1,40})$/);
  if (rec) {
    const file = join(RECORDINGS, `${rec[1]}.json`);
    if (req.method === "GET") {
      try {
        return send(res, 200, await readFile(file), "application/json; charset=utf-8");
      } catch {
        return send(res, 404, { error: "no recording for this one yet" });
      }
    }
    if (req.method === "PUT") {
      const body = await readBody(req);
      try {
        JSON.parse(body.toString()); // refuse to persist anything unreadable
      } catch {
        return send(res, 400, { error: "body must be JSON" });
      }
      await mkdir(RECORDINGS, { recursive: true });
      await writeFile(file, body);
      console.log(`[rec] saved ${rec[1]}.json (${body.length} bytes)`);
      return send(res, 200, { ok: true, scene: rec[1], bytes: body.length });
    }
  }

  send(res, 404, { error: `unknown control route: ${path}` });
}

const server = createServer(async (req, res) => {
  const url = new URL(req.url, `http://localhost:${PORT}`);
  try {
    if (url.pathname.startsWith("/api/")) return await proxyApi(req, res, url);
    if (url.pathname.startsWith("/control")) return await handleControl(req, res, url);
    if (url.pathname === "/deck") return await serveStatic(res, "/deck.html");
    return await serveStatic(res, url.pathname);
  } catch (err) {
    console.error(err);
    send(res, 500, { error: String(err) });
  }
});

// ─── startup ─────────────────────────────────────────────────────────────────

const shutdown = () => {
  for (const w of workers) w.child.kill("SIGTERM");
  process.exit(0);
};
process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);

server.listen(PORT, async () => {
  console.log(`\n  Invokr demo`);
  console.log(`  demo   http://localhost:${PORT}`);
  console.log(`  deck   http://localhost:${PORT}/deck`);
  console.log(`  api    ${API_URL}`);

  try {
    provisioned = await bootstrap({ baseUrl: API_URL, apiKey: API_KEY, mockUrl: MOCK_URL });
    console.log(`  tenants ${Object.values(provisioned.workspaces).map((w) => w.slug).join(", ")}`);
  } catch (err) {
    // Not fatal: replay mode needs none of this, and the page shows the reason.
    console.error(`  ! provisioning failed: ${err.message ?? err}`);
    console.error(`    live takes will not run until Invokr is up; replay still works.`);
  }

  if (AUTO_WORKER) {
    const { cmd } = workerCommand();
    // Two, so the stage can show one worker taking a job and the other not.
    startWorker();
    startWorker();
    console.log(`  workers 2 × ${cmd}${WORKER_FEATURES ? ` (features: ${WORKER_FEATURES})` : ""}`);
  }
  console.log("");
});
