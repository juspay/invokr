// Invokr live demo.
//
// One rule holds the whole thing together: a scene's `run()` never touches the
// DOM. It only emits events, and `apply(event)` draws them. That is what makes
// replay honest — a recorded run replays through the same renderer, at the
// timings it really had, with nothing re-simulated.
//
// The second rule is about tone. Every scene fires a real job with whatever the
// room just suggested, and the page narrates it in sentences. The JSON is still
// there, one click down, for whoever wants to audit it.

const $ = (sel, root = document) => root.querySelector(sel);

const state = {
  status: null,
  current: 0,
  replay: false,
  recording: true,
  ran: new Set(),
  run: null,
  values: {}, // per-scene form values, so they survive re-renders
};

// ─── words and numbers ───────────────────────────────────────────────────────

const esc = (s) =>
  String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

const ms = (n) => (n == null ? "—" : n < 1000 ? `${Math.round(n)}ms` : `${(n / 1000).toFixed(2)}s`);

const clock = (t) => new Date(t).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });

const short = (id) => (id ? `${String(id).slice(0, 8)}…` : "—");

// Secrets resolve inside the worker and go only to the target. Mask them on the
// way to the screen and on the way to disk — a recording made against a real
// Invokr must not keep a live credential.
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

const card = (title, meta, bodyHtml, { tight = false, scroll = false } = {}) => `
  <div class="card">
    <h3>${esc(title)}${meta ? `<span class="meta">${esc(meta)}</span>` : ""}</h3>
    <div class="body${tight ? " tight" : ""}${scroll ? " scroll" : ""}">${bodyHtml}</div>
  </div>`;

// ─── the journey strip ───────────────────────────────────────────────────────

