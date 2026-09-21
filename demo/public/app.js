// Invokr live demo.
//
// The stage is the explanation. A job is a physical token: it is born in your
// app, lands in the queue, waits there if it is not due, gets taken by exactly
// one worker, reaches the other side, and either finishes or flies back to the
// queue with a countdown on it. Nothing about that is narrated in prose,
// because watching it happen is faster than reading about it.
//
// One rule underneath: a scene's `run()` never touches the DOM. It only emits
// events, and `apply(event)` draws them — which is what lets a recorded run
// replay through the same renderer at the timings it really had.

const $ = (sel, root = document) => root.querySelector(sel);

const state = {
  status: null,
  current: 0,
  replay: false,
  recording: true,
  ran: new Set(),
  run: null,
  values: {},
};

const esc = (s) =>
  String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

const ms = (n) => (n == null ? "—" : n < 1000 ? `${Math.round(n)}ms` : `${(n / 1000).toFixed(1)}s`);
const clock = (t) => new Date(t).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
const short = (id) => (id ? String(id).replace("worker_", "").slice(0, 6) : "—");

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
const TOKEN = { w: 128, h: 34, gap: 8 };

// One source of truth for where everything sits. The DOM gets these numbers at
// boot; token layout reads the same rectangles.
const ZONES = {
  app: { x: 14, y: 54, w: 190, h: 530, pad: 14, top: 32, cols: 1 },
  db: { x: 238, y: 54, w: 424, h: 530 },
  waiting: { x: 260, y: 92, w: 380, h: 234, pad: 12, top: 28, cols: 2 },
  ready: { x: 260, y: 352, w: 380, h: 214, pad: 12, top: 28, cols: 2 },
  workers: { x: 700, y: 54, w: 312, h: 530 },
  target: { x: 1048, y: 54, w: 250, h: 530, pad: 14, top: 32, cols: 1 },
  done: { x: 1320, y: 54, w: 92, h: 530, pad: 6, top: 30, cols: 1 },
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
  place($("#z-waiting"), ZONES.waiting);
  place($("#z-ready"), ZONES.ready);
  place($("#z-workers"), ZONES.workers);
  place($("#z-target"), ZONES.target);
  place($("#z-done"), ZONES.done);

  const arrow = $("#drop-arrow");
  arrow.style.left = `${ZONES.waiting.x + ZONES.waiting.w / 2 - 6}px`;
  arrow.style.top = `${ZONES.waiting.y + ZONES.waiting.h + 2}px`;
}

