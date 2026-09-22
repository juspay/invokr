# Leadership demo — deck and live site

Two artifacts for the 30-minute session:

| | |
|---|---|
| **Deck** | `demo/public/deck.html` — 11 slides plus an unshown appendix. Self-contained: no network, no build, opens from a file path |
| **Demo site** | `demo/public/index.html` — three pages, one board per page with the work moving through it, firing real jobs at a real Invokr and served by `demo/server.mjs`, with the receiving service reporting what it did |

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

The machine stays on screen and the work moves through it. A job is a **chip**
with its real id on it; the regions it travels between are the real ones.

```
┌────────────────────────────────────────────────────────────────────────────┐
│ Invokr   ① Set it up  ② Short tasks  ③ Long-running       ● api ● workers  │
├────────────────────────────────────────────────────────────────────────────┤
│  One task, end to end · Same name, two teams · Not just HTTP               │
├─────────────────┬──────────────────────────────────────────────────────────┤
│ SELECT …        │  ┌ your ─┐   ┌ executions ──┐   ┌ workers ┐   ┌ aarokya ┐│
│  FROM executions│  │service│──▶│ due now      │──▶│ 62e359  │──▶│  200    ││
│  WHERE status = │  └───────┘   │  ▸MND-8842   │   │ ▸chip   │   │ terminal││
│    'QUEUED'     │              │ not yet      │   │ 8f21c4  │   └─────────┘│
│  FOR UPDATE     │              │ finished     │   │ skipped │        │     │
│  SKIP LOCKED    │              └──────────────┘   └─────────┘        │     │
│                 │                    ▲──── retry · run_at + backoff ──┘     │
├─────────────────┴──────────────────────────────────────────────────────────┤
│ ◀ ▶ auto  5/7 One worker wins   mandate ▭  when ▾         wire 3   Fire it  │
└────────────────────────────────────────────────────────────────────────────┘
```

Nothing sits above the flow but the page and take pickers. Where you are — the
count and the current step's name — is in the console, with the controls.

Four regions in the order the work travels, and **one edge that goes back the
way it came**. The chip is written into `executions`, a worker takes it, it
goes out, and it comes back — to `finished`, or to `not yet` with the backoff
running on its face. That back edge is the mechanism: a retry, a cron tick and
a long-running poll are all the same circuit, walked again.

The panel on the left is whatever is **executing at that moment** — the claim
query, the resolved request, the `attempts` rows, the `polls` rows.

Everything on the board is read back from the running system: the worker ids
are the processes `just demo` started, the winner is the one Invokr wrote on
the execution row, and the loser's *skipped it — locked* is what `SKIP LOCKED`
actually did.

One honest gap: `executions.worker_id` is only populated once the row is
**terminal**. While a job is QUEUED, RUNNING or RETRYING the API returns `null`
for it, so mid-flight the board says a chip is *in the workers lane* rather than
naming a box — and the winner is named on the last step, where the row finally
carries it. Worth fixing in the API: mid-flight, "which worker holds this
execution" is exactly the question an operator asks.

Evidence stays behind the **wire** button: both sides of every call Invokr
made, and Aarokya's own log of what it did. Press `W` when somebody asks.

At rest, before you press anything, the machine is already drawn and the chip
is sitting at your service, with the request you are about to send in the left
panel. Change a field and both change with it.

### The controller

Nothing advances on its own until you say so.

| | |
|---|---|
| `▶` or `→` or `space` | draw the next step |
| `◀` or `←` | go back one step |
| `auto` or `A` | hand it over: each step is held for a beat, then the next |
| `W` | the wire drawer — every request and response, both sides |
| `P` | presentation view — the board at the size of a wall, one line of text |
| `Enter` | run (or replay) the current take |
| `1` `2` `3` | pages · `Tab` (`⇧Tab`) cycles takes on this page · `R` replay |

Backwards is real, not a rewind animation: what is on screen is always the
first *n* events of the run drawn in order, so `◀` draws one fewer. The run
carries on behind you either way — real work does not wait for the presenter —
and every step keeps the time it actually happened at.

`auto`'s beat is a *floor*, never a substitute. A 15-second wait still takes
fifteen seconds; it is only the 8-millisecond bursts that get stretched enough
to read.

### Presentation view (`P`)

For a room rather than a reader. The panel, the takes and the fields go away;
the board fills the wall, the boxes and arrows double in weight, and the only
text is **the step's own name**. Same board, same live data — you can
flip into it mid-run and back out again. The beat lengthens to 1.9s, because a
room reads slower than a person at a desk.

