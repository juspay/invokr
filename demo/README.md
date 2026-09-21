# Leadership demo — deck and live site

Two artifacts for the 30-minute session:

| | |
|---|---|
| **Deck** | `demo/public/deck.html` — 14 slides plus an unshown appendix. Self-contained: no network, no build, opens from a file path |
| **Demo site** | `demo/public/index.html` — three pages that fire real jobs at a real Invokr, served by `demo/server.mjs`, with the receiving service reporting what it did |

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
3. **owns the worker process**, so page 2 can actually kill it,
4. provisions the demo org, two workspaces and the supporting endpoints on
   startup (idempotent — re-running between rehearsals is fine).

Do not run `just dev` alongside it. That worker would pick up the jobs you
orphaned and quietly undo the point.

### Environment

| Variable | Default | Why you'd change it |
|---|---|---|
| `INVOKR_DEMO_PORT` | `4173` | Port clash |
| `INVOKR_URL` | `http://localhost:8080` | Demoing against a deployed Invokr |
| `INVOKR_MOCK_URL` | `http://localhost:9999` | A different target service |
| `INVOKR_DEMO_WORKER_FEATURES` | _(unset)_ | `kafka,redis-stream` for the transports take |
| `INVOKR_DEMO_MANAGE_WORKER` | `1` | `0` if you want to run the worker yourself (the kill button then has nothing to kill) |

---

## The story it tells

The demo is the thing Invokr is actually used for. A mandate is registered with
a bank; the bank does not answer straight away; so a job asks **Aarokya** for
the mandate's status every minute, and when the status is terminal the caller
cancels the job. Aarokya is `invokr-mock-server`, and it prints what it did:

```
✓ 200  Asked the bank about MND-8842, for the mandates team — still PENDING (check 1 of 3)
✓ 200  Asked the bank about MND-8842, for the mandates team — ACTIVE, the mandate is registered
```

The endpoint the demo builds is, header for header, the one the team already
runs — a URL assembled from config and input, an `Authorization` resolved from
the secret store at execution time, and a retry policy measured in minutes. It
carries two extra headers, `x-terminal-after` and `x-fail-times`, which are the
demo dialling the fake bank; the page says so under the spec.

---

## The screen

Four regions, and no more:

```
┌────────────────────────────────────────────────────────────────────────┐
│ Invokr   ① Set it up  ② Short tasks  ③ Long-running   ● api ● workers  │  header
├────────────────────────────────────────────────────────────────────────┤
│  One task, end to end · Same name, two teams · Not just HTTP           │  takes
├──────────────────────────────────────────┬─────────────────────────────┤
│                                          │  ON THE WIRE                │
│           the picture                    │  → POST /v1/jobs   ← 201    │
│                                          ├─────────────────────────────┤
│                                          │  AAROKYA'S OWN LOG          │
│                                          │  ✓ 200 Asked the bank …     │
├──────────────────────────────────────────┴─────────────────────────────┤
│ ◀ ▶ auto  4/7  ●●●○○○○  3.4s  a worker took it — the others skipped …  │  console
└────────────────────────────────────────────────────────────────────────┘
```

The **takes** row is one line, not a sidebar, and disappears when a page has
only one. The **wire** panel is both sides of every call, so there is no drawer
to open and nothing hidden. The **console** is the controller, one dot per step
of the current journey, one sentence, and the fields that drive it.

### The controller

Nothing advances on its own until you say so.

| | |
|---|---|
| `▶` or `→` or `space` | draw the next step |
| `◀` or `←` | go back one step |
| `auto` or `A` | hand it over: each step is held for a beat, then the next |
| `Enter` | run (or replay) the current take |
| `1` `2` `3` | pages · `Tab` cycles takes on this page |

Backwards is real, not a rewind animation: what is on screen is always the
first *n* events of the run drawn in order, so `◀` draws one fewer. The run
carries on behind you either way — real work does not wait for the presenter —
and every step keeps the time it actually happened at.

