# Leadership demo — deck and live site

Two artifacts for the 30-minute session:

| | |
|---|---|
| **Deck** | `demo/public/deck.html` — 14 slides plus an unshown appendix. Self-contained: no network, no build, opens from a file path |
| **Demo site** | `demo/public/index.html` — two acts that fire real jobs at a real Invokr, served by `demo/server.mjs`, with the receiving service reporting what it did |

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
3. **owns the worker process**, so the pick-up take can actually kill it,
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
| `INVOKR_DEMO_DASHBOARD_URL` | _(unset)_ | The hand-over link |
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
│ Invokr   ① Set it up  ② Run it            ● api ● workers ● aarokya    │  header
├────────────────────────────────────────────────────────────────────────┤
│  Poll until it is done · Exactly one worker takes it · …               │  takes
├──────────────────────────────────────────┬─────────────────────────────┤
│                                          │  ON THE WIRE                │
│           the picture                    │  → POST /v1/jobs   ← 201    │
│                                          ├─────────────────────────────┤
│                                          │  AAROKYA'S OWN LOG          │
│                                          │  ✓ 200 Asked the bank …     │
├──────────────────────────────────────────┴─────────────────────────────┤
│ ●●●○○○○  3.4s  a worker took it — the others skipped the locked row    │  console
└────────────────────────────────────────────────────────────────────────┘
```

The **takes** row is one line, not a sidebar. The **wire** panel is both sides
of every call — what Invokr sent, and what the receiving service says it did —
so there is no drawer to open and nothing hidden. The **console** is one dot per
step of the current journey, one sentence, and the inputs that drive it.

### Act 1 — Set it up

Five slots on the left, and on the right the one request they all feed. Each
placeholder in that request — `{{config.base_url}}`, `{{secret.…}}`,
`{{input.mandate_id}}` — is grey until the thing it needs exists, then takes its
namespace's colour.

| | What it is | What the room learns |
|---|---|---|
| 1 | **org + workspace** | a workspace is a PostgreSQL schema of its own; already there, because you do not make one per demo |
| 2 | **config** | the values that change between environments, as data |
| 3 | **secret** | encrypted at rest, and the API hands back the name and the timestamps — never the value |
| 4 | **payload spec** | JSON Schema; a job whose input does not match is refused at create time |
| 5 | **endpoint** | where, what, and how hard to try — four calls, no deploy |

The second take fires a job with no `mandate_id` and watches Invokr refuse it
with a 422 before any worker sees it, then fires a good one.

### Act 2 — Run it

The board. A job is a **token** — a small card with a real name on it — and it
moves:

```
                    ┌ endpoints ─────────────┐
your service  →  PostgreSQL ├ jobs · not due yet ┤  →  workers  →  Aarokya  →  done
                    └ jobs · due now ────────┘        ↑
                                        one takes it, the rest skip the locked row
