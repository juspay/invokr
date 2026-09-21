// Invokr live demo — three pages, one screen each.
//
//   1  Set it up    What has to exist before a job can run. Five slots fill in,
//                   and the one request they all feed lights up placeholder by
//                   placeholder.
//   2  Short tasks  One task from trigger to end, with the trigger, the delay,
//                   the schedule and the failures all under your hand.
//   3  Long-running When the other side answers 202 and keeps working: who
//                   called whom, in what order, and how long apart.
//
// Three rules hold it together:
//
//   1. A take's `run()` never touches the DOM. It emits events; one renderer
//      draws them — which is what lets a recorded run replay through the same
//      code.
//   2. Events are delivered as they happen and drawn one step at a time. You
//      step through them yourself, or hand it to `auto`, which holds each step
//      for a dwell. The dwell is a *floor*, never a substitute: a 15-second
//      wait still takes fifteen seconds, and every step carries the real time
//      it happened at.
//   3. What is on screen is always `film.events.slice(0, film.cursor)` drawn in
//      order, so stepping backwards is just drawing less of the same film.

const $ = (sel, root = document) => root.querySelector(sel);

const state = {
  status: null,
  page: "setup",
  current: 0, // index into takesIn(state.page)
  replay: false,
  recording: true,
  ran: new Set(),
  run: null,
  values: {},
  cronJob: null,
};

const esc = (s) =>
  String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

const ms = (n) => (n == null ? "—" : n < 1000 ? `${Math.round(n)}ms` : `${(n / 1000).toFixed(1)}s`);
const clock = (t) => new Date(t).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
const short = (id) => (id ? String(id).replace("worker_", "").slice(0, 6) : "—");
const sleep = (t) => new Promise((r) => setTimeout(r, t));
const clamp = (v, lo, hi, dflt) => Math.max(lo, Math.min(hi, Number(v) || dflt));

const maskSecrets = (text) =>
  String(text).replace(/(\\?"authorization\\?"\s*:\s*\\?")(?!\{\{)([^"\\]*)/gi, "$1••••••••");

/// An attempt's recorded `output.body` is whatever the other side sent, kept as
/// a string. Parse it when it is JSON, so the page can read the answer and the
/// wire can pretty-print it.
function asJson(v) {
  if (typeof v !== "string") return v;
  try {
    return JSON.parse(v);
  } catch {
    return v;
  }
}

function highlightJson(value) {
  const text = maskSecrets(typeof value === "string" ? value : JSON.stringify(value, null, 2));
  return esc(text)
    .replace(/\{\{input\.[^}]+\}\}/g, (m) => `<span class="tok-input">${m}</span>`)
    .replace(/\{\{config\.[^}]+\}\}/g, (m) => `<span class="tok-config">${m}</span>`)
    .replace(/\{\{secret\.[^}]+\}\}/g, (m) => `<span class="tok-secret">${m}</span>`)
    .replace(/\{\{execution\.[^}]+\}\}/g, (m) => `<span class="tok-exec">${m}</span>`);
}

// ─── page 2: the board ───────────────────────────────────────────────────────

const BOARD = { w: 1420, h: 620 };
const TOKEN = { w: 152, h: 34, gap: 8 };

const ZONES = {
  app: { x: 14, y: 54, w: 190, h: 530, pad: 14, top: 32, cols: 1 },
  db: { x: 238, y: 54, w: 424, h: 530 },
  endpoints: { x: 260, y: 92, w: 380, h: 116, pad: 12, top: 28, cols: 2 },
  waiting: { x: 260, y: 232, w: 380, h: 160, pad: 12, top: 28, cols: 2 },
  ready: { x: 260, y: 412, w: 380, h: 154, pad: 12, top: 28, cols: 2 },
  workers: { x: 700, y: 54, w: 312, h: 530 },
  target: { x: 1048, y: 54, w: 250, h: 530, pad: 14, top: 32, cols: 1 },
  done: { x: 1306, y: 54, w: 110, h: 530, pad: 5, top: 30, cols: 1 },
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
  const view = $("#view-short");
  const board = $("#board");
  // The board's view is display:none while another page is up, so a fit asked
  // for during the switch measures zero. Try again once layout has flushed.
  if (!view.clientWidth) {
    if (view.classList.contains("on")) requestAnimationFrame(fitBoard);
    return;
  }
  const k = Math.min((view.clientWidth - 28) / BOARD.w, (view.clientHeight - 24) / BOARD.h, 1.15);
  board.style.transform = `translate(-50%, -50%) scale(${Math.max(0.3, k)})`;
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

// ─── page 1: the slots and the spec card ─────────────────────────────────────
//
// Five things have to exist before a job can run, and four of them are made by
// a plain POST. The slots are those five; the card beside them is the one
// request they all feed, with each placeholder grey until its source lands.

const SLOTS = [
  { id: "workspace", kind: "org + workspace" },
  { id: "config", kind: "config" },
  { id: "secret", kind: "secret" },
  { id: "payload", kind: "payload spec" },
  { id: "endpoint", kind: "endpoint" },
];

function renderSlots() {
  $("#slots").innerHTML = SLOTS.map(
    (s, i) => `<div class="slot" data-slot="${s.id}">
      <span class="kind"><span class="num">${i + 1}</span>${esc(s.kind)}</span>
      <span class="name"></span>
      <span class="detail"></span>
      <span class="mark"></span>
    </div>`,
  ).join("");
}

function setSlot({ id, cls, name, detail, mark }) {
  const el = $(`#slots [data-slot="${id}"]`);
  if (!el) return;
  if (cls !== undefined) {
    el.classList.remove("doing", "done", "bad");
    if (cls) el.classList.add(cls);
  }
  if (name !== undefined) el.querySelector(".name").textContent = name;
  if (detail !== undefined) el.querySelector(".detail").innerHTML = detail;
  if (mark !== undefined) el.querySelector(".mark").textContent = mark;
}

/// The endpoint being described, pretty-printed with every `{{namespace.key}}`
/// wrapped so it can be lit up when the thing it needs exists.
function renderSpecCard(specSource) {
  const spec = specSource ?? state.status?.mandateSpec;
  if (!spec) {
    $("#spec-body").innerHTML = `<span class="empty">waiting for the demo server</span>`;
    return;
  }
  $("#spec-body").innerHTML = esc(JSON.stringify(spec, null, 2)).replace(
    /\{\{(input|config|secret|execution)\.([^}]+)\}\}/g,
    (m, ns) => `<span class="ph p-${ns}" data-ns="${ns}">${m}</span>`,
  );
}

function lightPlaceholders(ns) {
  $("#spec-body")
    .querySelectorAll(`.ph[data-ns="${ns}"]`)
    .forEach((n) => n.classList.add("live"));
}

// ─── page 3: who called whom ─────────────────────────────────────────────────
//
// Two lanes and time running downwards. Every message is one arrow, labelled
// with what was sent and stamped with when. It is not a sequence diagram — no
// activation bars, no lifelines you have to decode — just the calls, in order,
// with the gap between them visible.

function addMsg({ dir, verb, path, code, note, at, tone }) {
  const host = $("#msgs");
  host.querySelector(".empty")?.remove();
  const row = document.createElement("div");
  row.className = `msg ${dir} ${tone ?? ""}`;
  row.innerHTML = `
    <span class="at">${at == null ? "" : at < 1000 ? `${Math.round(at)}ms` : `${(at / 1000).toFixed(1)}s`}</span>
    <div class="wirebox">
      <span class="label">${verb ? `<b>${esc(verb)}</b> ` : ""}${esc(path ?? "")}${
        code ? ` <span class="code ${code >= 200 && code < 300 ? "ok" : "bad"}">${code}</span>` : ""
      }</span>
      <span class="line"></span>
    </div>
    <span class="tail">${note ?? ""}</span>`;
  host.appendChild(row);
  host.scrollTop = host.scrollHeight;
}

function setExecState({ status, polls, maxPolls, note }) {
  const el = $("#exec-state");
  if (!status) {
    el.innerHTML = `<span class="empty">not started</span>`;
    return;
  }
  const tone =
    status === "SUCCESS" ? "ok" : status === "FAILED" || status === "CANCELLED" ? "bad" : status === "WAITING" ? "warn" : "act";
  el.innerHTML = `
    <div class="state-row"><span class="k">status</span><span class="pill ${tone}">${esc(status)}</span></div>
    <div class="state-row"><span class="k">polls</span><span class="v">${polls ?? 0}${maxPolls ? ` / ${maxPolls}` : ""}</span></div>
    ${note ? `<div class="state-note">${note}</div>` : ""}`;
}

function clearLanes() {
  $("#msgs").innerHTML = `<span class="empty">nothing has been sent yet</span>`;
  setExecState({});
}

// ─── the step dots ───────────────────────────────────────────────────────────

function renderPips(take) {
  const steps = stepsOf(take) ?? [];
  $("#step-dot").innerHTML = steps
    .map((s) => `<i class="pip" data-step="${esc(s.id)}" title="${esc(s.label)}"></i>`)
    .join("");
  updateStepCount();
}