That is why the step names are short: in this view each one has to carry the
beat by itself.

## 1 · Set it up

No loop on this page: three stores feed one description of a call. The board is
`configs`, `secrets` and `payload_specs` on the left with an edge each into the
`endpoints` row on the right. Every `201` lands as a chip in its store, and the
placeholder it satisfies lights in the endpoint's template as it does.

| | Step | What the room learns |
|---|---|---|
| 1 | **config** | the values that change between environments, as data |
| 2 | **secret** | what you send, then what you can read back — there is no `value` field, and no read path for it at all |
| 3 | **payload spec** | the JSON Schema beside a real job refused `422` for a missing `mandate_id` |
| 4 | **endpoint** | the three stores feeding one template — nothing is resolved until a job fires |

## 2 · Short tasks

One task from trigger to end, with everything that shapes it under your hand:

| Field | What it changes |
|---|---|
| **mandate** | the id in the job's input, which is what Aarokya reports back |
| **when** | `now` (IMMEDIATE), `in a few seconds` (DELAYED), `on a schedule` (CRON) |
| **seconds** | how far out a DELAYED job is due |
| **every** | the cron expression — minute, 2 minutes, 5 minutes |
| **ticks** | how many ticks to watch before cancelling the schedule |
| **failures** | how many times Aarokya refuses before it works |
| **max tries** | the job's own `max_attempts`, overriding the endpoint's |

The journey changes shape with the trigger: pick `on a schedule` and it becomes
nine steps, with *pg_cron owns it*, *a row nobody inserted* and *cancel
when it is terminal* in place of *run_at is a column*. A second region appears
under *your service* — `pg_cron` — and the enqueue edge moves to it, because on
that path nothing of ours inserts the row.

Ask for failures and the run switches to `aarokya-mandate-sync-impatient` — the
same call with retries measured in seconds rather than the minutes the real
policy uses — and says so on screen, so nobody thinks Invokr retries that fast
by default. **Set `failures` to 2 and watch the chip go round the loop twice**:
out to a worker, out to Aarokya, back to *not yet* amber, and round again. The
`attempts` table on the left grows a row each time, all three carrying one key.

| Step | Where the chip goes | The claim |
|---|---|---|
| `POST /v1/jobs` | sitting at *your service* | one POST naming an endpoint **is** the integration |
| Two rows, then 201 | into `due now` | it existed **before the POST returned** |
| `run_at` is a column | into `not yet`, with the clock on its face | nothing is counting down |
| One worker wins | into the winner's box; the other says *skipped it — locked* | no leader election, no lock service, no coordination |
| The call goes out | stays, while the call edge fires | resolved **now**, not at registration |
| Every try is a row | back to `finished`, or to `not yet` for the next try | three tries, **one key** |
| The record | resting in `finished` | no log scraping, no agent, no separate store |

**Kill a worker** and **Add a worker** appear on the `run_at` step and nowhere
else, because that is the step where killing one proves something: the chip
does not move, and the job still goes out when a fresh worker arrives.

The retry is the thing to watch. Every attempt travels — the chip leaves the
lane for a worker, goes out, and comes back — because the row's leg out is
drawn on the `call` step and its leg back on the `answer` step. A failure is
not a red label; it is the work visibly going round again.

Two more takes sit on the same page. **Same name, two teams** fires the same
endpoint name into both workspaces and lets Aarokya's own log be the proof —
it names a different team each time, out of each workspace's own config, and
nobody templated that in. **Not just HTTP** sends the same job to a Kafka topic
and a Redis Stream, and says plainly which brokers are not running here.

## 3 · Long-running

Same board, same loop — which is the point. The middle lane's second shelf is
labelled `waiting · next check` instead of `not yet`, and the chip goes round
the circuit **once per check**: out of the lane, to a worker, to Aarokya, back
to the lane. Aarokya says `202 / still working`, the back edge says
`Retry-After 2.0s`, and the `polls` table on the left grows a row.

A callback is the same edge with a different label — Aarokya calling us, going
straight into the row — so *poll* and *callback* are visibly two ways round one
machine rather than two features.

Between checks, both workers read *asking for due rows*. Nothing is holding the
execution, which is the claim the page exists to make.

The console is the `async` block as a form — edit it and the next run is sent
with what you typed:

| Field | What it is |
|---|---|
| **mode** | `poll`, `callback`, or both — the spec allows either or both, and whichever finalizes first wins |
| **max_polls** | the bound; it or `max_wait_ms` trips and the execution is FAILED with `TIMEOUT` |
| **202s first** | how many times Aarokya says "still working" |
| **Retry-After (s)** | what Aarokya asks for between checks — Invokr honours it over the backoff |
| **calls back after (s)** | in callback mode, how long Aarokya works before POSTing `/v1/callbacks/…/complete` |

Two steps carry the whole page. *WAITING* is one table row and one line —
**nothing of yours is waiting**: no open socket, no blocked thread, no in-memory
state. And the last is two numbers side by side, attempts against polls, under
**polls are not attempts** — ten trips round the loop and the execution still
has one attempt on the record.

---

## Things worth knowing before you present

### Workers are real processes, and you can kill them

`just demo` starts **two** workers, so *exactly one worker takes it* has a real
loser to name. Page 2 has **Add a worker** (up to three) and **Kill a worker** —
a real `SIGKILL` to a real process, which is what makes it worth doing: set
`when = in a few seconds`, kill both on the *waits until due* frame, watch the
countdown carry on with nobody left to run it, start a fresh one, and the job
goes out on time.

The worker named on the claim frame is the one Invokr wrote on the execution
row, read back from the API — not a label the page chose.

Spawned workers get `INVOKR_DB_POOL_SIZE=8` and `INVOKR_WORKER_MAX_CONCURRENT=4`.
The defaults (50 connections each) exhaust a stock PostgreSQL's 100 once you run
two of them alongside the API, and the first thing to break is pg_cron — which
is what `when = on a schedule` needs.

### The schedule and the one-minute wait

`pg_cron` ticks on minute boundaries, so `when = on a schedule` sits waiting for
up to 60 seconds before the first row appears. That wait *is* a point worth
making — nothing is holding a connection open, the next tick is a row the
database will write — but it is a minute, so know it is coming. `ticks` decides
how many you watch; the run cancels the schedule itself on the way out, so
nothing is left firing.

The frame before that wait is the one that earns it: one `jobs` row, and an
`executions` table with **no rows yet — and that is correct**. A schedule is not
a queue of future runs.

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
`GET /_log`, which is what fills the bottom half of the wire drawer (`W`). With
**failures** turned up that is the whole point: Aarokya's log shows three calls
carrying **one** de-duplication key, which is the honest answer to "would a
retry register the mandate twice?".

That log is also where the key on screen comes from. `executions.idempotency_key`
is a real column but the API does not serialise it, so for a cron tick — where
the key is generated per execution rather than sent with the job — the only
honest source for "what key actually went out" is the side that received it.

Aarokya remembers how many times it has been asked about each mandate, so every
run resets that first. If you are testing by hand, `POST /_polls/reset` clears
it.

### Two teams needs the scoped caches

**Same name, two teams** only tells the truth on a build that has
`fix(worker): scope the config and secret caches to the workspace schema`. One
worker serves every active workspace from one cache; keyed on the object's name
alone, whichever workspace warms `aarokya-config` first serves its values to the
other. Run the take against a build without it and Aarokya's log names the same
team twice — the demo faithfully showing the bug.

That fix is on this branch. It is **not** on `main`, and it is not on
`feat/long-running-jobs`, which also added two call sites that read the secret
cache unscoped — `crates/worker/src/poll.rs`, on the poll path and the
stop-`DELETE` path, where `process_poll` already takes `schema_name` and
discards it with a `let _ =`. Merging the two branches as they stand would
leave the long-running paths handing one tenant's decrypted credential to
another's dispatch. Worth fixing in that branch before the merge.

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

There are eight tapes, one per journey shape:

```
setup                 short-task            long-running-poll
                      short-task-cron       long-running-callback
                      two-teams             long-running-both
                      any-transport
```

A take gets more than one tape when a dial changes the *steps*, not just their
contents — `when` on page 2 and `mode` on page 3. Those two are the only fields
left enabled in replay, because they are still a real choice; everything else is
greyed, since nothing is being dialled.

The recordings in this repo are from real runs against a local stack. **Re-record
them on the machine you will present from**, the day before:

```bash
just demo
# run every take once, live
```

Each successful run overwrites its take's tape, so the tape is whatever you last
ran — record the variant you intend to show. A failed run does not overwrite: a
bad take can't clobber a good one.

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
`A` hands it to auto, `P` presentation view, `W` opens the wire, `Enter` runs
the current one, `R` toggles replay.

**If a live take fails in the room:** the console says so and suggests replay.
Flip the toggle, re-run it, carry on. Do not debug in front of the room — the
tape is there so you never have to.
