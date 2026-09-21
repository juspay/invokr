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

## The scenes

Every scene fires a real job with whatever is in its form, so the room can hand
you a name and an order number and you type them in. Nothing is scripted except
the narration.

| # | Scene | Runs for | What you can change |
|---|-------|----------|---------------------|
| 1 | Tell Invokr where to deliver | ~1s | — |
| 2 | Send it now | ~1s | customer, email, order |
| 3 | **Send it later — then pull the plug** | your delay + a few s | customer, email, order, delay |
| 4 | When the other side breaks | ~7s | order, amount |
| 5 | Every minute, forever | up to 60s | — |
| 6 | Somewhere other than HTTP | ~3s | order |
| 7 | Two teams, one name | ~2s | customer, order |
| 8 | Hand over to the real thing | — | — |

**Scenes 1–5 are the run and should always be shown. 6–8 are overflow** — show
them if you are ahead of the clock, drop them if you are not.

### Both sides of every delivery

The target is `invokr-mock-server`, and it is no longer a silent echo. It prints
a sentence for every request it handles — "Sent the welcome email to Priya
<priya@example.com> about order-1234, from noreply@payments.invokr.internal" —
to its own terminal, and keeps the last 200 at `GET /_log`.

The demo page reads that log and shows it next to Invokr's account of the same
delivery. So the room gets two independent stories that have to agree, rather
than one system's word for it. In scene 4 this is the whole point: the payment
processor's log shows three tries carrying **one** de-duplication key, which is
the honest answer to "would a retry charge the card twice?".

The raw request and response are still there, behind **Show the raw request and
response** under each scene, for whoever wants to audit the JSON.

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
2. Check the header chips are green: Invokr, worker, email service.
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
  server.mjs          zero-dependency host: static files, API proxy, worker lifecycle,
                      the target's log, recordings
  bootstrap.mjs       idempotent provisioning (upserts, so re-running upgrades an
                      existing demo database)
  public/
    index.html        the demo page
    app.js            scene engine — scenes emit events, one renderer draws them, replay reuses it
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
