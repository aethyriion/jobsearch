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

## Seniority

A second axis, alongside company stage. The level is read from the job
title, and `L` cycles the filter: the configured policy, then each rung on
its own, then unlabelled, then everything.

```
intern · junior · mid · senior · staff · manager
```

Where the range sits is `seniority:` in `filters.yaml`. Like startup mode
it's a **view** policy — every posting is stored whatever its level, so
widening or narrowing takes effect instantly against what's already in the
database.

```yaml
seniority:
  min: junior          # widen or narrow the range
  max: staff           # `staff` keeps principal/lead IC in, manager out
  include_unknown: true
```

**Keep `include_unknown: true`.** A bare "Software Engineer" states no
level and is one of the commonest titles there is — excluding those throws
away most of the list to avoid a handful of mismatches.

Three things the title reader handles that a naive keyword match doesn't:

- **"Member of Technical Staff" is not staff level.** Several AI labs use
  it as a flat title at every rung, so reading its "staff" as staff-level
  would promote a new-grad posting to the top of the ladder.
- **A spelled-out rung beats a bare roman numeral.** "Principal Software
  Engineer I" is principal, not junior.
- **Management is a track, not a rung.** "Engineering Manager" and
  "Director of Platform" are caught before the engineering words are.

Level vetoes deliberately do **not** live in `hard_exclusion_keywords`.
That list is a *fetch-time* decision — a job it matches is marked failed
and never surfaces again, so changing your mind means a re-fetch. Only the
wrong *track* (manager, director, VP) is vetoed there.

## Applications the scrapers never found

Not every job arrives through a board. Referrals, recruiter emails,
postings taken down after you applied, companies on an ATS this tool
doesn't speak — press `a` in the TUI and type it in.

The form asks for exactly what a fetcher produces (company, title,
location, URL, posted date, description) plus the things only you know:
stage, headcount, your status and your notes. Paste the job description
straight in — bracketed paste is on, so a multi-KB JD lands as one event
rather than as a thousand keystrokes.

The form is modal, like the rest of the UI. It opens in **insert** —
you came here to type — and `Esc` drops to **normal**, where `ZZ` or `:w`
writes it:

```
 Company     Northwind Systems
 Title       Staff Platform Engineer
 Location    Remote - US
 Stage     ‹ seed ›
 Status    ‹ applied ›
 ┌ Description — paste the JD here ──────────────────┐
 │ We are hiring a Staff Platform Engineer to own... │
 └───────────────────────────────────────────────────┘
 -- NORMAL --  j/k field · i insert · h/l cycle · dd clear · ZZ or :w WRITE · ZQ discard
```

| | |
|---|---|
| `j` / `k`, `Tab` | move between fields |
| `gg` / `G` | first / last field |
| `i`, `a` | insert on this field |
| `Esc` | back to normal |
| `h` / `l` | cycle a choice field (stage, status) |
| `dd` | clear the field |
| `x` | delete a character |
| `ZZ`, `:w`, `:wq`, `:x` | write and close |
| `ZQ`, `:q!` | discard |
| `:q` | close, refusing if there are unsaved edits |

`Esc` in normal mode does nothing on purpose — losing a half-typed
application to one stray keypress is a worse trade than typing `:q!`.

Hand-entered rows are marked `+` in the list and behave like any other
job: they sort and filter alongside fetched ones, and `jobsearch evaluate`
scores them against your profile exactly the same way.

Two deliberate exceptions, both because these rows were never
*discovered*:

- **Startup mode never hides them.** Hiding a job you actually applied to
  because the employer turned out to be public would defeat the point of
  tracking it.
- **`refilter` never demotes them.** They didn't pass the filters to get
  here, so a filter tweak can't quietly drop an application off your list.

**Pasting a URL a fetcher already found merges instead of duplicating.**
Applied through a Greenhouse link that's already in the database? Paste
it, set your status, save — it attaches to the existing row, keeps that
row's `source` and description, and rescues it if the filters had dropped
it. Blank boxes mean "leave it alone", so adding a status can't wipe a
fetched description.

Leave the URL blank and you get a stable `manual:company/title` one, so
re-entering the same role updates it rather than piling up near-duplicates.

`c` (or `E`) edits any row, including fetched ones — useful for fixing a
mangled title or adding a stage. `dd` (or `D`) deletes, but only
hand-entered rows: deleting a fetched one would just invite it back on the
next run.

From a script:

```bash
jobsearch add --company "Northwind Systems" \
              --title "Staff Platform Engineer" \
              --location "Remote - US" --stage seed --headcount 14 \
              --status applied --notes "referred by Dana" \
              --description-file jd.txt      # or `-` for stdin
```

## Tech stack

Every posting is tagged with the technologies its description mentions —
deterministically, from `tech.yaml`'s regex patterns. No model, no cost,
no network. `t` opens a picker of the tags actually present in your data,
with counts:

```
┌ technology ──────────────────────────────┐
│ > language:go              316           │
│   language:python          315           │
│   cloud:aws                281           │
│   platform:kubernetes      270           │
│   cloud:gcp                183           │
│   language:rust             79           │
└──────────────────────────────────────────┘
```