function updateStepCount() {
  const host = $("#step-dot");
  const all = host.querySelectorAll(".pip").length;
  if (!all) return ($("#step-count").textContent = "");
  const at = host.querySelectorAll(".pip.done, .pip.now, .pip.bad, .pip.skipped").length;
  $("#step-count").textContent = `${at} / ${all}`;
}

/// Mark a step, and remember the time it claims so a later step is never shown
/// as having happened before it.
///
/// Two clocks feed this: steps the page stamps as it walks, and steps retimed
/// from a row's own timestamp on the database's clock. They agree to within a
/// few milliseconds, which is enough — when a take walks the journey twice — to
/// render a claim a hair before the write that produced it.
function markStep(id, cls, at) {
  const host = $("#step-dot");
  const el = host.querySelector(`[data-step="${id}"]`);
  if (!el) return;
  host.querySelectorAll(".pip.now").forEach((n) => {
    n.classList.remove("now");
    if (!n.classList.contains("bad") && !n.classList.contains("skipped")) n.classList.add("done");
  });
  el.classList.remove("skipped", "bad");
  el.classList.add(cls ?? "now");
  if (at != null) el.dataset.at = String(Math.max(at, priorStepTime(el)));
  updateStepCount();
}

function priorStepTime(el) {
  const pips = [...$("#step-dot").querySelectorAll(".pip")];
  const before = pips
    .slice(0, pips.indexOf(el))
    .reverse()
    .find((s) => s.dataset.at !== undefined);
  return before ? Number(before.dataset.at) : 0;
}

function retimeStep(id, at) {
  const el = $("#step-dot").querySelector(`[data-step="${id}"]`);
  if (el) el.dataset.at = String(Math.max(at, priorStepTime(el)));
}

/// A finished run has no current step. A run that stopped early keeps its
/// pulse — that is where it stopped.
function settlePips() {
  $("#step-dot")
    .querySelectorAll(".pip.now")
    .forEach((n) => {
      n.classList.remove("now");
      n.classList.add("done");
    });
}

// ─── the narration ───────────────────────────────────────────────────────────

function narrate(t, text, tone) {
  const el = $("#narration");
  const stamp = t == null ? "" : t < 1000 ? `${Math.round(t)}ms` : `${(t / 1000).toFixed(1)}s`;
  el.className = `narration ${tone ?? ""}`;
  el.innerHTML = `${stamp ? `<span class="t">${stamp}</span>` : ""}${text}`;
}

// ─── the wire ────────────────────────────────────────────────────────────────

function addWire({ verb, path, status, req, res, said }) {
  const host = $("#wire-body");
  host.querySelector(".empty")?.remove();
  const row = document.createElement("div");
  row.className = "exch";
  const ok = status == null || (status >= 200 && status < 300);
  row.innerHTML = `
    <div class="line"><span class="arrow">→</span><span class="verb">${esc(verb)}</span><span class="path">${esc(path)}</span></div>
    ${req === undefined ? "" : `<pre>${highlightJson(req)}</pre>`}
    ${
      status == null
        ? ""
        : `<div class="line"><span class="arrow">←</span><span class="code ${ok ? "ok" : "bad"}">${status}</span>${
            said ? `<span class="path">${esc(said)}</span>` : ""
          }</div>`
    }
    ${res === undefined ? "" : `<pre>${highlightJson(res)}</pre>`}`;
  host.appendChild(row);
  host.scrollTop = host.scrollHeight;
  $("#wire-meta").textContent = `${host.querySelectorAll(".exch").length} calls`;
}

function addHits(entries) {
  const host = $("#target-log");
  if (!entries?.length) return;
  host.querySelector(".empty")?.remove();
  for (const e of entries) {
    const row = document.createElement("div");
    row.className = `hit ${e.ok ? "" : "bad"}`;
    row.innerHTML = `<span class="mark">${e.ok ? "✓" : "!"}</span>
      <div>
        <div class="said">${esc(e.summary)}</div>
        <div class="sub">${esc(clock(e.at))}${e.status ? ` · ${e.status}` : ""}${
          e.idempotency_key ? ` · key ${esc(e.idempotency_key)}` : ""
        }</div>
      </div>`;
    host.appendChild(row);
  }
  host.scrollTop = host.scrollHeight;
  $("#log-meta").textContent = `${host.querySelectorAll(".hit").length} calls`;
}