```

| Take | Runs for | What you can change |
|---|---|---|
| **Poll until it is done** | up to ~70s | mandate, which check the bank says yes on |
| **Exactly one worker takes it** | your delay + a few s | mandate, delay |
| **When Aarokya is down** | ~25s | mandate, how many failures first |
| **When the work takes minutes** | ~20s | job name, check-ins |
| **Not just HTTP** | ~10s | mandate |
| **Same name, two teams** | ~15s | mandate |
| **Hand over** | — | — |

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
| A poll loop ends when the answer does | terminal status, and the caller cancels the job on screen |
| Long work doesn't hold a connection | the token parks on the other side and Invokr checks back |

### One step at a time, at a pace you choose

`slow` / `normal` / `quick` in the header sets how long each step is held. That
dwell is a *floor*, never a substitute: each step carries the real time it
happened at, and anything that genuinely took longer keeps its own timing. A
15-second wait still takes fifteen seconds; it is only the 8-millisecond bursts
that get stretched enough to read.

---

## Things worth knowing before you present

### Workers are real processes, and you can kill them

`just demo` starts **two** workers, so the board can show one taking a job while
the other skips it. The pick-up take has **Add a worker** (up to three) and
**Kill a worker** — a real `SIGKILL` to a real process, which is what makes it
worth doing: kill both, watch the job keep counting down with nobody left to run
it, start a fresh one, and it goes out on time.

The worker boxes are labelled with each process's own worker id, scraped from
its startup log, and a claimed job lands in the box of the worker that actually
claimed it — the same id Invokr wrote on the execution row.

Spawned workers get `INVOKR_DB_POOL_SIZE=8` and `INVOKR_WORKER_MAX_CONCURRENT=4`.
The defaults (50 connections each) exhaust a stock PostgreSQL's 100 once you run
two of them alongside the API, and the first thing to break is pg_cron — which
is the poll loop.

### The poll loop and the one-minute wait

`pg_cron` ticks on minute boundaries, so the poll take can sit waiting for up to
60 seconds. That wait *is* a point worth making — nothing is holding a
connection open, the next tick is a row the database will write — but if you
would rather not spend it, press **Arm it now** when you start act 1 and the
first tick will have landed by the time you arrive.

The take cancels its own schedule as soon as the bank answers `ACTIVE`, so
nothing is left firing. Leave **terminal on check** at `1` unless you want to
watch it stay `PENDING` and wait another minute.

### Act 1 is creation the first time and an update after that

`just demo` deliberately leaves the **mandates** workspace empty of the config,
secret, payload spec and endpoint, so act 1's first run in a session is four
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
`GET /_log`, which is what fills the bottom half of the wire panel. In the
retry take that is the whole point: Aarokya's log shows three calls carrying
**one** de-duplication key, which is the honest answer to "would a retry
register the mandate twice?".

Aarokya remembers how many times it has been asked about each mandate, so the
takes that poll it reset that first. If you are testing by hand,
`POST /_polls/reset` clears it.

### The transports take and the brokers

Kafka and Redis Stream dispatchers are feature-gated in the worker, and the
brokers have to be running:

```bash
docker compose --profile kafka --profile redis up -d
INVOKR_DEMO_WORKER_FEATURES=kafka,redis-stream just demo
```

Without them the take skips those two and says why. That is deliberate: firing
at a broker that isn't there only proves the broker isn't there.

### The long-running take needs another branch

Long-running jobs — an endpoint that answers `202 Accepted` and keeps working,
which Invokr then polls until it finishes — live on **`feat/long-running-jobs`**.
The demo detects whether the Invokr it is talking to has them, by offering the
API an endpoint whose `async` block enables neither polling nor callbacks: a
build with the feature rejects it, a build without it stores it as opaque JSON
and says 201. The throwaway endpoint is deleted either way.

On a build without the feature the take says so and points at replay. On a build
with it, the take registers an async endpoint, fires it, and the token parks on
the other side while the console counts the check-ins.

The target side works on any build: `invokr-mock-server` has `/async/start` and
`/async/status/{id}`, which take a script — "say 202 three times, then 200" —
from the request body, so the take's *check-ins* field is really the script it
sends.

### The hand-over and the dashboard

The dashboard needs its own build:

```bash
just dashboard-build
INVOKR_MODE=both INVOKR_DASHBOARD_DIST_DIR=crates/dashboard/pkg cargo run -p invokr-api
INVOKR_DEMO_DASHBOARD_URL=http://localhost:8080/dashboard just demo
```

Without a URL the take says so and tells you the command.

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

1. `just demo`, then run both acts live with the values you plan to use. This
   re-records them and warms every code path.
2. Check the header lights are green (api, workers, aarokya).
3. Make sure no schedule is left running — the poll take cancels its own, but a
   run you interrupted may not have.
4. Open the deck in a second tab or window (`http://localhost:4173/deck`), press
   `F` for full screen, `N` if you want speaker notes.
5. Decide now whether you are showing the transports, long-running and hand-over
   takes, and configure the brokers and dashboard URL if so. Deciding live costs
   you a minute you do not have.

**Keys:** `1` / `2` switch acts, `←` `→` move between takes, `Enter` runs the
current one, `R` toggles replay.

**If a live take fails in the room:** the console says so and suggests replay.
Flip the toggle, re-run it, carry on. Do not debug in front of the room — the
tape is there so you never have to.
