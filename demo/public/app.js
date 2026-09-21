// Invokr live demo.
//
// The stage is the explanation. A job is a physical token: you describe an
// endpoint, it becomes a row; you ask for a run, that becomes another row; it
// waits until it is due, exactly one worker takes it, it reaches the other
// side, and the try is recorded.
//
// Two rules hold it together:
//
//   1. A scene's `run()` never touches the DOM. It emits steps, and the
//      renderer draws them — which is what lets a recorded run replay through
//      the same code.
//   2. Steps are drawn one at a time with a dwell between them, so the eye can
//      follow. The dwell is a *floor*, never a substitute: a 15-second wait
//      still takes fifteen seconds, and every step carries the real time it
//      happened at.

const $ = (sel, root = document) => root.querySelector(sel);

const state = {
  status: null,
  current: 0,
  replay: false,
  recording: true,
  ran: new Set(),
  run: null,
  values: {},
  registered: false, // whether this session has already run the register phase
};

const esc = (s) =>
  String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

const ms = (n) => (n == null ? "—" : n < 1000 ? `${Math.round(n)}ms` : `${(n / 1000).toFixed(1)}s`);
const clock = (t) => new Date(t).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
const short = (id) => (id ? String(id).replace("worker_", "").slice(0, 6) : "—");
const sleep = (t) => new Promise((r) => setTimeout(r, t));

const maskSecrets = (text) =>
  String(text).replace(/(\\?"authorization\\?"\s*:\s*\\?")([^"\\]*)/gi, "$1Bearer ••••••••");

function highlightJson(value) {
  const text = maskSecrets(typeof value === "string" ? value : JSON.stringify(value, null, 2));
  return esc(text)
    .replace(/\{\{input\.[^}]+\}\}/g, (m) => `<span class="tok-input">${m}</span>`)
    .replace(/\{\{config\.[^}]+\}\}/g, (m) => `<span class="tok-config">${m}</span>`)
    .replace(/\{\{secret\.[^}]+\}\}/g, (m) => `<span class="tok-secret">${m}</span>`)
    .replace(/\{\{execution\.[^}]+\}\}/g, (m) => `<span class="tok-exec">${m}</span>`);
}

// ─── the board ───────────────────────────────────────────────────────────────

const BOARD = { w: 1420, h: 620 };
const TOKEN = { w: 148, h: 34, gap: 8 };

const ZONES = {
  app: { x: 14, y: 54, w: 190, h: 530, pad: 14, top: 32, cols: 1 },
  db: { x: 238, y: 54, w: 424, h: 530 },
  endpoints: { x: 260, y: 92, w: 380, h: 116, pad: 12, top: 28, cols: 2 },
  waiting: { x: 260, y: 232, w: 380, h: 160, pad: 12, top: 28, cols: 2 },
  ready: { x: 260, y: 412, w: 380, h: 154, pad: 12, top: 28, cols: 2 },
  workers: { x: 700, y: 54, w: 312, h: 530 },
  target: { x: 1048, y: 54, w: 250, h: 530, pad: 14, top: 32, cols: 1 },
  done: { x: 1306, y: 54, w: 110, h: 530, pad: 7, top: 30, cols: 1 },
};

const workerRect = (i) => ({ x: 718, y: 92 + i * 142, w: 276, h: 122, pad: 12, top: 30, cols: 1 });

function place(el, r) {
  el.style.left = `${r.x}px`;
  el.style.top = `${r.y}px`;
  el.style.width = `${r.w}px`;
  el.style.height = `${r.h}px`;
}

function layoutBoard() {
  place($("#z-app"), ZONES.app);
  place($("#z-db"), ZONES.db);
  place($("#z-endpoints"), ZONES.endpoints);
  place($("#z-waiting"), ZONES.waiting);
  place($("#z-ready"), ZONES.ready);
  place($("#z-workers"), ZONES.workers);
  place($("#z-target"), ZONES.target);
  place($("#z-done"), ZONES.done);

  const arrow = $("#drop-arrow");
  arrow.style.left = `${ZONES.waiting.x + ZONES.waiting.w / 2 - 6}px`;
  arrow.style.top = `${ZONES.waiting.y + ZONES.waiting.h + 2}px`;
}

function fitBoard() {
  const stage = $(".stage");
  const board = $("#board");
  const k = Math.min((stage.clientWidth - 28) / BOARD.w, (stage.clientHeight - 16) / BOARD.h, 1.15);
  board.style.transform = `scale(${Math.max(0.3, k)})`;
}

// ─── workers on the board ────────────────────────────────────────────────────

const workerBoxes = new Map(); // workerId -> element

function renderWorkers(list) {
  const host = $("#workers");
  const wanted = new Map((list ?? []).filter((w) => w.workerId).map((w) => [w.workerId, w]));

  for (const [id, el] of workerBoxes) {
    if (!wanted.has(id)) {
      el.remove();
      workerBoxes.delete(id);
    }
  }
  $("#z-workers").classList.toggle("vacant", wanted.size === 0);

  let i = 0;
  for (const [id] of wanted) {
    let el = workerBoxes.get(id);
    if (!el) {
      el = document.createElement("div");
      el.className = "worker";
      el.innerHTML = `<span class="who"></span><span class="state"></span>`;
      host.appendChild(el);
      workerBoxes.set(id, el);
    }
    el.querySelector(".who").textContent = short(id);
    el.dataset.index = String(i);
    place(el, workerRect(i));
    i++;
  }
  relayoutTokens();
}

function ensureWorkerBox(workerId) {
  if (!workerId || workerBoxes.has(workerId)) return;
  const list = [...workerBoxes.keys()].map((id) => ({ workerId: id }));
  list.push({ workerId });
  renderWorkers(list);
}

function setWorkerState(workerId, cls, label) {
  const el = workerBoxes.get(workerId);
  if (!el) return;
  el.classList.remove("busy", "skipped", "dead");
  if (cls) el.classList.add(cls);
  el.querySelector(".state").textContent = label ?? "";
}

// ─── tokens ──────────────────────────────────────────────────────────────────

const tokens = new Map();
let tokenSeq = 0;

function tokenRect(zone) {
  if (zone.startsWith("worker:")) {
    const el = workerBoxes.get(zone.slice(7));
    return el ? workerRect(Number(el.dataset.index)) : ZONES.ready;
  }
  if (zone === "workers") {
    const below = workerRect(Math.max(workerBoxes.size, 1));
    return { ...ZONES.workers, y: below.y - 14, top: 0, pad: 18, cols: 1 };
  }
  return ZONES[zone] ?? ZONES.ready;
}

function relayoutTokens() {
  const byZone = new Map();
  for (const [id, t] of tokens) {
    if (!byZone.has(t.zone)) byZone.set(t.zone, []);
    byZone.get(t.zone).push([id, t]);
  }
  for (const [zone, list] of byZone) {
    const r = tokenRect(zone);
    const cols = r.cols ?? 1;
    list.sort((a, b) => a[1].order - b[1].order);
    list.forEach(([, t], i) => {
      const x = r.x + (r.pad ?? 12) + (i % cols) * (TOKEN.w + TOKEN.gap);
      const y = r.y + (r.top ?? 26) + Math.floor(i / cols) * (TOKEN.h + TOKEN.gap);
      t.el.style.transform = `translate3d(${x}px, ${y}px, 0)`;
    });
  }
}