`auto`'s beat is a *floor*, never a substitute. A 15-second wait still takes
fifteen seconds; it is only the 8-millisecond bursts that get stretched enough
to read.

## 1 · Set it up

Five slots on the left, and on the right the one request they all feed. Each
placeholder in that request — `{{config.base_url}}`, `{{secret.…}}`,
`{{input.mandate_id}}` — is grey until the thing it needs exists, then takes
its namespace's colour.

| | What it is | What the room learns |
|---|---|---|
| 1 | **org + workspace** | a workspace is a PostgreSQL schema of its own; already there, because you do not make one per demo |
| 2 | **config** | the values that change between environments, as data |
| 3 | **secret** | encrypted at rest, and the API hands back the name and the timestamps — never the value |
| 4 | **payload spec** | JSON Schema; a job whose input does not match is refused at create time |
| 5 | **endpoint** | where, what, and how hard to try — four calls, no deploy |

The second take fires a job with no `mandate_id` and watches Invokr refuse it
with a 422 before any worker sees it, then fires a good one.

## 2 · Short tasks

One task from trigger to end, with everything that shapes it under your hand:

| Field | What it changes |
|---|---|
| **when** | `now` (IMMEDIATE), `in a few seconds` (DELAYED), `on a schedule` (CRON) |
| **seconds** | how far out a DELAYED job is due |
| **every** | the cron expression — minute, 2 minutes, 5 minutes |
| **stop after** | how many ticks to watch before cancelling the schedule |
| **failures first** | how many times Aarokya refuses before it works |
| **max tries** | the job's own `max_attempts`, overriding the endpoint's |

The journey changes shape with the trigger: pick `on a schedule` and the dots
become nine, with *pg_cron owns it*, *the database makes a row* and *cancelled*
in place of *waits until due*.

Ask for failures and the run switches to `aarokya-mandate-sync-impatient` — the
same call with retries measured in seconds rather than the minutes the real
policy uses — and says so on screen, so nobody thinks Invokr retries that fast
by default.

The board is the picture. A job is a **token** — a small card with a real name
on it — and it moves:

```
                    ┌ endpoints ─────────────┐
your service  →  PostgreSQL ├ jobs · not due yet ┤  →  workers  →  Aarokya  →  done
                    └ jobs · due now ────────┘        ↑
                                        one takes it, the rest skip the locked row
```

Everything you would otherwise have to say out loud is something the room can
watch instead:

| What you want them to understand | What they see |
|---|---|
| An endpoint is data, not a deploy | five slots fill in and the spec's placeholders light up |
| The job is safe the moment Invokr answers | the token is in the queue before anything else happens |
| A schedule is a row, not a process | it sits in **not due yet** with the seconds ticking on its face |
| Exactly one worker gets it | it flies into one worker's box; the others say *skipped — locked* |
| Retry with backoff | it flies **back** to the queue, amber, counting down to the next try |
| Crashes don't lose work | kill every worker and the token is still there, still counting |
| pg_cron schedules from inside the database | a token appears in the queue that nobody put there |
| A poll loop ends when the answer does | the schedule is cancelled on screen once the status is terminal |

Two more takes sit on the same page: **Same name, two teams** fires the same
endpoint name into both workspaces, and **Not just HTTP** sends the same job to
Kafka and a Redis Stream.

## 3 · Long-running

When the other side answers `202` and keeps working, the interesting thing is
not where a token is — it is **who called whom, in what order, and how long
apart**. So this page drops the board for two lanes with time running
downwards:

```
        INVOKR                                   AAROKYA
  142ms  ├────────── POST /async/start ─────────────▶│  here is the work
  149ms  │◀───────── 202  Location: /async/status/… ─┤  accepted, still working
         │  parked · WAITING, no connection held      │
  1.2s   ├────────── GET /async/status/… ───────────▶│  poll 1
  1.2s   │◀───────── 202  still working ─────────────┤  Retry-After 2s
  3.3s   ├────────── GET /async/status/… ───────────▶│  poll 2
  ⋮
  7.5s   │◀───────── 200  success ───────────────────┤  done
```