// Scale the fixed board into whatever space the window gives it.
function fitBoard() {
  const stage = $(".stage");
  const board = $("#board");
  const k = Math.min(
    (stage.clientWidth - 28) / BOARD.w,
    (stage.clientHeight - 16) / BOARD.h,
    1.15,
  );
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
  host.parentElement.querySelector("#z-workers").classList.toggle("vacant", wanted.size === 0);

  let i = 0;
  for (const [id, w] of wanted) {
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

// A claim can name a worker the page has not been told about yet.
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

const tokens = new Map(); // id -> { el, zone, order }
let tokenSeq = 0;

function tokenRect(zone) {
  if (zone.startsWith("worker:")) {
    const el = workerBoxes.get(zone.slice(7));
    return el ? workerRect(Number(el.dataset.index)) : ZONES.ready;
  }
  // "some worker has it, we did not catch which": sits under the boxes.
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
      const col = i % cols;
      const row = Math.floor(i / cols);
      const x = r.x + (r.pad ?? 12) + col * (TOKEN.w + TOKEN.gap);
      const y = r.y + (r.top ?? 26) + row * (TOKEN.h + TOKEN.gap);
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
    // Put it in place before it is allowed to be seen, so the first move reads
    // as a move and not as a flash.
    relayoutTokens();
    requestAnimationFrame(() => el.classList.remove("born"));
  }

  if (label !== undefined) t.el.querySelector(".who").textContent = label;
  if (note !== undefined) t.el.querySelector(".note").textContent = note;

  if (tokenState !== undefined) {
    t.el.classList.remove("waiting", "ready", "running", "ok", "bad");
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

function flashZone(zone, cls) {
  const el = zone === "target" ? $("#z-target") : null;
  if (!el) return;
  el.classList.remove("hit", "refused");
  if (cls) {
    el.classList.add(cls);
    setTimeout(() => el.classList.remove(cls), 900);
  }
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

const sleep = (t) => new Promise((r) => setTimeout(r, t));

async function targetLogHead() {
  try {
    const res = await fetch("/control/mock/log?limit=1");
    return ((await res.json())?.data ?? [])[0]?.seq ?? 0;
  } catch {
    return 0;
  }
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
    apply(this.scene, ev);
  }

  /// One short line. The stage has already said most of it.
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

function apply(scene, ev) {
  switch (ev.type) {
    case "token":
      setToken(ev);
      break;

    case "clear":
      clearTokens();
      break;

    case "workers":
      renderWorkers(ev.list);
      break;

    case "worker":
      setWorkerState(ev.workerId, ev.cls, ev.label);
      break;

    case "flash":
      flashZone(ev.zone, ev.cls);
      break;

    case "cron":
      $("#cron-badge").classList.toggle("tick", !!ev.on);
      break;

    case "drop":
      $("#drop-arrow").classList.toggle("on", !!ev.on);
      break;

    case "target-is":
      $("#target-label").textContent = ev.who;
      break;

    case "tick": {
      // Two lines, not one: a claim and its answer can land 8ms apart, and a
      // single line would swallow the first before anyone read it.
      const el = $("#ticker");
      const t = ev.t < 1000 ? `${ev.t}ms` : `${(ev.t / 1000).toFixed(1)}s`;
      const prev = el.querySelector(".line.now");
      if (prev) {
        prev.classList.remove("now");
        prev.classList.add("was");
      }
      el.querySelectorAll(".line.was").forEach((n, i, all) => {
        if (i < all.length - 1) n.remove();
      });
      const line = document.createElement("span");
      line.className = `line now ${ev.tone ?? ""}`;
      line.innerHTML = `<span class="t">${t}</span><span>${ev.text}</span>`;
      el.appendChild(line);
      break;
    }

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

    case "panel":
      scene.panel?.(ev.key, ev.data);
      break;
  }
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

// ─── watching one real execution, as motion ──────────────────────────────────

async function watchExecution(run, executionId, opts = {}) {
  const { ws = "a", timeout = 90000, targetSince = 0, token, label } = opts;
  const started = performance.now();
  let seenAttempts = 0;
  let claimedAttempt = 0;
  let announcedRetry = 0;
  let logSeq = targetSince;
  let lastStatus = null;

  const tailTarget = async () => {
    try {
      const res = await fetch(`/control/mock/log?since=${logSeq}&limit=20`);
      const entries = ((await res.json())?.data ?? []).slice().reverse();
      if (entries.length) {
        logSeq = Math.max(logSeq, ...entries.map((e) => e.seq));
        run.emit("receipt", { entries });
      }
    } catch {
      /* the target's log is a nicety, never a dependency */
    }
  };

  while (performance.now() - started < timeout && !run.cancelled) {
    const exec = (await api("GET", `/v1/executions/${executionId}`, { ws })).body?.data;

    if (exec) {
      if (exec.status !== lastStatus) lastStatus = exec.status;

      // Keyed on attempt_count, not on a status change: a try that takes 5ms is
      // back to RETRYING before the next poll, so RUNNING is never observed.
      let pendingRetry = null;
      if (exec.status === "RETRYING" && exec.run_at && exec.attempt_count > announcedRetry) {
        announcedRetry = exec.attempt_count;
        pendingRetry = exec;
      }

      // The claim: the token moves into the box of the worker that took it, and
      // every other worker briefly shows what it did instead — skip the locked
      // row and move on.
      const claim = (n) => {
        if (n <= claimedAttempt) return;
        claimedAttempt = n;
        const wid = exec.worker_id;
        if (wid) {
          run.emit("worker", { workerId: wid, cls: "busy", label: "holding it" });
          for (const other of workerBoxes.keys()) {
            if (other !== wid) run.emit("worker", { workerId: other, cls: "skipped", label: "skipped — locked" });
          }
        }
        run.emit("token", { id: token, zone: wid ? `worker:${wid}` : "workers", state: "running", note: "taken" });
        run.tick(
          wid
            ? `worker <b>${esc(short(wid))}</b> took it — the others skipped the locked row`
            : "<b>one worker</b> took it — the others skipped the locked row",
          "act",
        );
      };
      claim(exec.attempt_count);

      const attempts = (await api("GET", `/v1/executions/${executionId}/attempts`, { ws })).body?.data ?? [];
      if (attempts.length !== seenAttempts) {
        const ordered = [...attempts].sort((a, b) => a.attempt_number - b.attempt_number);
        for (let i = 1; i < ordered.length; i++) {
          ordered[i].gap_ms = new Date(ordered[i].started_at) - new Date(ordered[i - 1].completed_at);
        }
        for (const a of ordered.slice(seenAttempts)) {
          claim(a.attempt_number);
          const ok = a.status === "SUCCESS";
          run.emit("token", {
            id: token,
            zone: "target",
            state: ok ? "ok" : "bad",
            note: ok ? `${a.output?.status_code ?? 200} · ${ms(a.duration_ms)}` : `${a.error?.status_code ?? "no answer"}`,
          });
          run.emit("flash", { zone: "target", cls: ok ? "hit" : "refused" });
          run.tick(
            ok
              ? `the other side answered <b>${a.output?.status_code ?? 200}</b> in ${ms(a.duration_ms)}`
              : `try ${a.attempt_number} <b>failed</b> — ${esc(a.error?.status_code ? String(a.error.status_code) : (a.error?.type ?? "no answer").toLowerCase())}`,
            ok ? "ok" : "bad",
          );
          if (exec.worker_id) run.emit("worker", { workerId: exec.worker_id, cls: null, label: "" });
        }
        seenAttempts = attempts.length;
        run.emit("tries", { attempts: ordered });
      }

      await tailTarget();

      // A failed try flies back to the queue with the wait written on it.
      if (pendingRetry) {
        const waitMs = Math.max(0, new Date(pendingRetry.run_at) - Date.now());
        await sleep(450);
        run.emit("token", { id: token, zone: "waiting", state: "waiting", note: `retry in ${ms(waitMs)}` });
        run.tick(`back in the queue — next try in <b>${ms(waitMs)}</b>`, "warn");
        countdownToken(run, token, new Date(pendingRetry.run_at).getTime(), "retry in");
      }

      if (["SUCCESS", "FAILED", "CANCELLED"].includes(exec.status)) {
        await tailTarget();
        await sleep(650);
        run.emit("token", {
          id: token,
          zone: "done",
          state: exec.status === "SUCCESS" ? "ok" : "bad",
          label: label ?? "",
          note: exec.status === "SUCCESS" ? "done" : exec.status.toLowerCase(),
        });
        return exec;
      }
    }
    await sleep(160);
  }
  return null;
}

// Emits the countdown as events so replay reproduces it rather than recomputing.
function countdownToken(run, token, deadline, prefix) {
  const timer = setInterval(() => {
    const left = deadline - Date.now();
    if (run.cancelled) return clearInterval(timer);
    if (left <= 0) {
      clearInterval(timer);
      // Its time has come: eligible now, whether or not a worker is alive.
      run.emit("drop", { on: true });
      run.emit("token", { id: token, zone: "ready", state: "ready", note: "due now" });
      setTimeout(() => run.emit("drop", { on: false }), 700);
      return;
    }
    run.emit("token", { id: token, note: `${prefix} ${(left / 1000).toFixed(1)}s` });
  }, 500);
  return timer;
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
    $("#foot-note").textContent = `${s.workers.length} worker${s.workers.length === 1 ? "" : "s"}`;
  } catch {
    setChip("chip-api", "down", "demo server down");
  }
}

// ─── firing a job, as motion ─────────────────────────────────────────────────

const key = (p) => `${p}-${Date.now().toString(36)}`;

async function fire(run, { endpoint, input, trigger, runAt, ws = "a", label, token, maxAttempts }) {
  const body = {
    // A run_at without DELAYED is just an ignored field: the API fires it now.
    trigger: trigger ?? (runAt ? "DELAYED" : "IMMEDIATE"),
    endpoint,
    idempotency_key: key(`${input.order_id ?? "job"}-${token}`),
    input,
  };
  if (runAt) body.run_at = runAt.toISOString();
  if (maxAttempts) body.max_attempts = maxAttempts;

  run.emit("token", { id: token, zone: "app", label, note: "new" });
  run.emit("req", { body });
  await sleep(220);

  const res = await api("POST", "/v1/jobs", { body, ws });
  run.emit("res", { body: res.body });
  if (!res.ok) {
    run.tick(`Invokr refused it — <b>${res.status}</b>`, "bad");
    run.emit("token", { id: token, state: "bad", note: `refused ${res.status}` });
    return null;
  }

  const waiting = Boolean(runAt);
  run.emit("token", {
    id: token,
    zone: waiting ? "waiting" : "ready",
    state: waiting ? "waiting" : "ready",
    note: waiting ? "not due yet" : "due now",
  });
  run.tick(
    waiting ? `written down — due at <b>${clock(runAt)}</b>` : "written down, and safe before Invokr answered",
    "act",
  );
  return res.body.data.execution.execution_id;
}

// ─── scenes ──────────────────────────────────────────────────────────────────

const scenes = [
  {
    id: "register",
    n: 1,
    group: "core",
    title: "Tell it where to deliver",
    lede: "An endpoint is a row: where to send, what to put in it, how hard to try.",
    fields: [],
    action: "Save the instructions",
    async run(run) {
      const spec = {
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

      run.emit("token", { id: "ep", zone: "app", label: "send-welcome-email", note: "instructions" });
      run.emit("req", { body: spec });
      await sleep(300);

      const exists = (await api("GET", `/v1/endpoints/${spec.name}`)).ok;
      const { name, ...rest } = spec;
      const res = exists
        ? await api("PUT", `/v1/endpoints/${spec.name}`, { body: rest })
        : await api("POST", "/v1/endpoints", { body: spec });

      run.emit("token", { id: "ep", zone: "ready", state: "ready", note: "one row" });
      run.tick(exists ? "updated in place — <b>one row</b>, no deploy" : "saved — <b>one row</b>, no deploy", "act");

      await sleep(900);
      const secret = await api("GET", "/v1/secrets/email_api_key");
      run.emit("res", { body: secret.body });
      const leaked = JSON.stringify(secret.body ?? {}).includes('"value"');
      run.tick(
        leaked ? "the API returned a secret value — check this" : "asked for the API key: Invokr <b>will not hand it over</b>",
        leaked ? "bad" : "ok",
      );
      run.emit("token", { id: "ep", zone: "done", state: "ok", note: "stored" });
      return res.ok;
    },
  },

  {
    id: "fire-now",
    n: 2,
    group: "core",
    title: "Send one now",
    lede: "Type a real name. It really goes, and the other side really answers.",
    fields: [
      { key: "customer", label: "for", value: "Priya Sharma", width: 170 },
      { key: "email", label: "email", value: "priya@example.com", width: 195 },
      { key: "order_id", label: "order", value: "order-1234", width: 140 },
    ],
    action: "Send it",
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
      const execId = await fire(run, {
        endpoint: "send-welcome-email",
        input: { order_id: v.order_id, customer: v.customer, email: v.email },
        label: v.customer,
        token: "t1",
      });
      if (!execId) return false;
      const done = await watchExecution(run, execId, { targetSince: since, token: "t1", label: v.customer });
      if (done?.status === "SUCCESS") run.tick("delivered — and the other side got the de-duplication key", "ok");
      return done?.status === "SUCCESS";
    },
  },

  {
    id: "fire-later",
    n: 3,
    group: "core",
    title: "Send one later — then kill a worker",
    lede: "It waits in the queue as a row. Kill the workers; it still goes out on time.",
    fields: [
      { key: "customer", label: "for", value: "Arjun Mehta", width: 170 },
      { key: "email", label: "email", value: "arjun@example.com", width: 195 },
      { key: "order_id", label: "order", value: "order-5567", width: 140 },
      { key: "seconds", label: "in (s)", value: "15", width: 70 },
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
          }, 1400);
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
      const seconds = Math.max(5, Math.min(120, Number(v.seconds) || 15));
      const runAt = new Date(Date.now() + seconds * 1000);
      run.emit("target-is", { who: "the email service" });

      const execId = await fire(run, {
        endpoint: "send-welcome-email",
        input: { order_id: v.order_id, customer: v.customer, email: v.email },
        runAt,
        label: v.customer,
        token: "t1",
      });
      if (!execId) return false;

      run.tick("now kill a worker — nothing is holding this", "warn");
      const timer = countdownToken(run, "t1", runAt.getTime(), "due in");

      const done = await watchExecution(run, execId, {
        timeout: (seconds + 90) * 1000,
        targetSince: since,
        token: "t1",
        label: v.customer,
      });
      clearInterval(timer);
      if (done?.status === "SUCCESS") run.tick("went out <b>on time</b>, by whichever worker was alive", "ok");
      return done?.status === "SUCCESS";
    },
  },

  {
    id: "it-fails",
    n: 4,
    group: "core",
    title: "When the other side breaks",
    lede: "Two failures, then it works. Watch it fly back to the queue each time.",
    fields: [
      { key: "order_id", label: "order", value: "order-9931", width: 140 },
      { key: "amount", label: "amount", value: "₹1,499", width: 110 },
    ],
    action: "Take the payment",
    extras: [{ id: "prearm", label: "Start scene 5's job now" }],
    async onExtra(id, run) {
      if (id !== "prearm") return;
      const started = await startCron();
      run?.tick(started ? "scene 5's every-minute job is running now" : "could not start it", started ? "ok" : "bad");
    },
    async run(run, v) {
      await control("/mock/reset");
      const since = await targetLogHead();
      run.emit("target-is", { who: "the payment processor" });

      const execId = await fire(run, {
        endpoint: "charge-webhook",
        input: { order_id: v.order_id, amount: v.amount },
        label: v.amount,
        token: "t1",
      });
      if (!execId) return false;

      const done = await watchExecution(run, execId, {
        timeout: 120000,
        targetSince: since,
        token: "t1",
        label: v.amount,
      });
      if (done?.status === "SUCCESS") {
        run.tick("third try worked — and all three carried <b>the same key</b>", "ok");
      }
      return done?.status === "SUCCESS";
    },
  },

  {
    id: "fire-repeatedly",
    n: 5,
    group: "core",
    title: "Every minute, forever",
    lede: "No scheduler process. PostgreSQL drops the job into the queue itself.",
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

      let jobId = state.cronJob;
      if (jobId) {
        run.tick("using the job started back in scene 4", "act");
      } else {
        jobId = await startCron();
        if (!jobId) return false;
        run.tick("pg_cron owns the schedule now — <b>inside the database</b>", "act");
      }

      const started = performance.now();
      let seen = 0;
      let logSeq = since;

      while (performance.now() - started < 78000 && !run.cancelled) {
        const execs = (await api("GET", `/v1/jobs/${jobId}/executions?limit=10`)).body?.data ?? [];
        if (execs.length > seen) {
          run.emit("cron", { on: true });
          for (const e of execs.slice(0, execs.length - seen).reverse()) {
            const id = `tick-${e.execution_id.slice(0, 6)}`;
            run.emit("token", { id, zone: "ready", state: "ready", label: "tick", note: clock(e.created_at) });
            await sleep(120);
          }
          run.tick("<b>nobody put that there</b> — PostgreSQL wrote the row itself", "act");
          setTimeout(() => run.emit("cron", { on: false }), 1200);
          seen = execs.length;

          const newest = execs[0];
          if (newest) {
            await watchExecution(run, newest.execution_id, {
              timeout: 20000,
              targetSince: logSeq,
              token: `tick-${newest.execution_id.slice(0, 6)}`,
              label: "tick",
            });
            logSeq = await targetLogHead();
          }
          break;
        }
        await sleep(900);
      }

      if (seen === 0) run.tick("no tick landed in the time we waited", "warn");
      else run.tick("cancel it before moving on — it keeps going", "warn");
      return seen > 0;
    },
  },

  {
    id: "any-transport",
    n: 6,
    group: "overflow",
    title: "Not just HTTP",
    lede: "Same job, same retries. A Kafka topic or a Redis Stream instead of a URL.",
    fields: [{ key: "order_id", label: "order", value: "order-4410", width: 140 }],
    action: "Send it three ways",
    async run(run, v) {
      const probes = state.status?.transports ?? {};
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
          run.emit("token", { id: `t${i}`, zone: "app", state: "waiting", label: t.type, note: "not running here" });
          run.tick(`<b>${t.type}</b> is not running here — same job, nowhere to put it`, "warn");
          await sleep(700);
          continue;
        }
        run.emit("target-is", { who: `the ${t.type.toLowerCase()} side` });
        const execId = await fire(run, {
          endpoint: t.endpoint,
          input: { order_id: v.order_id, customer: "Priya Sharma", email: "priya@example.com" },
          label: t.type,
          token: `t${i}`,
          maxAttempts: 1,
        });
        if (!execId) {
          ok = false;
          continue;
        }
        const done = await watchExecution(run, execId, { timeout: 30000, token: `t${i}`, label: t.type });
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
    fields: [
      { key: "customer", label: "for", value: "Neha Rao", width: 170 },
      { key: "order_id", label: "order", value: "order-7782", width: 140 },
    ],
    action: "Send from both teams",
    async run(run, v) {
      const teams = state.status?.provisioned?.workspaces ?? {};
      const email = `${String(v.customer).split(" ")[0].toLowerCase()}@example.com`;
      run.emit("target-is", { who: "the email service" });
      let ok = true;
      let i = 0;

      for (const wsKey of ["a", "b"]) {
        i++;
        const team = teams[wsKey];
        const since = await targetLogHead();
        const execId = await fire(run, {
          endpoint: "send-welcome-email",
          input: { order_id: v.order_id, customer: v.customer, email },
          ws: wsKey,
          label: team?.name ?? wsKey,
          token: `t${i}`,
        });
        if (!execId) {
          ok = false;
          continue;
        }
        const done = await watchExecution(run, execId, {
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

async function findActiveCron() {
  const jobs = (await api("GET", "/v1/jobs?limit=50")).body?.data ?? [];
  return (
    jobs.find((j) => j.trigger === "CRON" && j.status === "ACTIVE" && j.endpoint === "minute-heartbeat")?.job_id ?? null
  );
}

// Cancels any schedule already running for the heartbeat endpoint, then starts
// one. Without the sweep, a rehearsal leaves an every-minute job firing forever.
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
    const input = document.getElementById(`f-${f.key}`);
    if (input) v[f.key] = input.value.trim() || f.value;
  }
  return v;
}

function resetStage() {
  clearTokens();
  clearReceipts();
  $("#ticker").innerHTML = "";
  $("#p-tries").innerHTML = `<span class="empty">nothing yet</span>`;
  $("#z-target").classList.remove("hit", "refused");
  $("#cron-badge").classList.remove("tick");
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
  $("#target-label").textContent = "the other side";
  $("#scene-title").textContent = scene.title;
  $("#scene-lede").textContent = scene.lede;

  const fields = (scene.fields ?? [])
    .map(
      (f) => `<div class="field">
        <label for="f-${f.key}">${esc(f.label)}</label>
        <input id="f-${f.key}" value="${esc(v[f.key] ?? f.value)}" style="--w:${f.width ?? 170}px"
               ${state.replay ? "disabled" : ""} />
      </div>`,
    )
    .join("");

  $("#scene-controls").innerHTML = `
    ${scene.html ? scene.html() : ""}
    ${fields ? `<div class="fields">${fields}</div>` : ""}
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

// Controls can fire outside a run — killing a worker before anything is
// scheduled, say. Those events still need somewhere to go.
function ensureRun(scene) {
  if (state.run && !state.run.cancelled) return state.run;
  const run = new Run(scene, { replay: state.replay });
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

  const run = new Run(scene);
  state.run = run;
  // Recorded first, so a replay rebuilds the same worker boxes.
  run.emit("workers", { list: state.status?.workers ?? [] });

  let ok = false;
  try {
    ok = await scene.run(run, values);
  } catch (err) {
    run.tick(`something broke here: <b>${esc(String(err.message ?? err))}</b>`, "bad");
  }

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
    $("#ticker").innerHTML =
      `<span class="line now bad"><span>${esc(String(err.message ?? err))} — run it live once and it is kept</span></span>`;
    btn.disabled = false;
    btn.textContent = "Play the recording";
    return;
  }

  resetStage();
  const run = new Run(scene, { replay: true });
  state.run = run;

  const startedAt = performance.now();
  for (const ev of tape.events ?? []) {
    if (run.cancelled) return;
    const due = startedAt + ev.t - performance.now();
    if (due > 0) await sleep(due);
    apply(scene, ev);
  }

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
addEventListener("resize", fitBoard);

document.addEventListener("keydown", (e) => {
  if (e.metaKey || e.ctrlKey) return;
  const typing = e.target.tagName === "INPUT";
  const k = e.key.toLowerCase();

  if (e.key === "Enter") {
    e.preventDefault();
    return (state.replay ? replayScene : runScene)();
  }
  if (e.key === "Escape") return $("#drawer").classList.remove("open");
  if (typing) return; // arrows belong to the field being edited
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
