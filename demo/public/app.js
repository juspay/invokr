// Invokr live demo.
//
// The machine stays on screen and the work moves through it. Four regions in
// the order the work travels — your service, executions, workers, the target —
// and one edge that goes back the way it came. A job is a chip with its real id
// on it; a retry, a cron tick and a long-running poll are all that same circuit
// walked again. The panel on the left is whatever is executing at that moment.
//
// Three rules hold it together:
//
//   1. A take's `run()` never touches the DOM. It emits two kinds of event:
//      `step` (move along the rail) and `facts` (here is more real data —
//      including where the chip is now). One renderer draws them, which is what
//      lets a recorded run replay through exactly the same code.
//   2. The board is a pure function of the facts gathered so far, so stepping
//      backwards is just drawing fewer events into a fresh bag. Chips are
//      reconciled by id rather than replaced, so one travels rather than
//      blinking from region to region.
//   3. Nothing advances until you say so. `auto` holds each step for a beat,
//      and that beat is a *floor*: a 15-second wait still takes fifteen
//      seconds, and every step keeps the time it really happened at.

const $ = (sel, root = document) => root.querySelector(sel);

const state = {
  status: null,
  page: "setup",
  current: 0,
  replay: false,
  recording: true,
  ran: new Set(),
  run: null,
  values: {},
  stepId: null,
  facts: {},
};

const esc = (s) =>
  String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

const ms = (n) => (n == null ? "—" : n < 1000 ? `${Math.round(n)}ms` : `${(n / 1000).toFixed(1)}s`);
const stamp = (t) => (t == null ? "" : t < 1000 ? `${Math.round(t)}ms` : `${(t / 1000).toFixed(1)}s`);
const clock = (t) => new Date(t).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
const short = (id) => (id ? String(id).replace("worker_", "").slice(0, 6) : "—");
const sleep = (t) => new Promise((r) => setTimeout(r, t));
const clamp = (v, lo, hi, dflt) => Math.max(lo, Math.min(hi, Number(v) || dflt));
const key = (p) => `${p}-${Date.now().toString(36)}`;

/// Anything that reads like a credential is masked before it reaches the screen
/// or a saved tape — including the resolved value a target echoes back in its
/// own log. A `{{…}}` placeholder is left alone: it is the point, not a leak.
const maskSecrets = (text) =>
  String(text).replace(
    /(\\?"[a-z0-9_-]*(?:authorization|auth|api[_-]?key|token|secret|password)\\?"\s*:\s*\\?")(?!\{\{)([^"\\]*)/gi,
    "$1••••••••",
  );

/// An attempt's recorded `output.body` is whatever the other side sent, kept as
/// a string. Parse it when it is JSON so a frame can read the answer.
function asJson(v) {
  if (typeof v !== "string") return v;
  try {
    return JSON.parse(v);
  } catch {
    return v;
  }
}

const json = (v) => esc(typeof v === "string" ? v : JSON.stringify(v, null, 2));

function tokens(text) {
  return text
    .replace(/\{\{input\.[^}]+\}\}/g, (m) => `<span class="tok-input">${m}</span>`)
    .replace(/\{\{config\.[^}]+\}\}/g, (m) => `<span class="tok-config">${m}</span>`)
    .replace(/\{\{secret\.[^}]+\}\}/g, (m) => `<span class="tok-secret">${m}</span>`)
    .replace(/\{\{execution\.[^}]+\}\}/g, (m) => `<span class="tok-exec">${m}</span>`);
}

// ─── the panel vocabulary ────────────────────────────────────────────────────
//
// What the left-hand panel is built from. The machine is on the stage; this is
// the artefact of the beat — the SQL, the request, the rows.

const F = {
  wrap: (...kids) => `<div class="f">${kids.filter(Boolean).join("")}</div>`,
  lead: (html) => `<p class="f-lead">${html}</p>`,
  note: (html, tone) => `<p class="f-note ${tone ?? ""}">${html}</p>`,
  pane: (title, body, { meta, metaTone, cls } = {}) =>
    `<div class="f-pane ${cls ?? ""}">
       <div class="f-pane-h"><span>${esc(title)}</span>${meta ? `<span class="meta ${metaTone ?? ""}">${esc(meta)}</span>` : ""}</div>
       ${body}
     </div>`,
  code: (html, cls) => `<pre class="f-code ${cls ?? ""}">${html}</pre>`,
  bigs: (...items) => `<div class="f-bigs">${items.join("")}</div>`,
  big: (v, k, tone) => `<div class="f-big ${tone ?? ""}"><span class="v">${esc(String(v))}</span><span class="k">${esc(k)}</span></div>`,

  table: (name, note, cols, rows) => `<div class="f-table">
      <div class="cap"><span class="tname">${esc(name)}</span><span class="tnote">${esc(note)}</span></div>
      <table>
        <thead><tr>${cols.map((c) => `<th>${esc(c.label)}</th>`).join("")}</tr></thead>
        <tbody>${
          rows.length
            ? rows.map((r) => `<tr>${cols.map((c) => `<td>${r[c.k] ?? "—"}</td>`).join("")}</tr>`).join("")
            : `<tr><td colspan="${cols.length}" class="none">no rows yet</td></tr>`
        }</tbody>
      </table>
    </div>`,

  status: (s) => (s ? `<span class="st ${String(s).toLowerCase()}">${esc(s)}</span>` : "—"),
};

/// The countdown that rides on a chip's face while its row is not yet due.
const chipBar = (left, total) =>
  `<span class="bar"><i style="width:${total ? Math.max(0, Math.min(100, (1 - left / total) * 100)) : 0}%"></i></span>`;

/// Which box a chip belongs in. A row that has gone back to QUEUED for a retry
/// has no `worker_id` yet, so the honest answer is "a worker has it" rather
/// than naming one — the whole zone, not a box inside it.
const workerAnchor = (id) => (id ? `w:${id}` : "workers");

/// Claiming the row and stamping `worker_id` on it are two writes, and the
/// attempt can be visible before the second lands. Give it a few hundred
/// milliseconds to appear rather than saying "a worker" when we could say
/// which one — but never block the walk waiting for it.
async function whoClaimed(executionId, ws, known) {
  if (known) return known;
  for (let i = 0; i < 5; i++) {
    const row = (await api("GET", `/v1/executions/${executionId}`, { ws })).body?.data;
    if (row?.worker_id) return row.worker_id;
    if (["SUCCESS", "FAILED", "CANCELLED"].includes(row?.status)) return row?.worker_id ?? null;
    await sleep(120);
  }
  return null;
}

/// The worker boxes, from the processes actually running. `winner` marks the
/// one that claimed the row; everyone else says what they did instead, which
/// is the honest answer and also the interesting one.
function workerSlots(winner, verb = "skipped it — locked") {
  return (state.status?.workers ?? []).map((w) => ({
    id: w.workerId ?? String(w.pid),
    short: short(w.workerId) === "—" ? `pid ${w.pid}` : short(w.workerId),
    state: !winner ? "asking for due rows" : w.workerId === winner ? "got the row" : verb,
    cls: !winner ? "" : w.workerId === winner ? "on" : "skip",
  }));
}

// ─── the rail ────────────────────────────────────────────────────────────────

function renderSteps(take) {
  const steps = stepsOf(take) ?? [];
  $("#rail").innerHTML = steps
    .map(
      (s) => `<li class="step" data-step="${esc(s.id)}">
        <span class="dot"></span>
        <span class="title">${esc(s.label)}<span class="at"></span></span>
      </li>`,
    )
    .join("");
  updateStepCount();
}

function markStep(id, cls, t) {
  const host = $("#rail");
  const el = host.querySelector(`[data-step="${id}"]`);
  if (!el) return;
  host.querySelectorAll(".step.now").forEach((n) => {
    n.classList.remove("now");
    if (!n.classList.contains("bad")) n.classList.add("done");
  });
  el.classList.remove("bad");
  el.classList.add(cls ?? "now");
  if (t != null) {
    el.dataset.at = String(Math.max(t, priorStepTime(el)));
    el.querySelector(".at").textContent = stamp(Number(el.dataset.at));
  }
  el.scrollIntoView({ inline: "nearest", block: "nearest", behavior: "smooth" });
  updateStepCount();
}

/// Two clocks feed the times on screen: steps the page stamps as it walks, and
/// steps taken from a row's own timestamp on the database's clock. They agree
/// to within a few milliseconds — enough, when a journey is walked twice, to
/// render a claim a hair before the write that produced it. A step is only ever
/// nudged up to the one it followed; nothing is invented.
function priorStepTime(el) {
  const all = [...$("#rail").querySelectorAll(".step")];
  const before = all
    .slice(0, all.indexOf(el))
    .reverse()
    .find((s) => s.dataset.at !== undefined);
  return before ? Number(before.dataset.at) : 0;
}

function settleSteps() {
  $("#rail")
    .querySelectorAll(".step.now")
    .forEach((n) => {
      n.classList.remove("now");
      n.classList.add("done");
    });
}

function updateStepCount() {
  const host = $("#rail");
  const all = host.querySelectorAll(".step").length;
  if (!all) return ($("#step-count").textContent = "");
  const done = host.querySelectorAll(".step.done, .step.now, .step.bad").length;
  $("#step-count").textContent = `${done} / ${all}`;
}

// ─── the stage ───────────────────────────────────────────────────────────────
//
// The machine stays on screen. Every step redraws the same regions — only what
// is lit, and where the work is sitting, changes. Chips are reconciled by id
// rather than replaced, which is what lets one travel across the board instead
// of blinking out of one region and into another.

function currentStep() {
  const steps = stepsOf(currentTake()) ?? [];
  return steps.find((s) => s.id === state.stepId) ?? steps[0];
}

function renderFrame() {
  const take = currentTake();
  const step = currentStep();
  $("#now").innerHTML = step?.now ? step.now(state.facts) : "";
  renderBigStep(take, step);
  const plan = take.board?.(state.facts, step?.id) ?? { cols: "flow", zones: [], edges: [] };
  $("#zones").className = `zones ${plan.cols ?? "flow"}`;
  $("#zones").innerHTML = (plan.zones ?? []).join("");
  renderTokens(state.facts.tokens);
  renderEdges(plan.edges ?? [], state.facts.edge);
  renderExtras(take, step);
}

/// In presentation view the step's own name is the only text on screen, so it
/// has to carry the beat by itself. That is why the labels are short.
function renderBigStep(take, step) {
  const steps = stepsOf(take) ?? [];
  const i = steps.findIndex((s) => s.id === step?.id);
  const bad = $("#rail").querySelector(`.step.bad[data-step="${CSS.escape(step?.id ?? "")}"]`);
  const el = $("#bigstep");
  el.className = `bigstep ${bad ? "bad" : ""}`;
  // A step whose point is a number says it here, because the panel that
  // normally carries it is not on screen in this view.
  const punch = step?.big?.(state.facts);
  el.innerHTML = step
    ? `${esc(step.label)}<span class="of">${i + 1} / ${steps.length}</span>${punch ? `<div class="punch">${punch}</div>` : ""}`
    : "";
}

const Z = {
  /// A region of the machine. Edges are drawn to the box; chips land in its
  /// body, so they never sit on top of the name.
  zone: (name, { sub, body = "", cls = "", anchor, drop = true } = {}) =>
    `<div class="zone ${cls}" ${anchor ? `data-anchor="${esc(anchor)}"` : ""}>
       <div class="zone-h"><span class="name">${esc(name)}</span>${sub ? `<span class="sub">${esc(sub)}</span>` : ""}</div>
       <div class="zone-b" ${anchor && drop ? `data-drop="${esc(anchor)}"` : ""}>${body}</div>
     </div>`,

  /// A named place inside a region — "due now" and "not yet" are the same
  /// table, which is the point, so they are shelves rather than two zones.
  // The caption says what the place is; the chip lands in the space under it.
  shelf: (anchor, cap, cls = "") =>
    `<div class="shelf ${cls}"><span class="cap">${esc(cap)}</span><div class="drop" data-drop="${esc(anchor)}"></div></div>`,

  slot: (anchor, who, stateText, cls = "") =>
    `<div class="slot ${cls}">
       <div class="who">${esc(who)}</div><div class="state">${esc(stateText)}</div>
       <div class="drop" data-drop="${esc(anchor)}"></div>
     </div>`,

  line: (html) => `<div class="line">${html}</div>`,
  verdict: (text, tone) => `<div class="verdict ${tone ?? ""}">${esc(text)}</div>`,
};

