# jobsearch

A startup-focused job search, as a single Rust binary with a TUI.

It pulls open roles straight from companies' ATS APIs and from job
aggregators, drops anything irrelevant, deduplicates against everything
it has ever seen, optionally scores each candidate's fit against your own
experience with Claude, and gives you a terminal UI to triage the result.

Two things distinguish it from a job board:

- **It knows what stage each company is at.** Startup mode is on by
  default and is one keypress away from off. It's a *view*, not a fetch
  setting — everything is always stored, so changing the cutoff is
  instant and never needs a re-fetch.
- **Nothing leaves your machine** except requests to the job sources you
  configure and, for the optional AI step, the Anthropic API. Scraped
  jobs, scores and your own notes all live in one local SQLite file.

```
 jobsearch  647 shown / 702 total · 647 startup · 19 unscored  STARTUP <= series-c, <= 600 people
┌ jobs ──────────────────────────────────────────────┐┌ detail ──────────────────────────────────┐
│scr company           title                stg  rec ││Backend Engineer, Control Plane           │
│ 88 Render            Software Engineer,…  ?    apply││Tailscale                                 │
│ 82 Tailscale         Backend Engineer,…   ?    apply││Remote (US) · series-b · ~60 people (yc)  │
│ 82 Depot             Staff Software Eng…  seed apply││                                          │
│ 78 Temporal          Software Engineer,…  B    apply││82/100  APPLY                             │
│  – Duets Network     Founding Engineer    ?    –    ││STRENGTHS …                               │
└────────────────────────────────────────────────────┘└──────────────────────────────────────────┘
j/k move · tab pane · s startup · / search · f ai · m mine · 1-5 status · o open · r fetch · e eval
```

## Quick start

```bash
cargo build --release          # or: make build
cp .env.example .env           # fill in what you need — see the comments
cp profile.example.yaml profile.yaml   # then write your real experience into it

./target/release/jobsearch check       # validate every config file
./target/release/jobsearch fetch       # pull jobs (free)
./target/release/jobsearch             # open the TUI
```

`make install` puts `jobsearch` on your `PATH`. It reads its config from
the working directory, so run it from this repo or pass `--root`.

Nothing but `cargo` is required — SQLite is compiled in, and TLS is
rustls, so there's no system OpenSSL dependency either.

## How it works

1. **Fetch** — open roles come from four kinds of source:

   | Source | Configured in | What it gives you |
   |---|---|---|
   | Company ATS boards | `companies.yaml` | Greenhouse, Ashby, Workable, Lever, SmartRecruiters. Precise, low noise, the whole board. |
   | Aggregators | `aggregators.yaml` | Adzuna, Remotive — broad keyword+location search across employers you haven't identified yet. Wider reach, much more noise. |
   | Hacker News | `aggregators.yaml` | The monthly "Who is hiring?" thread. Startup-dense by nature: almost every post is a small company hiring directly. |
   | Y Combinator | `jobsearch discover-yc` | ~1,500 YC companies currently hiring, each with a stage and a headcount. |

   Company boards are fetched concurrently (8 at a time by default), so a
   run over ~130 boards takes seconds rather than minutes.

2. **Filter** (`filters.yaml`) — drops anything that isn't an
   engineering-shaped title, isn't in an allowed location, or whose JD
   requires a stack you've excluded.

3. **Dedup** — everything fetched goes into `data/seen_jobs.sqlite3`,
   keyed both by URL and by normalized company+title+location, so re-runs
   only ever surface genuinely new postings. Jobs that *fail* the filters
   are stored too, which is what makes `jobsearch refilter` possible.

4. **Score** (optional, costs money) — sends each filtered candidate plus
   your `profile.yaml` to Claude, which returns a 0-100 fit score,
   concrete gaps, transferable strengths, risk factors, an
   apply/consider/skip recommendation — and a guess at the company's
   stage.

5. **Triage** — the TUI. Mark each job `applied / interview / rejected /
   skipped / silence`, attach notes, open the posting in a browser.

## Startup mode

The point of the tool. A company sits somewhere on this axis:

```
pre-seed · seed · series-a · series-b · series-c · growth · public · big-tech
└──────────────── startup, by default ────────────────┘
```

Where the cutoff sits is `startup:` in `filters.yaml`. Press `s` in the
TUI to toggle it, or run `jobsearch evaluate --all-stages` to widen it for
one scoring run.

**Stage comes from three sources**, and a weaker one never overwrites a
stronger one:

| Source | How it's set | Trust |
|---|---|---|
| `curated` | A `stage:` field on a `companies.yaml` entry, written by hand. | Highest — a human looked it up. |
| `source` | The YC directory, via `jobsearch discover-yc`. Stage is derived from YC's Early/Growth label refined by team size, with `status: Public` overriding both. | High — real data, coarse mapping. |
| `ai` | Inferred by the scoring step from the JD: funding mentioned, team size, "founding engineer" in the title, equity in the comp. Discarded below 50% confidence. | Lowest — covers the long tail nothing else reaches. |

The TUI shows the provenance next to the stage (`series-b (curated)` vs.
`seed (ai)`) so you know how much to trust it.

Stage is stored per *company*, not per job, keyed by a normalized name. A
stage learned once applies to every posting from that employer — including
ones fetched months later from a completely different source.

### Getting stage coverage

A fresh checkout knows nothing about stage, so start here:

```bash
jobsearch discover-yc
```

That records stage and headcount for every YC company currently hiring
(~1,500 of them), then probes each US-reachable one for a Greenhouse,
Ashby or Lever board and appends the ones it finds to `companies.yaml`
with their stage filled in. Expect roughly a 50% board-resolution rate and
a few minutes of runtime; results are cached, so a re-run only probes
companies it hasn't seen. Use `--limit 40` first to see the hit rate
before committing to a full pass.

Then, as coverage grows, tighten `include_unknown`:

```yaml
startup:
  include_unknown: true   # start here — unknown-stage jobs stay visible
  # include_unknown: false  # switch once `jobsearch check` shows good coverage
```

With `false`, "startup" means *known to be a startup* rather than *not
known to be big*. With `true` on a fresh database, everything shows, which
is the right failure mode — an empty startup view would look like a broken
filter rather than like missing data.

## Commands

| Command | What it does |
|---|---|
| `jobsearch` / `jobsearch tui` | The review TUI. Fetch (`r`), score (`e`) and triage without leaving it. |
| `jobsearch fetch` | Pull from every configured source, filter, dedup, store. Free. `--company <slug>` or `--aggregator <substring>` to run one source in isolation. |
| `jobsearch evaluate` | Score unevaluated candidates with Claude. **The only step that costs money.** `--dry-run` to see the queue, `--limit N` to cap, `--all-stages` to score beyond startups. |
| `jobsearch discover-yc` | Pull the YC directory, record stages, resolve ATS boards. |
| `jobsearch refilter` | Re-apply the current `filters.yaml` to everything already stored, without re-fetching. Run it after editing filters. |
| `jobsearch inspect [URL]` | Print a stored job in full, or with no URL list recent rejections and why each was dropped. |
| `jobsearch check` | Validate every config file and report stage coverage. |

Cost control on `evaluate` is deliberate: the queue respects the startup
cutoff, so startup mode doesn't just hide big-company jobs in the UI after
you've already paid to score them.

## TUI keys

```
MOVE     j/k or ↑/↓  move          g/G  top/bottom
         tab          switch pane   J/K  scroll detail

FILTER   s   startup mode on/off
         f   AI verdict: all / apply / consider / skip / unscored
         m   your status: all / untouched / applied / …
         /   search company, title and location
         S   cycle sort key        R  reverse sort

TRIAGE   1 applied  2 interview  3 rejected  4 skipped  5 silence  0 clear
         n   edit notes            o  open in browser

RUN      r   fetch          e  score with Claude        F5  reload
         ?   help           q  quit
```

Status edits are written to SQLite on the keystroke, not batched — triage
survives a crash, and the frozen web board sees it immediately.

## Configuration

Everything tunable is YAML; retargeting the search never needs a
recompile.

- **`companies.yaml`** — the company registry: `name`, `ats`, `slug`, and
  optionally `stage` and `headcount`. `discover-yc` and the aggregator
  full-JD fetch both append to it automatically, preserving your comments.
- **`aggregators.yaml`** — aggregator and HN search config. Entries can be
  `enabled: false` to keep them documented but off.
- **`filters.yaml`** — title allowlist and exclusions, location patterns,
  stack dealbreakers, and the `startup:` policy.
- **`profile.yaml`** — your experience, fed to the scoring step.
  Gitignored. `profile.example.yaml` is the blank template;
  `profile.sample.yaml` is a fully worked fictional one showing the level
  of specificity each evidence bullet actually needs.

### The three settings that decide whether a run returns anything

1. **`filters.yaml` → `location_allow_patterns`** — the #1 reason a first
   run returns nothing. Two traps the shipped patterns document inline: a
   bare `\bremote\b` lets in "Remote Poland", and a bare `\bus\b` lets in
   every US *on-site* role, because SmartRecruiters appends the country
   code to every location ("Santa Clara, CA, us").