const journey = {
  set(hop, cls, said) {
    const el = document.getElementById(`hop-${hop}`);
    if (!el) return;
    el.classList.remove("active", "done", "fail", "gone", "flowing");
    if (cls) el.classList.add(cls);
    if (said !== undefined) {
      const s = document.getElementById(`said-${hop}`);
      if (s) s.textContent = said ?? "";
    }
  },
  flow(hop) {
    document.getElementById(`hop-${hop}`)?.classList.add("flowing");
  },
  target(who, what) {
    const w = document.getElementById("who-target");
    const t = document.getElementById("what-target");
    if (w && who) w.textContent = who;
    if (t && what) t.textContent = what;
  },
  reset() {
    for (const h of ["you", "api", "db", "worker", "target"]) this.set(h, null, "");
    this.target("The email service", "someone else's system");
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

/// Where the target's log is right now, so a scene only shows what it caused.
async function targetLogHead() {
  try {
    const res = await fetch("/control/mock/log?limit=1");
    const data = (await res.json())?.data ?? [];
    return data[0]?.seq ?? 0;
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

  /// A sentence for the room, with the mechanism underneath it for the engineers.
  say(text, { tone = "", mech = "" } = {}) {
    this.emit("beat", { text, tone, mech });
  }

  async save(ok) {
    if (!ok || this.replay || !state.recording) return;
    try {
      await fetch(`/control/recordings/${this.scene.id}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
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

// ─── the one renderer ────────────────────────────────────────────────────────

function apply(scene, ev) {
  const els = scene._els ?? {};

  switch (ev.type) {
    case "beat":
      addBeat(ev.t, ev.text, ev.tone, ev.mech);
      break;

    case "req":
      if (els.request) els.request.innerHTML = `<pre>${highlightJson(ev.body ?? {})}</pre>`;
      break;

    case "res":
      if (els.response) els.response.innerHTML = `<pre>${highlightJson(ev.body ?? {})}</pre>`;
      break;

    case "hop":
      journey.set(ev.hop, ev.state, ev.said);
      if (ev.flow) journey.flow(ev.flow);
      break;

    case "target-is":
      journey.target(ev.who, ev.what);
      break;

    case "said":
      addSaid(els, ev.entries);
      break;

    case "exec":
      if (els.exec) els.exec.innerHTML = execFacts(ev);
      break;

    case "tries":
      if (els.tries) els.tries.innerHTML = triesTable(ev.attempts);
      break;

    case "countdown":
      if (els.countdown) {
        const late = ev.remaining <= 0;
        els.countdown.innerHTML = `<div class="countdown ${late ? "late" : ""}">
          <div class="n">${late ? "now" : (ev.remaining / 1000).toFixed(1)}</div>
          <div class="cap">${esc(ev.label ?? "")}</div>
        </div>`;
      }
      break;

    case "panel":
      scene.panel?.(ev.key, ev.data, els);
      break;

    case "flag":
      if (ev.key === "worker") {
        setChip("chip-worker", ev.value === "running" ? "up" : "down", `worker ${ev.value}`);
      }
      break;
  }
}

function addBeat(t, html, tone = "", mech = "") {
  const story = $("#story");
  if (!story) return;
  story.querySelector(".empty")?.remove();
  const row = document.createElement("div");
  row.className = `beat ${tone}`;
  row.innerHTML = `<span class="t">+${t < 1000 ? `${t}ms` : `${(t / 1000).toFixed(2)}s`}</span>
    <span class="m">${html}${mech ? `<span class="mech">${mech}</span>` : ""}</span>`;
  story.appendChild(row);
  story.scrollTop = story.scrollHeight;
}

function addSaid(els, entries) {
  if (!els.said || !entries?.length) return;
  els.said.querySelector(".empty")?.remove();
  for (const e of entries) {
    const row = document.createElement("div");
    row.className = `said-row ${e.ok ? "" : "bad"}`;
    row.innerHTML = `<span class="mark">${e.ok ? "✓" : "!"}</span>
      <div>
        <div class="line">${esc(e.summary)}</div>
        <div class="sub">${esc(clock(e.at))} · ${esc(e.method)} ${esc(e.path)} → ${e.status}${
          e.idempotency_key ? ` · key ${esc(e.idempotency_key)}` : ""
        }${e.authenticated ? " · authenticated" : ""}</div>
      </div>`;
    els.said.appendChild(row);
  }
  els.said.scrollTop = els.said.scrollHeight;
}

function execFacts(ev) {
  const cls = { SUCCESS: "ok", FAILED: "bad", RUNNING: "run", RETRYING: "wait", CANCELLED: "bad" }[ev.status] ?? "wait";
  const human = {
    QUEUED: "waiting for a worker",
    PENDING: "waiting for its time",
    RUNNING: "being delivered",
    RETRYING: "backing off before the next try",
    SUCCESS: "delivered",
    FAILED: "gave up",
    CANCELLED: "cancelled",
  }[ev.status] ?? ev.status;

  return `<dl class="facts">
    <dt>right now</dt><dd><span class="pill ${cls}">${esc(human)}</span></dd>
    <dt>tries used</dt><dd>${ev.attempt_count ?? 0} of ${ev.max_attempts ?? "—"}</dd>
    ${ev.run_at ? `<dt>due at</dt><dd>${esc(clock(ev.run_at))}</dd>` : ""}
    ${ev.worker_id ? `<dt>worker</dt><dd class="id">${esc(short(ev.worker_id.replace("worker_", "")))}</dd>` : ""}
    <dt>execution</dt><dd class="id">${esc(short(ev.execution_id))}</dd>
  </dl>`;
}

function triesTable(attempts) {
  if (!attempts?.length) return `<div class="body"><span class="empty">Nothing tried yet.</span></div>`;
  const rows = attempts
    .map((a) => {
      const ok = a.status === "SUCCESS";
      const said = ok
        ? `answered ${a.output?.status_code ?? 200}`
        : `${a.error?.status_code ? `answered ${a.error.status_code}` : (a.error?.type ?? "failed").toLowerCase()}`;
      return `<tr>
        <td class="num">${a.attempt_number}</td>
        <td>${ok ? '<span class="pill ok">worked</span>' : '<span class="pill bad">failed</span>'}</td>
        <td class="num">${ms(a.duration_ms)}</td>
        <td class="num">${a.gap_ms != null ? ms(a.gap_ms) : "—"}<span class="sub">${
          a.gap_ms != null ? "waited before this try" : "went straight out"
        }</span></td>
        <td>${esc(said)}<span class="sub">${esc(
          String(ok ? "" : a.error?.message ?? "").slice(0, 90),
        )}</span></td>
      </tr>`;
    })
    .join("");
  return `<table>
    <thead><tr><th>try</th><th></th><th>took</th><th>waited</th><th>the other side</th></tr></thead>
    <tbody>${rows}</tbody></table>`;
}

// ─── watching a real execution ───────────────────────────────────────────────

// Polls Invokr the way an operator would, and emits only when something
// actually changed. Also tails the target's own log so the room sees both
// accounts of the same delivery.
async function watchExecution(run, executionId, { ws = "a", timeout = 90000, targetSince = 0, who = "" } = {}) {
  const started = performance.now();
  let lastStatus = null;
  let seenAttempts = 0;
  let claimedAttempt = 0;
  let announcedRetry = 0;
  let logSeq = targetSince;

  const tailTarget = async () => {
    try {
      const res = await fetch(`/control/mock/log?since=${logSeq}&limit=20`);
      const entries = ((await res.json())?.data ?? []).slice().reverse();
      if (entries.length) {
        logSeq = Math.max(logSeq, ...entries.map((e) => e.seq));
        run.emit("said", { entries });
        const last = entries[entries.length - 1];
        run.emit("hop", {
          hop: "target",
          state: last.ok ? "done" : "fail",
          said: last.ok ? "handled it" : "refused it",
        });
      }
    } catch {
      /* the target's log is a nicety, never a dependency */
    }
  };

  while (performance.now() - started < timeout && !run.cancelled) {
    const res = await api("GET", `/v1/executions/${executionId}`, { ws });
    const exec = res.body?.data;

    if (exec) {
      // Held until this poll's attempt rows are out, so the failure is on
      // screen before the backoff that follows from it.
      //
      // Keyed on attempt_count, not on a status change: a try that takes 5ms is
      // RETRYING again before the next poll, so RUNNING is never observed and
      // the second backoff — the interesting one, since it is the doubling —
      // would otherwise go unmentioned.
      let pendingRetry = null;
      if (exec.status === "RETRYING" && exec.run_at && exec.attempt_count > announcedRetry) {
        announcedRetry = exec.attempt_count;
        pendingRetry = exec;
      }
      if (exec.status !== lastStatus) {
        lastStatus = exec.status;
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
      // edge to watch — RUNNING is usually over before the next poll lands.
      // Called from the attempts loop too: an attempt row can surface in the
      // same poll whose execution snapshot was read a moment too early, and the
      // pick-up must never be narrated after the try it produced.
      const claim = (n) => {
        if (n <= claimedAttempt) return;
        claimedAttempt = n;
        run.emit("hop", { hop: "db", state: "done", said: "handed it over", flow: "db" });
        run.emit("hop", {
          hop: "worker",
          state: "active",
          said: exec.worker_id ? exec.worker_id.replace("worker_", "").slice(0, 8) : "picked it up",
        });
        run.say(`A worker <b>picked it up</b>${who ? ` for ${esc(who)}` : ""}.`, {
          tone: "act",
          mech: "Exactly one worker can hold it — SELECT FOR UPDATE SKIP LOCKED, inside the transaction.",
        });
      };
      claim(exec.attempt_count);

      const attempts = (await api("GET", `/v1/executions/${executionId}/attempts`, { ws })).body?.data ?? [];
      if (attempts.length !== seenAttempts) {
        const ordered = [...attempts].sort((a, b) => a.attempt_number - b.attempt_number);
        // The real gap between one try finishing and the next starting: the
        // backoff that happened, not the one the policy asked for.
        for (let i = 1; i < ordered.length; i++) {
          ordered[i].gap_ms = new Date(ordered[i].started_at) - new Date(ordered[i - 1].completed_at);
        }
        for (const a of ordered.slice(seenAttempts)) {
          claim(a.attempt_number);
          const ok = a.status === "SUCCESS";
          run.emit("hop", {
            hop: "worker",
            state: "done",
            said: `delivered in ${ms(a.duration_ms)}`,
            flow: "worker",
          });
          if (ok) {
            run.say(
              `The other side answered <b>${a.output?.status_code ?? 200} OK</b> in ${ms(a.duration_ms)}.`,
              { tone: "ok", mech: `Try ${a.attempt_number}, recorded with its duration and its response body.` },
            );
          } else {
            run.say(
              `Try ${a.attempt_number} <b>failed</b> — ${esc(
                a.error?.status_code ? `it answered ${a.error.status_code}` : (a.error?.type ?? "no answer").toLowerCase(),
              )}.`,
              { tone: "bad", mech: "The failure is a row, not a log line: kept with its body and its duration." },
            );
          }
        }
        seenAttempts = attempts.length;
        run.emit("tries", { attempts: ordered });
      }

      await tailTarget();

      if (pendingRetry) {
        const waitMs = new Date(pendingRetry.run_at) - Date.now();
        run.emit("hop", { hop: "worker", state: "done", said: `waiting ${ms(Math.max(0, waitMs))}` });
        run.say(
          `Invokr will try again at <b>${clock(pendingRetry.run_at)}</b> — in about ${ms(Math.max(0, waitMs))}.`,
          {
            tone: "warn",
            mech: "The worker wrote run_at = now() + backoff and let go of the row. Nothing is sleeping on it.",
          },
        );
      }

      if (["SUCCESS", "FAILED", "CANCELLED"].includes(exec.status)) {
        await tailTarget();
        return exec;
      }
    }
    await sleep(170);
  }
  return null;
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
    const res = await fetch("/control/status");
    const s = await res.json();
    state.status = s;
    setChip("chip-api", s.api ? "up" : "down", s.api ? "Invokr" : "Invokr is down");
    setChip("chip-worker", s.worker.state === "running" ? "up" : "down", `worker ${s.worker.state}`);
    setChip("chip-target", s.mock ? "up" : "down", s.mock ? "email service" : "email service is down");
    const t = s.transports;
    setChip(
      "chip-transports",
      t.kafka && t.redis ? "up" : t.kafka || t.redis ? "warn" : "",
      `Kafka ${t.kafka ? "on" : "off"} · Redis ${t.redis ? "on" : "off"}`,
    );
    $("#foot-note").textContent = s.provisioned
      ? `teams: ${Object.values(s.provisioned.workspaces).map((w) => w.name).join(" · ")}`
      : "not set up yet — live scenes unavailable";
  } catch {
    setChip("chip-api", "down", "demo server is down");
  }
}

// ─── the pieces a scene is built from ────────────────────────────────────────

const storyCard = () =>
  card("What happened", "", `<div id="story"><span class="empty">Nothing yet — press the button.</span></div>`, {
    tight: true,
    scroll: true,
  });

const saidCard = (who) =>
  card(`What ${who} says`, "printed on its own terminal", `<div id="said"><span class="empty">Nothing received yet.</span></div>`, {
    tight: true,
    scroll: true,
  });

const triesCard = () =>
  card("Every try", "real durations", `<div id="p-tries"><span class="empty">Nothing tried yet.</span></div>`, {
    tight: true,
  });

const jobCard = () => card("This job", "", `<div id="p-exec"><span class="empty">No job yet.</span></div>`);

const rawCard = () => `
  <details class="raw">
    <summary>Show the raw request and response</summary>
    <div class="panes">
      <div class="pane"><h4>what we sent to Invokr</h4><div id="p-request"><span class="empty">Not sent yet.</span></div></div>
      <div class="pane"><h4>what Invokr sent back</h4><div id="p-response"><span class="empty">Waiting.</span></div></div>
    </div>
  </details>`;

const TEMPLATE_LEGEND = `<div class="legend">
  <span><code class="tok-input">{{input.*}}</code> from this job</span>
  <span><code class="tok-config">{{config.*}}</code> shared settings</span>
  <span><code class="tok-secret">{{secret.*}}</code> the encrypted store</span>
  <span><code class="tok-exec">{{execution.*}}</code> this delivery</span>
</div>`;

const key = (p) => `${p}-${Date.now().toString(36)}`;

// ─── the scenes ──────────────────────────────────────────────────────────────

const scenes = [
  {
    id: "register",
    n: 1,
    group: "core",
    title: "Tell Invokr where to deliver",
    lede: "An endpoint is a row in a table: where to send it, what to put in it, how hard to try. Nothing of yours runs inside Invokr.",
    watch:
      "The instructions hold <b>references</b>, not values. <code>{{secret.email_api_key}}</code> is looked up inside the worker at the last moment — and if you ask Invokr for that secret, it will not give it to you.",
    fields: [],
    action: "Save these instructions",
    layout: () => `
      <div class="two-up">
        ${card("The delivery instructions", "sent to Invokr", `<div id="p-request"><span class="empty">Not sent yet.</span></div>${TEMPLATE_LEGEND}`)}
        ${storyCard()}
      </div>
      <div class="two-up" style="margin-top:20px">
        ${card("Asking Invokr for the secret", "GET /v1/secrets/email_api_key", `<div id="p-response"><span class="empty">Not asked yet.</span></div>`)}
        ${card("Why this matters", "", `<p style="margin:0;color:var(--ink-soft)">Everyone's hand-rolled tracker ends up with credentials in a config file, in git. Here the value is encrypted at rest, resolved in memory at the moment of delivery, and never returned by the API — not even to you.</p>`)}
      </div>`,
    async run(run) {
      const spec = {
        name: "send-welcome-email",
        type: "HTTP",
        payload_spec: "order-input",
        config: "email-service",
        spec: {
          url: "{{config.api_base_url}}/emails/welcome",
          method: "POST",
          headers: {
            Authorization: "Bearer {{secret.email_api_key}}",
            "Content-Type": "application/json",
          },
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

      run.emit("hop", { hop: "you", state: "active", said: "saving instructions" });
      run.emit("req", { method: "POST", path: "/v1/endpoints", body: spec });

      const exists = (await api("GET", `/v1/endpoints/${spec.name}`)).ok;
      const { name, ...rest } = spec;
      const res = exists
        ? await api("PUT", `/v1/endpoints/${spec.name}`, { body: rest })
        : await api("POST", "/v1/endpoints", { body: spec });

      run.emit("hop", { hop: "api", state: "done", said: `${res.status} ok` });
      run.emit("hop", { hop: "db", state: "done", said: "one row written" });
      run.say(
        exists
          ? "These instructions were already here, so Invokr <b>updated them in place</b>."
          : "Invokr <b>wrote them down</b>.",
        { tone: "act", mech: "One row in this team's own schema. No deploy, no restart, nothing to review." },
      );

      await sleep(450);
      const secret = await api("GET", "/v1/secrets/email_api_key");
      run.emit("res", { status: secret.status, body: secret.body });
      const leaked = JSON.stringify(secret.body ?? {}).includes("value");
      run.say(
        leaked
          ? "The API returned something that looks like a value — check this."
          : "We asked for the API key. Invokr gave back <b>its name and when it changed</b>, and nothing else.",
        { tone: leaked ? "bad" : "ok", mech: "Write-only: AES-256-GCM at rest, decrypted in the worker, never returned." },
      );
      return res.ok;
    },
  },

  {
    id: "fire-now",
    n: 2,
    group: "core",
    title: "Send it now",
    lede: "Type a real name and a real order. Invokr will actually call the email service, and the email service will tell you what it did.",
    watch:
      "Two accounts of the same delivery: <b>Invokr's</b> on the left, <b>the email service's</b> on the right. They should agree, down to the de-duplication key.",
    fields: [
      { key: "customer", label: "Who is it for?", value: "Priya Sharma", width: 190 },
      { key: "email", label: "Their email", value: "priya@example.com", width: 220 },
      { key: "order_id", label: "About which order", value: "order-1234", width: 160 },
    ],
    action: "Send it now",
    said: "the email service",
    layout: () => `
      <div class="two-up">${storyCard()}${saidCard("the email service")}</div>
      <div class="two-up" style="margin-top:20px">${jobCard()}${triesCard()}</div>
      ${rawCard()}`,
    async run(run, v) {
      const since = await targetLogHead();
      const body = {
        endpoint: "send-welcome-email",
        trigger: "IMMEDIATE",
        idempotency_key: key(`${v.order_id}-welcome`),
        input: { order_id: v.order_id, customer: v.customer, email: v.email },
      };

      run.emit("target-is", { who: "The email service", what: "someone else's system" });
      run.emit("hop", { hop: "you", state: "active", said: "asking", flow: "you" });
      run.emit("req", { method: "POST", path: "/v1/jobs", body });
      run.say(`You asked Invokr to email <b>${esc(v.customer)}</b> about ${esc(v.order_id)}.`);

      const t0 = performance.now();
      const res = await api("POST", "/v1/jobs", { body });
      run.emit("res", { status: res.status, body: res.body });
      if (!res.ok) {
        run.say(`Invokr refused it — <b>${res.status}</b>.`, { tone: "bad" });
        return false;
      }

      const exec = res.body.data.execution;
      run.emit("hop", { hop: "you", state: "done", said: "asked" });
      run.emit("hop", { hop: "api", state: "done", said: `said yes in ${ms(performance.now() - t0)}`, flow: "api" });
      run.emit("hop", { hop: "db", state: "active", said: "written down · queued" });
      run.say(`Invokr said <b>yes</b> in ${ms(performance.now() - t0)}, and it is already safe.`, {
        tone: "act",
        mech: "The job and its first delivery were committed to PostgreSQL before you got that answer back.",
      });

      const done = await watchExecution(run, exec.execution_id, { targetSince: since });
      if (done?.status === "SUCCESS") {
        run.emit("hop", { hop: "db", state: "done", said: "all of it recorded" });
        run.say("Done — and the email service saw the de-duplication key, so a repeat would be its own to ignore.", {
          tone: "ok",
          mech: "Every HTTP delivery carries x-invokr-idempotency-key.",
        });
      }
      return done?.status === "SUCCESS";
    },
  },

  {
    id: "fire-later",
    n: 3,
    group: "core",
    title: "Send it later — then pull the plug",
    lede: "Ask for it in fifteen seconds. Then kill the worker mid-wait and watch it land anyway.",
    watch:
      "Nothing is holding a timer. <b>The row is the timer.</b> Kill the worker, start another one, and the job still goes out at the time you asked for.",
    fields: [
      { key: "customer", label: "Who is it for?", value: "Arjun Mehta", width: 190 },
      { key: "email", label: "Their email", value: "arjun@example.com", width: 220 },
      { key: "order_id", label: "About which order", value: "order-5567", width: 160 },
      { key: "seconds", label: "In how many seconds?", value: "15", width: 90, hint: "10–60 works best" },
    ],
    action: "Schedule it",
    extras: [
      { id: "kill", label: "Kill the worker", danger: true },
      { id: "start", label: "Start a worker" },
    ],
    layout: () => `
      <div class="two-up">
        ${card("Time until it should go out", "", `<div id="p-countdown"><span class="empty">Not scheduled yet.</span></div>`)}
        ${storyCard()}
      </div>
      <div class="two-up" style="margin-top:20px">${jobCard()}${saidCard("the email service")}</div>
      ${rawCard()}`,
    async onExtra(id, run) {
      if (id === "kill") {
        const res = await control("/worker/kill");
        const killed = res.body?.ok;
        run?.emit("flag", { key: "worker", value: "killed" });
        run?.emit("hop", { hop: "worker", state: "gone", said: killed ? `killed (pid ${res.body.pid})` : "not running" });
        run?.say(
          killed
            ? `We just <b>killed the worker</b> outright — no warning, no graceful shutdown.`
            : "There was no worker running to kill.",
          {
            tone: killed ? "bad" : "warn",
            mech: killed ? "SIGKILL. Any transaction it was holding is aborted by PostgreSQL." : "",
          },
        );
        refreshStatus();
        return;
      }
      if (id === "start") {
        const res = await control("/worker/start");
        run?.emit("flag", { key: "worker", value: "running" });
        run?.emit("hop", { hop: "worker", state: "active", said: `new worker (pid ${res.body?.pid ?? "?"})` });
        run?.say("A <b>different worker</b> is up now. It knows nothing about what came before.", {
          tone: "act",
          mech: "It just polls the same table. There is no handover, because there is no state to hand over.",
        });
        refreshStatus();
      }
    },
    async run(run, v) {
      const since = await targetLogHead();
      const seconds = Math.max(5, Math.min(120, Number(v.seconds) || 15));
      const runAt = new Date(Date.now() + seconds * 1000);
      const body = {
        endpoint: "send-welcome-email",
        trigger: "DELAYED",
        idempotency_key: key(`${v.order_id}-later`),
        run_at: runAt.toISOString(),
        input: { order_id: v.order_id, customer: v.customer, email: v.email },
      };

      run.emit("hop", { hop: "you", state: "active", said: "asking for later" });
      run.emit("req", { method: "POST", path: "/v1/jobs", body });
      run.say(`You asked for this one at <b>${clock(runAt)}</b> — ${seconds} seconds from now.`);

      const res = await api("POST", "/v1/jobs", { body });
      run.emit("res", { status: res.status, body: res.body });
      if (!res.ok) {
        run.say(`Invokr refused it — <b>${res.status}</b>.`, { tone: "bad" });
        return false;
      }

      run.emit("hop", { hop: "you", state: "done", said: "asked" });
      run.emit("hop", { hop: "api", state: "done", said: "accepted" });
      run.emit("hop", { hop: "db", state: "active", said: `waiting until ${clock(runAt)}` });
      run.say("It is a row with a time on it. Nothing is counting down anywhere.", {
        tone: "act",
        mech: "No in-memory timer, no cron entry, no scheduler process. Just run_at in a column.",
      });
      run.say("Now kill the worker.", { tone: "warn" });

      // Emitted, not computed on screen, so replay reproduces the countdown.
      const deadline = runAt.getTime();
      const ticker = setInterval(() => {
        run.emit("countdown", {
          remaining: Math.max(0, deadline - Date.now()),
          label: `due at ${clock(runAt)}`,
        });
      }, 500);

      const done = await watchExecution(run, res.body.data.execution.execution_id, {
        timeout: (seconds + 90) * 1000,
        targetSince: since,
      });
      clearInterval(ticker);
      run.emit("countdown", { remaining: 0, label: `went out at ${clock(Date.now())}` });

      if (done?.status === "SUCCESS") {
        run.emit("hop", { hop: "db", state: "done", said: "fired on time" });
        run.say("It went out at the time you asked for — by a worker that did not exist when you asked.", {
          tone: "ok",
        });
      }
      return done?.status === "SUCCESS";
    },
  },

  {
    id: "it-fails",
    n: 4,
    group: "core",
    title: "When the other side breaks",
    lede: "The payment processor is going to fail twice before it works. This is the part a hand-rolled tracker can never show you afterwards.",
    watch:
      "Three tries, each kept as a row: how long it took, what came back, and <b>how long Invokr actually waited</b> before trying again — not what the policy asked for. There is ±25% jitter, so the numbers are never round.",
    fields: [
      { key: "order_id", label: "Which order", value: "order-9931", width: 160 },
      { key: "amount", label: "How much", value: "₹1,499", width: 120 },
    ],
    action: "Take the payment",
    extras: [{ id: "prearm", label: "Start scene 5's schedule now" }],
    layout: () => `
      <div class="banner info">This endpoint retries up to <b>3 times</b>, backing off exponentially from 2 seconds, with ±25% jitter.</div>
      <div class="two-up">${storyCard()}${saidCard("the payment processor")}</div>
      <div class="two-up" style="margin-top:20px">${jobCard()}${triesCard()}</div>
      ${rawCard()}`,
    async onExtra(id, run) {
      if (id !== "prearm") return;
      const started = await startCron();
      run?.say(
        started
          ? "Scene 5's every-minute schedule is running now, so a tick will have landed by the time you get there."
          : "Could not start that schedule.",
        { tone: started ? "ok" : "bad" },
      );
    },
    async run(run, v) {
      await control("/mock/reset"); // the processor's failure counter is global
      const since = await targetLogHead();

      const body = {
        endpoint: "charge-webhook",
        trigger: "IMMEDIATE",
        idempotency_key: key(`${v.order_id}-charge`),
        input: { order_id: v.order_id, amount: v.amount },
      };

      run.emit("target-is", { who: "The payment processor", what: "having a bad day" });
      run.emit("hop", { hop: "you", state: "active", said: "asking" });
      run.emit("req", { method: "POST", path: "/v1/jobs", body });
      run.say(`You asked Invokr to take <b>${esc(v.amount)}</b> for ${esc(v.order_id)}.`);

      const res = await api("POST", "/v1/jobs", { body });
      run.emit("res", { status: res.status, body: res.body });
      if (!res.ok) return false;

      run.emit("hop", { hop: "you", state: "done", said: "asked" });
      run.emit("hop", { hop: "api", state: "done", said: "accepted" });
      run.emit("hop", { hop: "db", state: "active", said: "queued" });

      const done = await watchExecution(run, res.body.data.execution.execution_id, {
        timeout: 120000,
        targetSince: since,
      });

      if (done?.status === "SUCCESS") {
        run.emit("hop", { hop: "db", state: "done", said: "every try recorded" });
        run.say("It went through on the third try — and the two failures are still on the record.", {
          tone: "ok",
          mech: "Nobody had to be watching. Nobody had to re-run anything by hand.",
        });
        run.say("Look at the processor's own log: all three tries carry <b>the same key</b>.", {
          tone: "act",
          mech:
            "That is how the other side avoids charging twice if our first request did land and only the answer " +
            "was lost. Invokr delivers at least once; the key is what makes that safe.",
        });
      } else {
        run.say("It never got through. That is also recorded, try by try.", { tone: "bad" });
      }
      return done?.status === "SUCCESS";
    },
  },

  {
    id: "fire-repeatedly",
    n: 5,
    group: "core",
    title: "Every minute, forever",
    lede: "A recurring job, with no scheduler process anywhere. PostgreSQL itself writes the row when it's due.",
    watch:
      "<b>pg_cron puts the work in the queue</b> from inside the database, on schedule, whether or not a worker happens to be up. Cancel it and the schedule goes with it, in the same transaction.",
    fields: [],
    action: "Start the every-minute job",
    extras: [{ id: "cancel", label: "Cancel it", danger: true }],
    layout: () => `
      <div class="banner warn">A minute is the finest schedule pg_cron offers, so the first tick can take up to 60 seconds. Scene 4 has a button to start this early.</div>
      <div class="two-up">
        ${card("Next tick", "", `<div id="p-countdown"><span class="empty">Not started.</span></div>`)}
        ${storyCard()}
      </div>
      <div class="two-up" style="margin-top:20px">
        ${card("Ticks so far", "written by PostgreSQL", `<div id="p-cron"><span class="empty">None yet.</span></div>`, { tight: true })}
        ${saidCard("the service being poked")}
      </div>`,
    async onExtra(id, run) {
      if (id !== "cancel") return;
      // Looked up rather than remembered: a reloaded page has no state, but the
      // schedule is still out there firing every minute.
      const job = state.cronJob ?? (await findActiveCron());
      if (!job) return run?.say("Nothing is scheduled right now.", { tone: "warn" });
      const res = await api("POST", `/v1/jobs/${job}/cancel`);
      run?.say(res.ok ? "<b>Cancelled.</b> It will not fire again." : "That cancel did not take.", {
        tone: res.ok ? "ok" : "bad",
        mech: res.ok ? "The pg_cron entry is removed in the same transaction that retires the job." : "",
      });
      state.cronJob = null;
    },
    panel(k, data, els) {
      if (k !== "cron" || !els.cron) return;
      if (!data.executions.length) {
        els.cron.innerHTML = `<div class="body"><span class="empty">Waiting for the first tick…</span></div>`;
        return;
      }
      els.cron.innerHTML = `<table>
        <thead><tr><th>tick</th><th>when</th><th></th><th>took</th></tr></thead>
        <tbody>${data.executions
          .map(
            (e, i) => `<tr>
              <td class="num">${data.executions.length - i}</td>
              <td>${esc(clock(e.created_at))}</td>
              <td>${
                e.status === "SUCCESS"
                  ? '<span class="pill ok">worked</span>'
                  : e.status === "FAILED"
                    ? '<span class="pill bad">failed</span>'
                    : `<span class="pill wait">${esc(e.status.toLowerCase())}</span>`
              }</td>
              <td class="num">${e.duration_ms != null ? ms(e.duration_ms) : "—"}</td>
            </tr>`,
          )
          .join("")}</tbody></table>`;
    },
    async run(run) {
      const since = await targetLogHead();
      run.emit("target-is", { who: "The health sweep", what: "an internal service" });

      let jobId = state.cronJob;
      if (jobId) {
        run.say("Using the schedule that was started back in scene 4.", { tone: "act" });
      } else {
        jobId = await startCron();
        if (!jobId) {
          run.say("Could not start the schedule.", { tone: "bad" });
          return false;
        }
        run.say("Invokr handed the schedule <b>to PostgreSQL</b> the moment the job was created.", {
          tone: "act",
          mech: "pg_cron owns it now. No scheduler process exists to fall over.",
        });
      }

      const job = (await api("GET", `/v1/jobs/${jobId}`)).body?.data;
      run.emit("res", { status: 200, body: job });
      run.emit("hop", { hop: "db", state: "active", said: "every minute" });

      const next = job?.next_run_at ? new Date(job.next_run_at).getTime() : Date.now() + 60000;
      const started = performance.now();
      let seen = 0;
      let logSeq = since;

      while (performance.now() - started < 75000 && !run.cancelled) {
        run.emit("countdown", { remaining: Math.max(0, next - Date.now()), label: "until the next tick" });

        const execs = (await api("GET", `/v1/jobs/${jobId}/executions?limit=10`)).body?.data ?? [];
        if (execs.length !== seen) {
          run.emit("panel", { key: "cron", data: { executions: execs } });
          if (execs.length > seen) {
            run.emit("hop", { hop: "worker", state: "active", said: `tick ${execs.length}` });
            run.say("A tick just appeared in the queue — <b>nobody put it there</b>.", {
              tone: "act",
              mech: "PostgreSQL wrote that row itself, then a worker claimed it like any other.",
            });
          }
          seen = execs.length;
        }

        try {
          const res = await fetch(`/control/mock/log?since=${logSeq}&limit=10`);
          const entries = ((await res.json())?.data ?? []).slice().reverse();
          if (entries.length) {
            logSeq = Math.max(logSeq, ...entries.map((e) => e.seq));
            run.emit("said", { entries });
            run.emit("hop", { hop: "target", state: "done", said: "swept" });
          }
        } catch {}

        if (seen >= 1) break;
        await sleep(900);
      }

      run.say(seen > 0 ? "Remember to cancel it — otherwise it keeps going, every minute." : "No tick landed in the time we waited.", {
        tone: seen > 0 ? "warn" : "warn",
      });
      return seen > 0;
    },
  },

  {
    id: "any-transport",
    n: 6,
    group: "overflow",
    title: "Somewhere other than HTTP",
    lede: "The same job, delivered to a Kafka topic or a Redis Stream instead. Only the endpoint's address changes.",
    watch: "Same request, same retry policy, same history to debug from. The transport is a detail of the endpoint, not of your code.",
    fields: [{ key: "order_id", label: "Which order", value: "order-4410", width: 160 }],
    action: "Send it three ways",
    layout: () => {
      const t = state.status?.transports ?? {};
      const missing = [!t.kafka && "Kafka", !t.redis && "Redis"].filter(Boolean);
      return `
      ${
        missing.length
          ? `<div class="banner warn">${missing.join(" and ")} ${missing.length > 1 ? "are" : "is"} not running here,
             so ${missing.length > 1 ? "those two" : "that one"} will be skipped — you will see the instructions instead of a
             delivery. To fire them for real: <code>docker compose --profile kafka --profile redis up -d</code>, then
             <code>INVOKR_DEMO_WORKER_FEATURES=kafka,redis-stream just demo</code>.</div>`
          : ""
      }
      <div class="transports">
        ${["HTTP", "KAFKA", "REDIS_STREAM"]
          .map((k) => card(k.replace("_", " "), "", `<div id="p-t-${k}"><span class="empty">Not sent.</span></div>`))
          .join("")}
      </div>
      <div class="two-up" style="margin-top:20px">${storyCard()}${triesCard()}</div>`;
    },
    panel(k, data) {
      if (k !== "transport") return;
      const box = document.getElementById(`p-t-${data.type}`);
      if (!box) return;
      box.innerHTML = data.skipped
        ? `<p style="margin:0 0 10px;color:var(--muted);font-size:14px">Not running here. These are the only instructions that differ:</p>
           <pre>${highlightJson(data.spec ?? {})}</pre>`
        : `<dl class="facts">
             <dt>endpoint</dt><dd class="id">${esc(data.endpoint)}</dd>
             <dt>result</dt><dd><span class="pill ${data.ok ? "ok" : "bad"}">${esc(data.status)}</span></dd>
             ${data.detail ? `<dt>detail</dt><dd>${esc(data.detail)}</dd>` : ""}
           </dl>`;
    },
    async run(run, v) {
      const probes = state.status?.transports ?? {};
      const targets = [
        { type: "HTTP", endpoint: "send-welcome-email", up: true },
        { type: "KAFKA", endpoint: "order-events-kafka", up: !!probes.kafka },
        { type: "REDIS_STREAM", endpoint: "order-events-redis", up: !!probes.redis },
      ];

      let ok = true;
      for (const t of targets) {
        // Firing at a broker that isn't there only proves the broker isn't
        // there. Show the instructions instead — that is the whole claim anyway.
        if (!t.up) {
          const ep = (await api("GET", `/v1/endpoints/${t.endpoint}`)).body?.data;
          run.emit("panel", { key: "transport", data: { type: t.type, skipped: true, spec: ep?.spec ?? {} } });
          run.say(`Skipped <b>${t.type.replace("_", " ")}</b> — that broker is not running here.`, { tone: "warn" });
          continue;
        }

        run.emit("target-is", { who: `The ${t.type.replace("_", " ").toLowerCase()} side`, what: "same job, different pipe" });
        const body = {
          endpoint: t.endpoint,
          trigger: "IMMEDIATE",
          idempotency_key: key(`${v.order_id}-${t.type.toLowerCase()}`),
          input: { order_id: v.order_id, customer: "Priya Sharma", email: "priya@example.com" },
          max_attempts: 1,
        };
        run.emit("req", { method: "POST", path: "/v1/jobs", body });
        const res = await api("POST", "/v1/jobs", { body });
        if (!res.ok) {
          run.emit("panel", { key: "transport", data: { type: t.type, endpoint: t.endpoint, ok: false, status: `refused ${res.status}` } });
          ok = false;
          continue;
        }

        const execId = res.body.data.execution.execution_id;
        const done = await watchExecution(run, execId, { timeout: 30000 });
        const attempts = (await api("GET", `/v1/executions/${execId}/attempts`)).body?.data ?? [];
        const err = attempts.find((a) => a.error)?.error;
        const delivered = done?.status === "SUCCESS";
        ok = ok && delivered;
        run.emit("panel", {
          key: "transport",
          data: {
            type: t.type,
            endpoint: t.endpoint,
            ok: delivered,
            status: delivered ? "delivered" : (done?.status ?? "timed out").toLowerCase(),
            detail: delivered ? `in ${ms(attempts[0]?.duration_ms)}` : `${err?.type ?? ""} ${String(err?.message ?? "").slice(0, 80)}`,
          },
        });
      }
      return ok;
    },
  },

  {
    id: "two-tenants",
    n: 7,
    group: "overflow",
    title: "Two teams, one name",
    lede: "Payments and Risk both have an endpoint called send-welcome-email. They are not the same endpoint, and they cannot see each other.",
    watch:
      "Same name, two <b>separate schemas</b> in the same database. The connection's search path decides which one you are talking to — there is no shared table to leak through.",
    fields: [
      { key: "customer", label: "Who is it for?", value: "Neha Rao", width: 190 },
      { key: "order_id", label: "About which order", value: "order-7782", width: 160 },
    ],
    action: "Send it from both teams",
    layout: () => `
      <div class="tenants">
        ${card("Payments", "", `<div id="p-ws-a"><span class="empty">Not sent.</span></div>`)}
        ${card("Risk", "", `<div id="p-ws-b"><span class="empty">Not sent.</span></div>`)}
      </div>
      <div class="two-up" style="margin-top:20px">${storyCard()}${saidCard("the email service")}</div>`,
    panel(k, data) {
      if (k !== "tenant") return;
      const box = document.getElementById(`p-ws-${data.ws}`);
      if (!box) return;
      box.innerHTML = `<dl class="facts">
        <dt>lives in</dt><dd class="id">${esc(data.schema)}</dd>
        <dt>endpoint</dt><dd class="id">send-welcome-email</dd>
        <dt>job</dt><dd class="id">${esc(short(data.job_id))}</dd>
        <dt>result</dt><dd><span class="pill ${data.status === "SUCCESS" ? "ok" : "bad"}">${esc(
          data.status === "SUCCESS" ? "delivered" : data.status.toLowerCase(),
        )}</span></dd>
        <dt>sent as</dt><dd>${esc(data.sender ?? "—")}</dd>
      </dl>`;
    },
    async run(run, v) {
      const teams = state.status?.provisioned?.workspaces ?? {};
      const email = `${String(v.customer).split(" ")[0].toLowerCase()}@example.com`;
      let ok = true;

      for (const wsKey of ["a", "b"]) {
        // A fresh head per team: otherwise the second watch replays the first
        // team's line out of the target's log.
        const since = await targetLogHead();
        const team = teams[wsKey];
        const body = {
          endpoint: "send-welcome-email",
          trigger: "IMMEDIATE",
          idempotency_key: key(`${v.order_id}-${wsKey}`),
          input: { order_id: v.order_id, customer: v.customer, email },
        };
        run.emit("req", { method: "POST", path: "/v1/jobs", body });
        const res = await api("POST", "/v1/jobs", { body, ws: wsKey });
        if (!res.ok) {
          ok = false;
          continue;
        }
        run.say(`Sent from <b>${esc(team?.name ?? wsKey)}</b> — same endpoint name, different schema.`, { tone: "act" });

        const execId = res.body.data.execution.execution_id;
        const done = await watchExecution(run, execId, {
          ws: wsKey,
          timeout: 30000,
          targetSince: since,
          who: team?.name,
        });
        const attempts = (await api("GET", `/v1/executions/${execId}/attempts`, { ws: wsKey })).body?.data ?? [];
        let sender = null;
        try {
          sender = JSON.parse(attempts[0]?.output?.body ?? "{}")?.subject ?? null;
        } catch {}
        ok = ok && done?.status === "SUCCESS";
        run.emit("panel", {
          key: "tenant",
          data: {
            ws: wsKey,
            schema: team?.schema_name ?? "—",
            job_id: res.body.data.job_id,
            status: done?.status ?? "timed out",
            sender: team ? `noreply@${team.slug}.invokr.internal` : null,
          },
        });
      }
      run.say("Same name, same input — and the other side can tell them apart by who sent them.", {
        tone: ok ? "ok" : "warn",
        mech: "Each team's sender came out of its own config, in its own schema. Nothing in either query path crosses over.",
      });
      return ok;
    },
  },

  {
    id: "handoff",
    n: 8,
    group: "overflow",
    title: "Hand over to the real thing",
    lede: "Everything you just watched was read back out of ordinary rows. Here is the tool the people on call actually use.",
    watch: "This page is a demo. The dashboard is the product — same data, no narration.",
    fields: [],
    layout: () => {
      const url = state.status?.dashboardUrl || "";
      return `<div class="handoff">
        ${
          url
            ? `<div class="banner info">Dashboard: <a href="${esc(url)}" target="_blank" rel="noopener">${esc(url)}</a></div>`
            : `<div class="banner warn">No dashboard URL is configured. Build it with <code>just dashboard-build</code>, run the
               API with <code>INVOKR_MODE=both</code>, and set <code>INVOKR_DEMO_DASHBOARD_URL</code> before <code>just demo</code>.</div>`
        }
        <p class="lede">Three things to point at, in this order:</p>
        <ol>
          <li><b>The list of everything that ran.</b> Including this session's failures.</li>
          <li><b>One delivery's attempts.</b> The same rows this page has been reading — durations, responses, errors.</li>
          <li><b>Fire one by hand.</b> Which is how most "can you just re-run it" requests get answered.</li>
        </ol>
        <p style="color:var(--muted)">Then stop talking and take questions.</p>
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

// ─── the shell ───────────────────────────────────────────────────────────────

function renderRail() {
  const rail = $("#rail");
  const link = (s) =>
    `<button class="scene-link ${s.group} ${state.current === scenes.indexOf(s) ? "current" : ""} ${
      state.ran.has(s.id) ? "ran" : ""
    }" data-i="${scenes.indexOf(s)}"><span class="n">${s.n}</span><span>${esc(s.title)}</span></button>`;

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

function collectEls(scene) {
  scene._els = {
    story: $("#story"),
    said: $("#said"),
    exec: $("#p-exec"),
    tries: $("#p-tries"),
    countdown: $("#p-countdown"),
    cron: $("#p-cron"),
    request: $("#p-request"),
    response: $("#p-response"),
  };
}

function mountScene(i) {
  if (state.run) state.run.cancelled = true;
  state.run = null;
  state.current = Math.max(0, Math.min(scenes.length - 1, i));
  const scene = scenes[state.current];
  const v = valuesFor(scene);

  journey.reset();

  const fields = (scene.fields ?? [])
    .map(
      (f) => `<div class="field">
        <label for="f-${f.key}">${esc(f.label)}</label>
        <input id="f-${f.key}" value="${esc(v[f.key] ?? f.value)}" style="--w:${f.width ?? 190}px"
               ${state.replay ? "disabled" : ""} />
        ${f.hint ? `<span class="hint">${esc(f.hint)}</span>` : ""}
      </div>`,
    )
    .join("");

  const compose = scene.action
    ? `<div class="compose">
         ${fields ? `<h2>change any of this — it is really sent</h2><div class="fields">${fields}</div>` : ""}
         <div class="actions">
           <button class="primary" id="act-run">${state.replay ? "Play the recording" : esc(scene.action)}</button>
           ${(scene.extras ?? [])
             .map(
               (a) =>
                 `<button class="${a.danger ? "danger" : ""}" data-act="${esc(a.id)}" ${
                   state.replay ? "disabled" : ""
                 }>${esc(a.label)}</button>`,
             )
             .join("")}
           <span class="spacer"></span>
           <span class="aside">${
             state.status?.recordings?.includes(scene.id) ? "a recording of this exists" : "not recorded yet"
           }</span>
           <button class="quiet" id="act-next">next →</button>
         </div>
       </div>`
    : `<div class="actions" style="margin-bottom:22px"><span class="spacer"></span><button class="quiet" id="act-next">next →</button></div>`;

  $("#stage").innerHTML = `
    <h1>${esc(scene.title)}</h1>
    <p class="lede">${esc(scene.lede)}</p>
    <div class="watch">${scene.watch}</div>
    ${compose}
    ${scene.layout()}`;

  collectEls(scene);

  $("#act-run")?.addEventListener("click", () => (state.replay ? replayScene() : runScene()));
  $("#act-next")?.addEventListener("click", () => mountScene(state.current + 1));
  $("#stage")
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
}

// Controls can fire outside a run — killing the worker before anything is
// scheduled, say. Those sentences still need somewhere to go.
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
  btn.textContent = "sending…";

  const run = new Run(scene);
  state.run = run;
  journey.reset();

  let ok = false;
  try {
    ok = await scene.run(run, values);
  } catch (err) {
    run.say(`Something went wrong here: <b>${esc(String(err.message ?? err))}</b>`, { tone: "bad" });
  }

  if (ok) state.ran.add(scene.id);
  await run.save(ok);
  renderRail();

  btn.disabled = false;
  btn.textContent = state.replay ? "Play the recording" : scene.action;
  if (!ok) {
    run.say("That did not finish. Switch on replay if you need this scene right now.", { tone: "warn" });
  }
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
    if (!res.ok) throw new Error("there is no recording of this scene yet");
    tape = await res.json();
  } catch (err) {
    $("#stage").insertAdjacentHTML(
      "afterbegin",
      `<div class="banner bad">${esc(String(err.message ?? err))} — run it live once and it will be kept.</div>`,
    );
    btn.disabled = false;
    btn.textContent = "Play the recording";
    return;
  }

  const run = new Run(scene, { replay: true });
  state.run = run;
  journey.reset();
  remountPanels(scene);

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

// Re-render just the output area so a replay starts clean, without rebuilding
// the header and its listeners.
function remountPanels(scene) {
  const stage = $("#stage");
  const anchor = stage.querySelector(".compose") ?? stage.querySelector(".actions");
  while (anchor.nextSibling) anchor.nextSibling.remove();
  anchor.insertAdjacentHTML("afterend", scene.layout());
  collectEls(scene);
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
  if (e.metaKey || e.ctrlKey) return;
  const typing = e.target.tagName === "INPUT";
  const k = e.key.toLowerCase();

  if (e.key === "Enter") {
    e.preventDefault();
    return (state.replay ? replayScene : runScene)();
  }
  if (typing) return; // arrow keys belong to the text field being edited
  if (e.key === "ArrowRight") mountScene(state.current + 1);
  else if (e.key === "ArrowLeft") mountScene(state.current - 1);
  else if (k === "r") setReplay(!state.replay);
});

await refreshStatus();
setInterval(refreshStatus, 4000);
mountScene(0);
