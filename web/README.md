# Job Search Board (Next.js) — FROZEN

> **This UI is frozen as of 2026-09-25.** It still works, and it still reads and
> writes the same `data/seen_jobs.sqlite3` the Rust tool does — so nothing you
> track here is lost. But the primary interface is now the Rust TUI (`jobsearch`,
> or `make tui` from the repo root), and new features land there only.
>
> Most visibly, this board has no concept of **company stage**, so it can't do
> startup mode: it shows every candidate regardless of stage, where the TUI
> defaults to startups only. It also can't trigger a fetch or a scoring run.
>
> Keep using it if you like the mouse and the wide screen; reach for the TUI for
> anything stage-aware. See the root `README.md`.


A local web UI over `job_search_pipeline`'s SQLite store (`../data/seen_jobs.sqlite3`).
Reads and writes the database directly — no export/import step, no separate JSON file
to keep in sync. This was the recommended way to browse jobs and track your own
`applied / interview / rejected / skipped / silence` status with notes.

## Setup

```bash
cd web
npm install
npm run dev
```

Then open http://localhost:3000. It reads `../data/seen_jobs.sqlite3` relative to the
`web/` folder — i.e. it expects `job_search_pipeline/data/seen_jobs.sqlite3` to exist,
which means you should run the pipeline (`jobsearch fetch`, then
`jobsearch evaluate`) at least once first. If the file doesn't exist yet, the app still starts and creates the
tables it needs, you'll just see an empty board.

If your `job_search_pipeline` checkout lives somewhere other than the parent of `web/`,
point at the database explicitly:

```bash
DB_PATH=/path/to/seen_jobs.sqlite3 npm run dev
```

## What it shows

Only jobs that passed the deterministic filter (`passed_filters = 1`). Each row has:

- AI status (`apply` / `consider` / `skip` / `not evaluated`) — from `ai_evaluate.py`
- Your own status (`applied` / `interview` / `rejected` / `skipped` / `silence`) —
  set inline in the table or from the detail panel. Deliberately separate from the AI's
  suggestion, since one is "should I apply" and the other is "what actually happened".
- Notes — free text, editable from the detail panel.

Filter by either status, search by company/title, sort any column, click a row to open
the full JD plus the AI's reasoning (gaps / strengths / risk factors) and edit
status+notes.

## Production build

```bash
npm run build
npm run start
```
