// Invokr live demo.
//
// One idea at a time. Each step owns the whole canvas and is drawn for that
// idea alone — the SKIP LOCKED query, the secret that has no read path, the row
// parked as WAITING — with the list on the left acting as a table of contents
// rather than the explanation. Every page opens on an analogy, because the
// model has to exist before the mechanism means anything.
//
// Three rules hold it together:
//
//   1. A take's `run()` never touches the DOM. It emits two kinds of event:
//      `step` (move to the next frame) and `facts` (here is more real data).
//      One renderer draws them — which is what lets a recorded run replay
//      through exactly the same code.
//   2. A frame is a pure function of the facts gathered so far, so stepping
//      backwards is just drawing fewer events into a fresh bag.
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

// ─── the frame vocabulary ────────────────────────────────────────────────────
//
// Every frame is built from these, so the room learns one visual language and
// then only has to read the content.

const F = {
  wrap: (...kids) => `<div class="f">${kids.filter(Boolean).join("")}</div>`,
  lead: (html) => `<p class="f-lead">${html}</p>`,
  note: (html, tone) => `<p class="f-note ${tone ?? ""}">${html}</p>`,
  kicker: (text) => `<div class="f-kicker">${esc(text)}</div>`,
  cols: (...panes) => `<div class="f-cols">${panes.filter(Boolean).join("")}</div>`,
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

  flow: (...bits) => `<div class="f-flow">${bits.join("")}</div>`,
  box: (who, sub, cls) => `<div class="f-box ${cls ?? ""}"><div class="who">${esc(who)}</div><div class="sub">${sub ?? ""}</div></div>`,
  arrow: (cap, cls) => `<div class="f-arrow ${cls ?? ""}"><span class="glyph">→</span><span class="cap">${esc(cap ?? "")}</span></div>`,

  status: (s) => (s ? `<span class="st ${String(s).toLowerCase()}">${esc(s)}</span>` : "—"),
  bar: (left, total, label) => {
    const pct = total ? Math.max(0, Math.min(100, (1 - left / total) * 100)) : 0;
    return `${esc(label ?? "")}<span class="bar"><i style="width:${pct}%"></i></span>`;
  },
};

// ─── the steps, as a table of contents ──────────────────────────────────────

function renderSteps(take) {
  const steps = stepsOf(take) ?? [];
  $("#steps").innerHTML = steps
    .map(
      (s) => `<li class="step" data-step="${esc(s.id)}">
        <div class="title">${esc(s.label)}<span class="at"></span></div>
        <div class="blurb">${s.blurb ?? ""}</div>
      </li>`,
    )
    .join("");
  updateStepCount();
}

function markStep(id, cls, t) {
  const host = $("#steps");
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
  el.scrollIntoView({ block: "nearest", behavior: "smooth" });
  updateStepCount();
}

/// Two clocks feed the times on screen: steps the page stamps as it walks, and
/// steps taken from a row's own timestamp on the database's clock. They agree
/// to within a few milliseconds — enough, when a journey is walked twice, to
/// render a claim a hair before the write that produced it. A step is only ever
/// nudged up to the one it followed; nothing is invented.
function priorStepTime(el) {
  const all = [...$("#steps").querySelectorAll(".step")];
  const before = all
    .slice(0, all.indexOf(el))
    .reverse()
    .find((s) => s.dataset.at !== undefined);
  return before ? Number(before.dataset.at) : 0;
}

function settleSteps() {
  $("#steps")
    .querySelectorAll(".step.now")
    .forEach((n) => {
      n.classList.remove("now");
      n.classList.add("done");
    });
}

function updateStepCount() {
  const host = $("#steps");
  const all = host.querySelectorAll(".step").length;
  if (!all) return ($("#step-count").textContent = "");
  const done = host.querySelectorAll(".step.done, .step.now, .step.bad").length;
  $("#step-count").textContent = `${done} / ${all}`;
}

function renderFrame() {
  const take = currentTake();
  const steps = stepsOf(take) ?? [];
  const step = steps.find((s) => s.id === state.stepId) ?? steps[0];
  $("#frame").innerHTML = step?.frame ? step.frame(state.facts) : "";
  renderExtras(take, step);
}

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
  nudge: null,
  waiters: [],
};

const deliver = (ev) => film.events.push(ev);

function resetFilm() {
  film.events.length = 0;
  film.cursor = 0;
  film.closed = false;
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
    draw(take, ev);
    if (PACED.has(ev.type)) return true;
  }
  return false;
}