The counts are the feature — "aws 281 / gcp 183" answers the question
before you select anything. `jobsearch tech` prints the whole breakdown
as a histogram; `jobsearch tech --tag rust` lists the matching postings.

Tags are **derived, never stored**: recomputed from the stored description
on every read, so improving a pattern takes effect on the next run with no
migration and no re-fetch. `T` clears the filter.

### Writing patterns

`tech.yaml` groups tags by category. Three mechanisms exist because job
descriptions are full of words that look like technologies and aren't —
each of the following was a real false positive found in live data:

| | |
|---|---|
| `case_sensitive: true` | `\bgo\b` matches "go to market"; `\bGo\b` matches the language. The same trick makes `R` usable. |
| `exclude:` | Vetoes the tag for the whole posting. "Spark Capital" is a VC in one company's boilerplate and was counted as Apache Spark on every posting they publish. |
| Just being specific | A bare `\bworkers\b` matched *"unincorporated Los Angeles County workers"* far more often than Cloudflare Workers. `\bts\b` matched *"TS/SCI"* security clearances. |

Two regex traps worth knowing, both of which have bitten this repo:

- **Never put a trailing `\b` after punctuation.** `\bc\+\+\b` can never
  match, because `+` isn't a word character. Same for `C#`. (This is the
  same bug that made two of `filters.yaml`'s dealbreakers silently inert.)
- **Every pattern compiles with `(?-u)`.** Unicode-aware `\b` and case
  folding cost 1.21s across the corpus; the ASCII equivalents cost 43ms
  for byte-identical results. That 28× is what makes tagging-on-every-read
  affordable instead of needing a cache.

## Hiding noise

Not every posting is worth seeing twice — a role you've already been
rejected from, a listing that's no longer real, or a staffing agency
reposting the same three jobs under new URLs every week.

`x` hides a posting. `X` hides an employer entirely, including postings
they haven't made yet. Both ask first, and both are **soft**: nothing is
deleted, the row keeps its score and your notes, and `u` puts it back.

```
 x   hide this posting          X   hide this employer, now and in future
 u   restore whatever is hiding the selected row
 m   cycle to the `dismissed` view to find hidden rows
```

Rows are marked in the list: `+` you entered it, `-` it's hidden, `=`
both.

What "hidden" actually guarantees — each of these is a separate mechanism,
and all three are needed for a dismissal to stick:

| | |
|---|---|
| **Survives a refresh** | Dedup already stops a re-fetch re-adding the row, and the dismissal is keyed independently of it. |
| **Survives `refilter`** | Loosening a filter never resurrects a dismissed posting — otherwise widening the search would undo all your triage. |
| **Never scored** | Dismissed rows are cut from the AI queue before it runs. Paying to score something you've hidden is the clearest waste there is. |
| **Out of the CSVs** | `candidates.csv` and `scored_candidates.csv` are "current candidates"; hidden rows aren't. |

A company dismissal is the one that earns its keep against agencies: it
matches on the normalized name, so `Motion Recruitment, LLC` and
`motion recruitment llc.` are the same employer, and it applies to
postings that don't exist yet.

From a script:

```bash
jobsearch dismiss --url "https://..." --reason "already applied"
jobsearch dismiss --company "Motion Recruitment Partners, LLC" --reason "agency"
jobsearch dismissed                       # list everything hidden
jobsearch restore --company "Motion Recruitment Partners, LLC"
```

## Tracking where each application stands

`m` cycles the status filter: **all → tracked → untouched → applied →
interview → rejected → skipped → silence**. `tracked` is the application
list — everything you've acted on, whatever happened next.

`S` cycles the sort key, and `status` is one of them. It sorts by position
in the pipeline rather than alphabetically:

```
interview · applied · silence · rejected · skipped · untouched
```

Interview first because that's the one with a date attached; untouched
last because it's the absence of a decision, and 800 unmarked rows heading
the list would be useless.

The header keeps a running tally (`4 applied · 2 interview`), so you can
see where things stand without changing filters to count them.

## Commands

| Command | What it does |
|---|---|
| `jobsearch` / `jobsearch tui` | The review TUI. Fetch (`r`), score (`e`) and triage without leaving it. |
| `jobsearch fetch` | Pull from every configured source, filter, dedup, store. Free. `--company <slug>` or `--aggregator <substring>` to run one source in isolation. |
| `jobsearch evaluate` | Score unevaluated candidates with Claude. **The only step that costs money.** `--dry-run` to see the queue, `--limit N` to cap, `--all-stages` to score beyond startups. |
| `jobsearch discover-yc` | Pull the YC directory, record stages, resolve ATS boards. |
| `jobsearch refilter` | Re-apply the current `filters.yaml` to everything already stored, without re-fetching. Run it after editing filters. |
| `jobsearch add` | Record an application by hand, for scripting. `--description-file -` reads the JD from stdin. The TUI's `a` key is the interactive version. |
| `jobsearch inspect [URL]` | Print a stored job in full, or with no URL list recent rejections and why each was dropped. |
| `jobsearch tech` | What your candidates actually use, as a histogram. `--tag rust` lists the postings for one tag. |
| `jobsearch dismiss` | Hide a posting (`--url`) or an employer (`--company`). Soft: `restore` undoes it, `dismissed` lists what's hidden. |
| `jobsearch diagnose` | Where jobs die in the filter funnel, the daily intake rate, and which single config change reaches the most near-misses. Reads only what's stored; no network. |
| `jobsearch check` | Validate every config file and report stage coverage. |