function setToken({ id, zone, state: tokenState, label, note }) {
  let t = tokens.get(id);
  if (!t) {
    const el = document.createElement("div");
    el.className = "token born";
    el.innerHTML = `<span class="who"></span><span class="note"></span>`;
    $("#tokens").appendChild(el);
    t = { el, zone: zone ?? "app", order: tokenSeq++ };
    tokens.set(id, t);
    relayoutTokens();
    requestAnimationFrame(() => el.classList.remove("born"));
  }
  if (label !== undefined) t.el.querySelector(".who").textContent = label;
  if (note !== undefined) t.el.querySelector(".note").textContent = note;
  if (tokenState !== undefined) {
    t.el.classList.remove("waiting", "ready", "running", "ok", "bad", "spec", "polling");
    if (tokenState) t.el.classList.add(tokenState);
  }
  if (zone && zone !== t.zone) {
    if (zone.startsWith("worker:")) ensureWorkerBox(zone.slice(7));
    t.zone = zone;
    t.el.classList.toggle("shrink", zone === "done");
    relayoutTokens();
  }
}

function clearTokens() {
  for (const [, t] of tokens) t.el.remove();
  tokens.clear();
  tokenSeq = 0;
}

function flashZone(cls) {
  const el = $("#z-target");
  el.classList.remove("hit", "refused");
  if (cls) {
    el.classList.add(cls);
    setTimeout(() => el.classList.remove(cls), 1200);
  }
}

// ─── the journey strip ───────────────────────────────────────────────────────

const REGISTER_PHASE = {
  phase: "register",
  steps: [
    { id: "describe", label: "you describe it" },
    { id: "stored", label: "it becomes a row" },
    { id: "secret", label: "the key stays in" },
  ],
};

const INVOKE_STEPS = [
  { id: "ask", label: "you ask for a run" },
  { id: "written", label: "written down" },
  { id: "due", label: "waits until due" },
  { id: "claim", label: "one worker takes it" },
  { id: "call", label: "calls the other side" },
  { id: "answer", label: "the answer" },
  { id: "record", label: "recorded" },
];

const DEFAULT_JOURNEY = [REGISTER_PHASE, { phase: "invoke", steps: INVOKE_STEPS }];

function renderJourney(scene) {
  const journey = scene.journey ?? DEFAULT_JOURNEY;
  $("#journey").innerHTML = journey
    .map(
      (p) => `<div class="phase">
        <span class="phase-name">${esc(p.phase)}</span>
        ${p.steps
          .map(
            (s, i) =>
              `<span class="step" data-step="${esc(s.id)}">
                 <span class="dot">${i + 1}</span><span>${esc(s.label)}</span><span class="at"></span>
               </span>`,
          )
          .join("")}
      </div>`,
    )
    .join("");
}

/// Write a step's time, never earlier than the step before it.
///
/// Two clocks feed this strip: steps the page stamps as it walks, and steps
/// retimed from a row's own timestamp on the database's clock. They agree to
/// within a few milliseconds, which is enough — when a scene walks the journey
/// twice — to render a claim a hair before the write that produced it. Nothing
/// is invented: a step is only ever nudged up to the one it followed.
function showStepTime(el, at) {
  const prior = [...$("#journey").querySelectorAll(".step")];
  const before = prior
    .slice(0, prior.indexOf(el))
    .reverse()
    .find((s) => s.dataset.at !== undefined);
  const shown = Math.max(at, before ? Number(before.dataset.at) : 0);
  el.dataset.at = String(shown);
  el.querySelector(".at").textContent = shown < 1000 ? `${Math.round(shown)}ms` : `${(shown / 1000).toFixed(1)}s`;
}

function markStep(id, cls, at) {
  const strip = $("#journey");
  strip.querySelectorAll(".step.now").forEach((n) => {
    n.classList.remove("now");
    n.classList.add("done");
  });
  const el = strip.querySelector(`[data-step="${id}"]`);
  if (!el) return;
  el.classList.remove("skipped", "bad");
  el.classList.add(cls ?? "now");
  if (at != null) showStepTime(el, at);
  el.scrollIntoView({ block: "nearest", inline: "nearest", behavior: "smooth" });
}

/// A finished run has no current step. Settle the trailing one so the strip
/// stops pulsing at a journey that is over. A run that stopped early keeps its
/// pulse — that is where it stopped.
function settleJourney() {
  $("#journey")
    ?.querySelectorAll(".step.now")
    .forEach((n) => {
      n.classList.remove("now");
      n.classList.add("done");
    });
}

// ─── transport ───────────────────────────────────────────────────────────────

