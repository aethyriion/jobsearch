# jobsearch — reference

The full operator manual: every command, every key, and the configuration
knobs worth knowing about. The [README](../README.md) covers what the tool
is and how to get it running; this is what to read once it is.

## Contents

- [Commands](#commands)
- [TUI keys](#tui-keys)
- [Configuration](#configuration)
- [Applications the scrapers never found](#applications-the-scrapers-never-found)
- [Tracking where each application stands](#tracking-where-each-application-stands)
- [Seniority](#seniority)
- [Hiding noise](#hiding-noise)
- [Agency or direct](#agency-or-direct)
- [Scheduling](#scheduling)
- ["I'm not getting any new results"](#im-not-getting-any-new-results)

## Commands

| Command | What it does |
|---|---|
| `jobsearch init` | Write the default config files into your config directory. Run once after installing. `--force` overwrites files you've edited. |
| `jobsearch` / `jobsearch tui` | The review TUI. Fetch (`r`), score (`e`) and triage without leaving it. |
| `jobsearch fetch` | Pull from every configured source, filter, dedup, store. Free. `--company <slug>` or `--aggregator <substring>` to run one source in isolation. |
| `jobsearch evaluate` | Score unevaluated candidates with Claude. **The only step that costs money.** `--url <URL>` scores exactly one, ignoring every queue policy and re-scoring one that already has a score. `--rescore` re-scores everything, for after you change the prompt or `profile.yaml`. `--dry-run` to see the queue, `--limit N` to cap, `--all-stages` to score beyond startups. |
| `jobsearch discover-yc` | Pull the YC directory, record stages, resolve ATS boards. |
| `jobsearch refilter` | Re-apply the current `filters.yaml` to everything already stored, without re-fetching. Run it after editing filters. |
| `jobsearch add` | Record an application by hand, for scripting. `--description-file -` reads the JD from stdin. The TUI's `a` key is the interactive version. |
| `jobsearch inspect [URL]` | Print a stored job in full, or with no URL list recent rejections and why each was dropped. |
| `jobsearch channel` | Set or clear who is advertising a posting. |
| `jobsearch tech` | What your candidates actually use, as a histogram. `--tag rust` lists the postings for one tag. |
| `jobsearch stale` | Applications that have gone quiet — anything past its `staleness` threshold, oldest first. `--all` lists every tracked application with its age. |
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
         m   your status: all / tracked / stale / dismissed / untouched /
             applied / recruiter / interview / rejected / skipped / silence
         /   search company, title and location
         S   sort: score / company / title / stage / posted / status /
             level / age
         R   reverse sort

TRIAGE   in pipeline order:
         1 applied   2 recruiter  3 interview
         4 rejected  5 skipped    6 silence   0 clear
         n   edit notes            o  open in browser

HIDE     x   hide this posting (stays hidden across refreshes)
         X   hide EVERY posting from this employer, now and future
         u   restore whatever is hiding the selected row

ENTRIES  a / i   add an application the scrapers never found
         c / E   edit the selected row
         dd / D  delete it (hand-entered rows only)

RUN      r   fetch
         e   score JUST the selected row (a fraction of a cent)
         E   score every unscored row in view — asks first
         F5  reload
         ?   help           q  quit
```

Status edits are written to SQLite on the keystroke, not batched — triage
survives a crash, and anything else reading the database sees it
immediately.

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

## Tracking where each application stands

`m` cycles the status filter: **all → tracked → stale → dismissed →
untouched → applied → recruiter → interview → rejected → skipped →
silence**.
`recruiter` is a recruiter screen — a distinct state worth tracking,
because it means a human replied, which "applied" alone doesn't. `tracked` is the application
list — everything you've acted on, whatever happened next.

`S` cycles the sort key, and `status` is one of them. It sorts by position
in the pipeline rather than alphabetically:

```
interview · recruiter · applied · silence · rejected · skipped · untouched
```

Interview first because that's the one with a date attached; untouched
last because it's the absence of a decision, and 800 unmarked rows heading
the list would be useless.

The header keeps a running tally (`4 applied · 2 interview`), so you can
see where things stand without changing filters to count them.

### How long it's been sitting

The `mine · age` column carries both the status and how long it's held it:

```
mine · age
applied 4d
recruiter 2w
interview 5w      ← red
```

Every status change is timestamped. Editing the notes is not a status
change, so annotating a row doesn't reset its clock — a row you commented
on this morning shouldn't read as freshly moved.

The age turns red once it passes the threshold for that status, set in
`filters.yaml`:

```yaml
staleness:
  interview: 21
  recruiter: 14
  applied: 28
```

Terminal statuses (`rejected`, `skipped`, `silence`) have no threshold and
never go stale — a closed door isn't waiting on anything. The defaults are
deliberately generous: a fortnight of silence after a recruiter screen is
normal, and three weeks after an onsite happens. They're the point at
which it's worth a nudge, not proof of a no.

`m` cycles to **stale** to see just those, the header shows a `3 stale`
badge when there are any, and `S` sorts by **age**, longest wait first.
Outside the TUI:

```
jobsearch stale                # everything past its threshold, oldest first
jobsearch stale --all          # every tracked application with its age
```

The detail pane shows the whole path rather than just where it landed:

```
your status: interview  ·  set 5 days ago  ·  applied 41 days ago

TIMELINE
  2025-08-12  applied     7w
  2025-09-18  recruiter   11d
  2025-09-24  interview   5d
```

"Applied two months ago, interviewed last week" and "applied last week,
interviewed yesterday" are different situations, and the current status
alone can't tell them apart.

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

## Agency or direct

Staffing agencies are a different process: you can't see the employer, the
negotiation runs through a third party, and the same role turns up three
times under different URLs. Every posting carries a channel —
**direct / agency / referral / inbound** — shown as a one-character marker
in the list (`A` `R` `I` `D`) and a banner in the detail pane.

Agency listings are guessed from the posting, and anything you set by hand
wins. Two separate pattern lists in `filters.yaml`, because *where* a
pattern matches turns out to matter:

| | |
|---|---|
| `agency_text_patterns` | Phrases in the description — "on behalf of our client", "contract-to-hire", "W2 contract". |
| `agency_company_patterns` | Agency names, matched against the **company field only**. Matching a name against the body finds customer lists: a Dialpad posting naming *"brands including Randstad, Motorola, Netflix"* was labelled an agency. |

The guess only ever produces `agency`, never `direct` — "this reads like an
agency listing" is something the text can support; "this is a direct hire"
is merely the absence of that evidence, and recording it as a positive
would turn a non-observation into a fact. Set those by hand where it
matters:

```bash
jobsearch channel --url "https://..." --set direct   # or clear with no --set
jobsearch add --company "..." --title "..." --channel referral
```

## Scheduling

Once a full local run looks right:

```cron
0 7 * * * cd /path/to/job_search_tool && ./target/release/jobsearch fetch && ./target/release/jobsearch evaluate
```

Then open the TUI when you want to triage.

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