function stageRect() {
  return $("#stage").getBoundingClientRect();
}

function boxOf(sel) {
  const el = $(`#zones ${sel}`);
  if (!el) return null;
  const s = stageRect();
  const r = el.getBoundingClientRect();
  return { x: r.left - s.left, y: r.top - s.top, w: r.width, h: r.height };
}

/// Edges are drawn between whole regions; chips land in the place inside one.
const anchorRect = (key) => boxOf(`[data-anchor="${CSS.escape(key)}"]`);
const dropRect = (key) => boxOf(`[data-drop="${CSS.escape(key)}"]`) ?? anchorRect(key);

function renderTokens(list) {
  const host = $("#toks");
  const seen = new Set();

  for (const t of list ?? []) {
    seen.add(t.id);
    let el = host.querySelector(`[data-tok="${CSS.escape(t.id)}"]`);
    const fresh = !el;
    if (fresh) {
      el = document.createElement("div");
      el.dataset.tok = t.id;
      host.appendChild(el);
    }
    const moved = !fresh && el.dataset.at !== t.at;
    el.dataset.at = t.at;
    el.className = `tok ${t.tone ?? ""} ${fresh ? "placing" : ""}`;
    el.innerHTML =
      `<div class="name">${esc(t.label)}</div>` +
      (t.note ? `<div class="note">${t.note}</div>` : "") +
      (t.bar ?? "");
    if (moved) {
      el.classList.remove("pulse");
      void el.offsetWidth; // restart the animation rather than let it be a no-op
      el.classList.add("pulse");
    }
  }

  for (const el of [...host.children]) if (!seen.has(el.dataset.tok)) el.remove();
  placeTokens();
  requestAnimationFrame(() => {
    // Sizing a landing area changes the layout under the chips, and a chip
    // whose text just changed may have re-measured. Settle once more before
    // letting transitions back on.
    placeTokens();
    host.querySelectorAll(".placing").forEach((n) => n.classList.remove("placing"));
  });
}

function placeTokens() {
  const groups = {};
  for (const el of $("#toks").children) (groups[el.dataset.at] ??= []).push(el);

  // Chips are positioned outside the flow, so each landing area has to be told
  // how much it is holding — otherwise a second chip hangs out of its lane.
  for (const d of $("#zones").querySelectorAll("[data-drop]")) {
    const els = groups[d.dataset.drop] ?? [];
    const h = els[0]?.offsetHeight ?? 40;
    d.style.minHeight = els.length > 1 ? `${els.length * h + (els.length - 1) * 6 + 10}px` : "";
  }

  for (const [key, els] of Object.entries(groups)) {
    const r = dropRect(key);
    els.forEach((el, i) => {
      // An anchor that is not on this board leaves the chip where it was. Work
      // going quietly invisible is the one failure mode this view cannot have.
      if (!r) return;
      const w = el.offsetWidth;
      const h = el.offsetHeight;
      const x = r.x + Math.max(6, (r.w - w) / 2);
      const y = r.y + Math.max(0, (r.h - h) / 2) + i * (h + 6);
      el.style.transform = `translate(${Math.round(x)}px, ${Math.round(y)}px)`;
    });
  }
}

/// An edge is drawn between two anchors. `back` loops under the board, which
/// is how a retry and a re-poll read as going round again rather than onward.
function edgePath(a, b, kind) {
  if (kind === "back") {
    const y = Math.max(a.y + a.h, b.y + b.h) + 34;
    return {
      d: `M ${a.x + a.w / 2} ${a.y + a.h} V ${y} H ${b.x + b.w / 2} V ${b.y + b.h + 9}`,
      tip: { x: b.x + b.w / 2, y: b.y + b.h + 9, dir: "up" },
      label: { x: (a.x + a.w / 2 + b.x + b.w / 2) / 2, y: y - 6 },
    };
  }
  const x1 = a.x + a.w;
  const y1 = a.y + a.h / 2;
  const x2 = b.x - 9;
  const y2 = b.y + b.h / 2;
  const mx = (x1 + x2) / 2;
  return {
    d: `M ${x1} ${y1} C ${mx} ${y1}, ${mx} ${y2}, ${x2} ${y2}`,
    tip: { x: x2, y: y2, dir: "right" },
    label: { x: mx, y: (y1 + y2) / 2 - 8 },
  };
}

const arrowHead = ({ x, y, dir }, cls) =>
  dir === "up"
    ? `<polygon class="head ${cls}" points="${x},${y - 9} ${x - 5},${y + 1} ${x + 5},${y + 1}" />`
    : `<polygon class="head ${cls}" points="${x + 9},${y} ${x - 1},${y - 5} ${x - 1},${y + 5}" />`;

function renderEdges(defs, active) {
  const svg = $("#edges");
  const s = stageRect();
  svg.setAttribute("viewBox", `0 0 ${Math.round(s.width)} ${Math.round(s.height)}`);

  svg.innerHTML = defs
    .map((e) => {
      const a = anchorRect(e.from);
      const b = anchorRect(e.to);
      if (!a || !b) return "";
      const on = e.id === active;
      const tone = on ? (e.tone ?? "act") : "";
      const { d, tip, label } = edgePath(a, b, e.kind);
      return (
        `<path class="${tone} ${on ? "flow" : ""} ${e.kind === "back" ? "dashed" : ""}" d="${d}" />` +
        arrowHead(tip, tone) +
        (e.label
          ? `<text class="${tone}" x="${Math.round(label.x)}" y="${Math.round(label.y)}" text-anchor="middle">${esc(e.label)}</text>`
          : "")
      );
    })
    .join("");
}

/// The board is laid out by the browser, so anything that changes its size has
/// to re-measure. Cheap: one read pass and a transform per chip.
let relayoutPending = false;
function relayout() {
  if (relayoutPending) return;
  relayoutPending = true;
  requestAnimationFrame(() => {
    relayoutPending = false;
    if (!$("#zones").children.length) return;
    placeTokens();
    const take = currentTake();
    const plan = take?.board?.(state.facts, currentStep()?.id);
    if (plan) renderEdges(plan.edges ?? [], state.facts.edge);
  });
}
window.addEventListener("resize", relayout);

// ─── the wire drawer ─────────────────────────────────────────────────────────

let wireCount = 0;

