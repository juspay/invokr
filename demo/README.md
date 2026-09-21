# Leadership demo — deck and live site

Two artifacts for the 30-minute session:

| | |
|---|---|
| **Deck** | `demo/public/deck.html` — 14 slides plus an unshown appendix. Self-contained: no network, no build, opens from a file path |
| **Demo site** | `demo/public/index.html` — eight scenes that fire real jobs at a real Invokr, served by `demo/server.mjs` |

The narrative and the timing budget live in the design doc. This file is about
running the thing.

---

## Run it

```bash
just db-up && just db-migrate     # once, if the database isn't already up
just demo
```

Then:

- demo — <http://localhost:4173>
- deck — <http://localhost:4173/deck> (or `just deck`, which needs nothing running)

`just demo` builds the binaries, starts the API and the mock target, and then
runs `demo/server.mjs`, which:

1. serves the two pages,
2. proxies `/api/*` to Invokr with the API key and tenant headers attached
   server-side — no credential is ever in the page,
3. **owns the worker process**, so scene 3 can actually kill it,
4. provisions the demo org, two workspaces and the endpoints on startup
   (idempotent — re-running between rehearsals is fine).

Do not run `just dev` alongside it. That worker would pick up the jobs you
orphaned in scene 3 and quietly undo the point.

### Environment

| Variable | Default | Why you'd change it |
|---|---|---|
| `INVOKR_DEMO_PORT` | `4173` | Port clash |
| `INVOKR_URL` | `http://localhost:8080` | Demoing against a deployed Invokr |
| `INVOKR_MOCK_URL` | `http://localhost:9999` | A different target service |
| `INVOKR_DEMO_DASHBOARD_URL` | _(unset)_ | Scene 8's hand-off link |
| `INVOKR_DEMO_WORKER_FEATURES` | _(unset)_ | `kafka,redis-stream` for scene 6 |
| `INVOKR_DEMO_MANAGE_WORKER` | `1` | `0` if you want to run the worker yourself (scene 3's kill button then has nothing to kill) |

---

## The scenes

| # | Scene | Runs for | Notes |
|---|-------|----------|-------|
| 1 | Register | ~5s | Registers the endpoint, then asks the API for the secret and shows it won't hand it over |
| 2 | Fire now | ~1s | IMMEDIATE. The target echo shows the resolved body and the `x-invokr-idempotency-key` header |
| 3 | **Fire later + kill the worker** | ~15s | The strongest scene. `kill -9`, restart, the job still fires on time |
| 4 | It fails | ~7s | Three attempts with observed backoff, real error bodies |
| 5 | Fire repeatedly | up to 60s | pg_cron granularity is one minute — see the pre-arm note below |
| 6 | Any transport | ~3s | Overflow. Needs brokers (see below), otherwise shows the specs and skips |
| 7 | Two tenants | ~2s | Overflow. Same endpoint name, two schemas |
| 8 | Hand-off | — | Overflow. Link to the real dashboard |

**Scenes 1–5 are the core run and should always be shown. 6–8 are overflow** —
show them if you are ahead of the clock, drop them if you are not.

### Scene 5 and the one-minute wait

`pg_cron` ticks on minute boundaries, so scene 5 can sit waiting for up to 60
seconds. Scene 4 has a **"Pre-arm scene 5's schedule"** button for exactly this:
press it when you start scene 4, and a tick will have landed by the time you
arrive. The timeline says the schedule was pre-armed, so you are not pretending
otherwise.

Cancel the schedule before you leave scene 5 — it fires every minute until you
do. The cancel button looks the job up through the API, so it still works after
a page reload.

### Scene 6 and the brokers

Kafka and Redis Stream dispatchers are feature-gated in the worker, and the
brokers have to be running:

```bash
docker compose --profile kafka --profile redis up -d
INVOKR_DEMO_WORKER_FEATURES=kafka,redis-stream just demo
```

Without them the scene skips those two and shows their endpoint specs instead,
with a banner saying why. That is deliberate: firing at a broker that isn't
there only proves the broker isn't there.

### Scene 8 and the dashboard

The dashboard needs its own build:

```bash
just dashboard-build
INVOKR_MODE=both INVOKR_DASHBOARD_DIST_DIR=crates/dashboard/pkg cargo run -p invokr-api
INVOKR_DEMO_DASHBOARD_URL=http://localhost:8080/dashboard just demo
```

Without a URL the scene says so and tells you the command.

---

## Recording and replay

**Every successful live run is recorded to `demo/recordings/<scene>.json`** — the
full event tape with its real timings. Flip the **replay** toggle in the header
(or press `R`) and the scene plays that tape back through the same renderer, at
the timings it really had. Nothing is re-simulated and nothing is re-requested.

This is the insurance policy. If the network, the database or the room's Wi-Fi
turns against you mid-session, flip to replay and keep talking. The header shows
a `REPLAY` badge the whole time so you are never claiming a recording is live.

The recordings in this repo are from real runs against a local stack. **Re-record
them on the machine you will present from**, the day before:

```bash
just demo
# run scenes 1–5 (and 6–8 if you'll show them) once each, live
```

Each successful run overwrites its scene's tape. A failed run does not — a bad
take can't clobber a good one.

---

## The ten minutes before the session

1. `just demo`, then run scenes 1 through 5 live. This re-records them and warms
   every code path.
2. Check the header chips are green: api, worker, target.
3. Cancel any schedule left running from scene 5.
4. Open the deck in a second tab or window (`http://localhost:4173/deck`), press
   `F` for full screen, `N` if you want speaker notes.
5. Decide now whether you are showing scenes 6–8, and configure the brokers and
   dashboard URL if so. Deciding live costs you a minute you do not have.

**If a live scene fails in the room:** the timeline says so and suggests replay.
Flip the toggle, re-run the scene, carry on. Do not debug in front of the room —
the recording makes the same point.

---

## Files

```
demo/
  server.mjs          zero-dependency host: static files, API proxy, worker lifecycle, recordings
  bootstrap.mjs       idempotent provisioning of org, workspaces, specs, configs, secrets, endpoints
  public/
    index.html        the demo page
    app.js            scene engine — scenes emit events, one renderer draws them, replay reuses it
    styles.css
    deck.html         the deck, self-contained
  recordings/         captured event tapes, one per scene
```

The rule that makes replay honest: **a scene's `run()` never touches the DOM.**
It only emits events, and `apply(event)` draws them. Live and replay are the
same code path.
