# Leadership demo — deck and live site

Two artifacts for the 30-minute session:

| | |
|---|---|
| **Deck** | `demo/public/deck.html` — 14 slides plus an unshown appendix. Self-contained: no network, no build, opens from a file path |
| **Demo site** | `demo/public/index.html` — eight scenes that fire real jobs at a real Invokr, served by `demo/server.mjs`, with the receiving service reporting what it did |

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

## The stage

The page is one picture, borrowed from the way the JavaScript event loop gets
explained at conferences. A job is a **token** — a small card with a real name
on it — and it moves:

```
your app → PostgreSQL ─ not due yet ─┐   → workers → the other side → done
                       └─ due now ───┘        ↑
                                      one takes it, the rest skip the locked row
```

Everything you would otherwise have to say out loud is something the room can
watch instead:

| What you want them to understand | What they see |
|---|---|
| The job is safe the moment Invokr answers | the token lands in the queue before anything else happens |
| A delayed job is a row, not a timer | it sits in **not due yet** with the seconds ticking on its face |
| Exactly one worker gets it | it flies into one worker's box; the others say *skipped — locked* |
| Retry with backoff | it flies **back** to the queue, amber, counting down to the next try |
| Crashes don't lose work | kill every worker and the token is still there, still counting |
| pg_cron schedules from inside the database | a token appears in the queue that nobody put there |

Text is deliberately thin: a title, one line, and a two-line ticker under the
stage. The numbers (durations, attempt rows, raw JSON) live in the **details**
drawer — press `D`, or the button at the bottom right.

## The scenes

Every scene fires a real job with whatever is in its form, so the room can hand
you a name and an order number and you type them in.

| # | Scene | Runs for | What you can change |
|---|-------|----------|---------------------|
| 1 | Tell it where to deliver | ~1s | — |
| 2 | Send one now | ~1s | customer, email, order |
| 3 | **Send one later — then kill a worker** | your delay + a few s | customer, email, order, delay |
| 4 | When the other side breaks | ~7s | order, amount |
| 5 | Every minute, forever | up to 60s | — |
| 6 | Not just HTTP | ~3s | order |
| 7 | Two teams, one name | ~2s | customer, order |
| 8 | Hand over | — | — |

**Scenes 1–5 are the run and should always be shown. 6–8 are overflow** — show
them if you are ahead of the clock, drop them if you are not.

### Workers are real processes, and you can kill them

`just demo` starts **two** workers, so the stage can show one taking a job while
the other skips it. Scenes 2 and 3 have **Add a worker** (up to three) and
**Kill a worker** — a real `SIGKILL` to a real process, which is what makes
scene 3 worth doing: kill both, watch the job keep counting down with nobody
left to run it, start a fresh one, and it goes out on time.

The worker boxes are labelled with each process's own worker id, scraped from
its startup log, and a claimed job lands in the box of the worker that actually
claimed it — the same id Invokr wrote on the execution row.

Spawned workers get `INVOKR_DB_POOL_SIZE=8` and `INVOKR_WORKER_MAX_CONCURRENT=4`.
The defaults (50 connections each) exhaust a stock PostgreSQL's 100 once you run
two of them alongside the API, and the first thing to break is pg_cron — which
is scene 5.

### Both sides of every delivery

The target is `invokr-mock-server`, and it is not a silent echo. It prints a
sentence for every request it handles — "Sent the welcome email to Priya
<priya@example.com> about order-1234, from noreply@payments.invokr.internal" —
to its own terminal, and keeps the last 200 at `GET /_log`. The panel on the
right shows those lines as they arrive, so the room gets the receiving end's
account next to Invokr's.

In scene 4 that is the whole point: the payment processor's log shows three
tries carrying **one** de-duplication key, which is the honest answer to "would
a retry charge the card twice?".

### Scene 5 and the one-minute wait

`pg_cron` ticks on minute boundaries, so scene 5 can sit waiting for up to 60
seconds. Scene 4 has a **"Start scene 5's schedule now"** button for exactly this:
press it when you start scene 4, and a tick will have landed by the time you
arrive. Scene 5 then says it is using the schedule started earlier, so you are
not pretending otherwise.

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

1. `just demo`, then run scenes 1 through 5 live with the values you plan to
   use. This re-records them and warms every code path.
2. Check the header chips are green (Invokr, target) and the footer says two workers.
3. Cancel any schedule left running from scene 5.
4. Open the deck in a second tab or window (`http://localhost:4173/deck`), press
   `F` for full screen, `N` if you want speaker notes.
5. Decide now whether you are showing scenes 6–8, and configure the brokers and
   dashboard URL if so. Deciding live costs you a minute you do not have.

**If a live scene fails in the room:** the ticker says so and suggests replay.
Flip the toggle, re-run the scene, carry on. Do not debug in front of the room —
the recording makes the same point, at the same speed.

---

## Files

```
demo/
  server.mjs          zero-dependency host: static files, API proxy, worker lifecycle,
                      the target's log, recordings
  bootstrap.mjs       idempotent provisioning (upserts, so re-running upgrades an
                      existing demo database)
  public/
    index.html        the stage and its zones
    app.js            token engine + scenes — scenes emit events, one renderer draws them,
                      replay reuses it
    styles.css
    deck.html         the deck, self-contained
  recordings/         captured event tapes, one per scene
```

The target's routes live in `crates/mock-server`: `/emails/welcome`,
`/billing/charge` (fails twice on purpose) and `/ops/heartbeat`, alongside the
older `/success`, `/fail`, `/flaky` fixtures the test suite uses.

The rule that makes replay honest: **a scene's `run()` never touches the DOM.**
It only emits events, and `apply(event)` draws them. Live and replay are the
same code path.