Cost control on `evaluate` is deliberate: the queue respects the startup
cutoff, so startup mode doesn't just hide big-company jobs in the UI after
you've already paid to score them.

## TUI keys

```
MOVE     j/k or ↑/↓  move          g/G  top/bottom
         tab          switch pane   J/K  scroll detail

FILTER   s   startup mode on/off
         t   pick a technology (aws, gcp, rust, ...)   T  clear it
         L   level: the policy, then each rung, then unlabelled, then any
         f   AI verdict: all / apply / consider / skip / unscored
         m   your status: all / tracked / dismissed / untouched /
             applied / interview / rejected / skipped / silence
         /   search company, title and location
         S   sort: score / company / title / stage / posted / status / level
         R   reverse sort

TRIAGE   1 applied  2 interview  3 rejected  4 skipped  5 silence  0 clear
         n   edit notes            o  open in browser

HIDE     x   hide this posting (stays hidden across refreshes)
         X   hide EVERY posting from this employer, now and future
         u   restore whatever is hiding the selected row

ENTRIES  a / i   add an application the scrapers never found
         c / E   edit the selected row
         dd / D  delete it (hand-entered rows only)

RUN      r   fetch          e  score with Claude        F5  reload
         ?   help           q  quit
```

Status edits are written to SQLite on the keystroke, not batched — triage
survives a crash, and the frozen web board sees it immediately.

## "I'm not getting any new results"

Two very different problems look identical from the review list, and
`jobsearch diagnose` separates them:

```
INTAKE (jobs first seen, last 14 days)
  2026-09-28     256 new     29 passed filters
  2026-09-27     355 new     27 passed filters

FUNNEL (15075 stored)
     916 passed all filters  (6.1%)
    6266 failed on title+location
    5450 failed on location
    1391 failed on title

NEAR MISSES — failing exactly one check, so one config change reaches them
    5450 location only   (right role, wrong place)
    1391 title only      (right place, title didn't look like engineering)
      29 stack only      (right role and place, JD tripped stack_dealbreakers)
```

**A re-run finding nothing new is the normal case.** Boards only change
when companies post; running a fetch twice in a day will find nothing the
second time. The intake rows tell you the real rate — if those are healthy
and the pass rate is low, it's the filters. If intake itself is zero, it's
the sources.

The near-miss counts are the useful part: they're jobs one config change
away, broken down by which single check they failed. `diagnose` lists the
exact location strings and titles being dropped, so you can see whether
widening would buy you roles you want or just noise.

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

2. **`filters.yaml` → `seniority:`** — if junior roles aren't showing,
   check `min` here before anything else. And note that level and location
   interact: junior and new-grad postings are overwhelmingly on-site, so a
   remote-only `location_allow_patterns` will filter out most of them
   whatever the level range says.

3. **`filters.yaml` → `stack_dealbreakers` / `stack_core`** — a JD is
   rejected if it mentions a dealbreaker language and none of your core
   stack. If your own stack includes a "dealbreaker" language, move it
   into `stack_core`.

   One regex trap worth knowing, because it bit this repo: **don't put a
   trailing `\b` after a punctuation character.** `\bc#\b` requires a word
   character immediately after the `#`, so it never matches "C# developer"
   — it matched nothing at all until 2026-09-25. Same for `\bc\+\+\b`.
   A leading `\b` is fine. `cargo test` now fails on any dealbreaker
   pattern with this shape.

4. **`companies.yaml`** — who you're actually applying to.
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
| `dismissed_jobs` / `dismissed_companies` | What you've hidden. Separate from the job rows, so hiding destroys nothing and restoring is lossless. |
| `ats_resolution` | Which ATS board a company resolved to — including the misses, so re-runs don't re-probe. |

Hand-entered rows live in those same tables with `job_details.source =
'manual'` — no separate table, so the frozen web board shows them too.

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
cargo test          # 120 unit tests, no network
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Tests cover the filter rules (including the regex trap above), dedup and
stage-precedence in the store, the HN posting parser, YC's stage mapping,
the Anthropic tool-response parser (complete, truncated and refused
shapes), `companies.yaml` appending, the manual-entry form's modal keys and merge
semantics, the seniority title reader and its policy, dismissal
persistence across refilter and scoring, the tech detector's
false-positive guards, and the TUI's filter/sort/selection logic. None of them touch
the network.

### Layout

```
src/
  main.rs         CLI
  config.rs       YAML loaders
  model.rs        Job, Stage, Evaluation, JobRow
  filters.rs      filters.yaml rules + the startup policy
  store.rs        SQLite
  tech.rs         deterministic tech-stack detection (tech.yaml)
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
    form.rs       the manual-entry page's fields and validation
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