async function api(method, path, { body, ws = "a" } = {}) {
  const sep = path.includes("?") ? "&" : "?";
  const res = await fetch(`/api${path}${sep}ws=${ws}`, {
    method,
    headers: body === undefined ? {} : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let parsed = null;
  try {
    parsed = text ? JSON.parse(text) : null;
  } catch {
    parsed = { raw: text };
  }
  return { status: res.status, ok: res.ok, body: parsed };
}

const control = async (path, method = "POST") => {
  const res = await fetch(`/control${path}`, { method });
  return { ok: res.ok, body: await res.json().catch(() => null) };
};

async function targetLogHead() {
  try {
    const res = await fetch("/control/mock/log?limit=1");
    return ((await res.json())?.data ?? [])[0]?.seq ?? 0;
  } catch {
    return 0;
  }
}

// ─── the pacer ───────────────────────────────────────────────────────────────
//
// Steps are drawn no closer together than `dwell`. Anything that took longer
// than that in real life keeps its own timing — the floor only stretches the
// bursts the eye would otherwise miss.

const pacer = {
  dwell: 1400,
  queue: [],
  running: false,
  lastStep: 0,
  waiters: [],
};

function enqueue(scene, ev) {
  pacer.queue.push([scene, ev]);
  if (!pacer.running) drainLoop();
}

async function drainLoop() {
  pacer.running = true;
  while (pacer.queue.length) {
    const [scene, ev] = pacer.queue.shift();
    if (ev.type === "step") {
      const earliest = pacer.lastStep + pacer.dwell;
      const now = performance.now();
      if (now < earliest) await sleep(earliest - now);
      pacer.lastStep = performance.now();
    }
    applyNow(scene, ev);
  }
  pacer.running = false;
  pacer.waiters.splice(0).forEach((r) => r());
}

const drained = () =>
  pacer.running || pacer.queue.length ? new Promise((r) => pacer.waiters.push(r)) : Promise.resolve();

function resetPacer() {
  pacer.queue.length = 0;
  pacer.lastStep = 0;
  pacer.waiters.splice(0).forEach((r) => r());
}

// ─── the event tape ──────────────────────────────────────────────────────────

class Run {
  constructor(scene, { replay = false } = {}) {
    this.scene = scene;
    this.replay = replay;
    this.t0 = performance.now();
    this.events = [];
    this.cancelled = false;
  }

  emit(type, payload = {}) {
    if (this.cancelled) return;
    const ev = { t: Math.round(performance.now() - this.t0), type, ...payload };
    this.events.push(ev);
    enqueue(this.scene, ev);
  }

  /// One step of the journey: what it is, when it really happened, and what
  /// moves on the board because of it.
  step(id, opts = {}) {
    this.emit("step", { id, at: Math.round(performance.now() - this.t0), ...opts });
  }

  tick(text, tone = "") {
    this.emit("tick", { text, tone });
  }

  async save(ok) {
    if (!ok || this.replay || !state.recording) return;
    try {
      await fetch(`/control/recordings/${this.scene.id}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: maskSecrets(
          JSON.stringify({ scene: this.scene.id, recorded_at: new Date().toISOString(), events: this.events }),
        ),
      });
      const known = state.status?.recordings;
      if (known && !known.includes(this.scene.id)) known.push(this.scene.id);
    } catch {
      /* a failed save must never take the demo down */
    }
  }
}

// ─── the one renderer ────────────────────────────────────────────────────────

function applyNow(scene, ev) {
  switch (ev.type) {
    case "step":
      markStep(ev.id, ev.skip ? "skipped" : ev.bad ? "bad" : "now", ev.skip ? null : ev.at);
      if (ev.token) setToken(ev.token);
      if (ev.worker) setWorkerState(ev.worker.id, ev.worker.cls, ev.worker.label);
      if (ev.others) {
        for (const id of workerBoxes.keys()) {
          if (id !== ev.worker?.id) setWorkerState(id, ev.others.cls, ev.others.label);
        }
      }
      if (ev.flash) flashZone(ev.flash);
      if (ev.cron !== undefined) $("#cron-badge").classList.toggle("tick", !!ev.cron);
      if (ev.drop !== undefined) $("#drop-arrow").classList.toggle("on", !!ev.drop);
      if (ev.text) showTick(ev.t, ev.text, ev.tone);
      break;

    case "retime": {
      const el = $("#journey").querySelector(`[data-step="${ev.id}"]`);
      if (el) showStepTime(el, ev.at);
      break;
    }

    case "token":
      setToken(ev);
      break;

    case "workers":
      renderWorkers(ev.list);
      break;

    case "worker":
      setWorkerState(ev.workerId, ev.cls, ev.label);
      break;

    case "target-is":
      $("#target-label").textContent = ev.who;
      break;

    case "tick":
      showTick(ev.t, ev.text, ev.tone);
      break;

    case "receipt":
      addReceipts(ev.entries);
      break;

    case "tries":
      $("#p-tries").innerHTML = triesTable(ev.attempts);
      break;

    case "req":
      $("#p-request").innerHTML = `<pre>${highlightJson(ev.body ?? {})}</pre>`;
      break;

    case "res":
      $("#p-response").innerHTML = `<pre>${highlightJson(ev.body ?? {})}</pre>`;
      break;
  }
}

function showTick(t, text, tone) {
  const el = $("#ticker");
  const stamp = t < 1000 ? `${t}ms` : `${(t / 1000).toFixed(1)}s`;
  const prev = el.querySelector(".line.now");
  if (prev) {
    prev.classList.remove("now");
    prev.classList.add("was");
  }
  el.querySelectorAll(".line.was").forEach((n, i, all) => {
    if (i < all.length - 1) n.remove();
  });
  const line = document.createElement("span");
  line.className = `line now ${tone ?? ""}`;
  line.innerHTML = `<span class="t">${stamp}</span><span>${text}</span>`;
  el.appendChild(line);
}

function addReceipts(entries) {
  const host = $("#receipts");
  if (!entries?.length) return;
  host.querySelector(".empty")?.remove();
  for (const e of entries) {
    const row = document.createElement("div");
    row.className = `receipt ${e.ok ? "" : "bad"}`;
    row.innerHTML = `<span class="mark">${e.ok ? "✓" : "!"}</span>
      <div>
        <div class="line">${esc(e.summary)}</div>
        <div class="sub">${esc(clock(e.at))} · ${e.status}${e.idempotency_key ? ` · ${esc(e.idempotency_key)}` : ""}</div>
      </div>`;
    host.appendChild(row);
  }
  host.scrollTop = host.scrollHeight;
  $("#receipts-meta").textContent = `${host.querySelectorAll(".receipt").length} received`;
}

function clearReceipts() {
  $("#receipts").innerHTML = `<span class="empty">nothing yet</span>`;
  $("#receipts-meta").textContent = "";
}

function triesTable(attempts) {
  if (!attempts?.length) return `<span class="empty">nothing yet</span>`;
  return `<table>
    <thead><tr><th>try</th><th></th><th>took</th><th>waited first</th></tr></thead>
    <tbody>${attempts
      .map(
        (a) => `<tr>
          <td class="num">${a.attempt_number}</td>
          <td>${a.status === "SUCCESS" ? '<span class="pill ok">worked</span>' : '<span class="pill bad">failed</span>'}</td>
          <td class="num">${ms(a.duration_ms)}</td>
          <td class="num">${a.gap_ms != null ? ms(a.gap_ms) : "—"}</td>
        </tr>`,
      )
      .join("")}</tbody></table>`;
}

// ─── status ──────────────────────────────────────────────────────────────────

function setChip(id, cls, text) {
  const chip = document.getElementById(id);
  if (!chip) return;
  chip.classList.remove("up", "down", "warn");
  if (cls) chip.classList.add(cls);
  chip.innerHTML = `<i></i>${esc(text)}`;
}

async function refreshStatus() {
  try {
    const s = await (await fetch("/control/status")).json();
    state.status = s;
    setChip("chip-api", s.api ? "up" : "down", s.api ? "Invokr" : "Invokr down");
    setChip("chip-target", s.mock ? "up" : "down", s.mock ? "target" : "target down");
    if (!state.run || state.run.cancelled) renderWorkers(s.workers);
    $("#foot-note").textContent =
      `${s.workers.length} worker${s.workers.length === 1 ? "" : "s"}` +
      (s.longRunning ? " · long-running build" : "");
  } catch {
    setChip("chip-api", "down", "demo server down");
  }
}

// ─── watching one real execution, step by step ───────────────────────────────

async function watchExecution(run, executionId, opts = {}) {
  const { ws = "a", timeout = 90000, targetSince = 0, token, label } = opts;
  const started = performance.now();
  let seenAttempts = 0;
  let claimedAttempt = 0;
  let announcedRetry = 0;
  let seenPolls = 0;
  let sawWaiting = false;
  let logSeq = targetSince;

  const tailTarget = async () => {
    try {
      const res = await fetch(`/control/mock/log?since=${logSeq}&limit=20`);
      const entries = ((await res.json())?.data ?? []).slice().reverse();
      if (!entries.length) return;
      logSeq = Math.max(logSeq, ...entries.map((e) => e.seq));
      run.emit("receipt", { entries });

      // A long-running destination reports each check-in. Invokr's own
      // execution row does not expose a poll count over the API, so the
      // receiving side is where the evidence is.
      for (const e of entries) {
        if (!e.path?.startsWith("/async/status") || e.status !== 202) continue;
        seenPolls += 1;
        run.emit("step", {
          id: "poll",
          at: Math.max(0, new Date(e.at) - run.wallT0),
          token: { id: token, zone: "target", state: "polling", note: `check ${seenPolls}` },
          text: `still working — Invokr has checked back <b>${seenPolls}×</b>, honouring Retry-After`,
          tone: "act",
        });
      }
    } catch {
      /* the target's log is a nicety, never a dependency */
    }
  };

  // Offsets for steps we learn about after the fact come from the row's own
  // timestamps, so the strip shows when it happened, not when we noticed.
  const offset = (iso) => Math.max(0, new Date(iso) - run.wallT0);

  while (performance.now() - started < timeout && !run.cancelled) {
    const exec = (await api("GET", `/v1/executions/${executionId}`, { ws })).body?.data;

    if (exec) {
      let pendingRetry = null;
      if (exec.status === "RETRYING" && exec.run_at && exec.attempt_count > announcedRetry) {
        announcedRetry = exec.attempt_count;
        pendingRetry = exec;
      }

      if (exec.status === "WAITING" && !sawWaiting) {
        sawWaiting = true;
        run.step("accepted", {
          token: { id: token, zone: "target", state: "polling", note: "202 · working" },
          text: "the other side said <b>202</b> and kept working — Invokr parked the row",
          tone: "act",
        });
      }

      // `at` is the attempt's own start when we already have the row, so the
      // strip never shows the pick-up as later than the call it produced.
      const claim = (n, wid, at) => {
        if (n <= claimedAttempt) return;
        claimedAttempt = n;
        run.emit("step", {
          id: "claim",
          at: at ?? Math.round(performance.now() - run.t0),
          token: { id: token, zone: wid ? `worker:${wid}` : "workers", state: "running", note: "taken" },
          worker: wid ? { id: wid, cls: "busy", label: "holding it" } : null,
          others: wid ? { cls: "skipped", label: "skipped — locked" } : null,
          text: wid
            ? `worker <b>${esc(short(wid))}</b> took it — the others skipped the locked row`
            : "<b>one worker</b> took it — the others skipped the locked row",
          tone: "act",
        });
      };
      claim(exec.attempt_count, exec.worker_id);

      // Long-running: the destination said "working on it" and Invokr is now
      // checking back rather than holding a connection open.
      if (exec.poll_count != null && exec.poll_count > seenPolls) {
        seenPolls = exec.poll_count;
        run.step("poll", {
          token: { id: token, zone: "target", state: "polling", note: `poll ${seenPolls}` },
          text: `still working — Invokr has checked back <b>${seenPolls}×</b>`,
          tone: "act",
        });
      }

      const attempts = (await api("GET", `/v1/executions/${executionId}/attempts`, { ws })).body?.data ?? [];
      if (attempts.length !== seenAttempts) {
        const ordered = [...attempts].sort((a, b) => a.attempt_number - b.attempt_number);
        for (let i = 1; i < ordered.length; i++) {
          ordered[i].gap_ms = new Date(ordered[i].started_at) - new Date(ordered[i - 1].completed_at);
        }
        for (const a of ordered.slice(seenAttempts)) {
          claim(a.attempt_number, exec.worker_id, offset(a.started_at));
          // The claim and this attempt start in the same transaction, so the
          // attempt's own timestamp is the truthful one for both.
          run.emit("retime", { id: "claim", at: offset(a.started_at) });
          // The attempt that parks a long-running job is recorded as WAITING:
          // the destination took the work, it has not finished it.
          if (a.status === "WAITING") {
            sawWaiting = true;
            run.emit("step", {
              id: "call",
              at: offset(a.started_at),
              token: { id: token, zone: "target", state: "running", note: "calling" },
              text: "the worker calls the other side",
              tone: "act",
            });
            run.emit("step", {
              id: "accepted",
              at: offset(a.completed_at ?? a.started_at),
              token: { id: token, zone: "target", state: "polling", note: "202 · working" },
              text: "the other side said <b>202</b> and kept working — Invokr parked the row",
              tone: "act",
            });
            continue;
          }
          const ok = a.status === "SUCCESS";

          run.emit("step", {
            id: "call",
            at: offset(a.started_at),
            token: { id: token, zone: "target", state: "running", note: "calling" },
            text: "the worker calls the other side",
            tone: "act",
          });
          run.emit("step", {
            id: "answer",
            at: offset(a.completed_at ?? a.started_at),
            bad: !ok,
            token: {
              id: token,
              state: ok ? "ok" : "bad",
              note: ok ? `${a.output?.status_code ?? 200} · ${ms(a.duration_ms)}` : `${a.error?.status_code ?? "no answer"}`,
            },
            flash: ok ? "hit" : "refused",
            worker: exec.worker_id ? { id: exec.worker_id, cls: null, label: "" } : null,
            text: ok
              ? `it answered <b>${a.output?.status_code ?? 200}</b> in ${ms(a.duration_ms)}`
              : `try ${a.attempt_number} <b>failed</b> — ${esc(
                  a.error?.status_code ? String(a.error.status_code) : (a.error?.type ?? "no answer").toLowerCase(),
                )}`,
            tone: ok ? "ok" : "bad",
          });
        }
        seenAttempts = attempts.length;
        run.emit("tries", { attempts: ordered });
      }

      await tailTarget();

      if (pendingRetry) {
        const waitMs = Math.max(0, new Date(pendingRetry.run_at) - Date.now());
        run.step("due", {
          token: { id: token, zone: "waiting", state: "waiting", note: `retry in ${ms(waitMs)}` },
          text: `back in the queue — next try in <b>${ms(waitMs)}</b>`,
          tone: "warn",
        });
        countdownToken(run, token, new Date(pendingRetry.run_at).getTime(), "retry in");
      }

      if (["SUCCESS", "FAILED", "CANCELLED"].includes(exec.status)) {
        await tailTarget();
        if (sawWaiting) {
          run.step("answer", {
            bad: exec.status !== "SUCCESS",
            token: { id: token, state: exec.status === "SUCCESS" ? "ok" : "bad", note: "finished" },
            flash: exec.status === "SUCCESS" ? "hit" : "refused",
            text:
              exec.status === "SUCCESS"
                ? `it finished after <b>${seenPolls} check-in${seenPolls === 1 ? "" : "s"}</b> — still one attempt`
                : "it never finished",
            tone: exec.status === "SUCCESS" ? "ok" : "bad",
          });
        }
        run.step("record", {
          token: {
            id: token,
            zone: "done",
            state: exec.status === "SUCCESS" ? "ok" : "bad",
            label: label ?? "",
            note: exec.status === "SUCCESS" ? "done" : exec.status.toLowerCase(),
          },
          bad: exec.status !== "SUCCESS",
          text:
            exec.status === "SUCCESS"
              ? "recorded — duration, response and key, all queryable"
              : `finished as <b>${exec.status.toLowerCase()}</b>, with every try on the record`,
          tone: exec.status === "SUCCESS" ? "ok" : "bad",
        });
        return exec;
      }
    }
    await sleep(160);
  }
  return null;
}

// Countdowns are emitted, not computed on screen, so a replay reproduces them.
function countdownToken(run, token, deadline, prefix) {
  const timer = setInterval(() => {
    const left = deadline - Date.now();
    if (run.cancelled) return clearInterval(timer);
    if (left <= 0) {
      clearInterval(timer);
      run.emit("token", { id: token, zone: "ready", state: "ready", note: "due now" });
      return;
    }
    run.emit("token", { id: token, note: `${prefix} ${(left / 1000).toFixed(1)}s` });
  }, 500);
  return timer;
}

// ─── the two phases ──────────────────────────────────────────────────────────

const key = (p) => `${p}-${Date.now().toString(36)}`;

const WELCOME_SPEC = {
  name: "send-welcome-email",
  type: "HTTP",
  payload_spec: "order-input",
  config: "email-service",
  spec: {
    url: "{{config.api_base_url}}/emails/welcome",
    method: "POST",
    headers: { Authorization: "Bearer {{secret.email_api_key}}", "Content-Type": "application/json" },
    body_template: {
      customer: "{{input.customer}}",
      email: "{{input.email}}",
      order_id: "{{input.order_id}}",
      sent_by: "{{config.sender}}",
    },
    timeout_ms: 5000,
    expected_status_codes: [200],
  },
  retry_policy: { max_attempts: 3, backoff: "exponential", initial_delay_ms: 1000, max_delay_ms: 30000 },
};

/// Phase one: the endpoint. Three steps, and at the end there is a row.
async function registerPhase(run, { live = true } = {}) {
  if (!live) {
    for (const id of ["describe", "stored", "secret"]) run.step(id, { skip: true });
    run.emit("token", { id: "ep", zone: "endpoints", state: "spec", label: "send-welcome-email", note: "registered" });
    return true;
  }

  run.step("describe", {
    token: { id: "ep", zone: "app", state: "spec", label: "send-welcome-email", note: "where · what · how hard" },
    text: "one endpoint: where to send it, what to put in it, how hard to try",
  });
  run.emit("req", { body: WELCOME_SPEC });

  const exists = (await api("GET", `/v1/endpoints/${WELCOME_SPEC.name}`)).ok;
  const { name, ...rest } = WELCOME_SPEC;
  const res = exists
    ? await api("PUT", `/v1/endpoints/${WELCOME_SPEC.name}`, { body: rest })
    : await api("POST", "/v1/endpoints", { body: WELCOME_SPEC });

  run.step("stored", {
    token: { id: "ep", zone: "endpoints", state: "spec", note: "one row" },
    text: exists ? "already there, so it was updated in place — <b>still one row</b>" : "saved — <b>one row</b>, no deploy",
    tone: "act",
  });

  const secret = await api("GET", "/v1/secrets/email_api_key");
  run.emit("res", { body: secret.body });
  const leaked = JSON.stringify(secret.body ?? {}).includes('"value"');
  run.step("secret", {
    bad: leaked,
    text: leaked
      ? "the API returned a secret value — check this"
      : "asked Invokr for that API key: it returns the name and nothing else",
    tone: leaked ? "bad" : "ok",
  });
  state.registered = true;
  return res.ok;
}

/// Phase two: the run. Same three steps whatever the trigger is.
async function invokePhase(run, { endpoint, input, trigger, runAt, cron, ws = "a", label, token, maxAttempts }) {
  const body = {
    trigger: trigger ?? (runAt ? "DELAYED" : "IMMEDIATE"),
    endpoint,
    idempotency_key: key(`${input.order_id ?? "job"}-${token}`),
    input,
  };
  if (runAt) body.run_at = runAt.toISOString();
  if (cron) {
    body.cron = cron;
    body.timezone = "Asia/Kolkata";
  }
  if (maxAttempts) body.max_attempts = maxAttempts;

  run.step("ask", {
    token: { id: token, zone: "app", label, note: "new" },
    text: `you ask for a run — <b>${esc(body.trigger.toLowerCase())}</b>`,
  });
  run.emit("req", { body });

  const t0 = performance.now();
  const res = await api("POST", "/v1/jobs", { body, ws });
  run.emit("res", { body: res.body });
  if (!res.ok) {
    run.step("written", { bad: true, text: `Invokr refused it — <b>${res.status}</b>`, tone: "bad" });
    return null;
  }

  const waiting = Boolean(runAt);
  run.step("written", {
    token: {
      id: token,
      zone: waiting ? "waiting" : "ready",
      state: waiting ? "waiting" : "ready",
      note: waiting ? "not due yet" : "due now",
    },
    text: `Invokr answered in ${ms(performance.now() - t0)} — and it was <b>already durable</b>`,
    tone: "act",
  });

  if (waiting) {
    run.step("due", {
      token: { id: token, note: `due at ${clock(runAt)}` },
      text: `nothing is counting down — <b>the row is the timer</b>, due ${clock(runAt)}`,
      tone: "warn",
    });
  } else {
    run.step("due", { token: { id: token, note: "due now" }, text: "due immediately, so it is already eligible" });
  }

  return res.body.data;
}

// ─── scenes ──────────────────────────────────────────────────────────────────

const INVOKE_PHASE = { phase: "invoke", steps: INVOKE_STEPS };

const TRIGGER_FIELD = {
  key: "trigger",
  label: "when",
  type: "select",
  value: "IMMEDIATE",
  width: 150,
  options: [
    { value: "IMMEDIATE", label: "now" },
    { value: "DELAYED", label: "in a few seconds" },
    { value: "CRON", label: "every minute" },
  ],
};

const scenes = [
  {
    id: "register-and-send",
    n: 1,
    group: "core",
    title: "Register it, then send it",
    lede: "The whole journey: an endpoint becomes a row, then a run becomes another row.",
    journey: DEFAULT_JOURNEY,
    fields: [
      { key: "customer", label: "for", value: "Priya Sharma", width: 165 },
      { key: "email", label: "email", value: "priya@example.com", width: 190 },
      { key: "order_id", label: "order", value: "order-1234", width: 135 },
      TRIGGER_FIELD,
      { key: "seconds", label: "delay (s)", value: "12", width: 80 },
    ],
    action: "Run the journey",
    extras: [{ id: "addworker", label: "Add a worker" }],
    async onExtra(id, run) {
      if (id !== "addworker") return;
      const res = await control("/worker/start");
      await refreshStatus();
      run?.emit("workers", { list: state.status?.workers ?? [] });
      run?.tick(res.body?.ok ? `another worker is up — <b>pid ${res.body.pid}</b>` : esc(res.body?.error ?? "no"), "act");
    },
    async run(run, v) {
      const since = await targetLogHead();
      run.emit("target-is", { who: "the email service" });

      if (!(await registerPhase(run, { live: true }))) return false;

      const seconds = Math.max(5, Math.min(120, Number(v.seconds) || 12));
      const trigger = v.trigger ?? "IMMEDIATE";
      const job = await invokePhase(run, {
        endpoint: "send-welcome-email",
        input: { order_id: v.order_id, customer: v.customer, email: v.email },
        trigger,
        runAt: trigger === "DELAYED" ? new Date(Date.now() + seconds * 1000) : null,
        cron: trigger === "CRON" ? "* * * * *" : null,
        label: v.customer,
        token: "t1",
      });
      if (!job) return false;

      if (trigger === "CRON") {
        state.cronJob = job.job_id;
        run.tick("pg_cron owns this one — waiting for the top of the minute", "warn");
        const ok = await watchCronTick(run, job.job_id, { since, token: "t1", label: v.customer });
        await api("POST", `/v1/jobs/${job.job_id}/cancel`);
        state.cronJob = null;
        run.tick("cancelled, so it does not keep firing all session", "act");
        return ok;
      }

      if (trigger === "DELAYED") {
        countdownToken(run, "t1", new Date(job.execution.created_at).getTime() + seconds * 1000, "due in");
      }

      const done = await watchExecution(run, job.execution.execution_id, {
        targetSince: since,
        token: "t1",
        label: v.customer,
        timeout: (seconds + 90) * 1000,
      });
      return done?.status === "SUCCESS";
    },
  },

  {
    id: "fire-later",
    n: 2,
    group: "core",
    title: "Send one later — then kill a worker",
    lede: "It waits in the queue as a row. Kill every worker; it still goes out on time.",
    journey: [REGISTER_PHASE, INVOKE_PHASE],
    fields: [
      { key: "customer", label: "for", value: "Arjun Mehta", width: 165 },
      { key: "order_id", label: "order", value: "order-5567", width: 135 },
      { key: "seconds", label: "in (s)", value: "15", width: 75 },
    ],
    action: "Schedule it",
    extras: [
      { id: "kill", label: "Kill a worker", danger: true },
      { id: "addworker", label: "Add a worker" },
    ],
    async onExtra(id, run) {
      if (id === "kill") {
        const res = await control("/worker/kill");
        if (res.body?.ok) {
          run?.emit("worker", { workerId: res.body.workerId, cls: "dead", label: `killed · pid ${res.body.pid}` });
          run?.tick(`<b>killed</b> worker ${esc(short(res.body.workerId))} — SIGKILL, mid-transaction`, "bad");
          setTimeout(async () => {
            await refreshStatus();
            run?.emit("workers", { list: state.status?.workers ?? [] });
          }, 1600);
        } else {
          run?.tick("no worker left to kill", "warn");
        }
        return;
      }
      if (id === "addworker") {
        const res = await control("/worker/start");
        await refreshStatus();
        run?.emit("workers", { list: state.status?.workers ?? [] });
        run?.tick(res.body?.ok ? `a fresh worker is up — <b>pid ${res.body.pid}</b>` : esc(res.body?.error ?? "no"), "act");
      }
    },
    async run(run, v) {
      const since = await targetLogHead();
      run.emit("target-is", { who: "the email service" });
      await registerPhase(run, { live: false });

      const seconds = Math.max(5, Math.min(120, Number(v.seconds) || 15));
      const runAt = new Date(Date.now() + seconds * 1000);
      const job = await invokePhase(run, {
        endpoint: "send-welcome-email",
        input: { order_id: v.order_id, customer: v.customer, email: `${String(v.customer).split(" ")[0].toLowerCase()}@example.com` },
        runAt,
        label: v.customer,
        token: "t1",
      });
      if (!job) return false;

      run.tick("now kill the workers — nothing is holding this", "warn");
      const timer = countdownToken(run, "t1", runAt.getTime(), "due in");
      const done = await watchExecution(run, job.execution.execution_id, {
        timeout: (seconds + 90) * 1000,
        targetSince: since,
        token: "t1",
        label: v.customer,
      });
      clearInterval(timer);
      return done?.status === "SUCCESS";
    },
  },

  {
    id: "it-fails",
    n: 3,
    group: "core",
    title: "When the other side breaks",
    lede: "Two failures, then it works. Watch it fly back to the queue each time.",
    journey: [REGISTER_PHASE, INVOKE_PHASE],
    fields: [
      { key: "order_id", label: "order", value: "order-9931", width: 135 },
      { key: "amount", label: "amount", value: "₹1,499", width: 105 },
    ],
    action: "Take the payment",
    extras: [{ id: "prearm", label: "Start scene 4's job now" }],
    async onExtra(id, run) {
      if (id !== "prearm") return;
      const started = await startCron();
      run?.tick(started ? "scene 4's every-minute job is running now" : "could not start it", started ? "ok" : "bad");
    },
    async run(run, v) {
      await control("/mock/reset");
      const since = await targetLogHead();
      run.emit("target-is", { who: "the payment processor" });
      await registerPhase(run, { live: false });

      const job = await invokePhase(run, {
        endpoint: "charge-webhook",
        input: { order_id: v.order_id, amount: v.amount },
        label: v.amount,
        token: "t1",
      });
      if (!job) return false;

      const done = await watchExecution(run, job.execution.execution_id, {
        timeout: 120000,
        targetSince: since,
        token: "t1",
        label: v.amount,
      });
      if (done?.status === "SUCCESS") {
        run.tick("all three tries carried <b>the same key</b> — that is what keeps a retry safe", "act");
      }
      return done?.status === "SUCCESS";
    },
  },

  {
    id: "fire-repeatedly",
    n: 4,
    group: "core",
    title: "Every minute, forever",
    lede: "No scheduler process. PostgreSQL writes the row itself, on schedule.",
    journey: [
      REGISTER_PHASE,
      {
        phase: "every minute",
        steps: [
          { id: "ask", label: "you ask for a schedule" },
          { id: "written", label: "pg_cron takes over" },
          { id: "tick", label: "the database writes a row" },
          { id: "claim", label: "a worker takes it" },
          { id: "call", label: "calls the other side" },
          { id: "answer", label: "the answer" },
          { id: "record", label: "…and again next minute" },
        ],
      },
    ],
    fields: [],
    action: "Start the every-minute job",
    extras: [{ id: "cancel", label: "Cancel it", danger: true }],
    async onExtra(id, run) {
      if (id !== "cancel") return;
      const job = state.cronJob ?? (await findActiveCron());
      if (!job) return run?.tick("nothing is scheduled", "warn");
      const res = await api("POST", `/v1/jobs/${job}/cancel`);
      run?.tick(res.ok ? "<b>cancelled</b> — the pg_cron entry goes with it" : "that cancel did not take", res.ok ? "ok" : "bad");
      state.cronJob = null;
    },
    async run(run) {
      const since = await targetLogHead();
      run.emit("target-is", { who: "the health sweep" });
      await registerPhase(run, { live: false });

      let jobId = state.cronJob;
      if (jobId) {
        run.step("ask", { text: "using the schedule started back in scene 3", tone: "act" });
        run.step("written", { text: "pg_cron already owns it", tone: "act" });
      } else {
        run.step("ask", { text: "you ask for <b>every minute</b>" });
        jobId = await startCron();
        if (!jobId) {
          run.step("written", { bad: true, text: "could not create the schedule", tone: "bad" });
          return false;
        }
        run.step("written", {
          cron: true,
          text: "handed to <b>pg_cron</b>, inside the database — no scheduler process exists",
          tone: "act",
        });
      }

      const ok = await watchCronTick(run, jobId, { since, token: "tick", label: "tick" });
      if (ok) run.tick("cancel it before moving on — it keeps going", "warn");
      return ok;
    },
  },

  {
    id: "long-running",
    n: 5,
    group: "core",
    title: "When the work takes minutes",
    lede: "The other side answers 202 and keeps working. Invokr waits, and checks back.",
    journey: [
      {
        phase: "register",
        steps: [
          { id: "describe", label: "an async endpoint" },
          { id: "stored", label: "202 = pending" },
        ],
      },
      {
        phase: "invoke",
        steps: [
          { id: "ask", label: "you ask for a run" },
          { id: "written", label: "written down" },
          { id: "claim", label: "a worker takes it" },
          { id: "call", label: "calls the other side" },
          { id: "accepted", label: "202 · working" },
          { id: "poll", label: "checks back" },
          { id: "answer", label: "finished" },
          { id: "record", label: "recorded" },
        ],
      },
    ],
    fields: [
      { key: "order_id", label: "job", value: "report-0042", width: 145 },
      { key: "polls", label: "how many checks", value: "3", width: 130 },
    ],
    action: "Start the long job",
    async run(run, v) {
      if (!state.status?.longRunning) {
        run.step("describe", {
          bad: true,
          text:
            "this build has no long-running support — it lives on <b>feat/long-running-jobs</b>. " +
            "Switch on replay to watch a recorded run.",
          tone: "bad",
        });
        return false;
      }

      const since = await targetLogHead();
      run.emit("target-is", { who: "the report builder" });

      const pending = Math.max(1, Math.min(6, Number(v.polls) || 3));
      const script = [
        ...Array.from({ length: pending }, () => ({ status: 202, body: { state: "working" }, retry_after: 2 })),
        { status: 200, body: { state: "done", rows: 128_000 } },
      ];

      const spec = {
        name: "build-report",
        type: "HTTP",
        config: "email-service",
        spec: {
          url: "{{config.api_base_url}}/async/start",
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body_template: { job: "{{input.order_id}}", script },
          timeout_ms: 5000,
          expected_status_codes: [200],
          async: {
            status_codes: [202],
            poll: {
              success_statuses: [200],
              pending_statuses: [202],
              failure_statuses: [400, 404, 410],
              initial_delay_ms: 1000,
              max_delay_ms: 10000,
              backoff: "exponential",
            },
            callback: false,
            max_wait_ms: 120000,
            max_polls: 20,
          },
        },
        retry_policy: { max_attempts: 2, backoff: "exponential", initial_delay_ms: 2000, max_delay_ms: 10000 },
      };

      run.step("describe", {
        token: { id: "ep2", zone: "app", state: "spec", label: "build-report", note: "async endpoint" },
        text: "same shape as any endpoint, plus an <b>async</b> block",
      });
      run.emit("req", { body: spec });

      const exists = (await api("GET", `/v1/endpoints/${spec.name}`)).ok;
      const { name, ...rest } = spec;
      const saved = exists
        ? await api("PUT", `/v1/endpoints/${spec.name}`, { body: rest })
        : await api("POST", "/v1/endpoints", { body: spec });
      if (!saved.ok) {
        run.step("stored", { bad: true, text: `Invokr refused the endpoint — <b>${saved.status}</b>`, tone: "bad" });
        return false;
      }
      run.step("stored", {
        token: { id: "ep2", zone: "endpoints", state: "spec", note: "202 = pending" },
        text: "<b>202</b> is not success and not failure — it means “still working”",
        tone: "act",
      });

      const job = await invokePhase(run, {
        endpoint: "build-report",
        input: { order_id: v.order_id },
        label: v.order_id,
        token: "t1",
      });
      if (!job) return false;

      const done = await watchExecution(run, job.execution.execution_id, {
        timeout: 120000,
        targetSince: since,
        token: "t1",
        label: v.order_id,
      });
      if (done?.status === "SUCCESS") {
        run.tick("one attempt, several checks — <b>polls are not retries</b>", "ok");
      }
      return done?.status === "SUCCESS";
    },
  },

  {
    id: "any-transport",
    n: 6,
    group: "overflow",
    title: "Not just HTTP",
    lede: "Same job, same retries. A Kafka topic or a Redis Stream instead of a URL.",
    journey: [REGISTER_PHASE, INVOKE_PHASE],
    fields: [{ key: "order_id", label: "order", value: "order-4410", width: 135 }],
    action: "Send it three ways",
    async run(run, v) {
      const probes = state.status?.transports ?? {};
      await registerPhase(run, { live: false });
      const targets = [
        { type: "HTTP", endpoint: "send-welcome-email", up: true },
        { type: "Kafka", endpoint: "order-events-kafka", up: !!probes.kafka },
        { type: "Redis", endpoint: "order-events-redis", up: !!probes.redis },
      ];

      let ok = true;
      let i = 0;
      for (const t of targets) {
        i++;
        if (!t.up) {
          // Deliberately not a journey step: nothing travelled, and marking one
          // would stamp a later time on a step the HTTP pass already walked.
          run.emit("token", { id: `t${i}`, zone: "app", state: "waiting", label: t.type, note: "broker not running" });
          run.tick(`<b>${esc(t.type)}</b> is not running here — same job, nowhere to put it`, "warn");
          continue;
        }
        run.emit("target-is", { who: `the ${t.type.toLowerCase()} side` });
        const job = await invokePhase(run, {
          endpoint: t.endpoint,
          input: { order_id: v.order_id, customer: "Priya Sharma", email: "priya@example.com" },
          label: t.type,
          token: `t${i}`,
          maxAttempts: 1,
        });
        if (!job) {
          ok = false;
          continue;
        }
        const done = await watchExecution(run, job.execution.execution_id, { timeout: 30000, token: `t${i}`, label: t.type });
        ok = ok && done?.status === "SUCCESS";
      }
      return ok;
    },
  },

  {
    id: "two-tenants",
    n: 7,
    group: "overflow",
    title: "Two teams, one name",
    lede: "Both teams have an endpoint called send-welcome-email. They cannot see each other.",
    journey: [REGISTER_PHASE, INVOKE_PHASE],
    fields: [
      { key: "customer", label: "for", value: "Neha Rao", width: 165 },
      { key: "order_id", label: "order", value: "order-7782", width: 135 },
    ],
    action: "Send from both teams",
    async run(run, v) {
      const teams = state.status?.provisioned?.workspaces ?? {};
      const email = `${String(v.customer).split(" ")[0].toLowerCase()}@example.com`;
      run.emit("target-is", { who: "the email service" });
      await registerPhase(run, { live: false });

      let ok = true;
      let i = 0;
      for (const wsKey of ["a", "b"]) {
        i++;
        const team = teams[wsKey];
        const since = await targetLogHead();
        const job = await invokePhase(run, {
          endpoint: "send-welcome-email",
          input: { order_id: v.order_id, customer: v.customer, email },
          ws: wsKey,
          label: team?.name ?? wsKey,
          token: `t${i}`,
        });
        if (!job) {
          ok = false;
          continue;
        }
        const done = await watchExecution(run, job.execution.execution_id, {
          ws: wsKey,
          timeout: 30000,
          targetSince: since,
          token: `t${i}`,
          label: team?.name ?? wsKey,
        });
        ok = ok && done?.status === "SUCCESS";
      }
      run.tick("same name, same input — <b>different sender</b>, because the config is each team's own", ok ? "ok" : "warn");
      return ok;
    },
  },

  {
    id: "handoff",
    n: 8,
    group: "overflow",
    title: "Hand over",
    lede: "All of that was read back out of ordinary rows. This is the tool people on call use.",
    journey: [],
    fields: [],
    html: () => {
      const url = state.status?.dashboardUrl || "";
      return `<div class="handoff">
        ${
          url
            ? `<p style="margin:0 0 6px">Dashboard: <a href="${esc(url)}" target="_blank" rel="noopener">${esc(url)}</a></p>`
            : `<div class="banner warn">No dashboard URL set — <code>just dashboard-build</code>, run the API with
               <code>INVOKR_MODE=both</code>, then set <code>INVOKR_DEMO_DASHBOARD_URL</code>.</div>`
        }
        <ol>
          <li>Everything that ran, failures included.</li>
          <li>One delivery's attempts — the same rows this page was reading.</li>
          <li>Fire one by hand.</li>
        </ol>
      </div>`;
    },
    async run() {
      return true;
    },
  },
];

/// Waits for pg_cron to materialise a tick, then follows it like any other job.
async function watchCronTick(run, jobId, { since, token, label }) {
  const started = performance.now();
  let seen = (await api("GET", `/v1/jobs/${jobId}/executions?limit=10`)).body?.data?.length ?? 0;

  while (performance.now() - started < 78000 && !run.cancelled) {
    const execs = (await api("GET", `/v1/jobs/${jobId}/executions?limit=10`)).body?.data ?? [];
    if (execs.length > seen) {
      const newest = execs[0];
      run.step("tick", {
        cron: true,
        token: { id: token, zone: "ready", state: "ready", label, note: clock(newest.created_at) },
        text: "<b>nobody put that there</b> — PostgreSQL wrote the row itself",
        tone: "act",
      });
      // We poll for the row, so we notice it up to a second after it appears.
      // The row's own timestamp is the truthful one, and it has to be, or the
      // claim that follows reads as having happened before it.
      run.emit("retime", { id: "tick", at: Math.max(0, new Date(newest.created_at) - run.wallT0) });
      setTimeout(() => run.emit("step", { id: "tick-off", cron: false }), 1500);
      await watchExecution(run, newest.execution_id, { timeout: 30000, targetSince: since, token, label });
      return true;
    }
    await sleep(900);
  }
  run.tick("no tick landed in the time we waited", "warn");
  return false;
}

async function findActiveCron() {
  const jobs = (await api("GET", "/v1/jobs?limit=50")).body?.data ?? [];
  return (
    jobs.find((j) => j.trigger === "CRON" && j.status === "ACTIVE" && j.endpoint === "minute-heartbeat")?.job_id ?? null
  );
}

async function startCron() {
  for (let existing = await findActiveCron(); existing; existing = await findActiveCron()) {
    await api("POST", `/v1/jobs/${existing}/cancel`);
  }
  const res = await api("POST", "/v1/jobs", {
    body: { endpoint: "minute-heartbeat", trigger: "CRON", cron: "* * * * *", timezone: "Asia/Kolkata", input: {} },
  });
  if (!res.ok) return null;
  state.cronJob = res.body.data.job_id;
  return state.cronJob;
}

// ─── the shell ───────────────────────────────────────────────────────────────

function renderRail() {
  const link = (s) =>
    `<button class="scene-link ${state.current === scenes.indexOf(s) ? "current" : ""} ${
      state.ran.has(s.id) ? "ran" : ""
    }" data-i="${scenes.indexOf(s)}"><span class="n">${s.n}</span><span>${esc(s.title)}</span></button>`;

  const rail = $("#rail");
  rail.innerHTML = `
    <div class="rail-label">the run</div>
    ${scenes.filter((s) => s.group === "core").map(link).join("")}
    <div class="rail-label">if there's time</div>
    ${scenes.filter((s) => s.group === "overflow").map(link).join("")}`;
  rail.querySelectorAll(".scene-link").forEach((b) =>
    b.addEventListener("click", () => mountScene(Number(b.dataset.i))),
  );
}

function valuesFor(scene) {
  if (!state.values[scene.id]) {
    state.values[scene.id] = Object.fromEntries((scene.fields ?? []).map((f) => [f.key, f.value]));
  }
  return state.values[scene.id];
}

function readFields(scene) {
  const v = valuesFor(scene);
  for (const f of scene.fields ?? []) {
    const el = document.getElementById(`f-${f.key}`);
    if (el) v[f.key] = (el.value ?? "").trim() || f.value;
  }
  return v;
}

function resetStage() {
  clearTokens();
  clearReceipts();
  resetPacer();
  $("#ticker").innerHTML = "";
  $("#p-tries").innerHTML = `<span class="empty">nothing yet</span>`;
  $("#z-target").classList.remove("hit", "refused");
  $("#cron-badge").classList.remove("tick");
  $("#drop-arrow").classList.remove("on");
  for (const id of workerBoxes.keys()) setWorkerState(id, null, "");
  renderWorkers(state.status?.workers ?? []);
}

function mountScene(i) {
  if (state.run) state.run.cancelled = true;
  state.run = null;
  state.current = Math.max(0, Math.min(scenes.length - 1, i));
  const scene = scenes[state.current];
  const v = valuesFor(scene);

  resetStage();
  renderJourney(scene);
  $("#target-label").textContent = "the other side";
  $("#scene-title").textContent = scene.title;
  $("#scene-lede").textContent = scene.lede;

  const field = (f) => {
    const disabled = state.replay ? "disabled" : "";
    const input =
      f.type === "select"
        ? `<select id="f-${f.key}" style="--w:${f.width ?? 170}px" ${disabled}>
             ${f.options
               .map(
                 (o) =>
                   `<option value="${esc(o.value)}" ${String(v[f.key]) === o.value ? "selected" : ""}>${esc(o.label)}</option>`,
               )
               .join("")}
           </select>`
        : `<input id="f-${f.key}" value="${esc(v[f.key] ?? f.value)}" style="--w:${f.width ?? 170}px" ${disabled} />`;
    return `<div class="field"><label for="f-${f.key}">${esc(f.label)}</label>${input}</div>`;
  };

  $("#scene-controls").innerHTML = `
    ${scene.html ? scene.html() : ""}
    ${scene.fields?.length ? `<div class="fields">${scene.fields.map(field).join("")}</div>` : ""}
    <div class="actions">
      ${scene.action ? `<button class="primary" id="act-run">${state.replay ? "Play the recording" : esc(scene.action)}</button>` : ""}
      ${(scene.extras ?? [])
        .map(
          (a) =>
            `<button class="${a.danger ? "danger" : ""}" data-act="${esc(a.id)}" ${
              state.replay ? "disabled" : ""
            }>${esc(a.label)}</button>`,
        )
        .join("")}
      <span class="spacer"></span>
      <span class="aside">${state.status?.recordings?.includes(scene.id) ? "recorded" : "not recorded yet"}</span>
      <button class="quiet" id="act-next">next →</button>
    </div>`;

  $("#act-run")?.addEventListener("click", () => (state.replay ? replayScene() : runScene()));
  $("#act-next")?.addEventListener("click", () => mountScene(state.current + 1));
  $("#scene-controls")
    .querySelectorAll("[data-act]")
    .forEach((b) =>
      b.addEventListener("click", async () => {
        b.disabled = true;
        try {
          await scene.onExtra?.(b.dataset.act, ensureRun(scene));
        } finally {
          b.disabled = false;
        }
      }),
    );

  renderRail();
  fitBoard();
}

function ensureRun(scene) {
  if (state.run && !state.run.cancelled) return state.run;
  const run = new Run(scene, { replay: state.replay });
  run.wallT0 = Date.now();
  state.run = run;
  return run;
}

async function runScene() {
  const scene = scenes[state.current];
  const btn = $("#act-run");
  if (!btn) return;
  const values = readFields(scene);

  btn.disabled = true;
  btn.textContent = "running…";
  resetStage();
  renderJourney(scene);

  const run = new Run(scene);
  run.wallT0 = Date.now();
  state.run = run;
  run.emit("workers", { list: state.status?.workers ?? [] });

  let ok = false;
  try {
    ok = await scene.run(run, values);
  } catch (err) {
    run.tick(`something broke here: <b>${esc(String(err.message ?? err))}</b>`, "bad");
  }
  await drained(); // the stage is still walking through the steps
  if (ok) settleJourney();

  if (ok) state.ran.add(scene.id);
  await run.save(ok);
  renderRail();

  btn.disabled = false;
  btn.textContent = scene.action;
  if (!ok) run.tick("that did not finish — switch on replay if you need this scene now", "warn");
}

async function replayScene() {
  const scene = scenes[state.current];
  const btn = $("#act-run");
  if (!btn) return;
  btn.disabled = true;
  btn.textContent = "playing…";

  let tape;
  try {
    const res = await fetch(`/control/recordings/${scene.id}`);
    if (!res.ok) throw new Error("no recording of this scene yet");
    tape = await res.json();
  } catch (err) {
    showTick(0, `${esc(String(err.message ?? err))} — run it live once and it is kept`, "bad");
    btn.disabled = false;
    btn.textContent = "Play the recording";
    return;
  }

  resetStage();
  renderJourney(scene);
  const run = new Run(scene, { replay: true });
  state.run = run;

  // Recorded offsets decide when an event becomes available; the pacer still
  // holds each step long enough to read.
  const startedAt = performance.now();
  for (const ev of tape.events ?? []) {
    if (run.cancelled) return;
    const due = startedAt + ev.t - performance.now();
    if (due > 0) await sleep(due);
    enqueue(scene, ev);
  }
  await drained();
  settleJourney();

  btn.disabled = false;
  btn.textContent = "Play the recording";
  state.ran.add(scene.id);
  renderRail();
}

function setReplay(on) {
  state.replay = on;
  document.body.classList.toggle("replaying", on);
  $("#replay-toggle").classList.toggle("on", on);
  mountScene(state.current);
}

// ─── boot ────────────────────────────────────────────────────────────────────

$("#replay-toggle").addEventListener("click", () => setReplay(!state.replay));
$("#toggle-details").addEventListener("click", () => $("#drawer").classList.toggle("open"));
$("#pace").addEventListener("click", (e) => {
  const b = e.target.closest("button");
  if (!b) return;
  pacer.dwell = Number(b.dataset.pace);
  $("#pace").querySelectorAll("button").forEach((n) => n.classList.toggle("on", n === b));
});
addEventListener("resize", fitBoard);

document.addEventListener("keydown", (e) => {
  if (e.metaKey || e.ctrlKey) return;
  const typing = e.target.tagName === "INPUT" || e.target.tagName === "SELECT";
  const k = e.key.toLowerCase();

  if (e.key === "Enter") {
    e.preventDefault();
    return (state.replay ? replayScene : runScene)();
  }
  if (e.key === "Escape") return $("#drawer").classList.remove("open");
  if (typing) return;
  if (e.key === "ArrowRight") mountScene(state.current + 1);
  else if (e.key === "ArrowLeft") mountScene(state.current - 1);
  else if (k === "r") setReplay(!state.replay);
  else if (k === "d") $("#drawer").classList.toggle("open");
});

layoutBoard();
fitBoard();
await refreshStatus();
setInterval(refreshStatus, 4000);
mountScene(0);