2. **`filters.yaml` → `stack_dealbreakers` / `stack_core`** — a JD is
   rejected if it mentions a dealbreaker language and none of your core
   stack. If your own stack includes a "dealbreaker" language, move it
   into `stack_core`.

   One regex trap worth knowing, because it bit this repo: **don't put a
   trailing `\b` after a punctuation character.** `\bc#\b` requires a word
   character immediately after the `#`, so it never matches "C# developer"
   — it matched nothing at all until 2026-09-25. Same for `\bc\+\+\b`.
   A leading `\b` is fine. `cargo test` now fails on any dealbreaker
   pattern with this shape.

3. **`companies.yaml`** — who you're actually applying to.
   `aggregators.yaml`'s `where` and `country` are location-specific too;
   `country` is Adzuna's country code, which goes in the URL path rather
   than the query string.

   One more trap, also found the hard way: **Adzuna has no boolean syntax
   inside `what`.** `what: "backend engineer rust OR golang"` treats "OR"
   as a literal keyword and matches nothing at all. Use the separate
   `what_and` and `what_or` params instead — every param that isn't
   `max_pages` or `country` is forwarded to the API verbatim, so any of
   Adzuna's search fields works here.

## What's stored

One SQLite file, `data/seen_jobs.sqlite3`:

| Table | Holds |
|---|---|
| `seen_jobs` | Dedup keys. Every job ever fetched, pass or fail. |
| `job_details` | The full record including JD text, keyed by URL. |
| `ai_evaluations` | Scores and reasoning. |
| `user_status` | Your `my_status` and notes. |
| `company_stage` | Stage, headcount and provenance per company. |
| `ats_resolution` | Which ATS board a company resolved to — including the misses, so re-runs don't re-probe. |

JD text deliberately stays out of the CSVs: a multi-KB job description in
a spreadsheet cell defeats the point of the CSV being a quick scannable
list. `data/candidates.csv` and `data/scored_candidates.csv` are exports,
not the source of truth.

## A note on Wellfound

Wellfound (formerly AngelList Talent) is the obvious missing
startup-native source, and its absence is deliberate. Its GraphQL endpoint
returns 403 behind a Cloudflare interstitial for any client that doesn't
present as a full browser — and so does `robots.txt`. Reading it would
mean impersonating a browser specifically to defeat bot protection the
site is clearly choosing to apply, so this tool doesn't. The YC directory
and the HN thread cover the same ground through interfaces meant to be
read by programs. If you want Wellfound coverage, their email job alerts
into a mailbox you read yourself is the supported path.

## The web board

`web/` still works and reads the same database, but it is **frozen** — it
has no concept of company stage, so it can't do startup mode. See
[`web/README.md`](web/README.md).

## Development

```bash
cargo test          # 48 unit tests, no network
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Tests cover the filter rules (including the regex trap above), dedup and
stage-precedence in the store, the HN posting parser, YC's stage mapping,
the Anthropic tool-response parser (complete, truncated and refused
shapes), `companies.yaml` appending, and the TUI's filter/sort/selection
logic. None of them touch the network.

### Layout

```
src/
  main.rs         CLI
  config.rs       YAML loaders
  model.rs        Job, Stage, Evaluation, JobRow
  filters.rs      filters.yaml rules + the startup policy
  store.rs        SQLite
  pipeline.rs     fetch -> filter -> dedup -> store, and the scoring pass
  evaluate.rs     Anthropic Messages API (raw HTTP; there's no official Rust SDK)
  discover.rs     appending to companies.yaml without eating its comments
  sources/
    ats.rs          Greenhouse, Ashby, Workable, Lever, SmartRecruiters
    aggregators.rs  Adzuna, Remotive
    hn.rs           Hacker News "Who is hiring"
    yc.rs           Y Combinator directory
    http.rs         shared client
  tui/
    app.rs        state and transitions (no terminal I/O — unit-testable)
    ui.rs         rendering
    mod.rs        event loop and background tasks
```

## Scheduling

Once a full local run looks right:

```cron
0 7 * * * cd /path/to/job_search_tool && ./target/release/jobsearch fetch && ./target/release/jobsearch evaluate
```

Then open the TUI when you want to triage.

## Notes on scope

- Greenhouse, Ashby, Workable and Lever were each validated against live
  responses. SmartRecruiters is built from documentation and third-party
  corroboration only — verify a new SmartRecruiters company with
  `jobsearch fetch --company <slug>` before trusting it.
- No Workday support. Its job-search API is POST-based with a different
  pagination shape, and several large Ohio employers (Nationwide, Cardinal
  Health, Progressive, Hyland, CoverMyMeds) sit behind it — the Adzuna
  Ohio query is what covers them for now.
- A failing company board logs a warning and the run continues; the other
  129 boards are still worth having.

## License

[MIT](LICENSE)
