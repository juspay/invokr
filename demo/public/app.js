// Invokr live demo.
//
// One rule holds the whole thing together: a scene's `run()` never touches the
// DOM. It only emits events. Everything on screen is drawn by `apply(event)`.
// That is what makes replay honest — a captured run is replayed through the
// exact same renderer, at the timings it really had, with nothing re-simulated.

const $ = (sel, root = document) => root.querySelector(sel);

const state = {
  status: null,
  current: 0,
  replay: false,
  recording: true,
  ran: new Set(),
  run: null, // active Run
};

// ─── formatting ──────────────────────────────────────────────────────────────

const esc = (s) =>
  String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

const ms = (n) => (n == null ? "—" : n < 1000 ? `${Math.round(n)}ms` : `${(n / 1000).toFixed(2)}s`);

const short = (id) => (id ? `${String(id).slice(0, 8)}…` : "—");

// Secrets resolve inside the worker and go only to the target. The echo server
// hands the resolved header straight back, so mask it here — showing it on a
// projector would undercut the very claim the scene is making, and a recording
// made against a real Invokr must not write a live credential to disk.
//
// Matches both `"authorization":"…"` and the backslash-escaped form that turns
// up when the target's response body is itself a JSON string.
const maskSecrets = (text) =>
  String(text).replace(/(\\?"authorization\\?"\s*:\s*\\?")([^"\\]*)/gi, "$1Bearer ••••••••");

function highlightJson(value, { mask = true } = {}) {
  let text = typeof value === "string" ? value : JSON.stringify(value, null, 2);
  if (mask) text = maskSecrets(text);
  let html = esc(text);
  html = html
    .replace(/\{\{input\.[^}]+\}\}/g, (m) => `<span class="tok-input">${m}</span>`)
    .replace(/\{\{config\.[^}]+\}\}/g, (m) => `<span class="tok-config">${m}</span>`)
    .replace(/\{\{secret\.[^}]+\}\}/g, (m) => `<span class="tok-secret">${m}</span>`)
    .replace(/\{\{execution\.[^}]+\}\}/g, (m) => `<span class="tok-exec">${m}</span>`);
  return html;
}

function card(title, meta, bodyHtml, { tight = false } = {}) {
  return `<div class="card">
    <h3>${esc(title)}${meta ? `<span class="meta">${esc(meta)}</span>` : ""}</h3>
    <div class="body${tight ? " tight" : ""}">${bodyHtml}</div>
  </div>`;
}

// ─── the wire ────────────────────────────────────────────────────────────────

const WIRE_X = { client: 103, api: 353, db: 628, worker: 893, target: 1115 };

const wire = {
  set(node, cls, label) {
    const g = document.getElementById(`n-${node}`);
    if (!g) return;
    g.classList.remove("active", "done", "fail", "dead");
    if (cls) g.classList.add(cls);
    const s = document.getElementById(`s-${node}`);
    if (s && label !== undefined) s.textContent = label ?? "";
  },
  label(node, text) {
    const s = document.getElementById(`s-${node}`);
    if (s) s.textContent = text ?? "";
  },
  targetType(t) {
    const s = document.getElementById("s-target-type");
    if (s) s.textContent = t;
  },
  reset() {
    for (const n of ["client", "api", "db", "worker", "target"]) this.set(n, null, "");
    for (const l of ["l-1", "l-2", "l-3", "l-4"]) {
      const p = document.getElementById(l);
      p.classList.remove("hot", "fail");
    }
  },
  // A dot crossing the link, sized to how long the real step took where we know
  // it (attempt duration), otherwise a fixed 420ms.
  packet(from, to, { fail = false, duration = 420 } = {}) {
    const dot = document.getElementById("packet");
    const x0 = WIRE_X[from];
    const x1 = WIRE_X[to];
    if (x0 == null || x1 == null) return;
    dot.classList.toggle("fail", fail);
    dot.setAttribute("opacity", "1");
    const t0 = performance.now();
    const step = (now) => {
      const p = Math.min(1, (now - t0) / duration);
      dot.setAttribute("cx", String(x0 + (x1 - x0) * p));
      if (p < 1) requestAnimationFrame(step);
      else dot.setAttribute("opacity", "0");
    };
    requestAnimationFrame(step);
  },
  link(id, cls) {
    const p = document.getElementById(id);
    if (!p) return;
    p.classList.remove("hot", "fail");
    if (cls) p.classList.add(cls);
  },
};

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