/// Everything on screen is the first `n` events drawn in order, so going back
/// is drawing fewer of them into a fresh bag of facts.
function redrawTo(take, n) {
  resetStage(take);
  film.cursor = 0;
  while (film.cursor < n) draw(take, film.events[film.cursor++]);
  renderFrame();
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

// ─── page 1 · what has to exist ──────────────────────────────────────────────

const SETUP_STEPS = [
  {
    id: "why",
    label: "Why anything has to exist first",
    blurb: "The same reason you save a payee before you pay them.",
    frame: (f) => `<div class="f-analogy">
      <div class="line">Before you can send money to someone, you save them once —
        their account, the bank, the authorisation, the name you will use.</div>
      <div class="card">payee &nbsp;<b>Aarokya</b><br/>account &nbsp;…<br/>auth &nbsp;&nbsp;&nbsp;&nbsp;••••••••</div>
      <div class="line dim">After that you never type the account number again.
        You say <i>“pay Aarokya”</i>, and everything else is looked up.</div>
      <div class="tie">Invokr is the same. You describe the call <b>once</b>, and from then on
        anything in your estate fires it <b>by name</b> — without knowing the URL, the credential,
        or how many times to try.</div>
      ${f.halt ? F.note(f.halt, "bad") : ""}
    </div>`,
  },
  {
    id: "config",
    label: "A config, for what changes",
    blurb: "Base URLs, API versions, which team you are. The things that differ between staging and production.",
    frame: (f) =>
      F.wrap(
        F.cols(
          F.pane("POST /v1/configs", F.code(json(f.cfgBody ?? { name: "…", values: {} }), "big")),
          f.cfgRes
            ? F.pane("what came back", F.code(json(f.cfgRes)), {
                meta: f.cfgMark,
                metaTone: f.cfgOk ? "ok" : "bad",
                cls: f.cfgOk ? "ok" : "bad",
              })
            : F.pane("what comes back", `<div class="empty">sending…</div>`, { cls: "dim" }),
        ),
        F.note(
          f.cfgOk
            ? `That <code>base_url</code> is different in staging. Changing it is <b>one PUT</b> — not a redeploy, not a config map, not a restart.`
            : "Two values, and a name to reach them by.",
          f.cfgOk ? "ok" : "",
        ),
      ),
  },
  {
    id: "secret",
    label: "A secret, for what must not leak",
    blurb: "Encrypted at rest. Ask Invokr for it back and you get the name and the timestamps. Never the value.",
    frame: (f) =>
      F.wrap(
        F.cols(
          F.pane("what you send", F.code(json({ name: f.secName ?? "…", value: "Bearer aarokya-…" }), "big"), { meta: "201" }),
          F.pane(
            `what you can read back`,
            f.secRead ? F.code(json(f.secRead), "big") : `<div class="empty">reading it back…</div>`,
            { meta: f.secRead ? "200" : "", metaTone: "ok", cls: f.secRead ? "ok" : "dim" },
          ),
        ),
        f.secRead
          ? F.note(
              `No <code>value</code> field. There is no read path for it at all — not for you, not for an operator, not in a log. The worker decrypts it at call time and that is the only place it exists in the clear.`,
              "ok",
            )
          : F.note("Sent once. Encrypted before it hits the table."),
      ),
  },
  {
    id: "payload",
    label: "A payload spec, so callers cannot get it wrong",
    blurb: "JSON Schema. A job whose input does not match is refused when it is created, not at three in the morning.",
    frame: (f) =>
      F.wrap(
        F.cols(
          F.pane("POST /v1/payload-specs", F.code(json(f.psBody ?? MANDATE_SCHEMA))),
          F.pane(
            "a job that does not satisfy it",
            F.code(
              `POST /v1/jobs\n{ "endpoint": "…", "input": { "checks": "1" } }\n\n` +
                `<u>← 422</u>  input does not match payload spec\n      'mandate-input': missing mandate_id`,
            ),
            { cls: "bad" },
          ),
        ),
        F.note(
          `The caller finds out <b>at the call site, immediately</b>. Not from a worker, at 3am, in a log nobody is reading.`,
          "warn",
        ),
      ),
  },
  {
    id: "endpoint",
    label: "An endpoint, which is those three plus the call",
    blurb: "Where to send it, what to put in it, how hard to try. Four POSTs and it exists.",
    frame: (f) => {
      const N = SETUP_NAMES();
      return F.wrap(
        F.pane(
          "the whole call, as data",
          F.code(
            `POST  <span class="tok-config">{{config.base_url}}</span>/mandates/<span class="tok-input">{{input.mandate_id}}</span>/sync\n` +
              `      ▲                        ▲\n` +
              `      └─ the config            └─ the input, checked by the spec\n\n` +
              `Authorization: <span class="tok-secret">{{secret.${esc(N.secret)}}}</span>\n` +
              `      └─ resolved when the call is made, never stored here\n\n` +
              `retry  3 tries · exponential · 1m → 10m`,
            "big",
          ),
          { meta: f.epMark, metaTone: f.epOk ? "ok" : "", cls: f.epOk ? "ok" : "" },
        ),
        f.epOk
          ? F.note(
              `<b>Four calls. No deploy.</b> Anything in your estate can now fire this by name — a service, a script, a person with curl.`,
              "ok",
            )
          : F.note("Nothing here is code. It is four rows in a database."),
        f.halt ? F.note(f.halt, "bad") : "",
      );
    },
  },
];

// ─── page 2 · a short task ───────────────────────────────────────────────────

const askFrame = (f) =>
  F.wrap(
    F.cols(
      F.pane("your service", F.code(json(f.jobBody ?? {}), "big"), { meta: "POST /v1/jobs" }),
      F.pane(
        "what it does not have to know",
        F.code(
          `<s>where Aarokya lives</s>\n<s>which credential to use</s>\n<s>how many times to try</s>\n` +
            `<s>how long to wait between tries</s>\n<s>what to do if it is still failing</s>`,
          "big",
        ),
        { cls: "dim" },
      ),
    ),
    F.note(`One POST, naming the endpoint. <b>That is the entire integration.</b>`),
  );

const writtenFrame = (f) =>
  F.wrap(
    F.bigs(F.big(f.answeredIn ?? "—", "answered in", "act")),
    F.table("jobs", "one row per thing you asked for", [{ k: "e", label: "endpoint" }, { k: "t", label: "trigger" }, { k: "w", label: "run_at / cron" }, { k: "s", label: "status" }], f.job ? [f.job] : []),
    F.table("executions", "one row per time it should run", [{ k: "n", label: "#" }, { k: "s", label: "status" }, { k: "w", label: "run_at" }, { k: "worker", label: "worker" }, { k: "tries", label: "attempt_count" }], f.exec ? [f.exec] : []),
    F.note(
      `Both rows existed <b>before that POST returned</b>. Stop every process we run, pull the plug on the workers — the rows are still there, and the work still happens.`,
      "ok",
    ),
  );

/// The same beat for a schedule: the jobs row is there, but no execution is —
/// the database has not reached the minute yet.
const cronWrittenFrame = (f) =>
  F.wrap(
    F.bigs(F.big(f.answeredIn ?? "—", "answered in", "act"), F.big(f.nextRun ?? "—", "next run at")),
    F.table("jobs", "one row per thing you asked for", [{ k: "e", label: "endpoint" }, { k: "t", label: "trigger" }, { k: "w", label: "cron" }, { k: "s", label: "status" }], f.job ? [f.job] : []),
    F.table("executions", "nothing here yet — and that is correct", [{ k: "n", label: "#" }, { k: "s", label: "status" }, { k: "w", label: "run_at" }], []),
    F.note(
      `A schedule is not a queue of future runs. Invokr stores <b>one row</b> and the rule; the executions appear one at a time, as the minutes arrive.`,
      "ok",
    ),
    f.halt ? F.note(f.halt, "bad") : "",
  );

const dueFrame = (f) =>
  F.wrap(
    F.table(
      "executions",
      "the row is the timer",
      [{ k: "n", label: "#" }, { k: "s", label: "status" }, { k: "w", label: "run_at" }, { k: "worker", label: "worker" }],
      f.exec ? [{ ...f.exec, w: f.due ? F.bar(f.due.left, f.due.total, f.due.label) : f.exec.w }] : [],
    ),
    F.lead(`Nothing is counting down.`),
    F.note(
      `<code>run_at</code> is a column. There is no timer, no sleeping thread, no <code>setTimeout</code> anywhere. A worker asks the database for rows whose time has come.`,
    ),
    F.note(`Kill every worker right now — the row does not change, and the job still goes out on time.`, "warn"),
  );

const cronDueFrame = (f) =>
  F.wrap(
    F.cols(
      F.pane("your jobs row", F.code(`cron       <u>${esc(f.cron ?? "* * * * *")}</u>\ntimezone   Asia/Kolkata\nstatus     ACTIVE`, "big")),
      F.pane("pg_cron's own table", F.code(`a Postgres extension.\nnot a process of ours.\n\non the minute, <b>the database</b>\ninserts the next execution.`, "big"), { cls: "dim" }),
    ),
    F.note(
      `Nothing of Invokr's has to be awake for the next tick. If every worker and every API process is down when the minute turns, the row is still created — and picked up whenever someone comes back.`,
      "ok",
    ),
  );

const tickFrame = (f) =>
  F.wrap(
    F.bigs(F.big(f.tickAt ?? "—", "a row appeared at", "act"), F.big(f.tickN ?? 1, "tick")),
    F.table("executions", "nobody inserted this", [{ k: "n", label: "#" }, { k: "s", label: "status" }, { k: "w", label: "created_at" }, { k: "worker", label: "worker" }], f.exec ? [f.exec] : []),
    F.note(`From here it is an ordinary execution — indistinguishable from one you asked for by hand.`),
  );

const claimFrame = (f) =>
  F.wrap(
    F.flow(
      F.box(f.winner ? `worker ${short(f.winner)}` : "worker", f.winner ? "got the row" : "asks for due rows", f.winner ? "ok" : ""),
      F.arrow("both ask", "act"),
      F.box("PostgreSQL", "one row, one winner", "on"),
      F.arrow("at the same moment"),
      F.box("worker", "skipped it — did not wait", "dim"),
    ),
    F.pane(
      "how exactly-one is enforced",
      F.code(
        `SELECT … FROM executions\n WHERE status = 'QUEUED' AND run_at &lt;= now()\n   <u>FOR UPDATE SKIP LOCKED</u>\n LIMIT 1`,
        "big",
      ),
    ),
    F.note(
      `<code>SKIP LOCKED</code> is the whole trick. A second worker asking at the same instant does not block and does not wait — it <b>skips the locked row</b> and takes the next one. No leader election, no lock service, no coordination.`,
    ),
  );

const callFrame = (f) =>
  F.wrap(
    F.cols(
      F.pane("what you registered", F.code(tokens(esc(f.template ?? "")), "big"), { cls: "dim" }),
      F.pane(`what actually went out${f.sentAt ? `, at ${f.sentAt}` : ""}`, F.code(f.resolved ?? `<div class="empty">calling…</div>`, "big"), { cls: "ok" }),
    ),
    F.note(
      `Resolved <b>now</b>, not when you registered it. Rotate the secret and the next call uses the new one — the endpoint does not change, and nothing has to be redeployed.`,
    ),
    f.idem ? F.note(`<code>x-invokr-idempotency-key</code> was added for you. Every retry carries the same one.`, "ok") : "",
  );

const answerFrame = (f) =>
  F.wrap(
    f.answerBody ? F.pane(`← ${f.answerCode ?? ""} in ${f.answerTook ?? ""}`, F.code(json(f.answerBody), "big"), { cls: f.answerOk ? "ok" : "bad" }) : "",
    F.table(
      "attempts",
      "one row per time it was actually tried",
      [{ k: "n", label: "#" }, { k: "s", label: "status" }, { k: "c", label: "code" }, { k: "d", label: "duration_ms" }, { k: "k", label: "idempotency key" }],
      f.attempts ?? [],
    ),
    (f.attempts?.length ?? 0) > 1
      ? F.note(
          `Three tries, <b>one key</b>. That is the honest answer to “would a retry register the mandate twice?” — the receiving side can see it is the same request.`,
          "ok",
        )
      : F.note(`One row per try, with its status code, its duration and the key it carried.`),
  );

const recordFrame = (f) =>
  F.wrap(
    F.bigs(
      F.big(f.finalAttempts ?? 1, "attempts", "ok"),
      F.big(f.finalStatus ?? "—", "execution", f.finalStatus === "SUCCESS" ? "ok" : "warn"),
    ),
    F.pane(
      "and all of it is readable over the same API",
      F.code(
        `GET /v1/executions/{id}             status, timing, which worker\n` +
          `GET /v1/executions/{id}/attempts    every try, with its code and duration\n` +
          `GET /v1/jobs/{id}/executions        every time this schedule has fired`,
        "big",
      ),
    ),
    F.note(`No log scraping, no agent, no separate store. The rows the worker was reading are the rows you query.`),
    f.halt ? F.note(f.halt, "bad") : "",
  );

const cancelFrame = (f) =>
  F.wrap(
    F.cols(
      F.pane("POST /v1/jobs/{id}/cancel", F.code(`→ 200`, "big")),
      F.pane("jobs", F.code(`status  <span class="st retired">RETIRED</span>\n\npg_cron stops producing.`, "big"), { cls: "ok" }),
    ),
    F.lead(`This is the loop your team actually runs.`),
    F.note(
      `Ask for the mandate status every minute; the moment the bank says <b>ACTIVE</b>, stop asking. One POST retires the schedule — the executions that already ran stay on the record.`,
      "ok",
    ),
    f.halt ? F.note(f.halt, "bad") : "",
  );

const SHORT_STEPS = [
  {
    id: "why",
    label: "Why a row and not a process",
    blurb: "The hotel wake-up call.",
    frame: (f) => `<div class="f-analogy">
      <div class="line">You ask the front desk for 6am, and you go to sleep.</div>
      <div class="card">room 402 &nbsp;— &nbsp;<b>06:00</b></div>
      <div class="line dim">The desk does not stay awake. It writes 6am in the book.<br/>
        The night staff go home; new staff arrive. The book is still the book.<br/>
        At 6am, whoever is on shift rings you.</div>
      <div class="tie">Invokr is the <b>book</b>. The workers are the <b>shift</b>.
        That is why a deploy, a crash or a restart cannot lose your job —
        nothing was holding it.</div>
      ${f.halt ? F.note(f.halt, "bad") : ""}
    </div>`,
  },
  {
    id: "ask",
    label: "You ask for a run",
    blurb: "One POST to /v1/jobs, naming the endpoint and the input. This is the only call your service makes.",
    frame: askFrame,
  },
  {
    id: "written",
    label: "Invokr writes rows and answers",
    blurb: "The job you asked for, and the execution it is due to produce. Once that returns, the work cannot be lost.",
    frame: writtenFrame,
  },
  {
    id: "due",
    label: "The row waits until it is due",
    blurb: "run_at is a column. Kill every process and the row still says when.",
    frame: dueFrame,
    extras: [
      { id: "kill", label: "Kill a worker", danger: true },
      { id: "addworker", label: "Add a worker" },
    ],
  },
  {
    id: "claim",
    label: "Exactly one worker takes it",
    blurb: "SELECT … FOR UPDATE SKIP LOCKED. Whoever wins the row owns it; the others move on rather than wait.",
    frame: claimFrame,
  },
  {
    id: "call",
    label: "It calls the other side",
    blurb: "The URL, headers and body are assembled at call time, from config, secret and input.",
    frame: callFrame,
  },
  {
    id: "answer",
    label: "The answer is written down",
    blurb: "Every try becomes a row in attempts, with its status, its duration and the key it carried.",
    frame: answerFrame,
  },
  {
    id: "record",
    label: "And that is the record",
    blurb: "No log scraping. What happened is queryable, because it is the same rows the worker was reading.",
    frame: recordFrame,
  },
];

const CRON_STEPS = [
  SHORT_STEPS[0],
  { ...SHORT_STEPS[1], label: "You ask for a schedule", blurb: "Same POST, with a cron expression instead of a time." },
  { ...SHORT_STEPS[2], label: "Invokr writes the schedule", blurb: "One jobs row. No executions yet — the first one appears when the minute turns.", frame: cronWrittenFrame },
  {
    id: "due",
    label: "pg_cron owns it from here",
    blurb: "On the minute boundary the database itself inserts the next execution. Nothing of ours needs to be awake.",
    frame: cronDueFrame,
  },
  {
    id: "tick",
    label: "A row appears that nobody inserted",
    blurb: "That is the tick. From here it is an ordinary execution.",
    frame: tickFrame,
  },
  SHORT_STEPS[4],
  SHORT_STEPS[5],
  SHORT_STEPS[6],
  {
    id: "cancel",
    label: "You cancel it when the answer is terminal",
    blurb: "Poll a status until it stops changing, then stop asking. Cancelling is one POST.",
    frame: cancelFrame,
  },
];

// The two questions a room always asks once it has seen one job run: can two
// teams share this, and does it only do HTTP. Both are takes on the same page
// rather than pages of their own — they are one idea each, not a journey.

const TEAM_STEPS = [
  {
    id: "name",
    label: "The same name, in two places",
    blurb: "Two workspaces. One endpoint name. Two unrelated rows.",
    frame: (f) =>
      F.wrap(
        F.cols(
          F.pane("workspace · Mandates", F.code(`endpoint  ${esc(SETUP_NAMES().endpoint)}\nschema    <u>…${esc(f.schemaA ?? "")}</u>\nteam      mandates`, "big"), { meta: "X-Workspace-Id" }),
          F.pane("workspace · Rides", F.code(`endpoint  ${esc(SETUP_NAMES().endpoint)}\nschema    <u>…${esc(f.schemaB ?? "")}</u>\nteam      rides`, "big"), { meta: "X-Workspace-Id" }),
        ),
        F.lead(`Same name. Different row.`),
        F.note(
          `Each workspace is its own <b>Postgres schema</b> — its own jobs, executions, attempts, configs and secrets. Not a tenant column someone has to remember to filter on; a separate set of tables.`,
        ),
      ),
  },
  {
    id: "fire",
    label: "Both teams fire it",
    blurb: "Same endpoint name, same body shape. Only the header differs.",
    frame: (f) =>
      F.wrap(
        F.cols(
          F.pane("Mandates asks", F.code(json(f.bodyA ?? {}), "big"), { meta: f.statusA ?? "…", metaTone: "ok" }),
          F.pane("Rides asks", F.code(json(f.bodyB ?? {}), "big"), { meta: f.statusB ?? "…", metaTone: "ok" }),
        ),
        F.note(`Two POSTs, identical but for <code>X-Workspace-Id</code>. Neither caller knows the other exists.`),
        f.halt ? F.note(f.halt, "bad") : "",
      ),
  },
  {
    id: "ran",
    label: "Two runs, neither can see the other",
    blurb: "Two executions in two schemas. No query joins them.",
    frame: (f) =>
      F.wrap(
        F.table(
          "executions · Mandates",
          "in that workspace's own schema",
          [{ k: "s", label: "status" }, { k: "worker", label: "worker" }, { k: "tries", label: "attempt_count" }],
          f.execA ? [f.execA] : [],
        ),
        F.table(
          "executions · Rides",
          "in that workspace's own schema",
          [{ k: "s", label: "status" }, { k: "worker", label: "worker" }, { k: "tries", label: "attempt_count" }],
          f.execB ? [f.execB] : [],
        ),
        F.note(`The same worker pool served both. Isolation is in the data, not in a second deployment.`, "ok"),
      ),
  },
  {
    id: "proof",
    label: "Aarokya can tell them apart",
    blurb: "{{config.team}} resolved out of each workspace's own config — different header, same endpoint.",
    frame: (f) =>
      F.wrap(
        F.cols(
          F.pane("what Aarokya logged for the Mandates call", F.code(esc(f.saidA ?? "waiting…"), "big"), { cls: f.saidA ? "ok" : "dim" }),
          F.pane("what Aarokya logged for the Rides call", F.code(esc(f.saidB ?? "waiting…"), "big"), { cls: f.saidB ? "ok" : "dim" }),
        ),
        F.lead(`One endpoint description. Two credentials, two base URLs, two teams.`),
        F.note(
          `Nobody templated the team into the job. <code>{{config.team}}</code> and <code>{{secret.…}}</code> resolve against <b>the workspace the job was created in</b> — which is why Aarokya, reading only the headers, says a different team each time. Rotating one team's credential cannot touch the other's.`,
        ),
        f.halt ? F.note(f.halt, "bad") : "",
      ),
  },
];

const TRANSPORT_STEPS = [
  {
    id: "three",
    label: "Three endpoints, one difference",
    blurb: "type is a column. Everything else about the row is the same.",
    frame: (f) =>
      F.wrap(
        F.table(
          "endpoints",
          "the destination is a field",
          [{ k: "e", label: "name" }, { k: "t", label: "type" }, { k: "w", label: "where it goes" }, { k: "s", label: "available here" }],
          f.endpoints ?? [],
        ),
        F.note(
          `Retries, idempotency keys, the executions and attempts tables, the API you query — all identical. Moving a job from an HTTP call to a Kafka topic is <b>an endpoint edit</b>, not a rewrite of the caller.`,
        ),
      ),
  },
  {
    id: "send",
    label: "The same job, sent three ways",
    blurb: "One POST shape per destination. Whatever is not running here says so plainly.",
    frame: (f) =>
      F.wrap(
        F.table(
          "executions",
          "one per destination",
          [{ k: "e", label: "endpoint" }, { k: "t", label: "type" }, { k: "s", label: "status" }, { k: "worker", label: "worker" }],
          f.sent ?? [],
        ),
        f.skipped?.length
          ? F.note(`${esc(f.skipped.join(" and "))} ${f.skipped.length > 1 ? "are" : "is"} not running in this demo — same job, nowhere to put it. The row would be identical.`, "warn")
          : F.note(`Every destination is up here, so all three went out.`, "ok"),
        f.halt ? F.note(f.halt, "bad") : "",
      ),
  },
  {
    id: "same",
    label: "And the record is the same record",
    blurb: "Same attempts table, same query, whatever the transport.",
    frame: (f) =>
      F.wrap(
        F.bigs(F.big(f.sentCount ?? 0, "destinations", "ok"), F.big("1", "record shape", "act")),
        F.pane(
          "what differs",
          F.code(
            `HTTP    <u>x-invokr-idempotency-key</u> added for you\nKafka   you template the key into the message yourself\nRedis   you template the key into the entry yourself`,
            "big",
          ),
        ),
        F.note(
          `Worth naming out loud: the key is <b>automatic only on HTTP</b>. For the two stream transports you put <code>{{execution.idempotency_key}}</code> in the payload — it is in the template namespace for exactly that reason.`,
          "warn",
        ),
      ),
  },
];

// ─── page 3 · work that takes minutes ────────────────────────────────────────

function ladder(f) {
  const rungs = (f.plan ?? []).map((p) => {
    const live = f.live?.[p.key];
    return `<div class="rung ${p.dir ?? "gap"} ${live?.tone ?? p.tone ?? ""} ${live ? "live" : ""}">
      <span class="at">${live ? stamp(live.at) : ""}</span>
      <div class="wirebox"><span class="label">${live?.label ?? p.label}</span><span class="line"></span></div>
      <span class="tail">${esc(live?.tail ?? p.tail ?? "")}</span>
    </div>`;
  });
  return `<div class="ladder">
    <div class="ladder-h">
      <span class="who">Invokr <i>worker</i></span>
      <span class="who right">Aarokya <i>the target</i></span>
    </div>
    <div class="rungs">${rungs.join("")}</div>
  </div>`;
}

const LONG_STEPS = [
  {
    id: "why",
    label: "Why 202 needs its own answer",
    blurb: "You drop the car at the garage.",
    frame: (f) => `<div class="f-analogy">
      <div class="line">You drop the car off. They hand you a ticket, and you leave.</div>
      <div class="card">ticket &nbsp;<b>#204</b> &nbsp;&nbsp;— &nbsp;ready when it is ready</div>
      <div class="line dim">You do not sit in the workshop. Then one of two things happens:<br/>
        you ring them every so often &nbsp;·&nbsp; or they ring you when it is done.</div>
      <div class="tie">That is the whole of this page. <b>202 is the ticket.</b>
        Invokr either <b>polls</b> or takes a <b>callback</b> — or both, and whichever
        arrives first ends it.</div>
      ${f.halt ? F.note(f.halt, "bad") : ""}
    </div>`,
  },
  {
    id: "ask",
    label: "You ask for a run",
    blurb: "An ordinary job. Nothing at the call site says this one takes minutes.",
    frame: (f) =>
      F.wrap(
        F.pane("POST /v1/jobs", F.code(json(f.jobBody ?? {}), "big")),
        F.note(`Identical to any other job. The endpoint knows it is async; the caller does not have to.`),
        f.halt ? F.note(f.halt, "bad") : "",
      ),
  },
  {
    id: "written",
    label: "Invokr writes a row and answers",
    blurb: "Durable before the work has even begun.",
    frame: (f) => F.wrap(F.bigs(F.big(f.answeredIn ?? "—", "answered in", "act")), ladder(f), f.halt ? F.note(f.halt, "bad") : ""),
  },
  { id: "claim", label: "A worker takes it", blurb: "The same claim, the same SKIP LOCKED. Still one dispatch.", frame: (f) => F.wrap(ladder(f)) },
  {
    id: "send",
    label: "It sends the work",
    blurb: "One attempt. However long this takes, the retry budget has seen exactly one dispatch.",
    frame: (f) => F.wrap(ladder(f), F.note(`This is the <b>only</b> outbound dispatch there will be, no matter how long the work runs.`)),
  },
  {
    id: "accepted",
    label: "The other side says 202",
    blurb: "Not success, not failure. The Location header says where to check.",
    frame: (f) =>
      F.wrap(
        ladder(f),
        F.cols(
          F.pane("the endpoint's async block", F.code(`"async": {\n  "status_codes": [<u>202</u>],\n  …\n}`, "big"), { cls: "dim" }),
          F.pane("what came back", F.code(`HTTP/1.1 <u>202</u> Accepted\nLocation: /async/status/task-8`, "big"), { cls: "ok" }),
        ),
        F.note(`A 202 with no <code>Location</code> is a hard failure — <code>MISSING_POLL_URL</code>, and not retryable. Silence is not allowed to look like success.`),
      ),
  },
  {
    id: "wait",
    label: "The row parks as WAITING",
    blurb: "No connection held open. No thread blocked. The worker has already moved on.",
    frame: (f) =>
      F.wrap(
        F.table(
          "executions",
          "parked",
          [{ k: "s", label: "status" }, { k: "w", label: "next check" }, { k: "tries", label: "attempt_count" }, { k: "p", label: "poll_count" }],
          [{ s: F.status("WAITING"), w: f.nextCheck ?? "—", tries: 1, p: f.polls ?? 0 }],
        ),
        F.lead(`Nothing of yours is waiting.`),
        F.note(
          `No open socket, no blocked thread, no in-memory state. The execution is a row with a next-check time, exactly like a delayed job. If every worker restarted right now, this would carry on.`,
        ),
      ),
  },
  {
    id: "poll",
    label: "Invokr checks back — or gets called",
    blurb: "Retry-After wins over the configured backoff. Every check is a row in polls.",
    frame: (f) =>
      F.wrap(
        ladder(f),
        F.note(
          f.lastRetryAfter
            ? `It asked for <b>${f.lastRetryAfter}</b>, so that is what Invokr waited. <code>Retry-After</code> beats the backoff you configured — the target knows better than the config does.`
            : `Each check is a row in <code>polls</code>, with its own status code and its own Retry-After.`,
        ),
      ),
  },
  {
    id: "finish",
    label: "Whichever answer lands first, finishes it",
    blurb: "Poll and callback race safely: one row update wins, the other sees zero rows changed.",
    frame: (f) => F.wrap(ladder(f), f.finishNote ? F.note(f.finishNote, "ok") : ""),
  },
  {
    id: "record",
    label: "And that is the record",
    blurb: "One attempt, every poll, the response and the key — all queryable.",
    frame: (f) =>
      F.wrap(
        F.bigs(F.big(f.finalAttempts ?? 1, "attempts", "ok"), F.big(f.polls ?? 0, "polls", "act")),
        F.lead(`Polls are not attempts.`),
        F.note(
          `Ten check-ins is still <b>one dispatch</b>. The retry policy has not been touched — that is why they are different tables, and why a slow destination cannot silently eat your retry budget.`,
        ),
        F.pane(
          "readable the same way as everything else",
          F.code(`GET /v1/executions/{id}          status, poll_count, deadline\nGET /v1/executions/{id}/polls    every check, its code and its Retry-After`, "big"),
        ),
        f.halt ? F.note(f.halt, "bad") : "",
      ),
  },
];

// ─── the takes ───────────────────────────────────────────────────────────────

const takes = [
  {
    id: "setup",
    page: "setup",
    label: "Everything a job needs",
    claim: "Four calls, and the endpoint exists.",
    sub: "No deploy, no restart, no code review. Three pieces of data feed one description of a call — and the description is data too.",
    action: "Build it, live",
    steps: SETUP_STEPS,
    fields: [],
    run: runSetup,
  },
  {
    id: "short-task",
    page: "short",
    label: "One task, end to end",
    claim: "A job is a row. Nothing is counting down.",
    sub: "Postgres holds <b>run_at</b>. A worker asks for rows that are due, and exactly one wins each.",
    action: "Fire it",
    steps: (v) => (v.trigger === "CRON" ? CRON_STEPS : SHORT_STEPS),
    // A cron run walks a different set of steps, so it needs its own tape —
    // otherwise replaying one would draw steps the current page does not have.
    tape: (v) => (v.trigger === "CRON" ? "short-task-cron" : "short-task"),
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
    sub: "The same endpoint name in two workspaces is two unrelated rows in two Postgres schemas — and the target can tell which team called.",
    action: "Fire into both",
    steps: TEAM_STEPS,
    fields: [{ key: "mandate_id", label: "mandate", value: "MND-4410", width: 116 }],
    run: runTeams,
  },
  {
    id: "any-transport",
    page: "short",
    label: "Not just HTTP",
    claim: "The destination is a field.",
    sub: "HTTP, a Kafka topic or a Redis Stream. Same job, same retry policy, same record — the caller does not change.",
    action: "Send it three ways",
    steps: TRANSPORT_STEPS,
    fields: [{ key: "mandate_id", label: "mandate", value: "MND-7781", width: 116 }],
    run: runTransports,
  },
  {
    id: "long-running",
    page: "long",
    label: "Work that takes minutes",
    claim: "202 is a promise, not an answer.",
    sub: "The other side takes the work and keeps it. Invokr parks the row — nothing held open — and either checks back or gets called. <b>Either way it is one attempt.</b>",
    action: "Start the long job",
    steps: LONG_STEPS,
    tape: (v) => `long-running-${v.mode}`,
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
  run.step("why");
  if (!spec) {
    run.halt("config", "the demo server has not provisioned yet — give it a moment");
    return false;
  }

  const cfgBody = {
    name: N.config,
    values: { base_url: state.status?.mockUrl ?? "http://localhost:9999", team: wsA?.slug ?? "mandates" },
  };
  run.facts({ cfgBody });
  run.step("config");
  const cfg = await put(run, "configs", N.config, cfgBody);
  run.facts({ cfgRes: cfg.res, cfgMark: cfg.mark, cfgOk: cfg.ok });
  if (!cfg.ok) {
    run.halt("config", `Invokr refused it — <b>${cfg.status}</b>`);
    return false;
  }

  run.facts({ secName: N.secret, secRead: null });
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
  run.facts({ secRead: readBack.body?.data ?? readBack.body });
  if (!sec.ok || leaked) {
    run.halt("secret", leaked ? "the API returned a secret value — <b>check this</b>" : `Invokr refused it — <b>${sec.status}</b>`);
    return false;
  }

  run.facts({ psBody: MANDATE_SCHEMA });
  run.step("payload");
  const ps = await put(run, "payload-specs", N.payloadSpec, { name: N.payloadSpec, schema: MANDATE_SCHEMA });
  if (!ps.ok) {
    run.halt("payload", `Invokr refused it — <b>${ps.status}</b>`);
    return false;
  }

  run.step("endpoint");
  const ep = await put(run, "endpoints", N.endpoint, spec);
  run.facts({ epMark: ep.mark, epOk: ep.ok, halt: ep.ok ? null : `Invokr refused it — <b>${ep.status}</b>` });
  if (!ep.ok) run.step("endpoint", { bad: true });
  return ep.ok;
}

// ─── page 2's run ────────────────────────────────────────────────────────────

/// Countdowns are emitted, not computed on screen, so a replay reproduces them.
function countdown(run, deadline, total, label) {
  const timer = setInterval(() => {
    const left = deadline - Date.now();
    if (run.cancelled || left <= 0) {
      clearInterval(timer);
      if (!run.cancelled) run.facts({ due: { left: 0, total, label: "due now" } });
      return;
    }
    run.facts({ due: { left, total, label: `due in ${(left / 1000).toFixed(1)}s` } });
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

async function runShort(run, v) {
  const N = SETUP_NAMES();
  await control("/mock/reset");
  await ensureSetup();
  let logSeq = await targetLogHead();

  const failures = clamp(v.fail_times, 0, 2, 0);
  const maxTries = clamp(v.attempts, 1, 3, 3);
  // The team's real policy waits a minute before the second try. Ask for
  // failures and the same call runs with seconds instead, so the shape fits a
  // demo slot — and the frame says so rather than implying Invokr is that
  // impatient by default.
  const endpoint = failures > 0 ? "aarokya-mandate-sync-impatient" : N.endpoint;
  const seconds = clamp(v.seconds, 5, 120, 15);
  const idem = key(`${v.mandate_id}-t1`);

  const jobBody = {
    trigger: v.trigger ?? "IMMEDIATE",
    endpoint,
    idempotency_key: idem,
    input: { mandate_id: `${v.mandate_id}-${Date.now().toString(36).slice(-4)}`, checks: "1", fail_times: String(failures) },
    max_attempts: maxTries,
  };
  if (v.trigger === "DELAYED") jobBody.run_at = new Date(Date.now() + seconds * 1000).toISOString();
  if (v.trigger === "CRON") {
    jobBody.cron = v.cron || "* * * * *";
    jobBody.timezone = "Asia/Kolkata";
  }

  run.step("why");
  run.facts({ jobBody, cron: jobBody.cron });
  run.step("ask");

  const t0 = performance.now();
  const res = await api("POST", "/v1/jobs", { body: jobBody });
  run.wire({ verb: "POST", path: "/v1/jobs", status: res.status, req: jobBody, res: res.body });
  if (!res.ok) {
    run.halt("written", `Invokr refused it — <b>${res.status}</b>`);
    return false;
  }
  // A CRON job answers with the schedule and no execution: pg_cron inserts the
  // first one when the minute turns, which is the point of the next frame.
  // The create response's `execution` is a stub — id, created_at and PENDING —
  // so the row's `run_at` has to come off the job it belongs to.
  const job = res.body.data;
  run.facts({
    answeredIn: ms(performance.now() - t0),
    job: jobRow(job, endpoint),
    exec: job.execution ? execRow({ ...job.execution, run_at: job.run_at ?? job.execution.created_at }) : null,
    nextRun: job.next_run_at ? clock(job.next_run_at) : null,
  });
  run.step("written");

  if (v.trigger === "CRON") return await runCronRest(run, { job, v, endpoint, idem, logSeq });

  // The countdown belongs to the frame the moment it appears, so it goes on
  // before the step rather than one redraw later.
  if (v.trigger === "DELAYED") {
    run.facts({ due: { left: seconds * 1000, total: seconds * 1000, label: `due in ${seconds}.0s` } });
  }
  run.step("due");
  if (v.trigger === "DELAYED") countdown(run, new Date(jobBody.run_at).getTime(), seconds * 1000, "due in");

  const done = await follow(run, {
    job,
    executionId: job.execution.execution_id,
    endpoint,
    idem,
    logSeq,
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

async function runCronRest(run, { job, v, endpoint, idem, logSeq }) {
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
    run.facts({ tickAt: clock(tick.created_at), tickN: n, exec: execRow(tick) });
    run.emit("step", { id: "tick", at: Math.max(0, new Date(tick.created_at) - run.wallT0) });

    const done = await follow(run, {
      job,
      executionId: tick.execution_id,
      endpoint,
      idem,
      logSeq: seq,
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
async function follow(run, { job, executionId, ws = "a", endpoint, idem, logSeq, timeout = 90000, silent = false }) {
  const started = performance.now();
  const offset = (iso) => Math.max(0, new Date(iso) - run.wallT0);
  const mandateId = job.execution?.input?.mandate_id ?? jobInput(job);
  const team = state.status?.provisioned?.workspaces?.[ws]?.slug ?? "mandates";
  let seen = 0;
  let claimed = false;
  let seq = logSeq;

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
        const who =
          exec.worker_id ?? (await api("GET", `/v1/executions/${executionId}`, { ws })).body?.data?.worker_id ?? null;
        const at = exec.started_at ?? attempts[0]?.started_at;
        run.facts({ winner: who, exec: execRow({ ...exec, worker_id: who ?? exec.worker_id }) });
        run.emit("step", { id: "claim", at: at ? offset(at) : Math.round(performance.now() - run.t0) });
      }

      for (const a of silent ? [] : attempts.slice(seen)) {
        const ok = a.status === "SUCCESS";
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
        });
        run.emit("step", { id: "call", at: offset(a.started_at) });
        run.facts({
          answerBody: asJson(a.output?.body) ?? a.error ?? null,
          answerCode: a.output?.status_code ?? a.error?.status_code ?? "",
          answerTook: ms(a.duration_ms),
          answerOk: ok,
        });
        run.emit("step", { id: "answer", at: offset(a.completed_at ?? a.started_at), bad: !ok });
      }
      seen = attempts.length;

      if (["SUCCESS", "FAILED", "CANCELLED"].includes(exec.status)) {
        ({ seq } = await tailTarget(run, seq));
        if (!silent) {
          run.facts({ exec: execRow(exec), attempts: attemptRows(attempts, wireKey), finalAttempts: seen, finalStatus: exec.status });
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

  run.facts({
    schemaA: (ws.a?.schema_name ?? "").slice(-9),
    schemaB: (ws.b?.schema_name ?? "").slice(-9),
  });
  run.step("name");

  const stamp36 = Date.now().toString(36).slice(-4);
  const bodies = {
    a: { trigger: "IMMEDIATE", endpoint: N.endpoint, input: { mandate_id: `${v.mandate_id}-a-${stamp36}`, checks: "1", fail_times: "0" }, max_attempts: 1 },
    b: { trigger: "IMMEDIATE", endpoint: N.endpoint, input: { mandate_id: `${v.mandate_id}-b-${stamp36}`, checks: "1", fail_times: "0" }, max_attempts: 1 },
  };
  run.facts({ bodyA: bodies.a, bodyB: bodies.b, idA: bodies.a.input.mandate_id, idB: bodies.b.input.mandate_id });

  const jobs = {};
  for (const k of ["a", "b"]) {
    const res = await api("POST", "/v1/jobs", { body: bodies[k], ws: k });
    run.wire({ verb: "POST", path: `/v1/jobs  (${k === "a" ? "Mandates" : "Rides"})`, status: res.status, req: bodies[k], res: res.body });
    run.facts({ [k === "a" ? "statusA" : "statusB"]: String(res.status) });
    if (!res.ok) {
      run.halt("fire", `the ${k === "a" ? "Mandates" : "Rides"} workspace refused it — <b>${res.status}</b>`);
      return false;
    }
    jobs[k] = res.body.data;
  }
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

/// One job shape, three destinations. Whatever broker is not running here says
/// so rather than being quietly skipped.
async function runTransports(run, v) {
  await ensureSetup();
  const probes = state.status?.transports ?? {};
  const targets = [
    { type: "HTTP", endpoint: SETUP_NAMES().endpoint, where: "POST to Aarokya", up: true },
    { type: "Kafka", endpoint: "mandate-events-kafka", where: "topic mandate-events", up: !!probes.kafka },
    { type: "Redis", endpoint: "mandate-events-redis", where: "stream mandate-events", up: !!probes.redis },
  ];

  run.facts({
    endpoints: targets.map((t) => ({
      e: esc(t.endpoint),
      t: esc(t.type),
      w: esc(t.where),
      s: t.up ? F.status("ACTIVE") : `<span class="st">broker not running</span>`,
    })),
  });
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
    n++;
    const body = {
      trigger: "IMMEDIATE",
      endpoint: t.endpoint,
      input: { mandate_id: `${v.mandate_id}-${n}`, checks: "1", fail_times: "0" },
      max_attempts: 1,
    };
    const res = await api("POST", "/v1/jobs", { body });
    run.wire({ verb: "POST", path: `/v1/jobs  (${t.type})`, status: res.status, req: body, res: res.body });
    if (!res.ok) {
      ok = false;
      sent.push({ e: esc(t.endpoint), t: esc(t.type), s: F.status("FAILED"), worker: "—" });
      continue;
    }
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
    sent.push({
      e: esc(t.endpoint),
      t: esc(t.type),
      s: F.status(exec?.status ?? done?.status ?? "—"),
      worker: esc(short(exec?.worker_id)),
    });
    run.facts({ sent: [...sent], skipped });
  }

  run.facts({ sent, skipped, sentCount: sent.length });
  run.step("send", { bad: !ok });
  run.step("same", { bad: !ok });
  return ok;
}

// ─── page 3's run ────────────────────────────────────────────────────────────

function planFor(v) {
  const checks = v.mode === "callback" ? 0 : clamp(v.pending, 0, 8, 3) + 1;
  const plan = [
    { key: "send", dir: "out", label: `<b>POST</b> /async/start`, tail: "here is the work" },
    { key: "accepted", dir: "in", label: `Location: /async/status/… <span class="code ok">202</span>`, tail: "accepted, still working" },
    { key: "wait", label: "parked · WAITING — nothing held open" },
  ];
  for (let i = 1; i <= checks; i++) {
    const last = i === checks;
    plan.push({ key: `poll-${i}-out`, dir: "out", label: `<b>GET</b> /async/status/…`, tail: `check ${i}` });
    plan.push({
      key: `poll-${i}-in`,
      dir: "in",
      label: last ? `done <span class="code ok">200</span>` : `still working <span class="code">202</span>`,
      tail: last ? "terminal" : `Retry-After ${clamp(v.retry_after, 1, 30, 2)}s`,
    });
  }
  if (v.mode !== "poll") {
    plan.push({ key: "callback", dir: "in", label: `<b>POST</b> /v1/callbacks/…/complete`, tail: "Aarokya calls us" });
  }
  return plan;
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
  run.facts({ plan: planFor(v), live: {} });
  if (!state.status?.longRunning) {
    // Say so on the opening frame rather than on a step that would draw an
    // empty request body — nothing was asked for, so nothing should be shown.
    run.halt(
      "why",
      "This build has no long-running support — it lives on <b>feat/long-running-jobs</b>. Switch on <b>replay</b> to watch a recorded run of this page.",
    );
    return false;
  }
  run.step("why");

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

  const jobBody = {
    trigger: "IMMEDIATE",
    endpoint: spec.name,
    idempotency_key: key(`${v.job}-long`),
    input: { job: v.job },
    async_overrides: { max_wait_ms: asyncBlock.max_wait_ms, ...(asyncBlock.max_polls ? { max_polls: asyncBlock.max_polls } : {}) },
  };
  run.facts({ jobBody });
  run.step("ask");

  const t0 = performance.now();
  const res = await api("POST", "/v1/jobs", { body: jobBody });
  run.wire({ verb: "POST", path: "/v1/jobs", status: res.status, req: jobBody, res: res.body });
  if (!res.ok) {
    run.halt("written", `Invokr refused it — <b>${res.status}</b>`);
    return false;
  }
  run.facts({ answeredIn: ms(performance.now() - t0) });
  run.step("written");

  return await followLong(run, res.body.data.execution.execution_id, {
    logSeq,
    maxPolls: asyncBlock.max_polls,
    mode: v.mode,
  });
}

async function followLong(run, executionId, { logSeq, maxPolls, mode }) {
  const started = performance.now();
  const offset = (iso) => Math.max(0, new Date(iso) - run.wallT0);
  const live = {};
  let claimed = false;
  let sent = false;
  let seenPolls = 0;
  let seq = logSeq;
  let sawCallback = false;

  const light = (k, patch) => {
    live[k] = { ...(live[k] ?? {}), ...patch };
    run.facts({ live: { ...live } });
  };

  while (performance.now() - started < 240000 && !run.cancelled) {
    const exec = (await api("GET", `/v1/executions/${executionId}`)).body?.data;
    if (exec) {
      if (!claimed && exec.attempt_count > 0) {
        claimed = true;
        run.step("claim");
      }

      const attempts = (await api("GET", `/v1/executions/${executionId}/attempts`)).body?.data ?? [];
      const first = attempts.find((x) => x.attempt_number === 1);
      if (first && !sent) {
        sent = true;
        light("send", { at: offset(first.started_at), tone: "act" });
        run.emit("step", { id: "send", at: offset(first.started_at) });
        light("accepted", { at: offset(first.completed_at ?? first.started_at), tone: "act" });
        run.emit("step", { id: "accepted", at: offset(first.completed_at ?? first.started_at) });
        light("wait", { at: offset(first.completed_at ?? first.started_at) });
        run.facts({ nextCheck: exec.run_at ? clock(exec.run_at) : "—", polls: exec.poll_count ?? 0 });
        run.emit("step", { id: "wait", at: offset(first.completed_at ?? first.started_at) });
      }

      const polls = ((await api("GET", `/v1/executions/${executionId}/polls`)).body?.data ?? [])
        .slice()
        .sort((a, b) => a.poll_number - b.poll_number);
      for (const p of polls.slice(seenPolls)) {
        const pending = p.classification === "PENDING";
        light(`poll-${p.poll_number}-out`, { at: offset(p.polled_at), tone: "act" });
        light(`poll-${p.poll_number}-in`, {
          at: offset(p.polled_at) + (p.duration_ms ?? 0),
          tone: pending ? "warn" : p.classification === "SUCCESS" ? "ok" : "bad",
          label: pending
            ? `still working <span class="code">${p.status_code}</span>`
            : `${esc(p.classification.toLowerCase().replace("_", " "))} <span class="code ${
                p.classification === "SUCCESS" ? "ok" : "bad"
              }">${p.status_code}</span>`,
          tail: p.retry_after_ms ? `Retry-After ${Math.round(p.retry_after_ms / 1000)}s` : "terminal",
        });
        run.facts({
          polls: p.poll_number,
          lastRetryAfter: p.retry_after_ms ? ms(p.retry_after_ms) : null,
        });
        run.emit("step", { id: "poll", at: offset(p.polled_at) });
      }
      seenPolls = polls.length;

      const tailed = await tailTarget(run, seq);
      seq = tailed.seq;
      for (const e of tailed.entries) {
        if (e.path !== "(callback)" || sawCallback) continue;
        sawCallback = true;
        light("callback", { at: offset(e.at), tone: "ok" });
        run.emit("step", { id: "poll", at: offset(e.at) });
      }

      if (["SUCCESS", "FAILED", "CANCELLED"].includes(exec.status)) {
        ({ seq } = await tailTarget(run, seq));
        const ok = exec.status === "SUCCESS";
        run.facts({
          finishNote: ok
            ? sawCallback && seenPolls > 0
              ? `Both were in flight. The <b>callback</b> arrived first and finalized the row; the next poll would have seen zero rows changed and quietly stopped.`
              : sawCallback
                ? `The other side <b>called back</b>. Nobody polled anything — it knew where to reach us because the URL was in the body we sent.`
                : `Check ${seenPolls} came back terminal, so the polling stopped there.`
            : null,
          finalAttempts: exec.attempt_count ?? 1,
          polls: seenPolls,
        });
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
  if (t.page === "long") state.facts.plan = planFor(valuesFor(t));
  renderSteps(t);
  document.body.classList.add("idle");
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

// ─── boot ────────────────────────────────────────────────────────────────────

$("#go").addEventListener("click", () => (state.replay ? replayTake() : runTake()));
$("#replay-toggle").addEventListener("click", () => setReplay(!state.replay));
$("#t-next").addEventListener("click", stepNext);
$("#t-back").addEventListener("click", stepBack);
$("#t-auto").addEventListener("click", () => setMode(film.mode === "auto" ? "manual" : "auto"));
$("#t-wire").addEventListener("click", () => toggleWire());

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
  if (e.key.toLowerCase() === "r") return setReplay(!state.replay);
});

renderPages();
setMode("manual");
mountTake(0);
await refreshStatus();
setInterval(refreshStatus, 4000);