function clearWire() {
  $("#wire-body").innerHTML = `<span class="empty">nothing yet</span>`;
  $("#target-log").innerHTML = `<span class="empty">nothing yet</span>`;
  $("#wire-meta").textContent = "";
  $("#log-meta").textContent = "";
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

// ─── the film ────────────────────────────────────────────────────────────────
//
// Events are delivered as they happen. What is on screen is always the first
// `cursor` of them, drawn in order — so Next draws one more group, Back draws
// one fewer, and `auto` is Next on a timer.

const PACED = new Set(["step", "slot", "msg"]);

const film = {
  events: [],
  cursor: 0,
  closed: false,
  playing: false,
  mode: "manual", // "manual" | "auto"
  dwell: 1200,
  nudge: null, // resolves when the viewer asks for the next step
  waiters: [],
};

function deliver(ev) {
  film.events.push(ev);
}

function resetFilm() {
  film.events.length = 0;
  film.cursor = 0;
  film.closed = false;
  releaseNudge();
  film.waiters.splice(0).forEach((r) => r());
}

function waitForNudge() {
  return new Promise((r) => {
    film.nudge = r;
  });
}

function releaseNudge() {
  const r = film.nudge;
  film.nudge = null;
  r?.();
}

/// Draw events up to and including the next paced one.
function advanceOneGroup(take) {
  while (film.cursor < film.events.length) {
    const ev = film.events[film.cursor++];
    draw(take, ev);
    if (PACED.has(ev.type)) return true;
  }
  return false;
}

/// Redraw the film from the start, showing exactly `n` events.
function redrawTo(take, n) {
  resetStage();
  renderPips(take);
  film.cursor = 0;
  while (film.cursor < n) draw(take, film.events[film.cursor++]);
}

function pacedIndices() {
  const out = [];
  film.events.forEach((e, i) => PACED.has(e.type) && out.push(i));
  return out;
}

function stepBack() {
  const take = currentTake();
  const before = pacedIndices().filter((i) => i < film.cursor - 1);
  redrawTo(take, before.length ? before[before.length - 1] + 1 : 0);
  syncTransport();
}

function stepNext() {
  if (film.cursor < film.events.length) {
    advanceOneGroup(currentTake());
    syncTransport();
    return;
  }
  releaseNudge(); // nothing buffered: let the run deliver the next one
}

function setMode(mode) {
  film.mode = mode;
  $("#t-auto").classList.toggle("on", mode === "auto");
  $("#t-auto").textContent = mode === "auto" ? "pause" : "auto";
  if (mode === "auto") releaseNudge();
  syncTransport();
}

function syncTransport() {
  $("#t-back").disabled = film.cursor <= 0 || film.mode === "auto";
  $("#t-next").disabled = film.mode === "auto";
  updateStepCount();
}

/// Walk the film. Ends when every delivered event has been drawn and the run
/// says there will be no more.
async function play(take) {
  film.playing = true;
  syncTransport();
  while (film.playing) {
    if (film.cursor < film.events.length) {
      const drewStep = advanceOneGroup(take);
      syncTransport();
      if (!drewStep) continue; // tail of un-paced events; keep going
      if (film.mode === "auto") await sleep(film.dwell);
      else await waitForNudge();
      continue;
    }
    if (film.closed) break;
    await sleep(40);
  }
  film.playing = false;
  film.waiters.splice(0).forEach((r) => r());
  syncTransport();
}

const played = () => (film.playing ? new Promise((r) => film.waiters.push(r)) : Promise.resolve());

// ─── the event tape ──────────────────────────────────────────────────────────

class Run {
  constructor(take, { replay = false } = {}) {
    this.take = take;
    this.replay = replay;
    this.t0 = performance.now();
    this.wallT0 = Date.now();
    this.events = [];
    this.cancelled = false;
  }

  emit(type, payload = {}) {
    if (this.cancelled) return;
    const ev = { t: Math.round(performance.now() - this.t0), type, ...payload };
    this.events.push(ev);
    deliver(ev);
  }

  /// One step of a journey.
  step(id, opts = {}) {
    this.emit("step", { id, at: Math.round(performance.now() - this.t0), ...opts });
  }

  /// One of page 1's five slots.
  slot(id, opts = {}) {
    this.emit("slot", { id, ...opts });
  }

  /// One arrow on page 3.
  msg(opts) {
    this.emit("msg", { at: Math.round(performance.now() - this.t0), ...opts });
  }

  say(text, tone = "") {
    this.emit("say", { text, tone });
  }

  /// Say why it stopped, and keep that as the last word — the generic "that
  /// did not finish" helps nobody once the take has explained itself.
  halt(text, tone = "bad") {
    this.halted = true;
    this.say(text, tone);
  }

  wire(entry) {
    this.emit("wire", entry);
  }

  async save(ok) {
    if (!ok || this.replay || !state.recording) return;
    try {
      await fetch(`/control/recordings/${tapeId(this.take)}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: maskSecrets(
          JSON.stringify({ scene: this.take.id, recorded_at: new Date().toISOString(), events: this.events }),
        ),
      });
      const known = state.status?.recordings;
      const id = tapeId(this.take);
      if (known && !known.includes(id)) known.push(id);
    } catch {
      /* a failed save must never take the demo down */
    }
  }
}

// ─── the one renderer ────────────────────────────────────────────────────────

function draw(take, ev) {
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
      if (ev.text) narrate(ev.at ?? ev.t, ev.text, ev.tone);
      break;

    case "slot":
      setSlot(ev);
      if (ev.lights) lightPlaceholders(ev.lights);
      if (ev.text) narrate(ev.t, ev.text, ev.tone);
      break;

    case "msg":
      addMsg(ev);
      if (ev.id) markStep(ev.id, ev.bad ? "bad" : "now", ev.at);
      if (ev.exec) setExecState(ev.exec);
      if (ev.text) narrate(ev.at ?? ev.t, ev.text, ev.tone);
      break;

    case "exec":
      setExecState(ev);
      break;

    case "slots-reset":
      renderSlots();
      break;

    case "spec":
      renderSpecCard(ev.body);
      break;

    case "retime":
      retimeStep(ev.id, ev.at);
      break;

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

    case "say":
      narrate(ev.t, ev.text, ev.tone);
      break;

    case "wire":
      addWire(ev);
      break;

    case "receipt":
      addHits(ev.entries);
      break;
  }
}

// ─── status ──────────────────────────────────────────────────────────────────

function setLight(id, up) {
  const el = document.getElementById(id);
  if (!el) return;
  el.classList.toggle("up", !!up);
  el.classList.toggle("down", !up);
}

async function refreshStatus() {
  try {
    const s = await (await fetch("/control/status")).json();
    const first = !state.status;
    state.status = s;
    setLight("light-api", s.api);
    setLight("light-target", s.mock);
    setLight("light-worker", s.workers.length > 0);
    $("#worker-count").textContent = String(s.workers.length);
    if (!state.run || state.run.cancelled) renderWorkers(s.workers);
    if (first) renderSpecCard();
  } catch {
    setLight("light-api", false);
  }
}

// ─── watching one real execution, step by step ───────────────────────────────

async function watchExecution(run, executionId, opts = {}) {
  const { ws = "a", timeout = 90000, targetSince = 0, token, label } = opts;
  const started = performance.now();
  let seenAttempts = 0;
  let claimedAttempt = 0;
  let announcedRetry = 0;
  let logSeq = targetSince;
  let lastBody = null;

  const tailTarget = async () => {
    try {
      const res = await fetch(`/control/mock/log?since=${logSeq}&limit=20`);
      const entries = ((await res.json())?.data ?? []).slice().reverse();
      if (!entries.length) return;
      logSeq = Math.max(logSeq, ...entries.map((e) => e.seq));
      run.emit("receipt", { entries });
    } catch {
      /* the target's log is a nicety, never a dependency */
    }
  };

  // Offsets for steps we learn about after the fact come from the row's own
  // timestamps, so the console shows when it happened, not when we noticed.
  const offset = (iso) => Math.max(0, new Date(iso) - run.wallT0);

  while (performance.now() - started < timeout && !run.cancelled) {
    const exec = (await api("GET", `/v1/executions/${executionId}`, { ws })).body?.data;

    if (exec) {
      let pendingRetry = null;
      if (exec.status === "RETRYING" && exec.run_at && exec.attempt_count > announcedRetry) {
        announcedRetry = exec.attempt_count;
        pendingRetry = exec;
      }

      // `at` is the attempt's own start when we already have the row, so the
      // console never shows the pick-up as later than the call it produced.
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

      const attempts = (await api("GET", `/v1/executions/${executionId}/attempts`, { ws })).body?.data ?? [];
      if (attempts.length !== seenAttempts) {
        const ordered = [...attempts].sort((a, b) => a.attempt_number - b.attempt_number);
        for (const a of ordered.slice(seenAttempts)) {
          claim(a.attempt_number, exec.worker_id, offset(a.started_at));
          // The claim and this attempt start in the same transaction, so the
          // attempt's own timestamp is the truthful one for both.
          run.emit("retime", { id: "claim", at: offset(a.started_at) });
          if (a.status === "WAITING") continue; // page 3 tells that story

          const ok = a.status === "SUCCESS";
          lastBody = asJson(a.output?.body) ?? lastBody;

          run.emit("step", {
            id: "call",
            at: offset(a.started_at),
            token: { id: token, zone: "target", state: "running", note: "calling" },
            text: "the worker calls <b>Aarokya</b>, with the key resolved from the secret store",
            tone: "act",
          });
          run.emit("wire", {
            verb: "POST",
            path: `aarokya · try ${a.attempt_number}`,
            status: a.output?.status_code ?? a.error?.status_code ?? null,
            res: asJson(a.output?.body) ?? a.error ?? undefined,
            said: ok ? ms(a.duration_ms) : (a.error?.type ?? "").toLowerCase(),
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
      }

      await tailTarget();

      if (pendingRetry) {
        const waitMs = Math.max(0, new Date(pendingRetry.run_at) - Date.now());
        run.step("due", {
          token: { id: token, zone: "waiting", state: "waiting", note: `retry in ${ms(waitMs)}` },
          text: `back in the queue — next try in <b>${ms(waitMs)}</b>, and it is the same key`,
          tone: "warn",
        });
        countdownToken(run, token, new Date(pendingRetry.run_at).getTime(), "retry in");
      }

      if (["SUCCESS", "FAILED", "CANCELLED"].includes(exec.status)) {
        await tailTarget();
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
              ? `recorded — <b>${seenAttempts} attempt${seenAttempts === 1 ? "" : "s"}</b>, response and key, all queryable`
              : `finished as <b>${exec.status.toLowerCase()}</b>, with every try on the record`,
          tone: exec.status === "SUCCESS" ? "ok" : "bad",
        });
        exec.last_body = lastBody;
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

/// Waits for pg_cron to materialise a tick, then follows it like any other job.
async function watchCronTick(run, jobId, { since, token, label, ws = "a", nth = 1 }) {
  const started = performance.now();
  let seen = (await api("GET", `/v1/jobs/${jobId}/executions?limit=20`, { ws })).body?.data?.length ?? 0;

  while (performance.now() - started < 190000 && !run.cancelled) {
    const execs = (await api("GET", `/v1/jobs/${jobId}/executions?limit=20`, { ws })).body?.data ?? [];
    if (execs.length > seen) {
      const newest = execs[0];
      run.step("tick", {
        cron: true,
        token: { id: token, zone: "ready", state: "ready", label, note: clock(newest.created_at) },
        text: `tick ${nth} — <b>nobody put that there</b>, PostgreSQL wrote the row itself`,
        tone: "act",
      });
      // We poll for the row, so we notice it up to a second after it appears.
      // The row's own timestamp is the truthful one, and it has to be, or the
      // claim that follows reads as having happened before it.
      run.emit("retime", { id: "tick", at: Math.max(0, new Date(newest.created_at) - run.wallT0) });
      setTimeout(() => run.emit("step", { id: "tick-off", cron: false }), 1500);
      return await watchExecution(run, newest.execution_id, { ws, timeout: 40000, targetSince: since, token, label });
    }
    await sleep(900);
  }
  run.say("no tick landed in the time we waited", "warn");
  return null;
}

// ─── asking for a run ────────────────────────────────────────────────────────

const key = (p) => `${p}-${Date.now().toString(36)}`;

/// Create the job and follow the row it makes, whatever the trigger is.
async function invokePhase(run, { endpoint, input, trigger, runAt, cron, ws = "a", label, token, maxAttempts, asyncOverrides }) {
  const body = {
    trigger: trigger ?? (runAt ? "DELAYED" : "IMMEDIATE"),
    endpoint,
    idempotency_key: key(`${input.mandate_id ?? "job"}-${token}`),
    input,
  };
  if (runAt) body.run_at = runAt.toISOString();
  if (cron) {
    body.cron = cron;
    body.timezone = "Asia/Kolkata";
  }
  if (maxAttempts) body.max_attempts = maxAttempts;
  if (asyncOverrides) body.async_overrides = asyncOverrides;

  run.step("ask", {
    token: { id: token, zone: "app", label, note: "new" },
    text: `you ask for a run — <b>${esc(body.trigger.toLowerCase())}</b>`,
  });

  const t0 = performance.now();
  const res = await api("POST", "/v1/jobs", { body, ws });
  run.wire({ verb: "POST", path: "/v1/jobs", status: res.status, req: body, res: res.body });

  if (!res.ok) {
    run.step("written", { bad: true, text: `Invokr refused it — <b>${res.status}</b>`, tone: "bad" });
    return null;
  }

  const waiting = Boolean(runAt) || Boolean(cron);
  run.step("written", {
    token: {
      id: token,
      zone: waiting ? "waiting" : "ready",
      state: waiting ? "waiting" : "ready",
      note: cron ? "on a schedule" : waiting ? "not due yet" : "due now",
    },
    text: `Invokr answered in ${ms(performance.now() - t0)} — and it was <b>already durable</b>`,
    tone: "act",
  });

  if (cron) {
    run.step("due", {
      token: { id: token, note: cron },
      text: `<b>pg_cron owns it</b> — the schedule lives in the database, not in a process`,
      tone: "warn",
    });
  } else if (runAt) {
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

/// POST it, and if it is already there, PUT instead — showing both calls on the
/// wire either way.
///
/// Page 1 cannot start from nothing twice: an endpoint any job has ever pointed
/// at cannot be deleted (`jobs.endpoint` is a foreign key and a retired job
/// still holds it), and a config an endpoint points at cannot be deleted
/// either. So the honest thing on a second run is the call a team actually
/// makes: the same body, and the row is updated in place.
async function put(run, collection, name, body, shownBody) {
  const created = await api("POST", `/v1/${collection}`, { body });
  run.wire({
    verb: "POST",
    path: `/v1/${collection}`,
    status: created.status,
    req: shownBody ?? body,
    res: created.body,
  });
  if (created.ok) return { ok: true, status: created.status, mark: String(created.status), existed: false };
  if (created.status !== 409) return { ok: false, status: created.status, mark: String(created.status), existed: false };

  const { name: _name, ...rest } = body;
  const updated = await api("PUT", `/v1/${collection}/${name}`, { body: rest });
  run.wire({
    verb: "PUT",
    path: `/v1/${collection}/${name}`,
    status: updated.status,
    res: updated.body,
    said: "updated in place",
  });
  return { ok: updated.ok, status: updated.status, mark: `${updated.status} · updated`, existed: true };
}

const SETUP_NAMES = () =>
  state.status?.setup ?? {
    config: "aarokya-config",
    secret: "aarokya-callback-auth",
    payloadSpec: "mandate-input",
    endpoint: "aarokya-mandate-registration-sync",
  };

const MANDATE_SCHEMA = {
  type: "object",
  properties: {
    mandate_id: { type: "string" },
    checks: { type: "string" },
    fail_times: { type: "string" },
  },
  required: ["mandate_id"],
};

/// Make sure page 1's four exist, quietly.
///
/// Pages 2 and 3 can be shown on their own, and `just demo` deliberately leaves
/// the mandates workspace empty of them so page 1's first run is four real
/// creations.
async function ensureSetup() {
  const N = SETUP_NAMES();
  if ((await api("GET", `/v1/endpoints/${N.endpoint}`)).ok) return true;

  const quiet = { wire: () => {} };
  await put(quiet, "configs", N.config, {
    name: N.config,
    values: {
      base_url: state.status?.mockUrl ?? "http://localhost:9999",
      team: state.status?.provisioned?.workspaces?.a?.slug ?? "mandates",
    },
  });
  await put(quiet, "secrets", N.secret, { name: N.secret, value: "Bearer aarokya-demo-mandates-7d41c9" });
  await put(quiet, "payload-specs", N.payloadSpec, { name: N.payloadSpec, schema: MANDATE_SCHEMA });
  const spec = state.status?.mandateSpec;
  if (!spec) return false;
  return (await put(quiet, "endpoints", N.endpoint, spec)).ok;
}

// ─── the journeys ────────────────────────────────────────────────────────────

const RUN_STEPS = [
  { id: "ask", label: "you ask for a run" },
  { id: "written", label: "written down" },
  { id: "due", label: "waits until due" },
  { id: "claim", label: "one worker takes it" },
  { id: "call", label: "calls Aarokya" },
  { id: "answer", label: "the answer" },
  { id: "record", label: "recorded" },
];

const CRON_STEPS = [
  { id: "ask", label: "you ask for a schedule" },
  { id: "written", label: "written down" },
  { id: "due", label: "pg_cron owns it" },
  { id: "tick", label: "the database makes a row" },
  { id: "claim", label: "a worker takes it" },
  { id: "call", label: "calls Aarokya" },
  { id: "answer", label: "the answer" },
  { id: "record", label: "recorded" },
  { id: "cancel", label: "cancelled" },
];

const LONG_STEPS = [
  { id: "ask", label: "you ask for a run" },
  { id: "written", label: "written down" },
  { id: "claim", label: "a worker takes it" },
  { id: "send", label: "sends the work" },
  { id: "accepted", label: "202 · Location" },
  { id: "wait", label: "parked, WAITING" },
  { id: "poll", label: "checks back" },
  { id: "finish", label: "the terminal answer" },
  { id: "record", label: "recorded" },
];

// ─── the takes ───────────────────────────────────────────────────────────────

const takes = [
  // ── 1 · set it up ──────────────────────────────────────────────────────────
  {
    id: "setup",
    page: "setup",
    label: "Everything a job needs",
    action: "Build it, live",
    fields: [],
    async run(run) {
      const N = SETUP_NAMES();
      const spec = state.status?.mandateSpec;
      if (!spec) {
        run.say("the demo server has not provisioned yet — give it a moment", "bad");
        return false;
      }
      run.emit("spec", { body: spec });
      run.emit("slots-reset");

      // 1 — the workspace. Already there: it owns a PostgreSQL schema, and you
      // do not make one of those per demo run.
      const wsA = state.status?.provisioned?.workspaces?.a;
      run.slot("workspace", {
        cls: "done",
        name: `${state.status?.provisioned?.org_name ?? "Juspay"} / ${wsA?.name ?? "Mandates"}`,
        detail: `its own schema — <b>${esc((wsA?.schema_name ?? "").slice(-24) || "…")}</b>`,
        mark: "already here",
        text: "a workspace is a <b>PostgreSQL schema</b> — endpoints, jobs, executions, secrets, all its own",
        tone: "act",
      });

      // 2 — config: the values you would otherwise hardcode.
      run.slot("config", {
        cls: "doing",
        name: N.config,
        detail: "where Aarokya lives",
        text: "first, the things that change between environments",
        tone: "act",
      });
      const cfg = await put(run, "configs", N.config, {
        name: N.config,
        values: {
          base_url: state.status?.mockUrl ?? "http://localhost:9999",
          team: wsA?.slug ?? "mandates",
        },
      });
      run.slot("config", {
        cls: cfg.ok ? "done" : "bad",
        detail: "base_url, team — <b>2 values</b>",
        mark: cfg.mark,
        lights: "config",
        text: cfg.ok
          ? `two values, one row${cfg.existed ? " (already there, so the same call updated it)" : ""} — and every <b>{{config.*}}</b> in the endpoint now has something to resolve to`
          : `Invokr refused the config — <b>${cfg.status}</b>`,
        tone: cfg.ok ? "ok" : "bad",
      });
      if (!cfg.ok) return false;

      // 3 — secret: same idea, except it never comes back out.
      run.slot("secret", {
        cls: "doing",
        name: N.secret,
        detail: "the callback credential",
        text: "then the one value you cannot put in a config",
        tone: "act",
      });
      const sec = await put(
        run,
        "secrets",
        N.secret,
        { name: N.secret, value: `Bearer aarokya-demo-${wsA?.slug ?? "mandates"}-7d41c9` },
        { name: N.secret, value: "••••••••" },
      );
      const readBack = await api("GET", `/v1/secrets/${N.secret}`);
      run.wire({
        verb: "GET",
        path: `/v1/secrets/${N.secret}`,
        status: readBack.status,
        res: readBack.body,
        said: "no value field",
      });
      const leaked = JSON.stringify(readBack.body ?? {}).includes('"value"');
      run.slot("secret", {
        cls: leaked ? "bad" : sec.ok ? "done" : "bad",
        detail: "encrypted at rest — <b>write-only</b>",
        mark: sec.mark,
        lights: "secret",
        text: leaked
          ? "the API returned a secret value — <b>check this</b>"
          : "ask for it back and you get the name and the timestamps. <b>Never the value.</b>",
        tone: leaked ? "bad" : "ok",
      });
      if (!sec.ok || leaked) return false;

      // 4 — payload spec: the schema a job's input has to satisfy.
      run.slot("payload", {
        cls: "doing",
        name: N.payloadSpec,
        detail: "what a caller must send",
        text: "next, what a caller is allowed to ask for",
        tone: "act",
      });
      const ps = await put(run, "payload-specs", N.payloadSpec, { name: N.payloadSpec, schema: MANDATE_SCHEMA });
      run.slot("payload", {
        cls: ps.ok ? "done" : "bad",
        detail: "requires <b>mandate_id</b>",
        mark: ps.mark,
        lights: "input",
        text: ps.ok
          ? "JSON Schema. A job whose input does not match is <b>refused at create time</b>, not at 3am"
          : `Invokr refused the payload spec — <b>${ps.status}</b>`,
        tone: ps.ok ? "ok" : "bad",
      });
      if (!ps.ok) return false;

      // 5 — the endpoint itself, which is just those three plus the call.
      run.slot("endpoint", {
        cls: "doing",
        name: N.endpoint,
        detail: "the call itself",
        text: "and now the call — the same body your team already posts",
        tone: "act",
      });
      const ep = await put(run, "endpoints", N.endpoint, spec);
      run.slot("endpoint", {
        cls: ep.ok ? "done" : "bad",
        detail: "HTTP · POST · <b>3 tries, 1m → 10m</b>",
        mark: ep.mark,
        text: ep.ok
          ? "that is the whole setup — <b>four calls</b>, no deploy, and the endpoint is ready to be fired"
          : `Invokr refused the endpoint — <b>${ep.status}</b>`,
        tone: ep.ok ? "ok" : "bad",
      });
      return ep.ok;
    },
  },

  {
    id: "bad-payload",
    page: "setup",
    label: "What the payload spec is for",
    action: "Send a bad one",
    fields: [{ key: "mandate_id", label: "mandate", value: "MND-8842", width: 120 }],
    async run(run, v) {
      const N = SETUP_NAMES();
      await ensureSetup();
      run.emit("spec", { body: state.status?.mandateSpec });
      run.emit("slots-reset");
      run.slot("payload", {
        cls: "doing",
        name: N.payloadSpec,
        detail: "requires <b>mandate_id</b>",
        lights: "input",
        text: "the payload spec says a run must carry a <b>mandate_id</b>",
        tone: "act",
      });

      const bad = { trigger: "IMMEDIATE", endpoint: N.endpoint, input: { checks: "1" } };
      const res1 = await api("POST", "/v1/jobs", { body: bad });
      run.wire({ verb: "POST", path: "/v1/jobs", status: res1.status, req: bad, res: res1.body });
      const refused = res1.status === 400 || res1.status === 422;
      run.slot("payload", {
        cls: refused ? "done" : "bad",
        mark: String(res1.status),
        text: refused
          ? `refused with <b>${res1.status}</b>, before a worker ever saw it — the caller finds out now, not at 3am`
          : `expected a refusal, got <b>${res1.status}</b>`,
        tone: refused ? "ok" : "bad",
      });
      if (res1.ok) return false;

      const good = {
        trigger: "IMMEDIATE",
        endpoint: N.endpoint,
        idempotency_key: key(`${v.mandate_id}-good`),
        input: { mandate_id: v.mandate_id, checks: "1", fail_times: "0" },
      };
      const res2 = await api("POST", "/v1/jobs", { body: good });
      run.wire({ verb: "POST", path: "/v1/jobs", status: res2.status, req: good, res: res2.body });
      run.slot("endpoint", {
        cls: res2.ok ? "done" : "bad",
        name: N.endpoint,
        detail: "accepted, and already durable",
        mark: res2.ok ? "201" : String(res2.status),
        text: res2.ok
          ? "same call with the <b>mandate_id</b> in it — accepted, and on its way"
          : `Invokr refused the good one too — <b>${res2.status}</b>`,
        tone: res2.ok ? "ok" : "bad",
      });
      return res2.ok;
    },
  },

  // ── 2 · short tasks ────────────────────────────────────────────────────────
  {
    id: "short-task",
    page: "short",
    label: "One task, end to end",
    action: "Fire it",
    steps: (v) => (v.trigger === "CRON" ? CRON_STEPS : RUN_STEPS),
    fields: [
      { key: "mandate_id", label: "mandate", value: "MND-8842", width: 118 },
      {
        key: "trigger",
        label: "when",
        type: "select",
        value: "IMMEDIATE",
        width: 126,
        options: [
          { value: "IMMEDIATE", label: "now" },
          { value: "DELAYED", label: "in a few seconds" },
          { value: "CRON", label: "on a schedule" },
        ],
      },
      { key: "seconds", label: "seconds", value: "15", width: 48, show: (v) => v.trigger === "DELAYED" },
      {
        key: "cron",
        label: "every",
        type: "select",
        value: "* * * * *",
        width: 108,
        show: (v) => v.trigger === "CRON",
        options: [
          { value: "* * * * *", label: "minute" },
          { value: "*/2 * * * *", label: "2 minutes" },
          { value: "*/5 * * * *", label: "5 minutes" },
        ],
      },
      { key: "ticks", label: "stop after", value: "1", width: 40, show: (v) => v.trigger === "CRON" },
      { key: "fail_times", label: "failures first", value: "0", width: 40 },
      { key: "attempts", label: "max tries", value: "3", width: 40 },
    ],
    extras: [
      { id: "kill", label: "Kill a worker", danger: true },
      { id: "addworker", label: "Add a worker" },
    ],
    async onExtra(id, run) {
      if (id === "kill") {
        const res = await control("/worker/kill");
        if (res.body?.ok) {
          run?.emit("worker", { workerId: res.body.workerId, cls: "dead", label: `killed · pid ${res.body.pid}` });
          run?.say(`<b>killed</b> worker ${esc(short(res.body.workerId))} — SIGKILL, mid-transaction`, "bad");
          setTimeout(async () => {
            await refreshStatus();
            run?.emit("workers", { list: state.status?.workers ?? [] });
          }, 900);
        } else {
          run?.say(esc(res.body?.error ?? "nothing to kill"), "warn");
        }
        return;
      }
      if (id === "addworker") {
        const res = await control("/worker/start");
        await refreshStatus();
        run?.emit("workers", { list: state.status?.workers ?? [] });
        run?.say(res.body?.ok ? `a fresh worker is up — <b>pid ${res.body.pid}</b>` : esc(res.body?.error ?? "no"), "act");
      }
    },
    async run(run, v) {
      const N = SETUP_NAMES();
      await control("/mock/reset");
      await ensureSetup();
      const since = await targetLogHead();
      run.emit("target-is", { who: "Aarokya" });

      const failures = clamp(v.fail_times, 0, 2, 0);
      const attempts = clamp(v.attempts, 1, 3, 3);
      // The team's real policy waits a minute before the second try. When you
      // ask for failures, the same call runs with seconds instead, so the shape
      // is visible inside a demo slot.
      const endpoint = failures > 0 ? "aarokya-mandate-sync-impatient" : N.endpoint;
      run.emit("token", {
        id: "ep",
        zone: "endpoints",
        state: "spec",
        label: endpoint.replace("aarokya-", ""),
        note: failures > 0 ? "retries in seconds" : "registered",
      });
      if (failures > 0) {
        run.say(
          `<b>${failures} failure${failures === 1 ? "" : "s"}</b> first, and retries measured in seconds rather than the minutes the real policy uses`,
          "warn",
        );
      }

      const input = {
        mandate_id: `${v.mandate_id}-${Date.now().toString(36).slice(-4)}`,
        checks: "1",
        fail_times: String(failures),
      };

      if (v.trigger === "CRON") {
        const wanted = clamp(v.ticks, 1, 3, 1);
        const job = await invokePhase(run, {
          endpoint,
          input,
          trigger: "CRON",
          cron: v.cron || "* * * * *",
          label: v.mandate_id,
          token: "t1",
          maxAttempts: attempts,
        });
        if (!job) return false;
        state.cronJob = job.job_id;
        run.say("nothing is holding a connection open — the next tick is a row pg_cron will write", "warn");

        let ok = true;
        for (let n = 1; n <= wanted; n++) {
          const exec = await watchCronTick(run, job.job_id, { since, token: "t1", label: v.mandate_id, nth: n });
          if (!exec) {
            ok = false;
            break;
          }
        }
        const cancel = await api("POST", `/v1/jobs/${job.job_id}/cancel`);
        run.wire({ verb: "POST", path: `/v1/jobs/${job.job_id.slice(0, 8)}…/cancel`, status: cancel.status, res: cancel.body });
        state.cronJob = null;
        run.step("cancel", {
          token: { id: "t1", zone: "done", state: "ok", label: v.mandate_id, note: "cancelled" },
          text: `<b>cancelled</b> after ${wanted} tick${wanted === 1 ? "" : "s"} — which is what your service does when the status is terminal`,
          tone: "ok",
        });
        return ok && cancel.ok;
      }

      const seconds = clamp(v.seconds, 5, 120, 15);
      const runAt = v.trigger === "DELAYED" ? new Date(Date.now() + seconds * 1000) : null;
      const job = await invokePhase(run, {
        endpoint,
        input,
        runAt,
        label: v.mandate_id,
        token: "t1",
        maxAttempts: attempts,
      });
      if (!job) return false;

      if (runAt) {
        run.say("kill every worker now. The row does not care — <b>it is the timer</b>", "warn");
        countdownToken(run, "t1", new Date(job.execution.created_at).getTime() + seconds * 1000, "due in");
      }

      const done = await watchExecution(run, job.execution.execution_id, {
        targetSince: since,
        token: "t1",
        label: v.mandate_id,
        timeout: (seconds + 120) * 1000,
      });
      if (failures > 0 && done?.status === "SUCCESS") {
        run.say("look at Aarokya's own log: every try carried <b>one idempotency key</b>", "ok");
      }
      return done?.status === "SUCCESS";
    },
  },

  {
    id: "two-teams",
    page: "short",
    label: "Same name, two teams",
    action: "Fire into both",
    steps: RUN_STEPS,
    fields: [{ key: "mandate_id", label: "mandate", value: "MND-4410", width: 118 }],
    async run(run, v) {
      const N = SETUP_NAMES();
      await control("/mock/reset");
      await ensureSetup();
      const since = await targetLogHead();
      run.emit("target-is", { who: "Aarokya" });

      let ok = true;
      let i = 0;
      for (const wsKey of ["a", "b"]) {
        i++;
        const team = state.status?.provisioned?.workspaces?.[wsKey];
        run.emit("token", {
          id: `t${i}`,
          zone: "app",
          label: team?.name ?? wsKey,
          note: (team?.schema_name ?? "").slice(-14),
        });
        const job = await invokePhase(run, {
          endpoint: N.endpoint,
          input: { mandate_id: `${v.mandate_id}-${wsKey}`, checks: "1", fail_times: "0" },
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
          timeout: 40000,
          targetSince: since,
          token: `t${i}`,
          label: team?.name ?? wsKey,
        });
        ok = ok && done?.status === "SUCCESS";
      }
      run.say(
        "same endpoint name, same call — <b>different team</b> in Aarokya's log, because the config is each workspace's own",
        ok ? "ok" : "warn",
      );
      return ok;
    },
  },

  {
    id: "any-transport",
    page: "short",
    label: "Not just HTTP",
    action: "Send it three ways",
    steps: RUN_STEPS,
    fields: [{ key: "mandate_id", label: "mandate", value: "MND-7781", width: 118 }],
    async run(run, v) {
      const probes = state.status?.transports ?? {};
      await ensureSetup();
      const targets = [
        { type: "HTTP", endpoint: SETUP_NAMES().endpoint, up: true },
        { type: "Kafka", endpoint: "mandate-events-kafka", up: !!probes.kafka },
        { type: "Redis", endpoint: "mandate-events-redis", up: !!probes.redis },
      ];

      let ok = true;
      let i = 0;
      for (const t of targets) {
        i++;
        if (!t.up) {
          // Deliberately not a journey step: nothing travelled, and marking one
          // would stamp a later time on a step the HTTP pass already walked.
          run.emit("token", { id: `t${i}`, zone: "app", state: "waiting", label: t.type, note: "broker not running" });
          run.say(`<b>${esc(t.type)}</b> is not running here — same job, nowhere to put it`, "warn");
          continue;
        }
        run.emit("target-is", { who: t.type === "HTTP" ? "Aarokya" : `the ${t.type.toLowerCase()} side` });
        const job = await invokePhase(run, {
          endpoint: t.endpoint,
          input: { mandate_id: `${v.mandate_id}-${i}`, checks: "1", fail_times: "0" },
          label: t.type,
          token: `t${i}`,
          maxAttempts: 1,
        });
        if (!job) {
          ok = false;
          continue;
        }
        const done = await watchExecution(run, job.execution.execution_id, { timeout: 40000, token: `t${i}`, label: t.type });
        ok = ok && done?.status === "SUCCESS";
      }
      run.say("same job, same retries — <b>the destination is a field</b>, not a rewrite", ok ? "ok" : "warn");
      return ok;
    },
  },

  // ── 3 · long-running ───────────────────────────────────────────────────────
  {
    id: "long-running",
    page: "long",
    label: "Work that takes minutes",
    action: "Start the long job",
    steps: LONG_STEPS,
    tape: () => `long-running-${asyncValues().mode}`,
    fields: [{ key: "job", label: "job", value: "recon-0042", width: 118 }],
    async run(run, v) {
      if (!state.status?.longRunning) {
        run.halt(
          "this build has no long-running support — it lives on <b>feat/long-running-jobs</b>. Switch on replay to watch a recorded run.",
        );
        return false;
      }
      return await runLongJob(run, v);
    },
  },
];

const takesIn = (page) => takes.filter((t) => t.page === page);

// ─── page 3's run ────────────────────────────────────────────────────────────
//
// The async block is a form, so the endpoint is written fresh for each run. The
// arrows come from real rows: `polls` for each check-in, `attempts` for the
// original send, and Aarokya's own log for the callback it makes.

const ASYNC_FORM = [
  {
    key: "mode",
    label: "mode",
    type: "select",
    value: "poll",
    options: [
      { value: "poll", label: "Invokr polls" },
      { value: "callback", label: "Aarokya calls back" },
      { value: "both", label: "both — first one wins" },
    ],
    note: "at least one of <code>poll</code> or <code>callback</code> must be present · in replay this picks the tape",
  },
  { key: "initial_delay_ms", label: "poll.initial_delay_ms", value: "1000", width: 78, hide: (v) => v.mode === "callback" },
  { key: "max_delay_ms", label: "poll.max_delay_ms", value: "10000", width: 78, hide: (v) => v.mode === "callback" },
  {
    key: "backoff",
    label: "poll.backoff",
    type: "select",
    value: "exponential",
    hide: (v) => v.mode === "callback",
    options: [
      { value: "exponential", label: "exponential" },
      { value: "linear", label: "linear" },
      { value: "fixed", label: "fixed" },
    ],
  },
  { key: "max_polls", label: "max_polls", value: "20", width: 60, hide: (v) => v.mode === "callback" },
  { key: "max_wait_ms", label: "max_wait_ms", value: "120000", width: 78 },
  { sep: "what Aarokya does" },
  { key: "pending", label: "202s before it finishes", value: "3", width: 60, hide: (v) => v.mode === "callback" },
  { key: "retry_after", label: "Retry-After (s)", value: "2", width: 60, hide: (v) => v.mode === "callback" },
  { key: "callback_after", label: "calls back after (s)", value: "5", width: 60, hide: (v) => v.mode === "poll" },
];

function asyncValues() {
  if (!state.values.__async) {
    state.values.__async = Object.fromEntries(ASYNC_FORM.filter((f) => f.key).map((f) => [f.key, f.value]));
  }
  const v = state.values.__async;
  for (const f of ASYNC_FORM) {
    if (!f.key) continue;
    const el = document.getElementById(`a-${f.key}`);
    if (el) v[f.key] = (el.value ?? "").trim() || f.value;
  }
  return v;
}

function renderAsyncForm() {
  const v = asyncValues();
  $("#async-form").innerHTML = ASYNC_FORM.map((f) => {
    if (f.sep) return `<div class="form-sep">${esc(f.sep)}</div>`;
    if (f.hide?.(v)) return "";
    // In replay the values are whatever was recorded — except `mode`, which
    // chooses which recording to play.
    const disabled = state.replay && f.key !== "mode" ? "disabled" : "";
    const input =
      f.type === "select"
        ? `<select id="a-${f.key}" ${disabled}>${f.options
            .map((o) => `<option value="${esc(o.value)}" ${v[f.key] === o.value ? "selected" : ""}>${esc(o.label)}</option>`)
            .join("")}</select>`
        : `<input id="a-${f.key}" value="${esc(v[f.key] ?? f.value)}" style="--w:${f.width ?? 78}px" ${disabled} />`;
    return `<label class="form-row"><span class="fk">${esc(f.label)}</span>${input}
      ${f.note ? `<span class="fn">${f.note}</span>` : ""}</label>`;
  }).join("");

  $("#async-form")
    .querySelectorAll("input, select")
    .forEach((el) =>
      el.addEventListener("change", () => {
        asyncValues();
        renderAsyncForm();
      }),
    );
}

/// Build the `async` block the form describes.
function asyncSpecFrom(v) {
  const block = { status_codes: [202] };
  if (v.mode !== "callback") {
    block.poll = {
      success_statuses: [200],
      pending_statuses: [202],
      failure_statuses: [400, 404, 410],
      initial_delay_ms: clamp(v.initial_delay_ms, 100, 60000, 1000),
      max_delay_ms: clamp(v.max_delay_ms, 100, 600000, 10000),
      backoff: v.backoff || "exponential",
    };
    block.max_polls = clamp(v.max_polls, 1, 100, 20);
  }
  // A plain boolean, not `{ enabled: true }` — the design doc says the latter,
  // the implementation takes the former, and the implementation is what runs.
  if (v.mode !== "poll") block.callback = true;
  block.max_wait_ms = clamp(v.max_wait_ms, 1000, 1800000, 120000);
  return block;
}

async function runLongJob(run, v) {
  const a = asyncValues();
  const asyncBlock = asyncSpecFrom(a);
  const polls = a.mode === "callback" ? 0 : clamp(a.pending, 0, 8, 3);
  const retryAfter = clamp(a.retry_after, 1, 30, 2);
  const callbackAfter = clamp(a.callback_after, 1, 120, 5);

  await control("/mock/reset");
  const since = await targetLogHead();
  run.emit("target-is", { who: "Aarokya" });

  // The target answers 202 and then either waits to be asked or calls back.
  const body = { job: "{{input.job}}" };
  if (a.mode !== "callback") {
    body.script = [
      ...Array.from({ length: polls }, () => ({ status: 202, body: { state: "working" }, retry_after: retryAfter })),
      { status: 200, body: { state: "done", mandates: 128_000 } },
    ];
  }
  if (a.mode !== "poll") {
    body.callback_url = "{{execution.callback_url}}";
    // The target is the one calling Invokr, so it needs Invokr's key — held
    // where credentials belong rather than pasted into the endpoint spec.
    body.callback_auth = `{{secret.${state.status?.callbackKey ?? "invokr-api-key"}}}`;
    body.callback_after_ms = callbackAfter * 1000;
  }

  const spec = {
    name: "aarokya-bulk-recon",
    type: "HTTP",
    config: SETUP_NAMES().config,
    spec: {
      url: "{{config.base_url}}/async/start",
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body_template: body,
      timeout_ms: 5000,
      expected_status_codes: [200],
      async: asyncBlock,
    },
    retry_policy: { max_attempts: 2, backoff: "exponential", initial_delay_ms: 2000, max_delay_ms: 10000 },
  };

  await ensureSetup(); // the endpoint borrows page 1's config and secret
  const exists = (await api("GET", `/v1/endpoints/${spec.name}`)).ok;
  const { name: _n, ...rest } = spec;
  const saved = exists
    ? await api("PUT", `/v1/endpoints/${spec.name}`, { body: rest })
    : await api("POST", "/v1/endpoints", { body: spec });
  run.wire({ verb: exists ? "PUT" : "POST", path: `/v1/endpoints/${spec.name}`, status: saved.status, req: spec, res: saved.body });
  if (!saved.ok) {
    run.say(`Invokr refused the endpoint — <b>${saved.status}</b>`, "bad");
    return false;
  }

  const job = await invokePhaseQuiet(run, {
    endpoint: spec.name,
    input: { job: v.job },
    asyncOverrides: { max_wait_ms: asyncBlock.max_wait_ms, ...(asyncBlock.max_polls ? { max_polls: asyncBlock.max_polls } : {}) },
  });
  if (!job) return false;

  return await watchLongRunning(run, job.execution.execution_id, {
    since,
    maxPolls: asyncBlock.max_polls,
    mode: a.mode,
  });
}

/// Page 3 has no board, so the first two steps are narration plus one arrow.
async function invokePhaseQuiet(run, { endpoint, input, asyncOverrides }) {
  const body = {
    trigger: "IMMEDIATE",
    endpoint,
    idempotency_key: key(`${input.job}-long`),
    input,
    ...(asyncOverrides ? { async_overrides: asyncOverrides } : {}),
  };
  run.step("ask", { text: "you ask for a run — <b>immediate</b>", tone: "act" });
  const t0 = performance.now();
  const res = await api("POST", "/v1/jobs", { body });
  run.wire({ verb: "POST", path: "/v1/jobs", status: res.status, req: body, res: res.body });
  if (!res.ok) {
    run.step("written", { bad: true, text: `Invokr refused it — <b>${res.status}</b>`, tone: "bad" });
    return null;
  }
  run.step("written", {
    text: `Invokr answered in ${ms(performance.now() - t0)} — one row, already durable`,
    tone: "act",
  });
  run.emit("exec", { status: "QUEUED", polls: 0 });
  return res.body.data;
}

/// Follow a long-running execution, drawing one arrow per real call.
async function watchLongRunning(run, executionId, { since, maxPolls, mode }) {
  const started = performance.now();
  const offset = (iso) => Math.max(0, new Date(iso) - run.wallT0);
  let claimed = false;
  let sentAt = null;
  let seenPolls = 0;
  let logSeq = since;
  let sawCallback = false;

  const tail = async () => {
    try {
      const res = await fetch(`/control/mock/log?since=${logSeq}&limit=30`);
      const entries = ((await res.json())?.data ?? []).slice().reverse();
      if (!entries.length) return;
      logSeq = Math.max(logSeq, ...entries.map((e) => e.seq));
      run.emit("receipt", { entries });
      for (const e of entries) {
        if (e.path !== "(callback)" || sawCallback) continue;
        sawCallback = true;
        run.msg({
          dir: "in",
          verb: "POST",
          path: "/v1/callbacks/…/complete",
          at: offset(e.at),
          note: "Aarokya finished and said so",
          tone: "ok",
          text: "the other side <b>called back</b> — nobody polled anything",
          id: "poll",
        });
      }
    } catch {
      /* the target's log is a nicety, never a dependency */
    }
  };

  while (performance.now() - started < 240000 && !run.cancelled) {
    const exec = (await api("GET", `/v1/executions/${executionId}`)).body?.data;
    if (exec) {
      if (!claimed && exec.attempt_count > 0) {
        claimed = true;
        run.step("claim", {
          text: exec.worker_id
            ? `worker <b>${esc(short(exec.worker_id))}</b> took it`
            : "<b>one worker</b> took it",
          tone: "act",
        });
        run.emit("exec", { status: exec.status, polls: exec.poll_count ?? 0, maxPolls });
      }

      const attempts = (await api("GET", `/v1/executions/${executionId}/attempts`)).body?.data ?? [];
      const first = attempts.find((x) => x.attempt_number === 1);
      if (first && sentAt == null) {
        sentAt = offset(first.started_at);
        run.msg({
          dir: "out",
          verb: "POST",
          path: "/async/start",
          at: sentAt,
          note: "here is the work",
          text: "the worker sends the work — one <b>attempt</b>, and the only one there will be",
          tone: "act",
          id: "send",
        });
        run.msg({
          dir: "in",
          code: 202,
          path: "Location: /async/status/…",
          at: offset(first.completed_at ?? first.started_at),
          note: "accepted, still working",
          text: "<b>202</b> is not success and not failure — and the <b>Location</b> is where to check back",
          tone: "act",
          id: "accepted",
        });
        run.emit("step", {
          id: "wait",
          at: offset(first.completed_at ?? first.started_at),
          text:
            mode === "callback"
              ? "the row is parked <b>WAITING</b> — no connection is being held open, and nobody is polling"
              : "the row is parked <b>WAITING</b> — no connection is being held open",
          tone: "warn",
        });
        run.emit("exec", { status: "WAITING", polls: exec.poll_count ?? 0, maxPolls, note: "no connection held open" });
      }

      // Real poll rows, when the build has them.
      const pollRows = (await api("GET", `/v1/executions/${executionId}/polls`)).body?.data ?? [];
      const ordered = [...pollRows].sort((a, b) => a.poll_number - b.poll_number);
      for (const p of ordered.slice(seenPolls)) {
        const pending = p.classification === "PENDING";
        run.msg({
          dir: "out",
          verb: "GET",
          path: "/async/status/…",
          at: offset(p.polled_at),
          note: `check ${p.poll_number}`,
          tone: "act",
          text: `check ${p.poll_number} — Invokr asks, and is not holding anything open while it waits`,
        });
        run.msg({
          dir: "in",
          code: p.status_code ?? undefined,
          path: pending ? "still working" : p.classification.toLowerCase().replace("_", " "),
          at: offset(p.polled_at) + (p.duration_ms ?? 0),
          note: p.retry_after_ms ? `Retry-After ${Math.round(p.retry_after_ms / 1000)}s` : "",
          tone: pending ? "warn" : p.classification === "SUCCESS" ? "ok" : "bad",
          text: pending
            ? `check ${p.poll_number}: <b>${p.status_code}</b>, still working — asks again in ${ms(p.retry_after_ms ?? 0)}`
            : `check ${p.poll_number}: <b>${p.status_code}</b> — ${esc(p.classification.toLowerCase())}, and that is the end of it`,
          id: "poll",
          exec: { status: exec.status, polls: p.poll_number, maxPolls, note: "polls are not attempts" },
        });
      }
      seenPolls = ordered.length;

      await tail();

      if (["SUCCESS", "FAILED", "CANCELLED"].includes(exec.status)) {
        await tail();
        const ok = exec.status === "SUCCESS";
        run.step("finish", {
          bad: !ok,
          text: ok
            ? seenPolls > 0
              ? `finished after <b>${seenPolls} check-in${seenPolls === 1 ? "" : "s"}</b> — and still <b>one attempt</b>`
              : "finished because the other side <b>called back</b> — still one attempt"
            : `it ended as <b>${exec.status.toLowerCase()}</b>`,
          tone: ok ? "ok" : "bad",
        });
        run.emit("exec", {
          status: exec.status,
          polls: seenPolls,
          maxPolls,
          note: `${exec.attempt_count ?? 1} attempt · ${seenPolls} poll${seenPolls === 1 ? "" : "s"}`,
        });
        run.step("record", {
          bad: !ok,
          text: ok
            ? "recorded — the attempt, every poll, the response and the key, all queryable"
            : "recorded, with why it stopped",
          tone: ok ? "ok" : "bad",
        });
        return ok;
      }
    }
    await sleep(200);
  }
  run.halt("it did not finish inside the time we waited", "warn");
  return false;
}

// ─── the shell ───────────────────────────────────────────────────────────────

const PAGES = [
  { id: "setup", n: 1, label: "Set it up" },
  { id: "short", n: 2, label: "Short tasks" },
  { id: "long", n: 3, label: "Long-running" },
];

function renderPages() {
  $("#pages").innerHTML = PAGES.map(
    (p) => `<button data-page="${p.id}" class="${state.page === p.id ? "on" : ""}">
      <span class="n">${p.n}</span>${esc(p.label)}</button>`,
  ).join("");
  $("#pages")
    .querySelectorAll("button")
    .forEach((b) => b.addEventListener("click", () => setPage(b.dataset.page)));
}

function renderTakes() {
  const list = takesIn(state.page);
  $("#takes").style.display = list.length > 1 ? "" : "none";
  $("#takes").innerHTML = list
    .map(
      (t, i) =>
        `<button class="take ${i === state.current ? "on" : ""}" data-i="${i}">${esc(t.label)}${
          state.ran.has(t.id) ? `<span class="done-tick">●</span>` : ""
        }</button>`,
    )
    .join("");
  $("#takes")
    .querySelectorAll(".take")
    .forEach((b) => b.addEventListener("click", () => mountTake(Number(b.dataset.i))));
}

function valuesFor(take) {
  if (!state.values[take.id]) {
    state.values[take.id] = Object.fromEntries((take.fields ?? []).map((f) => [f.key, f.value]));
  }
  return state.values[take.id];
}

function readValues(take) {
  const v = valuesFor(take);
  for (const f of take.fields ?? []) {
    const el = document.getElementById(`f-${f.key}`);
    if (el) v[f.key] = (el.value ?? "").trim() || f.value;
  }
  return v;
}

const currentTake = () => takesIn(state.page)[state.current];

/// Which recording belongs to this take. Page 3's three modes tell three
/// different stories, so each keeps its own tape.
const tapeId = (take) => (take.tape ? take.tape() : take.id);
const stepsOf = (take) => (typeof take?.steps === "function" ? take.steps(valuesFor(take)) : take?.steps);

function resetStage() {
  clearTokens();
  clearWire();
  clearLanes();
  narrate(null, "");
  $("#z-target").classList.remove("hit", "refused");
  $("#cron-badge").classList.remove("tick");
  $("#drop-arrow").classList.remove("on");
  for (const id of workerBoxes.keys()) setWorkerState(id, null, "");
  renderWorkers(state.status?.workers ?? []);
  renderSlots();
  renderSpecCard();
}

function setPage(page) {
  if (state.page === page) return;
  state.page = page;
  state.current = 0;
  renderPages();
  mountTake(0);
}

function renderInputs(take) {
  const v = valuesFor(take);
  const shown = (take.fields ?? []).filter((f) => !f.show || f.show(v));
  $("#inputs").innerHTML = shown
    .map((f) => {
      const disabled = state.replay ? "disabled" : "";
      const input =
        f.type === "select"
          ? `<select id="f-${f.key}" style="--w:${f.width ?? 140}px" ${disabled}>
               ${f.options
                 .map((o) => `<option value="${esc(o.value)}" ${String(v[f.key]) === o.value ? "selected" : ""}>${esc(o.label)}</option>`)
                 .join("")}
             </select>`
          : `<input id="f-${f.key}" value="${esc(v[f.key] ?? f.value)}" style="--w:${f.width ?? 140}px" ${disabled} />`;
      return `<div class="field"><label for="f-${f.key}">${esc(f.label)}</label>${input}</div>`;
    })
    .join("");

  // A field can decide which other fields matter, so re-render on change — and
  // the journey can change shape with them.
  $("#inputs")
    .querySelectorAll("input, select")
    .forEach((el) =>
      el.addEventListener("change", () => {
        readValues(take);
        renderInputs(take);
        renderPips(take);
      }),
    );
}

function mountTake(i) {
  if (state.run) state.run.cancelled = true;
  state.run = null;
  film.playing = false;
  resetFilm();

  const list = takesIn(state.page);
  state.current = Math.max(0, Math.min(list.length - 1, i));
  const take = list[state.current];

  $("#view-setup").classList.toggle("on", state.page === "setup");
  $("#view-short").classList.toggle("on", state.page === "short");
  $("#view-long").classList.toggle("on", state.page === "long");

  resetStage();
  renderPips(take);
  $("#target-label").textContent = "Aarokya";
  if (state.page === "long") renderAsyncForm();

  renderInputs(take);
  $("#extras").innerHTML = (take.extras ?? [])
    .map(
      (a) =>
        `<button class="${a.danger ? "danger" : ""}" data-act="${esc(a.id)}" ${state.replay ? "disabled" : ""}>${esc(
          a.label,
        )}</button>`,
    )
    .join("");
  $("#extras")
    .querySelectorAll("[data-act]")
    .forEach((b) =>
      b.addEventListener("click", async () => {
        b.disabled = true;
        try {
          await take.onExtra?.(b.dataset.act, ensureRun(take));
        } finally {
          b.disabled = false;
        }
      }),
    );

  const go = $("#go");
  go.style.display = take.action ? "" : "none";
  go.textContent = state.replay ? "Play the recording" : (take.action ?? "");
  go.disabled = false;

  narrate(null, `<span style="color:var(--faint)">ready</span>`);
  renderTakes();
  syncTransport();
  fitBoard();
}

function ensureRun(take) {
  if (state.run && !state.run.cancelled) return state.run;
  const run = new Run(take, { replay: state.replay });
  state.run = run;
  return run;
}

async function runTake() {
  const take = currentTake();
  if (!take?.action) return;
  const go = $("#go");
  const values = readValues(take);

  go.disabled = true;
  go.textContent = "running…";
  resetStage();
  resetFilm();
  renderPips(take);

  const run = new Run(take);
  state.run = run;
  run.emit("workers", { list: state.status?.workers ?? [] });

  const walking = play(take);
  let ok = false;
  try {
    ok = await take.run(run, values);
  } catch (err) {
    run.say(`something broke here: <b>${esc(String(err.message ?? err))}</b>`, "bad");
  }
  film.closed = true;
  await walking;
  if (ok) settlePips();

  if (ok) state.ran.add(take.id);
  await run.save(ok);
  renderTakes();

  go.disabled = false;
  go.textContent = take.action;
  if (!ok && !run.halted) run.say("that did not finish — switch on replay if you need this now", "warn");
}

async function replayTake() {
  const take = currentTake();
  if (!take?.action) return;
  const go = $("#go");
  go.disabled = true;
  go.textContent = "playing…";

  let tape;
  try {
    const res = await fetch(`/control/recordings/${tapeId(take)}`);
    if (!res.ok) throw new Error("no recording of this one yet");
    tape = await res.json();
  } catch (err) {
    narrate(0, `${esc(String(err.message ?? err))} — run it live once and it is kept`, "bad");
    go.disabled = false;
    go.textContent = "Play the recording";
    return;
  }

  resetStage();
  resetFilm();
  renderPips(take);
  const run = new Run(take, { replay: true });
  state.run = run;

  const walking = play(take);
  // Recorded offsets decide when an event becomes available; the controller
  // still decides when it is drawn.
  const startedAt = performance.now();
  for (const ev of tape.events ?? []) {
    if (run.cancelled) return;
    const due = startedAt + ev.t - performance.now();
    if (due > 0) await sleep(due);
    deliver(ev);
  }
  film.closed = true;
  await walking;
  settlePips();

  go.disabled = false;
  go.textContent = "Play the recording";
  state.ran.add(take.id);
  renderTakes();
}

function setReplay(on) {
  state.replay = on;
  document.body.classList.toggle("replaying", on);
  $("#replay-toggle").classList.toggle("on", on);
  mountTake(state.current);
}

// ─── boot ────────────────────────────────────────────────────────────────────

$("#go").addEventListener("click", () => (state.replay ? replayTake() : runTake()));
$("#replay-toggle").addEventListener("click", () => setReplay(!state.replay));
$("#t-next").addEventListener("click", stepNext);
$("#t-back").addEventListener("click", stepBack);
$("#t-auto").addEventListener("click", () => setMode(film.mode === "auto" ? "manual" : "auto"));
addEventListener("resize", fitBoard);

document.addEventListener("keydown", (e) => {
  if (e.metaKey || e.ctrlKey) return;
  const typing = e.target.tagName === "INPUT" || e.target.tagName === "SELECT";

  if (e.key === "Enter") {
    e.preventDefault();
    return (state.replay ? replayTake : runTake)();
  }
  if (typing) return;
  if (e.key === "ArrowRight" || e.key === " ") {
    e.preventDefault();
    return stepNext();
  }
  if (e.key === "ArrowLeft") return stepBack();
  if (e.key === "1") return setPage("setup");
  if (e.key === "2") return setPage("short");
  if (e.key === "3") return setPage("long");
  if (e.key.toLowerCase() === "a") return setMode(film.mode === "auto" ? "manual" : "auto");
  if (e.key.toLowerCase() === "r") return setReplay(!state.replay);
  if (e.key === "Tab") {
    e.preventDefault();
    mountTake(state.current + 1 >= takesIn(state.page).length ? 0 : state.current + 1);
  }
});

layoutBoard();
renderSlots();
renderPages();
setMode("manual");
await refreshStatus();
setInterval(refreshStatus, 4000);
mountTake(0);
fitBoard();