Each arrow is a real row: the first pair from `attempts`, every poll from the
`polls` table with its own `status_code` and `retry_after_ms`, and a callback
from Aarokya's own log. The panel on the left is the `async` block itself, as a
form — edit it and the next run is sent with what you typed:

| Field | What it is |
|---|---|
| **mode** | `poll`, `callback`, or both — the spec allows either or both, and whichever finalizes first wins |
| **poll.initial_delay_ms** / **max_delay_ms** / **backoff** | the cadence when the target does not send `Retry-After` |
| **max_polls** / **max_wait_ms** | the bounds; either one trips and the execution is FAILED with `TIMEOUT` |
| **202s before it finishes** | how many times Aarokya says "still working" |
| **Retry-After (s)** | what Aarokya asks for between checks — Invokr honours it over the backoff |
| **calls back after (s)** | in callback mode, how long Aarokya works before POSTing `/v1/callbacks/…/complete` |

The state panel underneath tracks the execution through `QUEUED → WAITING →
POLLING → SUCCESS` and counts the polls against `max_polls`, which is where the
point lands: **polls are not attempts.** Ten check-ins and the execution still
has one attempt on the record.

---

## Things worth knowing before you present

### Workers are real processes, and you can kill them

`just demo` starts **two** workers, so the board can show one taking a job while
the other skips it. Page 2 has **Add a worker** (up to three) and **Kill a
worker** — a real `SIGKILL` to a real process, which is what makes it worth
doing: set `when = in a few seconds`, kill both, watch the job keep counting
down with nobody left to run it, start a fresh one, and it goes out on time.

The worker boxes are labelled with each process's own worker id, scraped from
its startup log, and a claimed job lands in the box of the worker that actually
claimed it — the same id Invokr wrote on the execution row.

Spawned workers get `INVOKR_DB_POOL_SIZE=8` and `INVOKR_WORKER_MAX_CONCURRENT=4`.
The defaults (50 connections each) exhaust a stock PostgreSQL's 100 once you run
two of them alongside the API, and the first thing to break is pg_cron — which
is what `when = on a schedule` needs.

### The schedule and the one-minute wait

`pg_cron` ticks on minute boundaries, so `when = on a schedule` sits waiting for
up to 60 seconds before the first row appears. That wait *is* a point worth
making — nothing is holding a connection open, the next tick is a row the
database will write — but it is a minute, so know it is coming. `stop after`
decides how many ticks you watch; the run cancels the schedule itself on the
way out, so nothing is left firing.

### Page 1 is creation the first time and an update after that

`just demo` deliberately leaves the **mandates** workspace empty of the config,
secret, payload spec and endpoint, so page 1's first run in a session is four
real `201 Created`s. Run it again and each POST comes back `409`, the page
follows with a `PUT`, and says so — *already there, so the same call updates it
in place, still one row, still no deploy*, which is its own point.

It cannot start from nothing again, and that is Invokr being careful rather than
the demo cheating: an endpoint any job has ever pointed at cannot be deleted
(`jobs.endpoint` is a foreign key and a retired job still holds it), and a
config an endpoint points at cannot be deleted either.

### Both sides of every delivery

The target is `invokr-mock-server`, and it is not a silent echo. It prints a
plain sentence for every request it handles — to its own terminal, and to
`GET /_log`, which is what fills the bottom half of the wire panel. With
**failures first** turned up that is the whole point: Aarokya's log shows three
calls carrying **one** de-duplication key, which is the honest answer to "would
a retry register the mandate twice?".

Aarokya remembers how many times it has been asked about each mandate, so every
run resets that first. If you are testing by hand, `POST /_polls/reset` clears
it.

### Not just HTTP, and the brokers