function addWire({ verb, path, status, req, res, said }) {
  const host = $("#wire-body");
  host.querySelector(".empty")?.remove();
  const row = document.createElement("div");
  row.className = "exch";
  const ok = status == null || (status >= 200 && status < 300);
  row.innerHTML = `
    <div class="line"><span class="arrow">→</span><span class="verb">${esc(verb)}</span><span class="path">${esc(path)}</span></div>
    ${req === undefined ? "" : `<pre>${tokens(json(maskSecrets(JSON.stringify(req, null, 2))))}</pre>`}
    ${
      status == null
        ? ""
        : `<div class="line"><span class="arrow">←</span><span class="code ${ok ? "ok" : "bad"}">${status}</span>${
            said ? `<span class="path">${esc(said)}</span>` : ""
          }</div>`
    }
    ${res === undefined ? "" : `<pre>${tokens(json(maskSecrets(JSON.stringify(res, null, 2))))}</pre>`}`;
  host.appendChild(row);
  host.scrollTop = host.scrollHeight;
  wireCount = host.querySelectorAll(".exch").length;
  $("#wire-meta").textContent = `${wireCount} calls`;
  $("#wire-badge").textContent = String(wireCount);
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
  wireCount = 0;
  $("#wire-badge").textContent = "";
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

const PACED = new Set(["step"]);

const film = {
  events: [],
  cursor: 0,
  closed: false,
  playing: false,
  mode: "manual",
  dwell: 1300,
  beatUntil: null,
  nudge: null,
  waiters: [],
};

const deliver = (ev) => film.events.push(ev);

function resetFilm() {
  film.events.length = 0;
  film.cursor = 0;
  film.closed = false;
  film.beatUntil = null;
  releaseNudge();
  film.waiters.splice(0).forEach((r) => r());
}

const waitForNudge = () =>
  new Promise((r) => {
    film.nudge = r;
  });

function releaseNudge() {
  const r = film.nudge;
  film.nudge = null;
  r?.();
}

function advanceOneGroup(take) {
  while (film.cursor < film.events.length) {
    const ev = film.events[film.cursor++];
    // Pressing ▶ means "next step", so a beat inside this one is skipped
    // rather than waited out.
    if (ev.type === "beat") continue;
    draw(take, ev);
    if (PACED.has(ev.type)) return true;
  }
  return false;
}

/// The data a step is about usually lands after the step itself — the HTTP call
/// has to happen first. So while a frame is held, keep drawing anything unpaced
/// that arrives: the frame fills in under the eye rather than only on the way
/// out of it.
/// A `beat` is a pause *inside* a step: the events after it belong to the same
/// frame but must not be drawn until the eye has had the ones before. One step
/// that contains a round trip — out to a worker and back — needs it, and the
/// run cannot provide it by sleeping: when the run gets ahead of the film, both
/// legs are already in the buffer and a single drain collapses them into one
/// frame. Pacing belongs here, where the drawing happens.
function drain(take, { instant = false } = {}) {
  let drew = false;
  while (film.cursor < film.events.length) {
    const ev = film.events[film.cursor];
    if (PACED.has(ev.type)) break;
    if (ev.type === "beat" && !instant) {
      film.beatUntil ??= performance.now() + (ev.ms ?? 650);
      if (performance.now() < film.beatUntil) break;
      film.beatUntil = null;
    }
    film.cursor++;
    if (ev.type !== "beat") draw(take, ev);
    drew = true;
  }
  return drew;
}

/// Everything on screen is the first `n` events drawn in order, so going back
/// is drawing fewer of them into a fresh bag of facts.
function redrawTo(take, n) {
  resetStage(take);
  film.cursor = 0;
  film.beatUntil = null;
  while (film.cursor < n) draw(take, film.events[film.cursor++]);
  drain(take, { instant: true }); // land on the frame as it looked, not as it first appeared
  renderFrame();
}

function pacedIndices() {
  const out = [];
  film.events.forEach((e, i) => PACED.has(e.type) && out.push(i));
  return out;
}

function stepBack() {
  const take = currentTake();
  const paced = pacedIndices();
  // Which frame we are on, counted in steps rather than raw events — the
  // cursor sits past the trailing data of the current one.
  const target = paced.filter((i) => i < film.cursor).length - 2;
  redrawTo(take, target >= 0 ? paced[target] + 1 : 0);
  syncTransport();
}

function stepNext() {
  if (film.cursor < film.events.length) {
    const take = currentTake();
    film.beatUntil = null; // pressing ▶ abandons any pause inside this step
    advanceOneGroup(take);
    drain(take); // whatever already landed for this frame belongs on it
    // Stepping back and then forward again should end on the same picture the
    // run ended on, settled rather than still mid-stride.
    if (film.closed && film.cursor >= film.events.length) settleSteps();
    syncTransport();
    return;
  }
  releaseNudge();
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

async function play(take) {
  film.playing = true;
  syncTransport();
  while (film.playing) {
    if (film.cursor < film.events.length) {
      const drewStep = advanceOneGroup(take);
      syncTransport();
      if (!drewStep) continue;
      await hold(take);
      continue;
    }
    if (film.closed) break;
    await sleep(40);
  }
  film.playing = false;
  film.waiters.splice(0).forEach((r) => r());
  syncTransport();
}

/// Hold on this frame — for a beat in auto, until you say so in manual — and
/// keep draining the data that lands while we hold. Switching to auto releases
/// the nudge, so a mode change during a hold ends it.
async function hold(take) {
  const until = film.mode === "auto" ? performance.now() + film.dwell : Infinity;
  let nudged = false;
  if (until === Infinity) waitForNudge().then(() => (nudged = true));

  // A pending beat holds the frame past its dwell: a round trip drawn inside
  // one step still gets both of its legs.
  while (!nudged && film.playing && (performance.now() < until || film.beatUntil != null)) {
    if (drain(take)) syncTransport();
    await sleep(Math.min(60, Math.max(1, until - performance.now())));
  }
  if (drain(take)) syncTransport();
}

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

  /// Move to a step. Its frame takes over the canvas.
  step(id, opts = {}) {
    this.emit("step", { id, at: Math.round(performance.now() - this.t0), ...opts });
  }

  /// More real data. The current frame redraws with it; no beat is spent.
  facts(data) {
    this.emit("facts", { data });
  }

  wire(entry) {
    this.emit("wire", entry);
  }

  /// Say why it stopped, and make that the last thing the room reads.
  halt(id, why) {
    this.halted = true;
    this.facts({ halt: why });
    this.step(id, { bad: true });
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
      state.stepId = ev.id;
      markStep(ev.id, ev.bad ? "bad" : "now", ev.at);
      renderFrame();
      break;

    case "facts":
      Object.assign(state.facts, ev.data);
      renderFrame();
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
    if (first) mountTake(0);
  } catch {
    setLight("light-api", false);
  }
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
  properties: { mandate_id: { type: "string" }, checks: { type: "string" }, fail_times: { type: "string" } },
  required: ["mandate_id"],
};

// ─── page 1 · the board ──────────────────────────────────────────────────────
//
// No loop here: three stores feed one description of a call. Each store lights
// as its row lands, and the placeholder it satisfies lights with it.

const PLACEHOLDER_OF = { config: "tok-config", secret: "tok-secret", payload: "tok-input" };

function buildBoard(f, stepId) {
  const N = SETUP_NAMES();
  const done = f.built ?? {};
  const tone = (k) => (stepId === k ? "on" : done[k] ? "" : "dim");

  // The body is left empty on purpose: the row landing in it is the chip.
  const store = (k, table, sub) => Z.zone(table, { sub, anchor: k, cls: tone(k), body: `<div class="drop"></div>` });

  const mark = (k, text) => (done[k] || stepId === k ? `<span class="${PLACEHOLDER_OF[k]}">${text}</span>` : text);

  const spec =
    `POST  ${mark("config", "{{config.base_url}}")}/mandates/${mark("payload", "{{input.mandate_id}}")}/sync\n` +
    `Authorization: ${mark("secret", `{{secret.${esc(N.secret)}}}`)}\n` +
    `x-team: ${mark("config", "{{config.team}}")}\n\n` +
    `retry  3 tries · exponential · 1m → 10m`;

  return {
    cols: "build",
    zones: [
      `<div class="stack">
         ${store("config", "configs", "what changes")}
         ${store("secret", "secrets", "what must not leak")}
         ${store("payload", "payload_specs", "what callers may send")}
       </div>`,
      Z.zone("endpoints", {
        sub: N.endpoint,
        anchor: "endpoint",
        cls: stepId === "endpoint" ? "on" : done.endpoint ? "" : "dim",
        body: `<pre class="f-code big">${spec}</pre>`,
      }),
    ],
    edges: [
      { id: "config", from: "config", to: "endpoint", label: "config.*" },
      { id: "secret", from: "secret", to: "endpoint", label: "secret.*" },
      { id: "payload", from: "payload", to: "endpoint", label: "input.*" },
    ],
  };
}

const SETUP_STEPS = [
  {
    id: "config",
    label: "config",
    now: (f) =>
      F.wrap(
        F.pane("POST /v1/configs", F.code(json(f.cfgBody ?? { name: "…", values: {} }), "big")),
        f.cfgRes && F.pane("response", F.code(json(f.cfgRes)), { meta: f.cfgMark, metaTone: f.cfgOk ? "ok" : "bad", cls: f.cfgOk ? "ok" : "bad" }),
        F.note(f.cfgOk ? `Staging has a different <code>base_url</code>. Changing it is one PUT.` : "What differs between environments."),
        f.halt && F.note(f.halt, "bad"),
      ),
  },
  {
    id: "secret",
    label: "secret",
    now: (f) =>
      F.wrap(
        F.pane("POST /v1/secrets", F.code(json({ name: f.secName ?? "…", value: "Bearer aarokya-…" }), "big")),
        f.secRead && F.pane(`GET /v1/secrets/${esc(f.secName ?? "")}`, F.code(json(f.secRead), "big"), { meta: "200", metaTone: "ok", cls: "ok" }),
        f.secRead
          ? F.note(`No <code>value</code> field. There is no read path for it — the worker decrypts at call time.`, "ok")
          : F.note("Write-only. Encrypted before it hits the table."),
      ),
  },
  {
    id: "payload",
    label: "payload spec",
    now: (f) =>
      F.wrap(
        F.pane("POST /v1/payload-specs", F.code(json(f.psBody ?? MANDATE_SCHEMA))),
        F.pane(
          "a job that does not satisfy it",
          F.code(
            `POST /v1/jobs\n{ "input": { "checks": "1" } }\n\n<u>← 422</u>  missing mandate_id`,
          ),
          { cls: "bad" },
        ),
        F.note(`422 at the call site, not at 3am in a log.`, "warn"),
      ),
  },
  {
    id: "endpoint",
    label: "endpoint",
    big: (f) => (f.epOk ? "four rows · <b>no deploy</b>" : null),
    now: (f) =>
      F.wrap(
        F.lead(f.epOk ? "Four rows. No deploy." : "The call itself."),
        F.note(`The three stores feed the placeholders. Nothing is resolved until a job actually fires.`),
        F.pane(
          "the endpoint row",
          F.code(
            `name           ${esc(SETUP_NAMES().endpoint)}\n` +
              `config         ${esc(SETUP_NAMES().config)}\n` +
              `payload_spec   ${esc(SETUP_NAMES().payloadSpec)}\n` +
              `timeout_ms     30000\n` +
              `expected       [200, 201]\n` +
              `retry_policy   3 · exponential · 60s → 600s`,
            "big",
          ),
          { meta: f.epMark, metaTone: f.epOk ? "ok" : "", cls: f.epOk ? "ok" : "" },
        ),
        F.note(`Every field of it is queryable, and a PUT changes any of them.`),
        f.halt && F.note(f.halt, "bad"),
      ),
  },
];

// ─── pages 2 and 3 · the board ───────────────────────────────────────────────
//
// Four regions in the order the work travels, and one edge that goes back the
// way it came. A job is a chip with its real id on it: it is written into the
// executions lane, a worker takes it, it goes out, and it comes back — to
// `finished`, or to `not yet` with the clock running. That back edge is the
// whole mechanism.

function flowBoard(f, stepId) {
  const cron = f.isCron;
  const long = f.isLong;
  const at = (id) => (f.tokens ?? []).some((t) => t.at === id);

  const source = cron
    ? `<div class="stack">
         ${Z.zone("your service", { sub: "POST /v1/jobs", anchor: "service", cls: stepId === "ask" ? "on" : "dim", body: Z.line(`one call, <b>once</b>`) })}
         ${Z.zone("pg_cron", {
           sub: esc(f.cron ?? "* * * * *"),
           anchor: "pgcron",
           cls: stepId === "due" || stepId === "tick" ? "on" : "dim",
           body: Z.line(`a Postgres extension.<br/><b>the database</b> inserts the next row.`),
         })}
       </div>`
    : Z.zone("your service", {
        sub: "POST /v1/jobs",
        anchor: "service",
        cls: stepId === "ask" ? "on" : "dim",
        body: Z.line(`names the endpoint.<br/>nothing else.`),
      });

  const execs = Z.zone("executions", {
    sub: "one row per run",
    anchor: "execs",
    cls: ["written", "due", "tick", "wait", "poll"].includes(stepId) ? "on" : "",
    body:
      Z.shelf("due", "due now · run_at <= now()", at("due") ? "hot" : "") +
      Z.shelf("later", long ? "waiting · next check" : "not yet · run_at in the future", at("later") ? "cool" : "") +
      Z.shelf("done", "finished"),
  });

  const slots = (f.workerSlots ?? []).map((w) =>
    Z.slot(`w:${w.id}`, w.short, w.state ?? "asking for due rows", w.cls ?? ""),
  );
  const workers = Z.zone("workers", {
    sub: `${slots.length} process${slots.length === 1 ? "" : "es"}`,
    anchor: "workers",
    cls: stepId === "claim" ? "on" : "",
    body: slots.join("") || Z.line(`<b>none running</b>`),
  });

  const target = Z.zone("aarokya", {
    sub: "the target",
    anchor: "target",
    cls: ["call", "send", "accepted", "answer", "poll", "finish"].includes(stepId) ? "on" : "dim",
    body: f.verdict ? Z.verdict(f.verdict.text, f.verdict.tone) + Z.line(f.verdict.sub ?? "") : Z.line(`waiting to be called`),
  });

  return {
    cols: "flow",
    zones: [source, execs, workers, target],
    edges: [
      { id: "enqueue", from: cron ? "pgcron" : "service", to: "execs", label: cron ? "tick" : "201" },
      { id: "claim", from: "execs", to: "workers", label: f.claimLabel ?? "SKIP LOCKED" },
      { id: "call", from: "workers", to: "target", label: f.callLabel ?? "resolved" },
      { id: "back", from: "target", to: "execs", kind: "back", tone: f.backTone ?? "warm", label: f.backLabel ?? "the answer is written back" },
    ],
  };
}

// ─── page 2 · a short task ───────────────────────────────────────────────────

const askNow = (f) =>
  F.wrap(
    F.pane("POST /v1/jobs", F.code(json(f.jobBody ?? {}), "big")),
    F.pane(
      "not in the body",
      F.code(`<s>where Aarokya lives</s>\n<s>which credential</s>\n<s>how many times to try</s>\n<s>how long between tries</s>`, "big"),
      { cls: "dim" },
    ),
    F.note(`One POST naming an endpoint. That is the integration.`),
  );

const writtenNow = (f) =>
  F.wrap(
    F.bigs(F.big(f.answeredIn ?? "—", "answered in", "act")),
    F.table("jobs", "what you asked for", [{ k: "e", label: "endpoint" }, { k: "t", label: "trigger" }, { k: "s", label: "status" }], f.job ? [f.job] : []),
    F.table("executions", "one per run", [{ k: "s", label: "status" }, { k: "w", label: "run_at" }, { k: "tries", label: "attempt_count" }], f.exec ? [f.exec] : []),
    F.note(`Both rows existed before the POST returned. Kill everything now and the work still happens.`, "ok"),
  );

const cronWrittenNow = (f) =>
  F.wrap(
    F.bigs(F.big(f.answeredIn ?? "—", "answered in", "act"), F.big(f.nextRun ?? "—", "next run")),
    F.table("jobs", "what you asked for", [{ k: "e", label: "endpoint" }, { k: "w", label: "cron" }, { k: "s", label: "status" }], f.job ? [f.job] : []),
    F.table("executions", "empty, correctly", [{ k: "s", label: "status" }, { k: "w", label: "run_at" }], []),
    F.note(`One row and a rule, not a queue of future runs.`, "ok"),
    f.halt && F.note(f.halt, "bad"),
  );

const dueNow = (f) =>
  F.wrap(
    F.lead(`<code>run_at</code> is a column.`),
    F.table(
      "executions",
      "the row is the timer",
      [{ k: "s", label: "status" }, { k: "w", label: "run_at" }, { k: "worker", label: "worker" }],
      f.exec ? [f.exec] : [],
    ),
    F.note(`No timer, no sleeping thread, no <code>setTimeout</code>. A worker asks for rows whose time has come.`),
    F.note(`Kill both workers now. The chip does not move and the job still goes out.`, "warn"),
  );

const cronDueNow = (f) =>
  F.wrap(
    F.lead(`Nothing of Invokr's is awake for the next tick.`),
    F.pane("your jobs row", F.code(`cron       <u>${esc(f.cron ?? "* * * * *")}</u>\ntimezone   Asia/Kolkata\nstatus     ACTIVE`, "big")),
    F.note(`pg_cron runs inside Postgres. If every worker and every API process is down when the minute turns, the row is still created.`),
  );

const tickNow = (f) =>
  F.wrap(
    F.bigs(F.big(f.tickAt ?? "—", "row appeared", "act"), F.big(f.tickN ?? 1, "tick")),
    F.table("executions", "nobody inserted this", [{ k: "s", label: "status" }, { k: "w", label: "created_at" }], f.exec ? [f.exec] : []),
    F.note(`From here it is an ordinary execution.`),
  );

const claimNow = (f) =>
  F.wrap(
    F.lead(`One row, one winner.`),
    F.pane("the claim", F.code(`SELECT … FROM executions\n WHERE status = 'QUEUED'\n   AND run_at &lt;= now()\n <u>FOR UPDATE SKIP LOCKED</u>\n LIMIT 1`, "big")),
    F.note(`The second worker does not block. It skips the locked row and takes the next one.`),
    F.note(`No leader election, no lock service, no coordination.`, "ok"),
  );

const callNow = (f) =>
  F.wrap(
    F.pane("registered", F.code(tokens(esc(f.template ?? "")), "big"), { cls: "dim" }),
    F.pane(`sent${f.sentAt ? `, ${f.sentAt}` : ""}`, F.code(f.resolved ?? `<div class="empty">calling…</div>`, "big"), { cls: "ok" }),
    F.note(`Resolved now, not at registration. Rotate the secret and the next call uses it.`),
  );

const answerNow = (f) =>
  F.wrap(
    f.answerBody && F.pane(`← ${f.answerCode ?? ""} in ${f.answerTook ?? ""}`, F.code(json(f.answerBody), "big"), { cls: f.answerOk ? "ok" : "bad" }),
    F.table(
      "attempts",
      "one per actual try",
      [{ k: "n", label: "#" }, { k: "s", label: "status" }, { k: "c", label: "code" }, { k: "d", label: "ms" }, { k: "k", label: "idempotency key" }],
      f.attempts ?? [],
    ),
    (f.attempts?.length ?? 0) > 1
      ? F.note(`${f.attempts.length} tries, <b>one key</b>. The other side can see it is the same request.`, "ok")
      : F.note(`Status code, duration, and the key it carried.`),
  );

const recordNow = (f) =>
  F.wrap(
    F.bigs(
      F.big(f.finalAttempts ?? 1, "attempts", "ok"),
      F.big(f.finalStatus ?? "—", "execution", f.finalStatus === "SUCCESS" ? "ok" : "warn"),
    ),
    F.pane(
      "same API, reading back",
      F.code(`GET /v1/executions/{id}\nGET /v1/executions/{id}/attempts\nGET /v1/jobs/{id}/executions`, "big"),
    ),
    F.note(`No log scraping, no agent, no separate store.`),
    f.halt && F.note(f.halt, "bad"),
  );

const cancelNow = (f) =>
  F.wrap(
    F.lead(`Poll until terminal, then stop asking.`),
    F.pane("POST /v1/jobs/{id}/cancel", F.code(`→ 200\n\njobs.status  <span class="st retired">RETIRED</span>`, "big"), { cls: "ok" }),
    F.note(`One POST retires the schedule. pg_cron stops producing; what already ran stays on the record.`),
    f.halt && F.note(f.halt, "bad"),
  );

const SHORT_STEPS = [
  { id: "ask", label: "POST /v1/jobs", now: askNow },
  { id: "written", label: "Two rows, then 201", now: writtenNow },
  {
    id: "due",
    label: "run_at is a column",
    now: dueNow,
    extras: [
      { id: "kill", label: "Kill a worker", danger: true },
      { id: "addworker", label: "Add a worker" },
    ],
  },
  { id: "claim", label: "One worker wins", now: claimNow },
  { id: "call", label: "The call goes out", now: callNow },
  {
    id: "answer",
    label: "Every try is a row",
    now: answerNow,
    big: (f) => ((f.attempts?.length ?? 0) > 1 ? `${f.attempts.length} tries · <b>one key</b>` : null),
  },
  {
    id: "record",
    label: "The record",
    now: recordNow,
    big: (f) => `${f.finalAttempts ?? 1} attempt${(f.finalAttempts ?? 1) === 1 ? "" : "s"} · ${esc(f.finalStatus ?? "")}`,
  },
];

const CRON_STEPS = [
  { ...SHORT_STEPS[0], label: "POST /v1/jobs, with a cron" },
  { ...SHORT_STEPS[1], label: "One row, then 201", now: cronWrittenNow },
  { id: "due", label: "pg_cron owns it", now: cronDueNow },
  { id: "tick", label: "A row nobody inserted", now: tickNow },
  SHORT_STEPS[3],
  SHORT_STEPS[4],
  SHORT_STEPS[5],
  SHORT_STEPS[6],
  { id: "cancel", label: "Cancel when it is terminal", now: cancelNow },
];

// The two questions a room always asks once it has seen one job run: can two
// teams share this, and does it only do HTTP.

const TEAM_STEPS = [
  {
    id: "name",
    label: "One name, two workspaces",
    now: (f) =>
      F.wrap(
        F.lead(`A workspace is a Postgres schema.`),
        F.pane("Mandates", F.code(`endpoint  ${esc(SETUP_NAMES().endpoint)}\nschema    <u>…${esc(f.schemaA ?? "")}</u>`, "big")),
        F.pane("Rides", F.code(`endpoint  ${esc(SETUP_NAMES().endpoint)}\nschema    <u>…${esc(f.schemaB ?? "")}</u>`, "big")),
        F.note(`Its own jobs, executions, attempts, configs and secrets — not a tenant column to remember to filter on.`),
      ),
  },
  {
    id: "fire",
    label: "Both fire it",
    now: (f) =>
      F.wrap(
        F.pane("Mandates", F.code(json(f.bodyA ?? {}), "big"), { meta: f.statusA ?? "…", metaTone: "ok" }),
        F.pane("Rides", F.code(json(f.bodyB ?? {}), "big"), { meta: f.statusB ?? "…", metaTone: "ok" }),
        F.note(`Identical but for <code>X-Workspace-Id</code>. Neither caller knows the other exists.`),
        f.halt && F.note(f.halt, "bad"),
      ),
  },
  {
    id: "ran",
    label: "Two runs, no join between them",
    now: (f) =>
      F.wrap(
        F.table("executions · Mandates", "that schema", [{ k: "s", label: "status" }, { k: "worker", label: "worker" }], f.execA ? [f.execA] : []),
        F.table("executions · Rides", "that schema", [{ k: "s", label: "status" }, { k: "worker", label: "worker" }], f.execB ? [f.execB] : []),
        F.note(`The same worker pool served both. Isolation is in the data, not in a second deployment.`, "ok"),
      ),
  },
  {
    id: "proof",
    label: "Aarokya tells them apart",
    now: (f) =>
      F.wrap(
        F.lead(`Nobody templated the team into the job.`),
        F.pane("Aarokya's log · Mandates", F.code(esc(f.saidA ?? "waiting…"), "big"), { cls: f.saidA ? "ok" : "dim" }),
        F.pane("Aarokya's log · Rides", F.code(esc(f.saidB ?? "waiting…"), "big"), { cls: f.saidB ? "ok" : "dim" }),
        F.note(`<code>{{config.team}}</code> resolves against the workspace the job was created in.`),
        f.halt && F.note(f.halt, "bad"),
      ),
  },
];

const TRANSPORT_STEPS = [
  {
    id: "three",
    label: "Three endpoints, one difference",
    now: (f) =>
      F.wrap(
        F.lead(`<code>type</code> is a column.`),
        F.table(
          "endpoints",
          "the destination is a field",
          [{ k: "t", label: "type" }, { k: "w", label: "where it goes" }, { k: "s", label: "up here" }],
          f.endpoints ?? [],
        ),
        F.note(`Retries, keys, tables, the API you query — identical. Moving to Kafka is an endpoint edit.`),
      ),
  },
  {
    id: "send",
    label: "Same job, three ways",
    now: (f) =>
      F.wrap(
        F.table("executions", "one per destination", [{ k: "t", label: "type" }, { k: "s", label: "status" }, { k: "worker", label: "worker" }], f.sent ?? []),
        f.skipped?.length
          ? F.note(`${esc(f.skipped.join(" and "))} not running here. Same job, nowhere to put it.`, "warn")
          : F.note(`All three destinations are up.`, "ok"),
        f.halt && F.note(f.halt, "bad"),
      ),
  },
  {
    id: "same",
    label: "Same record either way",
    now: (f) =>
      F.wrap(
        F.bigs(F.big(f.sentCount ?? 0, "destinations", "ok"), F.big("1", "record shape", "act")),
        F.pane(
          "what differs",
          F.code(`HTTP    <u>x-invokr-idempotency-key</u> added for you\nKafka   you template the key in\nRedis   you template the key in`, "big"),
        ),
        F.note(`The key is automatic on HTTP only. For the streams, put <code>{{execution.idempotency_key}}</code> in the payload.`, "warn"),
      ),
  },
];

// ─── page 3 · work that takes minutes ────────────────────────────────────────

const LONG_STEPS = [
  {
    id: "ask",
    label: "POST /v1/jobs",
    now: (f) =>
      F.wrap(
        F.pane("POST /v1/jobs", F.code(json(f.jobBody ?? {}), "big")),
        F.note(`Identical to any other job. The endpoint knows it is async; the caller does not.`),
        f.halt && F.note(f.halt, "bad"),
      ),
  },
  {
    id: "written",
    label: "One row, then 201",
    now: (f) =>
      F.wrap(
        F.bigs(F.big(f.answeredIn ?? "—", "answered in", "act")),
        F.note(`Durable before the work has begun.`),
        f.halt && F.note(f.halt, "bad"),
      ),
  },
  { id: "claim", label: "One worker wins", now: () => F.wrap(F.lead(`Same claim, same SKIP LOCKED.`), F.note(`Nothing about this execution is special yet.`)) },
  {
    id: "send",
    label: "One dispatch",
    now: (f) =>
      F.wrap(
        F.lead(`The only outbound dispatch there will be.`),
        F.pane("the endpoint's async block", F.code(json(f.asyncBlock ?? {}), "big"), { cls: "dim" }),
        F.note(`However long this runs, the retry budget sees one attempt.`),
      ),
  },
  {
    id: "accepted",
    label: "202 + Location",
    now: () =>
      F.wrap(
        F.pane("response", F.code(`HTTP/1.1 <u>202</u> Accepted\nLocation: /async/status/task-8`, "big"), { cls: "ok" }),
        F.note(`Not success, not failure.`),
        F.note(`202 with no <code>Location</code> is <code>MISSING_POLL_URL</code> — a hard failure, not retryable.`, "warn"),
      ),
  },
  {
    id: "wait",
    label: "WAITING",
    big: () => "<b>nothing of yours is waiting</b>",
    now: (f) =>
      F.wrap(
        F.lead(`Nothing of yours is waiting.`),
        F.table(
          "executions",
          "parked",
          [{ k: "s", label: "status" }, { k: "w", label: "next check" }, { k: "tries", label: "attempts" }, { k: "p", label: "polls" }],
          [{ s: F.status("WAITING"), w: f.nextCheck ?? "—", tries: 1, p: f.polls ?? 0 }],
        ),
        F.note(`No socket, no thread, no in-memory state. A row with a next-check time, exactly like a delayed job.`),
      ),
  },
  {
    id: "poll",
    label: "Checks, or a callback",
    now: (f) =>
      F.wrap(
        F.bigs(F.big(f.polls ?? 0, "polls so far", "act")),
        F.table(
          "polls",
          "one row per check",
          [{ k: "n", label: "#" }, { k: "c", label: "code" }, { k: "r", label: "retry_after" }, { k: "k", label: "classification" }],
          f.pollRows ?? [],
        ),
        F.note(
          f.lastRetryAfter
            ? `It asked for <b>${f.lastRetryAfter}</b>, so that is what Invokr waited. <code>Retry-After</code> beats the configured backoff.`
            : `Each check goes round the same loop: out of the lane, to a worker, to Aarokya, back to the lane.`,
        ),
      ),
  },
  {
    id: "finish",
    label: "First answer wins",
    now: (f) => F.wrap(F.lead(`One row update wins.`), F.note(f.finishNote ?? `The other sees zero rows changed and quietly stops.`)),
  },
  {
    id: "record",
    label: "Attempts vs polls",
    big: (f) => `${f.finalAttempts ?? 1} attempt · ${f.polls ?? 0} polls — <b>polls are not attempts</b>`,
    now: (f) =>
      F.wrap(
        F.bigs(F.big(f.finalAttempts ?? 1, "attempts", "ok"), F.big(f.polls ?? 0, "polls", "act")),
        F.lead(`Polls are not attempts.`),
        F.note(`Ten check-ins is still one dispatch. A slow destination cannot eat your retry budget.`),
        F.pane("reading it back", F.code(`GET /v1/executions/{id}\nGET /v1/executions/{id}/polls`, "big")),
        f.halt && F.note(f.halt, "bad"),
      ),
  },
];

// ─── the takes ───────────────────────────────────────────────────────────────

const takes = [
  {
    id: "setup",
    page: "setup",
    label: "Everything a job needs",
    claim: "Four POSTs and the endpoint exists.",
    sub: "Config, secret, payload spec, endpoint. No deploy, no restart — the call is four rows in Postgres.",
    action: "Build it, live",
    steps: SETUP_STEPS,
    board: buildBoard,
    fields: [],
    preview: () => ({
      cfgBody: {
        name: SETUP_NAMES().config,
        values: {
          base_url: state.status?.mockUrl ?? "http://localhost:9999",
          team: state.status?.provisioned?.workspaces?.a?.slug ?? "mandates",
        },
      },
      secName: SETUP_NAMES().secret,
      built: {},
    }),
    run: runSetup,
  },
  {
    id: "short-task",
    page: "short",
    label: "One task, end to end",
    claim: "A job is a row.",
    sub: "<b>run_at</b> is a column. A worker asks for rows that are due, and exactly one wins each.",
    action: "Fire it",
    steps: (v) => (v.trigger === "CRON" ? CRON_STEPS : SHORT_STEPS),
    // A cron run walks a different set of steps, so it needs its own tape —
    // otherwise replaying one would draw steps the current page does not have.
    tape: (v) => (v.trigger === "CRON" ? "short-task-cron" : "short-task"),
    board: flowBoard,
    preview: (v) => {
      const jobBody = shortJobBody(v);
      return {
        jobBody,
        cron: jobBody.cron,
        isCron: v.trigger === "CRON",
        workerSlots: workerSlots(),
        tokens: [{ id: "job", label: v.mandate_id, note: "not sent yet", at: "service", tone: "ghost" }],
      };
    },
    fields: [
      { key: "mandate_id", label: "mandate", value: "MND-8842", width: 116 },
      {
        key: "trigger",
        label: "when",
        type: "select",
        value: "IMMEDIATE",
        width: 126,
        picksTape: true,
        options: [
          { value: "IMMEDIATE", label: "now" },
          { value: "DELAYED", label: "in a few seconds" },
          { value: "CRON", label: "on a schedule" },
        ],
      },
      { key: "seconds", label: "seconds", value: "15", width: 46, show: (v) => v.trigger === "DELAYED" },
      {
        key: "cron",
        label: "every",
        type: "select",
        value: "* * * * *",
        width: 104,
        show: (v) => v.trigger === "CRON",
        options: [
          { value: "* * * * *", label: "minute" },
          { value: "*/2 * * * *", label: "2 minutes" },
          { value: "*/5 * * * *", label: "5 minutes" },
        ],
      },
      { key: "ticks", label: "ticks", value: "1", width: 38, show: (v) => v.trigger === "CRON" },
      { key: "fail_times", label: "failures", value: "0", width: 38 },
      { key: "attempts", label: "max tries", value: "3", width: 38 },
    ],
    async onExtra(id) {
      if (id === "kill") await control("/worker/kill");
      if (id === "addworker") await control("/worker/start");
      await refreshStatus();
    },
    run: runShort,
  },
  {
    id: "two-teams",
    page: "short",
    label: "Same name, two teams",
    claim: "Two teams, one deployment, nothing shared.",
    sub: "One endpoint name in two workspaces is two unrelated rows in two Postgres schemas.",
    action: "Fire into both",
    steps: TEAM_STEPS,
    board: flowBoard,
    fields: [{ key: "mandate_id", label: "mandate", value: "MND-4410", width: 116 }],
    preview: (v) => {
      const ws = state.status?.provisioned?.workspaces ?? {};
      return {
        schemaA: (ws.a?.schema_name ?? "").slice(-9),
        schemaB: (ws.b?.schema_name ?? "").slice(-9),
        workerSlots: workerSlots(),
        tokens: [
          { id: "a", label: `${v.mandate_id}-a`, note: "mandates", at: "service", tone: "ghost" },
          { id: "b", label: `${v.mandate_id}-b`, note: "rides", at: "service", tone: "ghost" },
        ],
      };
    },
    run: runTeams,
  },
  {
    id: "any-transport",
    page: "short",
    label: "Not just HTTP",
    claim: "The destination is a field.",
    sub: "HTTP, a Kafka topic or a Redis Stream. Same job, same retries, same record.",
    action: "Send it three ways",
    steps: TRANSPORT_STEPS,
    board: flowBoard,
    fields: [{ key: "mandate_id", label: "mandate", value: "MND-7781", width: 116 }],
    preview: (v) => ({
      endpoints: transportRows(),
      workerSlots: workerSlots(),
      tokens: transportTargets()
        .filter((t) => t.up)
        .map((t, i) => ({ id: `t${i}`, label: `${v.mandate_id}-${i + 1}`, note: t.type, at: "service", tone: "ghost" })),
    }),
    run: runTransports,
  },
  {
    id: "long-running",
    page: "long",
    label: "Work that takes minutes",
    claim: "202 is not an answer.",
    sub: "The row parks. Nothing is held open. Invokr checks back or gets called — either way, <b>one attempt</b>.",
    action: "Start the long job",
    steps: LONG_STEPS,
    tape: (v) => `long-running-${v.mode}`,
    board: flowBoard,
    preview: (v) => ({
      jobBody: longJobBody(v),
      asyncBlock: asyncSpecFrom(v),
      isLong: true,
      workerSlots: workerSlots(),
      tokens: [{ id: "job", label: v.job, note: "not sent yet", at: "service", tone: "ghost" }],
    }),
    fields: [
      { key: "job", label: "job", value: "recon-0042", width: 112 },
      {
        key: "mode",
        label: "mode",
        type: "select",
        value: "poll",
        width: 158,
        picksTape: true,
        options: [
          { value: "poll", label: "Invokr polls" },
          { value: "callback", label: "Aarokya calls back" },
          { value: "both", label: "both — first wins" },
        ],
      },
      { key: "pending", label: "202s first", value: "3", width: 38, show: (v) => v.mode !== "callback" },
      { key: "retry_after", label: "Retry-After s", value: "2", width: 38, show: (v) => v.mode !== "callback" },
      { key: "callback_after", label: "calls back after s", value: "5", width: 38, show: (v) => v.mode !== "poll" },
      { key: "max_polls", label: "max_polls", value: "20", width: 44, show: (v) => v.mode !== "callback" },
    ],
    run: runLong,
  },
];

const takesIn = (page) => takes.filter((t) => t.page === page);

// ─── shared helpers ──────────────────────────────────────────────────────────

/// POST it, and if it is already there, PUT instead.
///
/// Page 1 cannot start from nothing twice: an endpoint any job has ever pointed
/// at cannot be deleted (`jobs.endpoint` is a foreign key and a retired job
/// still holds it), and a config an endpoint points at cannot be deleted
/// either. So the honest thing on a second run is the call a team actually
/// makes: the same body, and the row is updated in place.
async function put(run, collection, name, body, shownBody) {
  const created = await api("POST", `/v1/${collection}`, { body });
  run.wire({ verb: "POST", path: `/v1/${collection}`, status: created.status, req: shownBody ?? body, res: created.body });
  if (created.ok) return { ok: true, status: created.status, mark: String(created.status), res: created.body?.data ?? created.body };
  if (created.status !== 409) return { ok: false, status: created.status, mark: String(created.status), res: created.body };

  const { name: _name, ...rest } = body;
  const updated = await api("PUT", `/v1/${collection}/${name}`, { body: rest });
  run.wire({ verb: "PUT", path: `/v1/${collection}/${name}`, status: updated.status, res: updated.body, said: "updated in place" });
  return {
    ok: updated.ok,
    status: updated.status,
    mark: `${updated.status} · updated in place`,
    res: updated.body?.data ?? updated.body,
  };
}

/// Make sure page 1's four exist, quietly. `just demo` deliberately leaves the
/// mandates workspace empty of them so page 1's first run is four real
/// creations — but pages 2 and 3 can be shown on their own.
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

async function tailTarget(run, from) {
  try {
    const res = await fetch(`/control/mock/log?since=${from}&limit=30`);
    const entries = ((await res.json())?.data ?? []).slice().reverse();
    if (!entries.length) return { seq: from, entries: [] };
    run.emit("receipt", { entries });
    return { seq: Math.max(from, ...entries.map((e) => e.seq)), entries };
  } catch {
    return { seq: from, entries: [] }; // the target's log is a nicety, never a dependency
  }
}

// ─── page 1's run ────────────────────────────────────────────────────────────

async function runSetup(run) {
  const N = SETUP_NAMES();
  const spec = state.status?.mandateSpec;
  const wsA = state.status?.provisioned?.workspaces?.a;
  if (!spec) {
    run.halt("config", "the demo server has not provisioned yet — give it a moment");
    return false;
  }

  const cfgBody = {
    name: N.config,
    values: { base_url: state.status?.mockUrl ?? "http://localhost:9999", team: wsA?.slug ?? "mandates" },
  };
  // Each row that lands lights its store, and the placeholder it satisfies
  // lights with it — so the endpoint on the right assembles itself.
  const built = {};
  const rows = [];
  const land = (k, label, res) => {
    rows.push({ id: k, label, note: res.mark, at: k, tone: res.ok ? "ok" : "bad" });
    run.facts({ built: { ...built }, tokens: [...rows] });
  };

  run.facts({ cfgBody, built, tokens: [], edge: "config" });
  run.step("config");
  const cfg = await put(run, "configs", N.config, cfgBody);
  built.config = true;
  run.facts({ cfgRes: cfg.res, cfgMark: cfg.mark, cfgOk: cfg.ok });
  land("config", N.config, cfg);
  if (!cfg.ok) {
    run.halt("config", `Invokr refused it — <b>${cfg.status}</b>`);
    return false;
  }

  run.facts({ secName: N.secret, secRead: null, edge: "secret" });
  run.step("secret");
  const sec = await put(
    run,
    "secrets",
    N.secret,
    { name: N.secret, value: `Bearer aarokya-demo-${wsA?.slug ?? "mandates"}-7d41c9` },
    { name: N.secret, value: "••••••••" },
  );
  const readBack = await api("GET", `/v1/secrets/${N.secret}`);
  run.wire({ verb: "GET", path: `/v1/secrets/${N.secret}`, status: readBack.status, res: readBack.body, said: "no value field" });
  const leaked = JSON.stringify(readBack.body ?? {}).includes('"value"');
  built.secret = true;
  run.facts({ secRead: readBack.body?.data ?? readBack.body });
  land("secret", N.secret, sec);
  if (!sec.ok || leaked) {
    run.halt("secret", leaked ? "the API returned a secret value — <b>check this</b>" : `Invokr refused it — <b>${sec.status}</b>`);
    return false;
  }

  run.facts({ psBody: MANDATE_SCHEMA, edge: "payload" });
  run.step("payload");
  const ps = await put(run, "payload-specs", N.payloadSpec, { name: N.payloadSpec, schema: MANDATE_SCHEMA });
  built.payload = true;
  land("payload", N.payloadSpec, ps);
  if (!ps.ok) {
    run.halt("payload", `Invokr refused it — <b>${ps.status}</b>`);
    return false;
  }

  run.facts({ edge: null });
  run.step("endpoint");
  const ep = await put(run, "endpoints", N.endpoint, spec);
  built.endpoint = ep.ok;
  run.facts({
    built: { ...built },
    epMark: ep.mark,
    epOk: ep.ok,
    halt: ep.ok ? null : `Invokr refused it — <b>${ep.status}</b>`,
  });
  if (!ep.ok) run.step("endpoint", { bad: true });
  return ep.ok;
}

// ─── page 2's run ────────────────────────────────────────────────────────────

/// Countdowns are emitted, not computed on screen, so a replay reproduces them.
/// The clock runs on the chip's face, in the lane, which is the claim: the row
/// is the timer.
function countdown(run, deadline, total, chip) {
  const timer = setInterval(() => {
    const left = deadline - Date.now();
    if (run.cancelled || left <= 0) {
      clearInterval(timer);
      if (!run.cancelled) run.facts({ tokens: [chip("due", "due now", "", chipBar(0, total))] });
      return;
    }
    run.facts({ tokens: [chip("later", `due in ${(left / 1000).toFixed(1)}s`, "hold", chipBar(left, total))] });
  }, 500);
  return timer;
}

const jobRow = (job, endpoint) => ({
  e: esc(endpoint ?? job.endpoint),
  t: esc(job.trigger),
  w: esc(job.cron ? job.cron : job.run_at ? clock(job.run_at) : "now"),
  s: F.status(job.status),
});

const execRow = (exec) => ({
  n: 1,
  s: F.status(exec.status),
  w: exec.run_at ? esc(clock(exec.run_at)) : "—",
  worker: esc(short(exec.worker_id)),
  tries: exec.attempt_count ?? 0,
});

const attemptRows = (attempts, idem) =>
  attempts.map((a) => ({
    n: a.attempt_number,
    s: F.status(a.status),
    c: esc(String(a.output?.status_code ?? a.error?.status_code ?? "—")),
    d: a.duration_ms != null ? `${a.duration_ms}ms` : "—",
    k: esc(idem ?? "—"),
  }));

/// The body page 2 will POST, built from whatever the fields say. Shown on the
/// first frame before you press anything, and sent verbatim when you do.
///
/// The team's real policy waits a minute before the second try. Ask for
/// failures and the same call runs with seconds instead, so the shape fits a
/// demo slot — the frame says so rather than implying Invokr is that impatient.
function shortJobBody(v) {
  const failures = clamp(v.fail_times, 0, 2, 0);
  const seconds = clamp(v.seconds, 5, 120, 15);
  const body = {
    trigger: v.trigger ?? "IMMEDIATE",
    endpoint: failures > 0 ? "aarokya-mandate-sync-impatient" : SETUP_NAMES().endpoint,
    idempotency_key: key(`${v.mandate_id}-t1`),
    input: { mandate_id: `${v.mandate_id}-${Date.now().toString(36).slice(-4)}`, checks: "1", fail_times: String(failures) },
    max_attempts: clamp(v.attempts, 1, 3, 3),
  };
  if (v.trigger === "DELAYED") body.run_at = new Date(Date.now() + seconds * 1000).toISOString();
  if (v.trigger === "CRON") {
    body.cron = v.cron || "* * * * *";
    body.timezone = "Asia/Kolkata";
  }
  return body;
}

async function runShort(run, v) {
  await control("/mock/reset");
  await ensureSetup();
  let logSeq = await targetLogHead();

  const jobBody = shortJobBody(v);
  const { endpoint, idempotency_key: idem } = jobBody;
  const seconds = clamp(v.seconds, 5, 120, 15);
  const mandate = jobBody.input.mandate_id;
  const chip = (at, note, tone, bar) => ({ id: "job", label: mandate, note, at, tone, bar });

  run.facts({
    jobBody,
    cron: jobBody.cron,
    isCron: v.trigger === "CRON",
    workerSlots: workerSlots(),
    verdict: null,
    tokens: [chip("service", "about to be sent")],
    edge: "enqueue",
  });
  run.step("ask");

  const t0 = performance.now();
  const res = await api("POST", "/v1/jobs", { body: jobBody });
  run.wire({ verb: "POST", path: "/v1/jobs", status: res.status, req: jobBody, res: res.body });
  if (!res.ok) {
    run.facts({ tokens: [chip("service", `refused — ${res.status}`, "bad")], edge: null });
    run.halt("written", `Invokr refused it — <b>${res.status}</b>`);
    return false;
  }
  // A CRON job answers with the schedule and no execution: pg_cron inserts the
  // first one when the minute turns, which is the point of the next frame.
  // The create response's `execution` is a stub — id, created_at and PENDING —
  // so the row's `run_at` has to come off the job it belongs to.
  const job = res.body.data;
  const later = v.trigger !== "IMMEDIATE";
  run.facts({
    answeredIn: ms(performance.now() - t0),
    job: jobRow(job, endpoint),
    exec: job.execution ? execRow({ ...job.execution, run_at: job.run_at ?? job.execution.created_at }) : null,
    nextRun: job.next_run_at ? clock(job.next_run_at) : null,
  });
  run.step("written");
  // The chip enters the lane on this beat, not the one before it.
  if (job.execution) run.facts({ tokens: [chip(later ? "later" : "due", "QUEUED", later ? "hold" : "")], edge: null });

  if (v.trigger === "CRON") return await runCronRest(run, { job, v, endpoint, idem, logSeq, chip });

  run.step("due");
  if (v.trigger === "DELAYED") {
    run.facts({ tokens: [chip("later", `due in ${seconds}.0s`, "hold", chipBar(seconds * 1000, seconds * 1000))] });
  }
  if (v.trigger === "DELAYED") countdown(run, new Date(jobBody.run_at).getTime(), seconds * 1000, chip);

  const done = await follow(run, {
    job,
    executionId: job.execution.execution_id,
    endpoint,
    idem,
    logSeq,
    chip,
    timeout: (seconds + 140) * 1000,
  });
  if (!done) {
    run.halt("record", "it did not finish inside the time we waited");
    return false;
  }
  run.facts({ finalAttempts: done.seen, finalStatus: done.status });
  run.step("record", { bad: done.status !== "SUCCESS" });
  return done.status === "SUCCESS";
}

async function runCronRest(run, { job, v, endpoint, idem, logSeq, chip }) {
  run.facts({ tokens: [], edge: "enqueue" });
  run.step("due");
  const wanted = clamp(v.ticks, 1, 3, 1);
  let ok = true;
  let seq = logSeq;

  for (let n = 1; n <= wanted; n++) {
    const started = performance.now();
    let seen = (await api("GET", `/v1/jobs/${job.job_id}/executions?limit=20`)).body?.data?.length ?? 0;
    let tick = null;
    while (performance.now() - started < 190000 && !run.cancelled && !tick) {
      const execs = (await api("GET", `/v1/jobs/${job.job_id}/executions?limit=20`)).body?.data ?? [];
      if (execs.length > seen) tick = execs[0];
      else await sleep(900);
    }
    if (!tick) {
      run.halt("tick", "no tick landed in the time we waited");
      return false;
    }
    const tickChip = (at, note, tone, bar) => ({ id: `tick${n}`, label: `tick ${n}`, note, at, tone, bar });
    run.facts({
      tickAt: clock(tick.created_at),
      tickN: n,
      exec: execRow(tick),
      tokens: [tickChip("due", "QUEUED")],
      edge: null,
    });
    run.emit("step", { id: "tick", at: Math.max(0, new Date(tick.created_at) - run.wallT0) });

    const done = await follow(run, {
      job,
      executionId: tick.execution_id,
      endpoint,
      idem: null,
      logSeq: seq,
      chip: tickChip,
      timeout: 60000,
    });
    seq = done?.logSeq ?? seq;
    ok = ok && done?.status === "SUCCESS";
  }

  run.step("record", { bad: !ok });
  const cancel = await api("POST", `/v1/jobs/${job.job_id}/cancel`);
  run.wire({ verb: "POST", path: `/v1/jobs/${job.job_id.slice(0, 8)}…/cancel`, status: cancel.status, res: cancel.body });
  run.facts({ halt: cancel.ok ? null : `cancel failed — ${cancel.status}` });
  run.step("cancel", { bad: !cancel.ok });
  return ok && cancel.ok;
}

/// Follow one execution to its end, moving through claim / call / answer.
/// The caller marks `record` itself, because cron has to cancel the schedule
/// before the record frame is the last thing on screen.
///
/// `silent` follows the execution without walking those three steps: the takes
/// that run two or three jobs in a row have their own frames, and stepping
/// through somebody else's journey three times is exactly the noise this page
/// is trying to lose. The wire drawer still gets everything.
async function follow(run, { job, executionId, ws = "a", endpoint, idem, logSeq, chip, timeout = 90000, silent = false }) {
  const started = performance.now();
  const offset = (iso) => Math.max(0, new Date(iso) - run.wallT0);
  const mandateId = job.execution?.input?.mandate_id ?? jobInput(job);
  const team = state.status?.provisioned?.workspaces?.[ws]?.slug ?? "mandates";
  /// Moving the chip is the explanation, so it is emitted like any other fact
  /// and a replay reproduces the journey exactly.
  ///
  /// Order matters: panel data goes *before* its step so the frame draws
  /// complete, and the move goes *after* so it happens while you are looking at
  /// the step that explains it. Emitted first, it would be drained during the
  /// previous beat — the chip leaving for the worker while the room is still
  /// reading the answer that sent it there.
  const moveChip = (at, note, tone, extra = {}) => chip && run.facts({ tokens: [chip(at, note, tone)], ...extra });
  let seen = 0;
  let claimed = false;
  let seq = logSeq;
  let lastBackoff = null;

  // The key the room is shown is the key Aarokya reports having received.
  // `executions.idempotency_key` is a real column but the API does not
  // serialise it, so for a cron tick — where the key is generated per
  // execution rather than sent with the job — the target's log is the only
  // honest source. `idem` is the fallback for the case where we chose it.
  let wireKey = idem ?? null;

  while (performance.now() - started < timeout && !run.cancelled) {
    const exec = (await api("GET", `/v1/executions/${executionId}`, { ws })).body?.data;
    if (exec) {
      const attempts = ((await api("GET", `/v1/executions/${executionId}/attempts`, { ws })).body?.data ?? [])
        .slice()
        .sort((a, b) => a.attempt_number - b.attempt_number);

      let fresh = [];
      ({ seq, entries: fresh } = await tailTarget(run, seq));
      const mine = fresh.find((e) => e.idempotency_key && e.body?.mandate_id === mandateId);
      if (mine) wireKey = mine.idempotency_key;

      if (!silent) run.facts({ exec: execRow(exec), attempts: attemptRows(attempts, wireKey) });

      // Winning the claim writes `worker_id` and `started_at` on the row, and
      // the first attempt follows immediately. All three can land between our
      // two GETs, so take whichever evidence arrived — and stamp the step at
      // the row's own `started_at`, not at the moment we happened to notice,
      // or the claim reads as having happened after the call it caused.
      if (!claimed && !silent && (exec.worker_id || attempts.length)) {
        claimed = true;
        const who = await whoClaimed(executionId, ws, exec.worker_id);
        const at = exec.started_at ?? attempts[0]?.started_at;
        run.facts({
          winner: who,
          exec: execRow({ ...exec, worker_id: who ?? exec.worker_id }),
          workerSlots: workerSlots(who),
        });
        run.emit("step", { id: "claim", at: at ? offset(at) : Math.round(performance.now() - run.t0) });
        moveChip(workerAnchor(who), "RUNNING", "", { edge: "claim" });
      }

      // While the row is backing off, the clock runs on the chip's face in the
      // lane. Once a second, not every poll, or the tape fills with ticks.
      if (!silent && seen > 0 && exec.run_at) {
        const left = new Date(exec.run_at) - Date.now();
        const secs = Math.ceil(left / 1000);
        if (left > 0 && secs !== lastBackoff) {
          lastBackoff = secs;
          moveChip("later", `try ${seen + 1} in ${secs}s`, "hold");
        }
      }

      for (const a of silent ? [] : attempts.slice(seen)) {
        const ok = a.status === "SUCCESS";
        const code = a.output?.status_code ?? a.error?.status_code ?? "";
        lastBackoff = null;
        const more = !ok && a.attempt_number < (exec.max_attempts ?? 1);

        run.facts({
          winner: exec.worker_id,
          template: `POST {{config.base_url}}/mandates/{{input.mandate_id}}/sync\nAuthorization: {{secret.${
            SETUP_NAMES().secret
          }}}\nx-team: {{config.team}}`,
          resolved:
            `POST ${esc(state.status?.mockUrl ?? "")}/mandates/${esc(mandateId)}/sync\n` +
            `Authorization: ••••••••\nx-team: ${esc(team)}\n<u>x-invokr-idempotency-key: ${esc(wireKey ?? "—")}</u>`,
          idem: wireKey,
          sentAt: clock(a.started_at),
          verdict: null,
        });
        run.emit("step", { id: "call", at: offset(a.started_at) });
        // Every attempt goes out from a worker, so the chip travels for every
        // attempt — not only the first. A retry re-claims the row in the gap
        // between two polls, so waiting to catch that gap meant the second and
        // third tries never left the lane on screen.
        run.facts({ workerSlots: workerSlots(exec.worker_id), edge: "call" });
        moveChip(workerAnchor(exec.worker_id), a.attempt_number > 1 ? `try ${a.attempt_number}` : "RUNNING");

        // The answer comes back and the row goes one of two ways. That fork is
        // the loop: onward to `finished`, or round again to `not yet` with the
        // backoff running.
        run.facts({
          answerBody: asJson(a.output?.body) ?? a.error ?? null,
          answerCode: code,
          answerTook: ms(a.duration_ms),
          answerOk: ok,
        });
        run.emit("step", { id: "answer", at: offset(a.completed_at ?? a.started_at), bad: !ok });
        run.facts({
          verdict: { text: `${code || (ok ? "200" : "error")}`, tone: ok ? "ok" : "bad", sub: ok ? "terminal" : "it refused" },
          edge: "back",
          backTone: ok ? "act" : "warm",
          backLabel: ok ? "SUCCESS, written back" : more ? `try ${a.attempt_number + 1} · run_at + backoff` : "FAILED, out of tries",
        });
        moveChip(more ? "later" : "done", more ? `retry ${a.attempt_number + 1}` : ok ? "SUCCESS" : "FAILED", more ? "hold" : ok ? "ok" : "bad");
      }
      seen = attempts.length;

      if (["SUCCESS", "FAILED", "CANCELLED"].includes(exec.status)) {
        ({ seq } = await tailTarget(run, seq));
        if (!silent) {
          // `executions.worker_id` is only published once the row is
          // terminal, so this is the first honest chance to name the winner —
          // and the loser's "skipped it — locked" with it.
          run.facts({
            exec: execRow(exec),
            attempts: attemptRows(attempts, wireKey),
            finalAttempts: seen,
            finalStatus: exec.status,
            workerSlots: workerSlots(exec.worker_id),
            edge: null,
          });
          moveChip("done", exec.status, exec.status === "SUCCESS" ? "ok" : "bad");
        }
        return { status: exec.status, seen, logSeq: seq };
      }
    }
    await sleep(160);
  }
  return null;
}

const jobInput = (job) => job?.input?.mandate_id ?? "";

/// Fire the same endpoint name into both workspaces and let Aarokya's own log
/// be the proof that they were two different calls.
async function runTeams(run, v) {
  const N = SETUP_NAMES();
  await control("/mock/reset");
  await ensureSetup();
  // Take the mark before anything is created: an IMMEDIATE job can be
  // delivered before a second round trip finishes, and reading the head
  // afterwards would skip past the very entry we want to quote.
  const seq0 = await targetLogHead();
  const ws = state.status?.provisioned?.workspaces ?? {};

  const stamp36 = Date.now().toString(36).slice(-4);
  const bodies = {
    a: { trigger: "IMMEDIATE", endpoint: N.endpoint, input: { mandate_id: `${v.mandate_id}-a-${stamp36}`, checks: "1", fail_times: "0" }, max_attempts: 1 },
    b: { trigger: "IMMEDIATE", endpoint: N.endpoint, input: { mandate_id: `${v.mandate_id}-b-${stamp36}`, checks: "1", fail_times: "0" }, max_attempts: 1 },
  };
  const team = { a: "mandates", b: "rides" };
  const chips = { a: { at: "service", note: "mandates" }, b: { at: "service", note: "rides" } };
  const draw = (extra = {}) =>
    run.facts({
      tokens: ["a", "b"].map((k) => ({ id: k, label: bodies[k].input.mandate_id, at: chips[k].at, note: chips[k].note, tone: chips[k].tone })),
      ...extra,
    });

  run.facts({
    schemaA: (ws.a?.schema_name ?? "").slice(-9),
    schemaB: (ws.b?.schema_name ?? "").slice(-9),
    workerSlots: workerSlots(),
    bodyA: bodies.a,
    bodyB: bodies.b,
  });
  draw({ edge: null });
  run.step("name");

  const jobs = {};
  for (const k of ["a", "b"]) {
    const res = await api("POST", "/v1/jobs", { body: bodies[k], ws: k });
    run.wire({ verb: "POST", path: `/v1/jobs  (${team[k]})`, status: res.status, req: bodies[k], res: res.body });
    run.facts({ [k === "a" ? "statusA" : "statusB"]: String(res.status) });
    if (!res.ok) {
      run.halt("fire", `the ${team[k]} workspace refused it — <b>${res.status}</b>`);
      return false;
    }
    jobs[k] = res.body.data;
    chips[k] = { at: "due", note: `${team[k]} · QUEUED` };
  }
  draw({ edge: "enqueue" });
  run.step("fire");

  let ok = true;
  let seq = seq0;
  for (const k of ["a", "b"]) {
    const done = await follow(run, {
      job: jobs[k],
      executionId: jobs[k].execution.execution_id,
      ws: k,
      endpoint: N.endpoint,
      idem: jobs[k].execution.idempotency_key,
      logSeq: seq,
      timeout: 60000,
      silent: true,
    });
    seq = done?.logSeq ?? seq;
    ok = ok && done?.status === "SUCCESS";
    const exec = (await api("GET", `/v1/executions/${jobs[k].execution.execution_id}`, { ws: k })).body?.data;
    if (exec) run.facts({ [k === "a" ? "execA" : "execB"]: execRow(exec) });
    chips[k] = { at: "done", note: `${team[k]} · ${exec?.status ?? "—"}`, tone: exec?.status === "SUCCESS" ? "ok" : "bad" };
    draw({
      edge: "back",
      backTone: "act",
      backLabel: "written back, to different schemas",
      verdict: { text: "200", tone: "ok", sub: `answered both, as different teams` },
    });
  }
  run.step("ran", { bad: !ok });

  // The proof is Aarokya's own sentence, not ours: it names the team out of the
  // header it was sent, and we never told it which workspace was calling.
  const log = await fetch(`/control/mock/log?since=${seq0}&limit=30`)
    .then((r) => r.json())
    .then((j) => j?.data ?? [])
    .catch(() => []);
  const said = (id) => log.find((e) => e.body?.mandate_id === id)?.summary;
  run.facts({ saidA: said(bodies.a.input.mandate_id), saidB: said(bodies.b.input.mandate_id) });
  run.step("proof", { bad: !ok });
  if (!ok) run.facts({ halt: "one of the two did not succeed — the wire drawer has both sides" });
  return ok;
}

/// The three destinations, and which of them this machine can actually reach.
function transportTargets() {
  const probes = state.status?.transports ?? {};
  return [
    { type: "HTTP", endpoint: SETUP_NAMES().endpoint, where: "POST to Aarokya", up: true },
    { type: "Kafka", endpoint: "mandate-events-kafka", where: "topic mandate-events", up: !!probes.kafka },
    { type: "Redis", endpoint: "mandate-events-redis", where: "stream mandate-events", up: !!probes.redis },
  ];
}

const transportRows = () =>
  transportTargets().map((t) => ({
    e: esc(t.endpoint),
    t: esc(t.type),
    w: esc(t.where),
    s: t.up ? F.status("ACTIVE") : `<span class="st">broker not running</span>`,
  }));

/// One job shape, three destinations. Whatever broker is not running here says
/// so rather than being quietly skipped.
async function runTransports(run, v) {
  await ensureSetup();
  const targets = transportTargets();
  const live = targets.filter((t) => t.up);
  const chips = live.map((t, i) => ({ id: `t${i}`, label: `${v.mandate_id}-${i + 1}`, note: t.type, at: "service" }));
  const draw = (extra = {}) => run.facts({ tokens: chips.map((c) => ({ ...c })), ...extra });

  run.facts({ endpoints: transportRows(), workerSlots: workerSlots() });
  draw({ edge: null });
  run.step("three");

  const sent = [];
  const skipped = [];
  let ok = true;
  let n = 0;

  for (const t of targets) {
    if (!t.up) {
      skipped.push(t.type);
      continue;
    }
    const chip = chips[n];
    n++;
    const body = {
      trigger: "IMMEDIATE",
      endpoint: t.endpoint,
      input: { mandate_id: chip.label, checks: "1", fail_times: "0" },
      max_attempts: 1,
    };
    const res = await api("POST", "/v1/jobs", { body });
    run.wire({ verb: "POST", path: `/v1/jobs  (${t.type})`, status: res.status, req: body, res: res.body });
    if (!res.ok) {
      ok = false;
      Object.assign(chip, { at: "done", note: `${t.type} · refused`, tone: "bad" });
      draw();
      sent.push({ e: esc(t.endpoint), t: esc(t.type), s: F.status("FAILED"), worker: "—" });
      continue;
    }
    Object.assign(chip, { at: "due", note: `${t.type} · QUEUED` });
    draw({ edge: "enqueue" });

    const job = res.body.data;
    const done = await follow(run, {
      job,
      executionId: job.execution.execution_id,
      endpoint: t.endpoint,
      idem: job.execution.idempotency_key,
      logSeq: 0,
      timeout: 60000,
      silent: true,
    });
    ok = ok && done?.status === "SUCCESS";
    const exec = (await api("GET", `/v1/executions/${job.execution.execution_id}`)).body?.data;
    Object.assign(chip, {
      at: "done",
      note: `${t.type} · ${exec?.status ?? done?.status ?? "—"}`,
      tone: (exec?.status ?? done?.status) === "SUCCESS" ? "ok" : "bad",
    });
    sent.push({
      e: esc(t.endpoint),
      t: esc(t.type),
      s: F.status(exec?.status ?? done?.status ?? "—"),
      worker: esc(short(exec?.worker_id)),
    });
    run.facts({ sent: [...sent], skipped });
    draw({
      edge: "back",
      backTone: "act",
      backLabel: "same record, whatever the destination",
      verdict: { text: t.type, tone: "ok", sub: "delivered" },
    });
  }

  run.facts({ sent, skipped, sentCount: sent.length });
  run.step("send", { bad: !ok });
  run.step("same", { bad: !ok });
  return ok;
}

// ─── page 3's run ────────────────────────────────────────────────────────────

/// Page 3's job body. It says nothing about the work taking minutes — the
/// endpoint carries that — which is the point of the first frame.
function longJobBody(v) {
  const block = asyncSpecFrom(v);
  return {
    trigger: "IMMEDIATE",
    endpoint: "aarokya-bulk-recon",
    idempotency_key: key(`${v.job}-long`),
    input: { job: v.job },
    async_overrides: { max_wait_ms: block.max_wait_ms, ...(block.max_polls ? { max_polls: block.max_polls } : {}) },
  };
}

function asyncSpecFrom(v) {
  const block = { status_codes: [202] };
  if (v.mode !== "callback") {
    block.poll = {
      success_statuses: [200],
      pending_statuses: [202],
      failure_statuses: [400, 404, 410],
      initial_delay_ms: 1000,
      max_delay_ms: 10000,
      backoff: "exponential",
    };
    block.max_polls = clamp(v.max_polls, 1, 100, 20);
  }
  // A plain boolean, not `{ enabled: true }` — the design doc says the latter,
  // the implementation takes the former, and the implementation is what runs.
  if (v.mode !== "poll") block.callback = true;
  block.max_wait_ms = 120000;
  return block;
}

async function runLong(run, v) {
  run.facts({
    isLong: true,
    workerSlots: workerSlots(),
    asyncBlock: asyncSpecFrom(v),
    tokens: [{ id: "job", label: v.job, note: "about to be sent", at: "service" }],
    edge: "enqueue",
    verdict: null,
  });
  if (!state.status?.longRunning) {
    run.halt("ask", "This build has no long-running support — it lives on <b>feat/long-running-jobs</b>. Switch on replay.");
    return false;
  }

  const asyncBlock = asyncSpecFrom(v);
  const pending = v.mode === "callback" ? 0 : clamp(v.pending, 0, 8, 3);
  const retryAfter = clamp(v.retry_after, 1, 30, 2);
  const callbackAfter = clamp(v.callback_after, 1, 120, 5);

  await control("/mock/reset");
  let logSeq = await targetLogHead();

  const body = { job: "{{input.job}}" };
  if (v.mode !== "callback") {
    body.script = [
      ...Array.from({ length: pending }, () => ({ status: 202, body: { state: "working" }, retry_after: retryAfter })),
      { status: 200, body: { state: "done", mandates: 128_000 } },
    ];
  }
  if (v.mode !== "poll") {
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

  await ensureSetup();
  const exists = (await api("GET", `/v1/endpoints/${spec.name}`)).ok;
  const { name: _n, ...rest } = spec;
  const saved = exists
    ? await api("PUT", `/v1/endpoints/${spec.name}`, { body: rest })
    : await api("POST", "/v1/endpoints", { body: spec });
  run.wire({ verb: exists ? "PUT" : "POST", path: `/v1/endpoints/${spec.name}`, status: saved.status, req: spec, res: saved.body });
  if (!saved.ok) {
    run.halt("ask", `Invokr refused the endpoint — <b>${saved.status}</b>`);
    return false;
  }

  const jobBody = longJobBody(v);
  run.facts({ jobBody });
  run.step("ask");

  const t0 = performance.now();
  const res = await api("POST", "/v1/jobs", { body: jobBody });
  run.wire({ verb: "POST", path: "/v1/jobs", status: res.status, req: jobBody, res: res.body });
  if (!res.ok) {
    run.halt("written", `Invokr refused it — <b>${res.status}</b>`);
    return false;
  }
  run.facts({
    answeredIn: ms(performance.now() - t0),
    tokens: [{ id: "job", label: v.job, note: "QUEUED", at: "due" }],
    edge: null,
  });
  run.step("written");

  return await followLong(run, res.body.data.execution.execution_id, { logSeq, jobName: v.job, mode: v.mode });
}

/// Page 3's loop. Each check is the same circuit as a retry: out of the lane,
/// to a worker, to the target, back to the lane. The chip goes round it once
/// per poll, which is the only honest way to show that ten check-ins are still
/// one dispatch.
async function followLong(run, executionId, { logSeq, jobName, mode }) {
  const started = performance.now();
  const offset = (iso) => Math.max(0, new Date(iso) - run.wallT0);
  let claimed = false;
  let sent = false;
  let seenPolls = 0;
  let seq = logSeq;
  let sawCallback = false;

  const chip = (at, note, tone) => ({ id: "job", label: jobName, note, at, tone });
  const move = (at, note, tone, extra = {}) => run.facts({ tokens: [chip(at, note, tone)], ...extra });
  const pollRows = [];

  while (performance.now() - started < 240000 && !run.cancelled) {
    const exec = (await api("GET", `/v1/executions/${executionId}`)).body?.data;
    if (exec) {
      const attempts = (await api("GET", `/v1/executions/${executionId}/attempts`)).body?.data ?? [];
      const first = attempts.find((x) => x.attempt_number === 1);

      // `attempt_count`, `worker_id` and the attempt row are separate writes,
      // and a read can land between them. Any of the three is proof a worker
      // took it; the row's own `started_at` says when, so the claim never
      // reads as having happened after the dispatch it caused.
      if (!claimed && (exec.attempt_count > 0 || exec.worker_id || first)) {
        claimed = true;
        const at = exec.started_at ?? first?.started_at;
        const who = await whoClaimed(executionId, "a", exec.worker_id);
        run.facts({ workerSlots: workerSlots(who) });
        run.emit("step", { id: "claim", at: at ? offset(at) : Math.round(performance.now() - run.t0) });
        move(workerAnchor(who), "RUNNING", "", { edge: "claim" });
      }

      if (first && !sent) {
        sent = true;
        run.emit("step", { id: "send", at: offset(first.started_at) });
        move(workerAnchor(exec.worker_id), "one dispatch", "", { edge: "call", verdict: null });

        run.emit("step", { id: "accepted", at: offset(first.completed_at ?? first.started_at) });
        run.facts({ verdict: { text: "202", tone: "hold", sub: "accepted, still working" } });

        run.facts({ nextCheck: exec.run_at ? clock(exec.run_at) : "—", polls: exec.poll_count ?? 0 });
        run.emit("step", { id: "wait", at: offset(first.completed_at ?? first.started_at) });
        run.facts({ edge: "back", backTone: "warm", backLabel: "parked · nothing held open" });
        move("later", "WAITING", "hold");
      }

      const polls = ((await api("GET", `/v1/executions/${executionId}/polls`)).body?.data ?? [])
        .slice()
        .sort((a, b) => a.poll_number - b.poll_number);

      for (const p of polls.slice(seenPolls)) {
        const pending = p.classification === "PENDING";
        pollRows.push({
          n: p.poll_number,
          c: esc(String(p.status_code)),
          r: p.retry_after_ms ? ms(p.retry_after_ms) : "—",
          k: esc(p.classification.toLowerCase().replace("_", " ")),
        });
        run.facts({
          polls: p.poll_number,
          pollRows: [...pollRows],
          lastRetryAfter: p.retry_after_ms ? ms(p.retry_after_ms) : null,
          claimLabel: `check ${p.poll_number}`,
          callLabel: "GET",
        });
        run.emit("step", { id: "poll", at: offset(p.polled_at) });

        // Round the loop once, for this one check — out of the lane, to a
        // worker, and back. Both legs land inside this step's own beat, so the
        // outbound one needs room to be seen before the return overwrites it.
        // The pause is in our narration, not in the system: the check already
        // happened, and we are reading its row.
        move(workerAnchor(exec.worker_id), `check ${p.poll_number}`, "", { edge: "claim" });
        run.emit("beat", { ms: 700 });
        run.facts({
          verdict: { text: String(p.status_code), tone: pending ? "hold" : p.classification === "SUCCESS" ? "ok" : "bad", sub: pending ? "still working" : "terminal" },
          edge: "back",
          backTone: pending ? "warm" : "act",
          backLabel: p.retry_after_ms ? `Retry-After ${ms(p.retry_after_ms)}` : "terminal",
        });
        move(pending ? "later" : "done", pending ? "WAITING" : "done", pending ? "hold" : "ok");
      }
      seenPolls = polls.length;

      const tailed = await tailTarget(run, seq);
      seq = tailed.seq;
      for (const e of tailed.entries) {
        if (e.path !== "(callback)" || sawCallback) continue;
        sawCallback = true;
        run.facts({
          verdict: { text: "callback", tone: "ok", sub: "POST /v1/callbacks/…/complete" },
          edge: "back",
          backTone: "act",
          backLabel: "Aarokya calls us — straight into the row",
        });
        move("done", "finalised by callback", "ok");
        run.emit("step", { id: "poll", at: offset(e.at) });
      }

      if (["SUCCESS", "FAILED", "CANCELLED"].includes(exec.status)) {
        ({ seq } = await tailTarget(run, seq));
        const ok = exec.status === "SUCCESS";
        run.facts({
          finishNote: ok
            ? sawCallback && seenPolls > 0
              ? `Both were in flight. The <b>callback</b> arrived first; the next poll would have seen zero rows changed and quietly stopped.`
              : sawCallback
                ? `The other side <b>called back</b>. Nobody polled anything — the URL was in the body we sent.`
                : `Check ${seenPolls} came back terminal, so the polling stopped there.`
            : null,
          finalAttempts: exec.attempt_count ?? 1,
          polls: seenPolls,
          workerSlots: workerSlots(exec.worker_id),
          edge: null,
        });
        move("done", exec.status, ok ? "ok" : "bad");
        run.step("finish", { bad: !ok });
        run.step("record", { bad: !ok });
        return ok;
      }
    }
    await sleep(200);
  }
  run.halt("record", "it did not finish inside the time we waited");
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
const stepsOf = (take) => (typeof take?.steps === "function" ? take.steps(valuesFor(take)) : take?.steps);

/// Which recording belongs to this take. Page 3's three modes tell three
/// different stories, so each keeps its own tape.
const tapeId = (take) => (take.tape ? take.tape(valuesFor(take)) : take.id);

function resetStage(take) {
  const t = take ?? currentTake();
  clearWire();
  state.facts = {};
  state.stepId = stepsOf(t)?.[0]?.id ?? null;
  // At rest the machine is already drawn and the work is sitting at the left,
  // with the call you are about to make built from whatever the fields say.
  // Change a field and both change with it.
  Object.assign(state.facts, t.preview?.(valuesFor(t)) ?? {});
  renderSteps(t);
  document.body.classList.add("idle");
  // Chips carried over from the last run would slide across the board from
  // wherever they were; start them where the new run starts.
  $("#toks").innerHTML = "";
  renderFrame();
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
      // In replay nothing is being dialled, so the knobs are dead — except the
      // one that chooses which recording plays, which is still a real choice.
      const disabled = state.replay && !f.picksTape ? "disabled" : "";
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

  // A field can decide which other fields matter, and the journey can change
  // shape with them.
  $("#inputs")
    .querySelectorAll("input, select")
    .forEach((el) =>
      el.addEventListener("change", () => {
        readValues(take);
        renderInputs(take);
        resetStage(take);
      }),
    );
}

/// Buttons that belong to the frame you are on — Kill a worker appears on the
/// frame where killing one proves something.
function renderExtras(take, step) {
  const list = step?.extras ?? (step ? [] : (take.extras ?? []));
  $("#extras").innerHTML = list
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
          await take.onExtra?.(b.dataset.act);
        } finally {
          b.disabled = false;
        }
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

  $("#claim").textContent = take.claim ?? "";
  $("#subclaim").innerHTML = take.sub ?? "";

  renderInputs(take);
  resetStage(take);

  const go = $("#go");
  go.style.display = take.action ? "" : "none";
  go.textContent = state.replay ? "Play the recording" : (take.action ?? "");
  go.disabled = false;

  renderTakes();
  syncTransport();
}

async function runTake() {
  const take = currentTake();
  if (!take?.action) return;
  const go = $("#go");
  const values = readValues(take);

  go.disabled = true;
  go.textContent = "running…";
  resetFilm();
  resetStage(take);
  document.body.classList.remove("idle");

  const run = new Run(take);
  state.run = run;

  const walking = play(take);
  let ok = false;
  try {
    ok = await take.run(run, values);
  } catch (err) {
    run.halt(stepsOf(take)?.[0]?.id, `something broke here: <b>${esc(String(err.message ?? err))}</b>`);
  }
  film.closed = true;
  await walking;
  if (ok) settleSteps();

  if (ok) state.ran.add(take.id);
  await run.save(ok);
  renderTakes();

  go.disabled = false;
  go.textContent = take.action;
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
    resetStage(take);
    state.facts.halt = `${esc(String(err.message ?? err))} — run it live once and it is kept`;
    renderFrame();
    go.disabled = false;
    go.textContent = "Play the recording";
    return;
  }

  resetFilm();
  resetStage(take);
  document.body.classList.remove("idle");
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
  settleSteps();

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

function toggleWire(force) {
  const open = force ?? !$("#drawer").classList.contains("open");
  $("#drawer").classList.toggle("open", open);
  $("#t-wire").classList.toggle("on", open);
}

/// Boxes and arrows at the size of a wall, one line of text, nothing else.
/// Same board and same live data — only what sits beside it changes. The beat
/// lengthens, because a room reads slower than a person at a desk.
function togglePresent(force) {
  const on = force ?? !document.body.classList.contains("present");
  document.body.classList.toggle("present", on);
  $("#t-present").classList.toggle("on", on);
  film.dwell = on ? 1900 : 1300;
  if (on) toggleWire(false);
  renderFrame();
  relayout();
}

// ─── boot ────────────────────────────────────────────────────────────────────

$("#go").addEventListener("click", () => (state.replay ? replayTake() : runTake()));
$("#replay-toggle").addEventListener("click", () => setReplay(!state.replay));
$("#t-next").addEventListener("click", stepNext);
$("#t-back").addEventListener("click", stepBack);
$("#t-auto").addEventListener("click", () => setMode(film.mode === "auto" ? "manual" : "auto"));
$("#t-wire").addEventListener("click", () => toggleWire());
$("#t-present").addEventListener("click", () => togglePresent());

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
  if (e.key === "Tab") {
    const list = takesIn(state.page);
    if (list.length < 2) return;
    e.preventDefault();
    return mountTake((state.current + (e.shiftKey ? list.length - 1 : 1)) % list.length);
  }
  if (e.key === "1") return setPage("setup");
  if (e.key === "2") return setPage("short");
  if (e.key === "3") return setPage("long");
  if (e.key.toLowerCase() === "a") return setMode(film.mode === "auto" ? "manual" : "auto");
  if (e.key.toLowerCase() === "w") return toggleWire();
  if (e.key.toLowerCase() === "p") return togglePresent();
  if (e.key.toLowerCase() === "r") return setReplay(!state.replay);
});

renderPages();
setMode("manual");
mountTake(0);
await refreshStatus();
setInterval(refreshStatus, 4000);