// ─── runs: the event tape ────────────────────────────────────────────────────

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

  note(text, tone) {
    this.emit("note", { text, tone });
  }

  async save(ok) {
    if (!ok || this.replay || !state.recording) return;
    try {
      await fetch(`/control/recordings/${this.scene.id}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        // Masked on the way to disk, not just on the way to the screen: a tape
        // recorded against a real deployment would otherwise hold the resolved
        // credential the target was called with.
        body: maskSecrets(
          JSON.stringify({
            scene: this.scene.id,
            recorded_at: new Date().toISOString(),
            events: this.events,
          }),
        ),
      });
      const known = state.status?.recordings;
      if (known && !known.includes(this.scene.id)) known.push(this.scene.id);
    } catch {
      /* a failed save must never take the demo down */
    }
  }
}

// ─── the single renderer ─────────────────────────────────────────────────────

function apply(scene, ev) {
  const els = scene._els ?? {};

  switch (ev.type) {
    case "note":
      pushEvent(ev.t, ev.text, ev.tone);
      break;

    case "req":
      pushEvent(ev.t, `<b>${esc(ev.method)}</b> ${esc(ev.path)}`, "act");
      if (els.request) {
        els.request.innerHTML = `<pre>${highlightJson(ev.body ?? {})}</pre>`;
        setMeta(els.request, `${ev.method} ${ev.path}`);
      }
      break;

    case "res":
      pushEvent(ev.t, `${esc(ev.label ?? "response")} <b>${ev.status}</b>`, ev.status < 400 ? "ok" : "bad");
      if (els.response) {
        els.response.innerHTML = `<pre>${highlightJson(ev.body ?? {})}</pre>`;
        setMeta(els.response, `HTTP ${ev.status}`);
      }
      break;

    case "stage":
      wire.set(ev.node, ev.state, ev.label);
      if (ev.text) pushEvent(ev.t, ev.text, ev.tone ?? "hi");
      break;

    case "packet":
      wire.packet(ev.from, ev.to, { fail: ev.fail, duration: ev.duration });
      if (ev.link) wire.link(ev.link, ev.fail ? "fail" : "hot");
      break;

    case "target":
      wire.targetType(ev.kind);
      break;

    case "exec":
      if (els.exec) els.exec.innerHTML = execCard(ev);
      break;

    case "attempt":
      if (els.attempts) els.attempts.innerHTML = attemptsTable(ev.attempts);
      break;

    case "countdown":
      if (els.countdown) {
        const late = ev.remaining <= 0;
        els.countdown.innerHTML = `<div class="big-count ${late ? "late" : ""}">${
          late ? "firing" : (ev.remaining / 1000).toFixed(1)
        }<small>${late ? "" : "s"}</small></div>
        <div class="muted">${esc(ev.label ?? "")}</div>`;
      }
      break;

    case "panel":
      scene.panel?.(ev.key, ev.data, els);
      break;

    case "flag":
      if (ev.key === "worker") setChip("chip-worker", ev.value === "running" ? "up" : "down", `worker ${ev.value}`);
      break;
  }
}

function setMeta(bodyEl, text) {
  const meta = bodyEl.parentElement?.querySelector(".meta");
  if (meta) meta.textContent = text;
}

function pushEvent(t, html, tone = "") {
  const tl = $("#timeline");
  if (!tl) return;
  const row = document.createElement("div");
  row.className = `ev ${tone}`;
  row.innerHTML = `<span class="t">+${t < 1000 ? `${t}ms` : `${(t / 1000).toFixed(2)}s`}</span><span class="m">${html}</span>`;
  tl.appendChild(row);
  tl.scrollTop = tl.scrollHeight;
}

function execCard(ev) {
  const cls = { SUCCESS: "ok", FAILED: "bad", RUNNING: "run", RETRYING: "wait", CANCELLED: "bad" }[ev.status] ?? "wait";
  return `<dl class="kv">
    <dt>execution</dt><dd>${esc(short(ev.execution_id))}</dd>
    <dt>status</dt><dd><span class="pill ${cls}">${esc(ev.status)}</span></dd>
    <dt>attempts</dt><dd>${ev.attempt_count ?? 0} / ${ev.max_attempts ?? "—"}</dd>
    ${ev.worker_id ? `<dt>claimed by</dt><dd>${esc(short(ev.worker_id.replace("worker_", "")))}</dd>` : ""}
    ${ev.run_at ? `<dt>run_at</dt><dd>${esc(new Date(ev.run_at).toLocaleTimeString())}</dd>` : ""}
  </dl>`;
}

function attemptsTable(attempts) {
  if (!attempts?.length) return `<div class="body"><span class="muted">No attempts yet.</span></div>`;
  const rows = attempts
    .map((a) => {
      const ok = a.status === "SUCCESS";
      const detail = ok
        ? `${a.output?.status_code ?? ""} ${maskSecrets(String(a.output?.body ?? "")).slice(0, 150)}`
        : `${a.error?.type ?? "error"}${a.error?.status_code ? ` ${a.error.status_code}` : ""} — ${String(
            a.error?.message ?? "",
          ).slice(0, 120)}`;
      return `<tr>
        <td>${a.attempt_number}</td>
        <td class="${ok ? "ok" : "bad"}">${esc(a.status)}</td>
        <td>${ms(a.duration_ms)}</td>
        <td>${a.gap_ms != null ? ms(a.gap_ms) : "—"}<span class="sub">${a.gap_ms != null ? "since previous attempt" : "first try"}</span></td>
        <td>${esc(detail)}</td>
      </tr>`;
    })
    .join("");
  return `<table>
    <thead><tr><th>#</th><th>status</th><th>took</th><th>backoff</th><th>target said</th></tr></thead>
    <tbody>${rows}</tbody>
  </table>`;
}

// ─── execution watcher ───────────────────────────────────────────────────────

// Polls the API the way an operator would, and emits an event only when
// something actually changed. Returns the terminal execution, or null on
// timeout.
async function watchExecution(run, executionId, { ws = "a", timeout = 90000, label = "" } = {}) {
  const started = performance.now();
  let lastStatus = null;
  let seenAttempts = 0;
  let claimedAttempt = 0;

  while (performance.now() - started < timeout && !run.cancelled) {
    const res = await api("GET", `/v1/executions/${executionId}`, { ws });
    const exec = res.body?.data;

    if (exec) {
      // Held until after this poll's attempt rows are emitted, so the failure
      // is on screen before the backoff that follows from it.
      let pendingRetry = null;
      if (exec.status !== lastStatus) {
        lastStatus = exec.status;
        if (exec.status === "RETRYING" && exec.run_at) pendingRetry = exec;
        run.emit("exec", {
          execution_id: exec.execution_id,
          status: exec.status,
          attempt_count: exec.attempt_count,
          max_attempts: exec.max_attempts,
          worker_id: exec.worker_id,
          run_at: exec.run_at,
        });
      }

      // attempt_count is bumped by the claiming UPDATE, so a rise in it is the
      // edge to watch: RUNNING is frequently over before the next poll lands.
      // Called again from the attempts loop because an attempt row can surface
      // in the same poll whose execution snapshot was read a moment too early —
      // the claim must never be narrated after the attempt it produced.
      const claim = (n) => {
        if (n <= claimedAttempt) return;
        claimedAttempt = n;
        run.emit("stage", {
          node: "worker",
          state: "active",
          label: exec.worker_id ? exec.worker_id.replace("worker_", "").slice(0, 10) : "claimed",
          text: `worker claimed the row — <b>SKIP LOCKED</b>${label ? ` (${label})` : ""}`,
          tone: "act",
        });
        run.emit("packet", { from: "db", to: "worker", link: "l-3" });
      };
      claim(exec.attempt_count);

      const attempts = (await api("GET", `/v1/executions/${executionId}/attempts`, { ws })).body?.data ?? [];
      if (attempts.length !== seenAttempts) {
        const ordered = [...attempts].sort((a, b) => a.attempt_number - b.attempt_number);
        // The real gap between one attempt finishing and the next starting: the
        // observed backoff, not the configured one.
        for (let i = 0; i < ordered.length; i++) {
          if (i > 0) {
            ordered[i].gap_ms =
              new Date(ordered[i].started_at) - new Date(ordered[i - 1].completed_at);
          }
        }
        for (const a of ordered.slice(seenAttempts)) {
          const ok = a.status === "SUCCESS";
          claim(a.attempt_number);
          run.emit("packet", { from: "worker", to: "target", link: "l-4", fail: !ok, duration: Math.min(900, Math.max(220, a.duration_ms || 300)) });
          run.emit("stage", {
            node: "target",
            state: ok ? "done" : "fail",
            label: ok ? `${a.output?.status_code ?? 200} in ${ms(a.duration_ms)}` : `${a.error?.status_code ?? "err"} · ${ms(a.duration_ms)}`,
            text: ok
              ? `attempt ${a.attempt_number}: target answered <b>${a.output?.status_code ?? 200}</b> in ${ms(a.duration_ms)}`
              : `attempt ${a.attempt_number} <b>failed</b> — ${esc(a.error?.type ?? "error")}${
                  a.error?.status_code ? ` ${a.error.status_code}` : ""
                }`,
            tone: ok ? "ok" : "bad",
          });
        }
        seenAttempts = attempts.length;
        run.emit("attempt", { attempts: ordered });
      }

      if (pendingRetry) {
        const waitMs = new Date(pendingRetry.run_at) - Date.now();
        run.emit("stage", {
          node: "worker",
          state: "done",
          label: `backing off ${ms(Math.max(0, waitMs))}`,
          text: `attempt ${pendingRetry.attempt_count + 1} of ${pendingRetry.max_attempts} is due at <b>${new Date(
            pendingRetry.run_at,
          ).toLocaleTimeString()}</b> — the worker wrote <b>run_at = now() + backoff</b> and let go of the row`,
          tone: "warn",
        });
      }

      if (["SUCCESS", "FAILED", "CANCELLED"].includes(exec.status)) return exec;
    }
    await sleep(170);
  }
  return null;
}

// ─── status polling ──────────────────────────────────────────────────────────

function setChip(id, cls, text) {
  const chip = document.getElementById(id);
  if (!chip) return;
  chip.classList.remove("up", "down", "warn");
  if (cls) chip.classList.add(cls);
  chip.innerHTML = `<i></i>${esc(text)}`;
}

async function refreshStatus() {
  try {
    const res = await fetch("/control/status");
    const s = await res.json();
    state.status = s;
    setChip("chip-api", s.api ? "up" : "down", s.api ? "api" : "api down");
    setChip("chip-worker", s.worker.state === "running" ? "up" : "down", `worker ${s.worker.state}`);
    setChip("chip-target", s.mock ? "up" : "down", s.mock ? "target" : "target down");
    const t = s.transports;
    setChip(
      "chip-transports",
      t.kafka || t.redis ? (t.kafka && t.redis ? "up" : "warn") : "down",
      `kafka ${t.kafka ? "up" : "off"} · redis ${t.redis ? "up" : "off"}`,
    );
    $("#foot-note").textContent = s.provisioned
      ? `tenants: ${Object.values(s.provisioned.workspaces).map((w) => w.slug).join(" · ")}`
      : "not provisioned — live scenes unavailable";
  } catch {
    setChip("chip-api", "down", "demo server down");
  }
}

// ─── shared layout pieces ────────────────────────────────────────────────────

const timelineCard = () =>
  card(
    "what actually happened",
    state.replay ? "replay" : "live",
    `<div class="timeline" id="timeline"></div>`,
    { tight: true },
  );

const reqResGrid = (leftTitle = "request", rightTitle = "response") => `
  <div class="grid">
    ${card(leftTitle, "", `<div id="p-request"><span class="muted">Not sent yet.</span></div>`)}
    ${card(rightTitle, "", `<div id="p-response"><span class="muted">Waiting.</span></div>`)}
  </div>`;

const execAndAttempts = () => `
  <div class="grid" style="margin-top:16px">
    ${card("execution", "", `<div id="p-exec"><span class="muted">No execution yet.</span></div>`)}
    ${timelineCard()}
  </div>
  <div style="margin-top:16px">
    ${card("attempt history", "every try, with real durations", `<div id="p-attempts"><span class="muted">No attempts yet.</span></div>`, { tight: true })}
  </div>`;

const TEMPLATE_LEGEND = `<div class="legend">
  <span><b class="tok-input">{{input.*}}</b> per-job payload</span>
  <span><b class="tok-config">{{config.*}}</b> shared config</span>
  <span><b class="tok-secret">{{secret.*}}</b> encrypted store</span>
  <span><b class="tok-exec">{{execution.*}}</b> execution metadata</span>
</div>`;

const WELCOME_INPUT = { order_id: "order-1234", user_id: "u_abc" };
const key = (p) => `${p}-${Date.now().toString(36)}`;

// ─── scenes ──────────────────────────────────────────────────────────────────

const scenes = [
  {
    id: "register",
    n: 1,
    group: "core",
    title: "Register",
    kicker: "An endpoint is a row: where to deliver, what to send, how to retry. No code ships to Invokr.",
    watch:
      "The spec holds <b>references</b>, not values. <code>{{secret.email_api_key}}</code> is resolved inside the worker at execution time — ask the API for the secret and it will not give it to you.",
    layout: () => `
      ${reqResGrid("endpoint spec — sent to POST /v1/endpoints", "GET /v1/secrets/email_api_key")}
      ${TEMPLATE_LEGEND}
      <div style="margin-top:16px">${timelineCard()}</div>`,
    actions: [{ label: "Register the endpoint", primary: true }],
    async run(run) {
      const spec = {
        name: "send-welcome-email",
        type: "HTTP",
        payload_spec: "order-input",
        config: "email-service",
        spec: {
          url: "{{config.api_base_url}}/echo",
          method: "POST",
          headers: {
            Authorization: "Bearer {{secret.email_api_key}}",
            "Content-Type": "application/json",
          },
          body_template: {
            order_id: "{{input.order_id}}",
            sender: "{{config.sender}}",
            attempt: "{{execution.attempt_count}}",
          },
          timeout_ms: 5000,
          expected_status_codes: [200],
        },
        retry_policy: { max_attempts: 3, backoff: "exponential", initial_delay_ms: 1000, max_delay_ms: 30000 },
      };

      run.emit("stage", { node: "client", state: "active", label: "registering" });
      run.emit("req", { method: "POST", path: "/v1/endpoints", body: spec });
      run.emit("packet", { from: "client", to: "api", link: "l-1" });

      const exists = (await api("GET", `/v1/endpoints/${spec.name}`)).ok;
      let res;
      if (exists) {
        run.note("this endpoint is already registered — updating it in place", "warn");
        const { name, ...rest } = spec;
        res = await api("PUT", `/v1/endpoints/${spec.name}`, { body: rest });
      } else {
        res = await api("POST", "/v1/endpoints", { body: spec });
      }
      run.emit("stage", { node: "api", state: "done", label: `${res.status}` });
      run.emit("packet", { from: "api", to: "db", link: "l-2" });
      run.emit("stage", { node: "db", state: "done", label: "endpoints row written" });
      run.note("the endpoint is a row in this workspace's schema — nothing was deployed", "hi");

      await sleep(500);
      const secret = await api("GET", "/v1/secrets/email_api_key");
      run.emit("res", { status: secret.status, body: secret.body, label: "GET /v1/secrets/email_api_key" });
      run.note(
        secret.body?.data && !("value" in (secret.body.data ?? {}))
          ? "the API returns the secret's <b>name and timestamps only</b> — the value is write-only, AES-256-GCM at rest"
          : "secret metadata returned",
        "ok",
      );
      return res.ok || res.status === 409;
    },
  },

  {
    id: "fire-now",
    n: 2,
    group: "core",
    title: "Fire now",
    kicker: "setTimeout(fn, 0). One POST, and the row is durable before you get your 201 back.",
    watch:
      "Follow the wire: the API writes job + execution in one transaction, the worker claims the row with <b>SKIP LOCKED</b>, the target answers. The attempt row carries the real latency — and the header the target received.",
    layout: () => `${reqResGrid("POST /v1/jobs", "201 Created")}${execAndAttempts()}`,
    actions: [{ label: "Fire it", primary: true }],
    async run(run) {
      const body = {
        endpoint: "send-welcome-email",
        trigger: "IMMEDIATE",
        idempotency_key: key("order-1234-welcome"),
        input: WELCOME_INPUT,
      };
      run.emit("target", { kind: "HTTP" });
      run.emit("stage", { node: "client", state: "active", label: "POST /v1/jobs" });
      run.emit("req", { method: "POST", path: "/v1/jobs", body });
      run.emit("packet", { from: "client", to: "api", link: "l-1" });

      const res = await api("POST", "/v1/jobs", { body });
      run.emit("res", { status: res.status, body: res.body });
      if (!res.ok) return false;

      const exec = res.body.data.execution;
      run.emit("stage", { node: "api", state: "done", label: "201 Created" });
      run.emit("packet", { from: "api", to: "db", link: "l-2" });
      run.emit("stage", {
        node: "db",
        state: "active",
        label: `execution ${exec.status}`,
        text: `job + execution committed in one transaction — execution is <b>${exec.status}</b>`,
        tone: "act",
      });

      const done = await watchExecution(run, exec.execution_id);
      run.emit("stage", { node: "db", state: "done", label: done?.status ?? "—" });
      run.note(
        done?.status === "SUCCESS"
          ? "the target's echo shows the resolved body and the <b>x-invokr-idempotency-key</b> header Invokr sent it"
          : "execution did not reach SUCCESS",
        done?.status === "SUCCESS" ? "ok" : "bad",
      );
      return done?.status === "SUCCESS";
    },
  },

  {
    id: "fire-later",
    n: 3,
    group: "core",
    title: "Fire later — then kill the worker",
    kicker: "setTimeout(fn, 15000), except nothing is holding a timer. The due time is a column.",
    watch:
      "Kill the worker while the job is pending. Nothing is lost, because nothing was in memory: <b>the row is the timer</b>. Start a worker again and it fires.",
    layout: () => `
      <div class="grid">
        ${card("countdown to run_at", "", `<div id="p-countdown"><span class="muted">Not scheduled yet.</span></div>`)}
        ${card("201 Created", "", `<div id="p-response"><span class="muted">Waiting.</span></div>`)}
      </div>
      ${execAndAttempts()}`,
    actions: [
      { label: "Schedule for +15s", primary: true },
      { label: "kill -9 the worker", danger: true, id: "kill" },
      { label: "Start a worker", id: "start" },
    ],
    async action(id, run) {
      if (id === "kill") {
        const res = await control("/worker/kill");
        const killed = res.body?.ok;
        (run ?? state.run)?.emit("flag", { key: "worker", value: "killed" });
        (run ?? state.run)?.emit("stage", {
          node: "worker",
          state: "dead",
          label: killed ? `SIGKILL pid ${res.body.pid}` : "not running",
          text: killed
            ? `<b>SIGKILL</b> to the worker (pid ${res.body.pid}) — no graceful drain, any open transaction is aborted`
            : "no worker was running",
          tone: killed ? "bad" : "warn",
        });
        refreshStatus();
        return;
      }
      if (id === "start") {
        const res = await control("/worker/start");
        (run ?? state.run)?.emit("flag", { key: "worker", value: "running" });
        (run ?? state.run)?.emit("stage", {
          node: "worker",
          state: "active",
          label: `pid ${res.body?.pid ?? "?"}`,
          text: `a worker is back (pid ${res.body?.pid ?? "?"}) — it polls the same table, knowing nothing about what came before`,
          tone: "act",
        });
        refreshStatus();
      }
    },
    async run(run) {
      const runAt = new Date(Date.now() + 15000);
      const body = {
        endpoint: "send-welcome-email",
        trigger: "DELAYED",
        idempotency_key: key("order-1234-reminder"),
        run_at: runAt.toISOString(),
        input: WELCOME_INPUT,
      };
      run.emit("target", { kind: "HTTP" });
      run.emit("req", { method: "POST", path: "/v1/jobs", body });
      run.emit("packet", { from: "client", to: "api", link: "l-1" });

      const res = await api("POST", "/v1/jobs", { body });
      run.emit("res", { status: res.status, body: res.body });
      if (!res.ok) return false;

      const exec = res.body.data.execution;
      run.emit("packet", { from: "api", to: "db", link: "l-2" });
      run.emit("stage", {
        node: "db",
        state: "active",
        label: "PENDING · run_at set",
        text: "execution is <b>PENDING</b> with a run_at 15s out — that row is the entire timer",
        tone: "act",
      });
      run.note("kill the worker now if you want to make the point the hard way", "warn");

      // Countdown is emitted, not computed on screen, so replay reproduces it.
      const deadline = runAt.getTime();
      const ticker = setInterval(() => {
        run.emit("countdown", {
          remaining: Math.max(0, deadline - Date.now()),
          label: `run_at ${runAt.toLocaleTimeString()}`,
        });
      }, 500);

      const done = await watchExecution(run, exec.execution_id, { timeout: 120000 });
      clearInterval(ticker);
      run.emit("countdown", { remaining: 0, label: `fired at ${new Date().toLocaleTimeString()}` });

      if (done?.status === "SUCCESS") {
        run.emit("stage", { node: "db", state: "done", label: "SUCCESS" });
        run.note("it fired — the worker that ran it is not the worker that was told about it", "ok");
      }
      return done?.status === "SUCCESS";
    },
  },

  {
    id: "it-fails",
    n: 4,
    group: "core",
    title: "It fails",
    kicker: "The target returns 500 twice. This is the scene a hand-rolled tracker cannot show you.",
    watch:
      "Three attempt rows, each with its own duration, error body and the <b>observed</b> gap before the next try — not the gap the policy asked for. Exponential with ±25% jitter, so the numbers are never round.",
    layout: () => `
      <div class="banner info">Retry policy on <code>charge-webhook</code>: <code>exponential</code>, initial 2000ms, max 30000ms, 3 attempts, ±25% jitter.</div>
      ${reqResGrid("POST /v1/jobs", "201 Created")}${execAndAttempts()}`,
    actions: [
      { label: "Fire at the failing target", primary: true },
      { label: "Pre-arm scene 5's schedule", id: "prearm" },
    ],
    async action(id, run) {
      if (id !== "prearm") return;
      const started = await startCron();
      (run ?? state.run)?.note(
        started ? "scene 5's every-minute schedule is running now — a tick will have landed by the time you get there" : "could not pre-arm the schedule",
        started ? "ok" : "bad",
      );
    },
    async run(run) {
      await control("/mock/reset"); // the mock's flaky counter is global
      run.note("reset the target so it fails twice, then succeeds", "");

      const body = {
        endpoint: "charge-webhook",
        trigger: "IMMEDIATE",
        idempotency_key: key("charge-9931"),
        input: { order_id: "order-9931" },
      };
      run.emit("target", { kind: "HTTP · flaky" });
      run.emit("req", { method: "POST", path: "/v1/jobs", body });
      run.emit("packet", { from: "client", to: "api", link: "l-1" });

      const res = await api("POST", "/v1/jobs", { body });
      run.emit("res", { status: res.status, body: res.body });
      if (!res.ok) return false;

      run.emit("packet", { from: "api", to: "db", link: "l-2" });
      run.emit("stage", { node: "db", state: "active", label: "QUEUED" });

      const done = await watchExecution(run, res.body.data.execution.execution_id, { timeout: 120000 });
      run.emit("stage", { node: "db", state: "done", label: done?.status ?? "—" });
      if (done?.status === "SUCCESS") {
        run.note("succeeded on attempt 3 — and every failed attempt is still on the record", "ok");
      }
      return done?.status === "SUCCESS";
    },
  },

  {
    id: "fire-repeatedly",
    n: 5,
    group: "core",
    title: "Fire repeatedly",
    kicker: "setInterval, except the interval lives in pg_cron. There is no scheduler process to fall over.",
    watch:
      "<b>pg_cron materializes the ticks</b> — PostgreSQL inserts the execution row itself, on schedule, whether or not a worker is up. Cancel, and the pg_cron entry goes with it.",
    layout: () => `
      <div class="grid">
        ${card("countdown to next tick", "", `<div id="p-countdown"><span class="muted">Not scheduled yet.</span></div>`)}
        ${card("the job", "", `<div id="p-response"><span class="muted">Waiting.</span></div>`)}
      </div>
      <div class="grid" style="margin-top:16px">
        ${card("executions materialized by pg_cron", "", `<div id="p-cron"><span class="muted">None yet.</span></div>`, { tight: true })}
        ${timelineCard()}
      </div>`,
    actions: [
      { label: "Start the every-minute schedule", primary: true },
      { label: "Cancel it", danger: true, id: "cancel" },
    ],
    async action(id, run) {
      if (id !== "cancel") return;
      // Looked up rather than remembered: a reloaded page has no state, but the
      // schedule is still out there firing every minute.
      const job = state.cronJob ?? (await findActiveCron());
      if (!job) return (run ?? state.run)?.note("no schedule is running", "warn");
      const res = await api("POST", `/v1/jobs/${job}/cancel`);
      (run ?? state.run)?.emit("res", { status: res.status, body: res.body, label: "cancel" });
      (run ?? state.run)?.note(
        res.ok ? "cancelled — the pg_cron entry is unscheduled in the same transaction" : "cancel failed",
        res.ok ? "ok" : "bad",
      );
      state.cronJob = null;
      refreshStatus();
    },
    panel(key, data, els) {
      if (key !== "cron" || !els.cron) return;
      if (!data.executions.length) {
        els.cron.innerHTML = `<div class="body"><span class="muted">Waiting for the first tick…</span></div>`;
        return;
      }
      els.cron.innerHTML = `<table>
        <thead><tr><th>tick</th><th>created</th><th>status</th><th>took</th></tr></thead>
        <tbody>${data.executions
          .map(
            (e, i) => `<tr>
              <td>${data.executions.length - i}</td>
              <td>${esc(new Date(e.created_at).toLocaleTimeString())}</td>
              <td class="${e.status === "SUCCESS" ? "ok" : e.status === "FAILED" ? "bad" : ""}">${esc(e.status)}</td>
              <td>${e.duration_ms != null ? ms(e.duration_ms) : "—"}</td>
            </tr>`,
          )
          .join("")}</tbody></table>`;
    },
    async run(run) {
      run.emit("target", { kind: "HTTP" });
      let jobId = state.cronJob;
      if (jobId) {
        run.note("using the schedule pre-armed during scene 4", "hi");
      } else {
        run.emit("req", {
          method: "POST",
          path: "/v1/jobs",
          body: { endpoint: "minute-heartbeat", trigger: "CRON", cron: "* * * * *", timezone: "Asia/Kolkata", input: {} },
        });
        jobId = await startCron();
        if (!jobId) return false;
        run.note("pg_cron entry registered at job creation — PostgreSQL owns the schedule now", "act");
      }

      const job = (await api("GET", `/v1/jobs/${jobId}`)).body?.data;
      run.emit("res", { status: 200, body: job, label: "job" });
      run.emit("packet", { from: "api", to: "db", link: "l-2" });
      run.emit("stage", { node: "db", state: "active", label: "pg_cron: * * * * *" });

      const next = job?.next_run_at ? new Date(job.next_run_at).getTime() : Date.now() + 60000;
      const deadline = Date.now() + Math.max(0, next - Date.now());
      const started = performance.now();
      let seen = 0;

      // Watch ticks land for a minute and a bit — long enough for at least one.
      while (performance.now() - started < 75000 && !run.cancelled) {
        run.emit("countdown", {
          remaining: Math.max(0, deadline - Date.now()),
          label: "pg_cron granularity is one minute",
        });
        const execs = (await api("GET", `/v1/jobs/${jobId}/executions?limit=10`)).body?.data ?? [];
        if (execs.length !== seen) {
          run.emit("panel", { key: "cron", data: { executions: execs } });
          if (execs.length > seen) {
            run.emit("packet", { from: "db", to: "worker", link: "l-3" });
            run.emit("stage", {
              node: "worker",
              state: "active",
              label: `tick ${execs.length}`,
              text: `<b>pg_cron</b> inserted a tick — the worker claimed it like any other row`,
              tone: "act",
            });
            const latest = execs[0];
            if (latest?.status === "SUCCESS") {
              run.emit("packet", { from: "worker", to: "target", link: "l-4", duration: 300 });
              run.emit("stage", { node: "target", state: "done", label: `tick in ${ms(latest.duration_ms ?? 0)}` });
            }
          }
          seen = execs.length;
          if (seen >= 1) break;
        }
        await sleep(900);
      }
      run.note(
        seen > 0 ? "that row was written by PostgreSQL, not by a scheduler process" : "no tick landed within the window",
        seen > 0 ? "ok" : "warn",
      );
      run.note("cancel it before moving on — it will keep firing every minute otherwise", "warn");
      return seen > 0;
    },
  },

  {
    id: "any-transport",
    n: 6,
    group: "overflow",
    title: "Any transport",
    kicker: "Same job shape, same retry policy, same attempt history — HTTP, Kafka topic, or Redis Stream.",
    watch:
      "Only the endpoint's <code>spec</code> changes. The job you POST, the retries you get, and the rows you debug from are identical.",
    layout: () => {
      const t = state.status?.transports ?? {};
      const missing = [!t.kafka && "Kafka", !t.redis && "Redis"].filter(Boolean);
      return `
      ${
        missing.length
          ? `<div class="banner warn">${missing.join(" and ")} ${missing.length > 1 ? "are" : "is"} not
             running here, so ${missing.length > 1 ? "those dispatches" : "that dispatch"} will be skipped — you will
             see the endpoint spec instead of a delivery. To fire them live:
             <code>docker compose --profile kafka --profile redis up -d</code>, then
             <code>INVOKR_DEMO_WORKER_FEATURES=kafka,redis-stream just demo</code>.</div>`
          : ""
      }
      <div class="transport-grid">
        ${["HTTP", "KAFKA", "REDIS_STREAM"]
          .map((t) => `${card(t, "", `<div id="p-t-${t}"><span class="muted">Not fired.</span></div>`)}`)
          .join("")}
      </div>
      ${execAndAttempts()}`;
    },
    actions: [
      { label: "Fire all three", primary: true },
    ],
    panel(key, data, els) {
      if (key !== "transport") return;
      const box = document.getElementById(`p-t-${data.type}`);
      if (!box) return;
      if (data.skipped) {
        box.innerHTML = `<div class="muted" style="margin-bottom:8px">Not running here — the spec is the only thing
          that differs from the HTTP endpoint.</div>
          <pre>${highlightJson(data.spec ?? {})}</pre>`;
        return;
      }
      box.innerHTML = `<dl class="kv">
        <dt>endpoint</dt><dd>${esc(data.endpoint)}</dd>
        <dt>result</dt><dd><span class="pill ${data.ok ? "ok" : "bad"}">${esc(data.status)}</span></dd>
        ${data.detail ? `<dt>detail</dt><dd>${esc(data.detail)}</dd>` : ""}
      </dl>`;
    },
    async run(run) {
      const probes = state.status?.transports ?? {};
      const targets = [
        { type: "HTTP", endpoint: "send-welcome-email", input: WELCOME_INPUT, up: true },
        { type: "KAFKA", endpoint: "order-events-kafka", input: WELCOME_INPUT, up: !!probes.kafka },
        { type: "REDIS_STREAM", endpoint: "order-events-redis", input: WELCOME_INPUT, up: !!probes.redis },
      ];

      let allOk = true;
      for (const t of targets) {
        // Firing at a broker that isn't there only proves the broker isn't
        // there. Show the spec instead — that is the whole claim anyway.
        if (!t.up) {
          const ep = (await api("GET", `/v1/endpoints/${t.endpoint}`)).body?.data;
          run.emit("panel", {
            key: "transport",
            data: { type: t.type, endpoint: t.endpoint, skipped: true, spec: ep?.spec ?? {} },
          });
          run.note(`<b>${t.type}</b> skipped — broker not running; the spec is the only difference`, "warn");
          continue;
        }
        run.emit("target", { kind: t.type });
        const body = {
          endpoint: t.endpoint,
          trigger: "IMMEDIATE",
          idempotency_key: key(`transport-${t.type.toLowerCase()}`),
          input: t.input,
          max_attempts: 1,
        };
        run.emit("req", { method: "POST", path: "/v1/jobs", body });
        const res = await api("POST", "/v1/jobs", { body });
        if (!res.ok) {
          run.emit("panel", { key: "transport", data: { type: t.type, endpoint: t.endpoint, ok: false, status: `HTTP ${res.status}` } });
          allOk = false;
          continue;
        }
        const done = await watchExecution(run, res.body.data.execution.execution_id, { timeout: 30000 });
        const attempts = (await api("GET", `/v1/executions/${res.body.data.execution.execution_id}/attempts`)).body?.data ?? [];
        const err = attempts.find((a) => a.error)?.error;
        const ok = done?.status === "SUCCESS";
        allOk = allOk && ok;
        run.emit("panel", {
          key: "transport",
          data: {
            type: t.type,
            endpoint: t.endpoint,
            ok,
            status: done?.status ?? "timed out",
            detail: ok ? `delivered in ${ms(attempts[0]?.duration_ms)}` : `${err?.type ?? ""} ${String(err?.message ?? "").slice(0, 90)}`,
          },
        });
        run.note(
          ok
            ? `<b>${t.type}</b> delivered — same job shape, same policy, same attempt row`
            : `<b>${t.type}</b> failed: ${esc(err?.type ?? done?.status ?? "unknown")}`,
          ok ? "ok" : "bad",
        );
      }
      return allOk;
    },
  },

  {
    id: "two-tenants",
    n: 7,
    group: "overflow",
    title: "Two tenants",
    kicker: "The same endpoint name in two workspaces. Different schemas, no shared table, no shared row.",
    watch:
      "Both columns say <code>send-welcome-email</code>. The <code>schema_name</code> under each is where its rows actually live — <b>schema-per-workspace</b>, enforced by the connection's search_path.",
    layout: () => `
      <div class="tenant-grid">
        ${card("workspace A", "", `<div id="p-ws-a"><span class="muted">Not fired.</span></div>`)}
        ${card("workspace B", "", `<div id="p-ws-b"><span class="muted">Not fired.</span></div>`)}
      </div>
      <div style="margin-top:16px">${timelineCard()}</div>`,
    actions: [{ label: "Fire the same endpoint in both", primary: true }],
    panel(key, data, els) {
      if (key !== "tenant") return;
      const box = document.getElementById(`p-ws-${data.ws}`);
      if (!box) return;
      box.innerHTML = `<dl class="kv">
        <dt>workspace</dt><dd>${esc(data.name)}</dd>
        <dt>schema</dt><dd>${esc(data.schema)}</dd>
        <dt>endpoint</dt><dd>send-welcome-email</dd>
        <dt>job</dt><dd>${esc(short(data.job_id))}</dd>
        <dt>execution</dt><dd>${esc(short(data.execution_id))}</dd>
        <dt>status</dt><dd><span class="pill ${data.status === "SUCCESS" ? "ok" : "bad"}">${esc(data.status)}</span></dd>
        <dt>sender used</dt><dd>${esc(data.sender ?? "—")}</dd>
      </dl>`;
    },
    async run(run) {
      run.emit("target", { kind: "HTTP ×2" });
      const wsMeta = state.status?.provisioned?.workspaces ?? {};
      let ok = true;

      for (const wsKey of ["a", "b"]) {
        const body = {
          endpoint: "send-welcome-email",
          trigger: "IMMEDIATE",
          idempotency_key: key(`tenant-${wsKey}`),
          input: WELCOME_INPUT,
        };
        const res = await api("POST", "/v1/jobs", { body, ws: wsKey });
        if (!res.ok) {
          ok = false;
          continue;
        }
        const execId = res.body.data.execution.execution_id;
        run.note(`fired into <b>${esc(wsMeta[wsKey]?.slug ?? wsKey)}</b> — same endpoint name, different schema`, "act");
        const done = await watchExecution(run, execId, { ws: wsKey, timeout: 30000, label: wsMeta[wsKey]?.slug });
        const attempts = (await api("GET", `/v1/executions/${execId}/attempts`, { ws: wsKey })).body?.data ?? [];
        let sender = null;
        try {
          sender = JSON.parse(attempts[0]?.output?.body ?? "{}")?.body?.sender ?? null;
        } catch {}
        ok = ok && done?.status === "SUCCESS";
        run.emit("panel", {
          key: "tenant",
          data: {
            ws: wsKey,
            name: wsMeta[wsKey]?.name ?? wsKey,
            schema: wsMeta[wsKey]?.schema_name ?? "—",
            job_id: res.body.data.job_id,
            execution_id: execId,
            status: done?.status ?? "timed out",
            sender,
          },
        });
      }
      run.note("the two rows cannot see each other — nothing in the query path crosses schemas", ok ? "ok" : "warn");
      return ok;
    },
  },

  {
    id: "handoff",
    n: 8,
    group: "overflow",
    title: "Hand-off",
    kicker: "Everything you just watched was reconstructed from rows. Here is the tool operators actually use.",
    watch: "This page is a demo. The dashboard is the product surface — same data, no narration.",
    layout: () => {
      const url = state.status?.dashboardUrl || "";
      return `<div class="handoff">
        ${
          url
            ? `<div class="banner info">Dashboard: <a href="${esc(url)}" target="_blank" rel="noopener">${esc(url)}</a></div>`
            : `<div class="banner warn">No dashboard URL configured. Build it with <code>just dashboard-build</code>, run the API with
               <code>INVOKR_MODE=both</code>, and set <code>INVOKR_DEMO_DASHBOARD_URL</code> before <code>just demo</code>.</div>`
        }
        <p class="kicker">Three things to point at, in this order:</p>
        <ol>
          <li><b>The executions list.</b> Every job you fired in this session is there, including the ones that failed.</li>
          <li><b>An execution's attempts.</b> The same rows this page has been reading — durations, error bodies, the lot.</li>
          <li><b>Ad-hoc invoke.</b> An operator can fire a registered endpoint without a client, which is how most
              "can you just re-run it" requests get answered.</li>
        </ol>
        <p class="muted">Then stop talking and take questions.</p>
      </div>`;
    },
    actions: [],
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
    body: {
      endpoint: "minute-heartbeat",
      trigger: "CRON",
      cron: "* * * * *",
      timezone: "Asia/Kolkata",
      input: {},
    },
  });
  if (!res.ok) return null;
  state.cronJob = res.body.data.job_id;
  return state.cronJob;
}

// ─── scene shell ─────────────────────────────────────────────────────────────

function renderRail() {
  const rail = $("#rail");
  const core = scenes.filter((s) => s.group === "core");
  const overflow = scenes.filter((s) => s.group === "overflow");
  const link = (s) =>
    `<button class="scene-link ${s.group} ${state.current === scenes.indexOf(s) ? "current" : ""} ${
      state.ran.has(s.id) ? "ran" : ""
    }" data-i="${scenes.indexOf(s)}"><span class="n">${s.n}</span><span>${esc(s.title)}</span></button>`;

  rail.innerHTML = `
    <div class="rail-label">core run · always show</div>
    ${core.map(link).join("")}
    <div class="rail-label">overflow · if time allows</div>
    ${overflow.map(link).join("")}`;

  rail.querySelectorAll(".scene-link").forEach((b) =>
    b.addEventListener("click", () => mountScene(Number(b.dataset.i))),
  );
}

function mountScene(i) {
  if (state.run) state.run.cancelled = true;
  state.run = null;
  state.current = Math.max(0, Math.min(scenes.length - 1, i));
  const scene = scenes[state.current];

  wire.reset();
  const primary = scene.actions?.[0];
  const extras = (scene.actions ?? []).slice(1);
  const hasRecording = state.status?.recordings?.includes(scene.id);

  $("#stage").innerHTML = `
    <div class="scene-head">
      <span class="num">${scene.n} / ${scenes.length}</span>
      <div>
        <h1>${esc(scene.title)}</h1>
        <p class="kicker">${esc(scene.kicker)}</p>
      </div>
    </div>
    <div class="watch">${scene.watch}</div>
    <div class="controls">
      ${
        primary
          ? `<button class="primary" id="act-run">${state.replay ? "Replay this scene" : esc(primary.label)}</button>`
          : ""
      }
      ${extras
        .map(
          (a) =>
            `<button class="${a.danger ? "danger" : ""}" data-act="${esc(a.id)}" ${
              state.replay ? "disabled" : ""
            }>${esc(a.label)}</button>`,
        )
        .join("")}
      <span class="spacer"></span>
      ${
        hasRecording
          ? `<span class="hint">recording available</span>`
          : `<span class="hint">no recording yet</span>`
      }
      <button class="ghost" id="act-next">next →</button>
    </div>
    ${scene.layout()}`;

  scene._els = {
    request: $("#p-request"),
    response: $("#p-response"),
    exec: $("#p-exec"),
    attempts: $("#p-attempts"),
    countdown: $("#p-countdown"),
    cron: $("#p-cron"),
  };

  $("#act-run")?.addEventListener("click", () => (state.replay ? replayScene() : runScene()));
  $("#act-next")?.addEventListener("click", () => mountScene(state.current + 1));
  $("#stage")
    .querySelectorAll("[data-act]")
    .forEach((b) =>
      b.addEventListener("click", async () => {
        b.disabled = true;
        try {
          await scene.action?.(b.dataset.act, ensureRun(scene));
        } finally {
          b.disabled = false;
        }
      }),
    );

  renderRail();
}

// Controls can fire outside a run (killing the worker before scheduling, say).
// Those events still need somewhere to go.
function ensureRun(scene) {
  if (state.run && !state.run.cancelled) return state.run;
  const run = new Run(scene, { replay: state.replay });
  state.run = run;
  return run;
}

async function runScene() {
  const scene = scenes[state.current];
  const btn = $("#act-run");
  if (!btn) return; // scenes with nothing to fire (the hand-off) have no button
  btn.disabled = true;
  btn.textContent = "running…";

  const run = new Run(scene);
  state.run = run;
  wire.reset();
  wire.set("client", "active", "");

  let ok = false;
  try {
    ok = await scene.run(run);
  } catch (err) {
    run.note(`<b>error</b> ${esc(String(err.message ?? err))}`, "bad");
  }

  if (ok) state.ran.add(scene.id);
  await run.save(ok);
  renderRail();

  btn.disabled = false;
  btn.textContent = state.replay ? "Replay this scene" : scene.actions[0].label;
  if (!ok) {
    run.note("live run did not complete — flip to replay if you need this scene now", "warn");
  }
}

async function replayScene() {
  const scene = scenes[state.current];
  const btn = $("#act-run");
  if (!btn) return;
  btn.disabled = true;
  btn.textContent = "replaying…";

  let tape;
  try {
    const res = await fetch(`/control/recordings/${scene.id}`);
    if (!res.ok) throw new Error("no recording for this scene yet");
    tape = await res.json();
  } catch (err) {
    $("#stage").insertAdjacentHTML(
      "afterbegin",
      `<div class="banner bad">${esc(String(err.message ?? err))} — run it live once and it will be captured.</div>`,
    );
    btn.disabled = false;
    btn.textContent = "Replay this scene";
    return;
  }

  // Reset the panel, then replay the tape at the timings it really had.
  const run = new Run(scene, { replay: true });
  state.run = run;
  wire.reset();
  mountPanelsOnly(scene);

  const events = tape.events ?? [];
  const startedAt = performance.now();
  for (const ev of events) {
    if (run.cancelled) return;
    const due = startedAt + ev.t - performance.now();
    if (due > 0) await sleep(due);
    apply(scene, ev);
  }

  btn.disabled = false;
  btn.textContent = "Replay this scene";
  state.ran.add(scene.id);
  renderRail();
}

// Re-render just the scene's output area so a replay starts from a clean panel
// without rebuilding the header and its listeners.
function mountPanelsOnly(scene) {
  const stage = $("#stage");
  const marker = stage.querySelector(".controls");
  while (marker.nextSibling) marker.nextSibling.remove();
  marker.insertAdjacentHTML("afterend", scene.layout());
  scene._els = {
    request: $("#p-request"),
    response: $("#p-response"),
    exec: $("#p-exec"),
    attempts: $("#p-attempts"),
    countdown: $("#p-countdown"),
    cron: $("#p-cron"),
  };
}

function setReplay(on) {
  state.replay = on;
  document.body.classList.toggle("replaying", on);
  $("#replay-toggle").classList.toggle("on", on);
  mountScene(state.current);
}

// ─── boot ────────────────────────────────────────────────────────────────────

$("#replay-toggle").addEventListener("click", () => setReplay(!state.replay));

document.addEventListener("keydown", (e) => {
  if (e.target.tagName === "INPUT" || e.metaKey || e.ctrlKey) return;
  if (e.key === "ArrowRight") mountScene(state.current + 1);
  else if (e.key === "ArrowLeft") mountScene(state.current - 1);
  else if (e.key === "Enter") (state.replay ? replayScene : runScene)();
  else if (e.key.toLowerCase() === "r") setReplay(!state.replay);
});

await refreshStatus();
setInterval(refreshStatus, 4000);
mountScene(0);