Kafka and Redis Stream dispatchers are feature-gated in the worker, and the
brokers have to be running:

```bash
docker compose --profile kafka --profile redis up -d
INVOKR_DEMO_WORKER_FEATURES=kafka,redis-stream just demo
```

Without them the take skips those two and says why. That is deliberate: firing
at a broker that isn't there only proves the broker isn't there.

### Page 3 needs another branch

Long-running jobs — an endpoint that answers `202 Accepted` and keeps working,
which Invokr then polls until it finishes — live on **`feat/long-running-jobs`**.
The demo detects whether the Invokr it is talking to has them, by offering the
API an endpoint whose `async` block enables neither polling nor callbacks: a
build with the feature rejects it, a build without it stores it as opaque JSON
and says 201. The throwaway endpoint is deleted either way.

On a build without the feature the page says so and points at replay. On a build
with it, the page writes the endpoint fresh from the form on every run, so what
is sent is what you typed.

The target side works on any build: `invokr-mock-server` has `/async/start` and
`/async/status/{id}`, which take a script — "say 202 three times, then 200" —
from the request body. Give `/async/start` a `callback_url` and a
`callback_after_ms` instead and it stops waiting to be asked: it sleeps, then
POSTs the result to `/v1/callbacks/…/complete` itself, which is what the page's
**callback** mode shows.

Three things make callback mode work, and all three are things a real
integration needs:

- The worker builds `{{execution.callback_url}}`, so it needs
  `INVOKR_API_BASE_URL` — without it the URL is a bare path the target cannot
  call. `demo/server.mjs` sets it from `INVOKR_URL` when it spawns a worker.
- The target is the one calling Invokr, so it needs Invokr's API key. It is
  held as a secret named `invokr-api-key` and templated into the body the
  target is given, rather than pasted into the endpoint spec.
- The `async` block's `callback` field is a plain boolean. The design doc shows
  `{"enabled": true}`; the implementation takes `true`. The implementation is
  what runs.

Each mode keeps its own tape — `long-running-poll`, `long-running-callback`,
`long-running-both` — so replay shows the one you have selected. **both** is
worth a minute of anyone's time: two polls go out, the callback lands first,
and the execution is finalized by whichever got there first, exactly as the
spec says.

---

## Recording and replay

**Every successful live run is recorded to `demo/recordings/<take>.json`** — the
full event tape with its real timings. Flip the **replay** toggle in the header
(or press `R`) and the take plays that tape back through the same renderer, from
the times it really happened at, with the same step dwell applied. Nothing is
re-simulated and nothing is re-requested.

This is the insurance policy. If the network, the database or the room's Wi-Fi
turns against you mid-session, flip to replay and keep talking. The header shows
a `replaying` badge the whole time so you are never claiming a recording is live.

The recordings in this repo are from real runs against a local stack. **Re-record
them on the machine you will present from**, the day before:

```bash
just demo
# run every take once, live
```

Each successful run overwrites its take's tape. A failed run does not — a bad
take can't clobber a good one.

---

## The ten minutes before the session

1. `just demo`, then run all three pages live with the values you plan to use.
   This re-records them and warms every code path.
2. Check the header lights are green (api, workers, aarokya).
3. Make sure no schedule is left running — a page-2 run on a schedule cancels
   its own, but one you interrupted may not have.
4. Open the deck in a second tab or window (`http://localhost:4173/deck`), press
   `F` for full screen, `N` if you want speaker notes.
5. Decide now whether you are showing **Not just HTTP** and page 3, and
   configure the brokers if so. Deciding live costs you a minute you do not
   have.

**Keys:** `1` `2` `3` pages, `Tab` cycles takes, `→` / `←` step through a run,
`A` hands it to auto, `Enter` runs the current one, `R` toggles replay.

**If a live take fails in the room:** the console says so and suggests replay.
Flip the toggle, re-run it, carry on. Do not debug in front of the room — the
tape is there so you never have to.
