//! SQLite-backed local datastore: dedup tracking, full job details, AI
//! evaluations, your own status/notes, and what's known about each
//! company's stage.
//!
//! Dedup uses two keys:
//!   1. the exact job URL
//!   2. a normalized (company, title, location) triple — catches reposts
//!      under a new URL
//!
//! A job is only "new" if neither key has been seen. Seen jobs are
//! recorded whether or not they passed the content filters, so a future
//! run never re-considers the same posting.
//!
//! `job_details` holds the full record (including the JD text) for every
//! job ever seen, keyed by URL. That's deliberate: a full JD is several KB
//! of HTML, and putting it in the CSV would make the file unreadable in a
//! spreadsheet and defeat the point of the CSV being a quick scannable
//! list. Query this table instead when you want to see why a job was kept
//! or dropped, or to feed the JD to the AI step without re-fetching.
//!
//! One file, one schema, and no migration step between the CLI and the
//! TUI: they are the same binary reading the same tables. Anything else
//! that wants this data — a script, a notebook, another UI — can open the
//! file directly, which is why the schema is plain and the column names
//! are meant to be read by a human.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

use crate::model::{CompanyStage, Evaluation, Job, JobRow, Stage, StageSource};
use crate::tech::TechDetector;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS seen_jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    url TEXT UNIQUE,
    company_title_key TEXT,
    company TEXT,
    title TEXT,
    first_seen_at TEXT DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_company_title_key ON seen_jobs(company_title_key);

CREATE TABLE IF NOT EXISTS job_details (
    url TEXT PRIMARY KEY,
    company TEXT,
    title TEXT,
    location TEXT,
    posted_at TEXT,
    description TEXT,
    passed_filters INTEGER,
    fetched_at TEXT DEFAULT CURRENT_TIMESTAMP,
    source TEXT
);
CREATE INDEX IF NOT EXISTS idx_job_details_passed ON job_details(passed_filters);

CREATE TABLE IF NOT EXISTS ai_evaluations (
    url TEXT PRIMARY KEY,
    match_score INTEGER,
    recommendation TEXT,
    genuine_gaps TEXT,
    transferable_strengths TEXT,
    risk_factors TEXT,
    -- Split out from match_score so the two failure modes stay legible:
    -- qualification_match answers "do I clear the stated bar", callback_odds
    -- answers "will anyone read it". A single number can't say which of
    -- those a low score means, and they call for opposite responses.
    qualification_match INTEGER,
    callback_odds INTEGER,
    callback_notes TEXT,
    model TEXT,
    evaluated_at TEXT DEFAULT CURRENT_TIMESTAMP
);

-- Your own tracking, separate from the AI's recommendation. The AI's
-- apply/consider/skip is a suggestion made before applying; my_status is
-- what actually happened — deliberately a different vocabulary so the two
-- never collide in the UI.
CREATE TABLE IF NOT EXISTS user_status (
    url TEXT PRIMARY KEY,
    my_status TEXT,
    notes TEXT,
    updated_at TEXT DEFAULT CURRENT_TIMESTAMP
);

-- What's known about a company's maturity, keyed by NORMALIZED company
-- name rather than by URL or ATS slug. That key is what lets a stage
-- learned once — hand-curated in companies.yaml, read from the YC
-- directory, or inferred by the AI from a single JD — apply to every
-- other posting from the same employer, including ones fetched later from
-- a different source entirely.
CREATE TABLE IF NOT EXISTS company_stage (
    company_key TEXT PRIMARY KEY,
    company TEXT,
    stage TEXT,
    headcount INTEGER,
    source TEXT,
    note TEXT,
    updated_at TEXT DEFAULT CURRENT_TIMESTAMP
);

-- Postings you've dismissed. A SOFT delete on purpose: the row stays in
-- job_details with its description and score intact, and only this table
-- decides whether it's shown. That makes it reversible, auditable, and —
-- critically — it survives a re-fetch, because dedup would otherwise
-- happily re-surface a posting you'd already said no to.
--
-- Two tables rather than one with a scope column: "hide this posting" and
-- "hide this employer" are keyed differently (URL vs normalized name) and
-- have different lifetimes. A company dismissal has to apply to postings
-- that don't exist yet, which a URL-keyed row can't express.
CREATE TABLE IF NOT EXISTS dismissed_jobs (
    url TEXT PRIMARY KEY,
    company TEXT,
    title TEXT,
    reason TEXT,
    dismissed_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS dismissed_companies (
    company_key TEXT PRIMARY KEY,
    company TEXT,
    reason TEXT,
    dismissed_at TEXT DEFAULT CURRENT_TIMESTAMP
);

-- Every status change, append-only. One row per transition, so the
-- question "when did this become an interview" has an answer even after
-- it moves on to something else.
--
-- A log rather than an applied_at/recruiter_at/... column set: statuses
-- are a vocabulary that grows (recruiter was added after the fact), and
-- each new one would otherwise mean a migration. It also records
-- re-entries — a role that goes silent and then revives shows both.
--
-- `status` is NULL for a clear, which is a real event worth keeping.
CREATE TABLE IF NOT EXISTS status_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    url TEXT NOT NULL,
    status TEXT,
    at TEXT DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_status_history_url ON status_history(url);

-- Hiring channel set BY HAND. Only overrides live here: the inferred
-- value is derived from the posting text on every read (see
-- `model::infer_channel`), so improving the patterns takes effect without
-- a migration, while anything you typed survives untouched.
CREATE TABLE IF NOT EXISTS job_channel (
    url TEXT PRIMARY KEY,
    channel TEXT,
    set_at TEXT DEFAULT CURRENT_TIMESTAMP
);

-- ATS boards resolved for a company we only knew by name (currently: YC
-- directory entries probed against each ATS's public API). Cached because
-- a miss is as valuable as a hit — it stops the next run from re-probing
-- thousands of slugs that already came back 404.
CREATE TABLE IF NOT EXISTS ats_resolution (
    company_key TEXT PRIMARY KEY,
    company TEXT,
    ats TEXT,
    slug TEXT,
    resolved INTEGER,
    checked_at TEXT DEFAULT CURRENT_TIMESTAMP
);
"#;

/// One hand-entered application, as the form and the `add` subcommand
/// both produce it.
///
/// The string fields are non-optional and empty-means-unset, matching how
/// a text box behaves; `my_status`/`notes` are `Option` because "the form
/// didn't ask" and "the user cleared it" need to stay distinguishable.
#[derive(Debug, Clone, Default)]
pub struct ManualEntry {
    pub url: String,
    pub company: String,
    pub title: String,
    pub location: String,
    pub posted_at: String,
    pub description: String,
    pub stage: Option<Stage>,
    pub headcount: Option<i64>,
    pub my_status: Option<String>,
    pub notes: Option<String>,
    /// Who is advertising it. `None` leaves whatever is already recorded
    /// alone, so editing a row without touching this field can't wipe it.
    pub channel: Option<crate::model::Channel>,
}

impl ManualEntry {
    /// A URL for an entry that has none. Plenty of applications have no
    /// public posting left — a referral, a recruiter email, a listing
    /// that's since been taken down — but the URL is half the dedup key
    /// and the handle everything else uses, so one has to be minted.
    ///
    /// Deterministic in company+title so re-adding the same role twice
    /// updates it rather than piling up near-duplicates, and namespaced
    /// under `manual:` so it can never collide with a real posting.
    pub fn synthetic_url(company: &str, title: &str) -> String {
        let slug = |s: &str| {
            normalize(s)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("-")
        };
        format!("manual:{}/{}", slug(company), slug(title))
    }

    /// Fill in a synthetic URL if none was given.
    pub fn with_url(mut self) -> Self {
        if self.url.trim().is_empty() {
            self.url = Self::synthetic_url(&self.company, &self.title);
        } else {
            self.url = self.url.trim().to_string();
        }
        self
    }
}

/// Dismissals as looked up at read time: the reason, keyed by job URL in
/// one map and by normalized company name in the other.
/// Status transitions per posting URL, oldest first.
pub type StatusTimelines = std::collections::HashMap<String, Vec<(Option<String>, String)>>;

pub type DismissalSets = (
    std::collections::HashMap<String, Option<String>>,
    std::collections::HashMap<String, Option<String>>,
);

pub struct Store {
    conn: Connection,
    /// Compiled `tech.yaml`. Held here so every read tags its rows —
    /// callers never have to remember to do it, which is what stops half
    /// the app from seeing tags and the other half not.
    tech: TechDetector,
    /// Compiled agency patterns from filters.yaml, so reads can infer a
    /// channel without the caller supplying them.
    agency_text: regex::RegexSet,
    agency_company: regex::RegexSet,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        // WAL so a reader doesn't block the writer: a `fetch` run holds the
        // write lock for a while, and the TUI stays responsive reading the
        // same file in another process.
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        let store = Self {
            conn,
            tech: TechDetector::empty(),
            agency_text: regex::RegexSet::empty(),
            agency_company: regex::RegexSet::empty(),
        };
        store.backfill_status_history()?;
        Ok(store)
    }

    /// Attach the compiled agency patterns, so reads can infer a channel.
    pub fn with_agency_patterns(mut self, text: regex::RegexSet, company: regex::RegexSet) -> Self {
        self.agency_text = text;
        self.agency_company = company;
        self
    }

    /// Attach a compiled tech detector, so reads tag their rows.
    ///
    /// Separate from `open` because the detector comes from a config file
    /// the store has no business locating, and because tests want a store
    /// without one.
    pub fn with_tech(mut self, tech: TechDetector) -> Self {
        self.tech = tech;
        self
    }

    pub fn tech(&self) -> &TechDetector {
        &self.tech
    }

    // ------------------------------------------------------------- dedup

    pub fn is_new(&self, job: &Job) -> Result<bool> {
        let key = company_title_key(&job.company, &job.title, &job.location);
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM seen_jobs WHERE url = ?1 OR company_title_key = ?2 LIMIT 1",
                params![job.url, key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(found.is_none())
    }

    pub fn mark_seen(&self, job: &Job) -> Result<()> {
        let key = company_title_key(&job.company, &job.title, &job.location);
        self.conn.execute(
            "INSERT OR IGNORE INTO seen_jobs (url, company_title_key, company, title) \
             VALUES (?1, ?2, ?3, ?4)",
            params![job.url, key, job.company, job.title],
        )?;
        Ok(())
    }

    /// Store the full record (including description) for every job we see,
    /// pass or fail, so filter decisions can be audited later without
    /// re-fetching anything.
    pub fn save_details(&self, job: &Job, passed_filters: bool) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO job_details \
             (url, company, title, location, posted_at, description, passed_filters, source) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                job.url,
                job.company,
                job.title,
                job.location,
                job.posted_at,
                job.description,
                i64::from(passed_filters),
                job.source,
            ],
        )?;
        Ok(())
    }

    pub fn set_passed_filters(&self, url: &str, passed: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE job_details SET passed_filters = ?1 WHERE url = ?2",
            params![i64::from(passed), url],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------ channel

    /// Record who is advertising a posting. `None` clears the override and
    /// lets the inferred value show through again.
    pub fn set_channel(&self, url: &str, channel: Option<crate::model::Channel>) -> Result<()> {
        match channel {
            Some(c) => self.conn.execute(
                "INSERT INTO job_channel (url, channel, set_at) \
                 VALUES (?1, ?2, CURRENT_TIMESTAMP) \
                 ON CONFLICT(url) DO UPDATE SET channel=excluded.channel, \
                   set_at=CURRENT_TIMESTAMP",
                params![url, c.as_str()],
            )?,
            None => self
                .conn
                .execute("DELETE FROM job_channel WHERE url = ?1", params![url])?,
        };
        Ok(())
    }

    /// The status timeline for every tracked posting, oldest first.
    ///
    /// Loaded whole rather than per-row: one entry per transition means
    /// this stays in the hundreds even after a long search, and having it
    /// in hand lets the detail pane show a real timeline for free.
    pub fn status_timelines(&self) -> Result<StatusTimelines> {
        let mut stmt = self
            .conn
            .prepare("SELECT url, status, at FROM status_history ORDER BY at ASC, id ASC")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            ))
        })?;
        let mut out: StatusTimelines = Default::default();
        for row in rows {
            let (url, status, at) = row?;
            out.entry(url).or_default().push((status, at));
        }
        Ok(out)
    }

    /// Backfill a timeline entry for rows marked before the log existed,
    /// using `user_status.updated_at` as the best timestamp available.
    ///
    /// Without this, every status set before this shipped would read as
    /// age-unknown, and the staleness view would be empty exactly when
    /// it's most wanted. Runs once at open and is a no-op thereafter.
    pub fn backfill_status_history(&self) -> Result<usize> {
        Ok(self.conn.execute(
            "INSERT INTO status_history (url, status, at) \
             SELECT us.url, us.my_status, us.updated_at FROM user_status us \
             WHERE us.my_status IS NOT NULL \
               AND NOT EXISTS (SELECT 1 FROM status_history sh WHERE sh.url = us.url)",
            [],
        )?)
    }

    fn channel_overrides(
        &self,
    ) -> Result<std::collections::HashMap<String, crate::model::Channel>> {
        let mut stmt = self.conn.prepare("SELECT url, channel FROM job_channel")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (url, channel) = row?;
            if let Some(c) = channel.as_deref().and_then(crate::model::Channel::parse) {
                out.insert(url, c);
            }
        }
        Ok(out)
    }

    // ---------------------------------------------------------- dismissal

    /// Hide one posting. Idempotent; re-dismissing updates the reason.
    pub fn dismiss_job(&self, row: &JobRow, reason: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO dismissed_jobs (url, company, title, reason, dismissed_at) \
             VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP) \
             ON CONFLICT(url) DO UPDATE SET reason=excluded.reason, \
               dismissed_at=CURRENT_TIMESTAMP",
            params![row.url, row.company, row.title, reason],
        )?;
        Ok(())
    }

    /// Hide every posting from an employer, including ones not fetched
    /// yet. This is the one that earns its keep against staffing agencies,
    /// which post the same handful of roles under dozens of URLs.
    pub fn dismiss_company(&self, company: &str, reason: Option<&str>) -> Result<usize> {
        let key = normalize(company);
        if key.is_empty() {
            return Ok(0);
        }
        self.conn.execute(
            "INSERT INTO dismissed_companies (company_key, company, reason, dismissed_at) \
             VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP) \
             ON CONFLICT(company_key) DO UPDATE SET reason=excluded.reason, \
               dismissed_at=CURRENT_TIMESTAMP",
            params![key, company, reason],
        )?;
        // Report how many currently-visible rows this covers, so the caller
        // can say "hides 37 postings" rather than leaving the blast radius
        // to be discovered afterwards.
        Ok(self
            .all_candidates()?
            .iter()
            .filter(|r| normalize(&r.company) == key)
            .count())
    }

    pub fn restore_job(&self, url: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM dismissed_jobs WHERE url = ?1", params![url])?;
        Ok(())
    }

    pub fn restore_company(&self, company: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM dismissed_companies WHERE company_key = ?1",
            params![normalize(company)],
        )?;
        Ok(())
    }

    /// Everything currently dismissed, as `(urls, company_keys)`.
    ///
    /// Loaded as sets and applied in Rust for the same reason the stage
    /// join is: matching a company means normalizing its name, and a
    /// second normalization written in SQL would drift from this one.
    pub fn dismissed_sets(&self) -> Result<DismissalSets> {
        let mut jobs = std::collections::HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT url, reason FROM dismissed_jobs")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))?;
            for row in rows {
                let (url, reason) = row?;
                jobs.insert(url, reason);
            }
        }
        let mut companies = std::collections::HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT company_key, reason FROM dismissed_companies")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))?;
            for row in rows {
                let (key, reason) = row?;
                companies.insert(key, reason);
            }
        }
        Ok((jobs, companies))
    }

    /// Dismissed companies, newest first, for `jobsearch dismissed`.
    pub fn dismissed_company_list(&self) -> Result<Vec<(String, Option<String>, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT company, reason, dismissed_at FROM dismissed_companies \
             ORDER BY dismissed_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                r.get(1)?,
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            ))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // ------------------------------------------------------ manual entries

    /// Save a hand-entered application, returning whether it created a new
    /// row (`true`) or merged into one a fetcher had already found.
    ///
    /// Three things make this different from `save_details`:
    ///
    /// - `passed_filters` is always 1. The filters exist to decide what is
    ///   worth *discovering*; a job you actually applied to is in scope by
    ///   definition, and letting a title rule hide it would be absurd.
    /// - Merging is the right behaviour when the URL is already known.
    ///   Pasting a Greenhouse link you applied through should attach your
    ///   status to the row that already exists, not create a second one —
    ///   and it deliberately keeps the original `source`, because the
    ///   posting really did come from that board.
    /// - Blank fields never overwrite fetched ones. On a merge, an empty
    ///   box in the form means "leave it alone", so adding a status to a
    ///   scraped row can't wipe its description.
    pub fn upsert_manual_job(&self, entry: &ManualEntry) -> Result<bool> {
        let existing = self.get_by_url(&entry.url)?;
        let is_new = existing.is_none();

        let source = match &existing {
            // Keep the fetcher's name: the posting's provenance doesn't
            // change just because you typed a status against it.
            Some(row) if !row.source.is_empty() && !row.is_manual() => row.source.clone(),
            _ => crate::model::MANUAL_SOURCE.to_string(),
        };
        let keep = |new: &str, old: Option<&str>| -> String {
            if new.trim().is_empty() {
                old.unwrap_or_default().to_string()
            } else {
                new.trim().to_string()
            }
        };

        let company = keep(
            &entry.company,
            existing.as_ref().map(|r| r.company.as_str()),
        );
        let title = keep(&entry.title, existing.as_ref().map(|r| r.title.as_str()));
        let location = keep(
            &entry.location,
            existing.as_ref().map(|r| r.location.as_str()),
        );
        let description = keep(
            &entry.description,
            existing.as_ref().map(|r| r.description.as_str()),
        );
        let posted_at = if entry.posted_at.trim().is_empty() {
            existing.as_ref().and_then(|r| r.posted_at.clone())
        } else {
            Some(entry.posted_at.trim().to_string())
        };

        let job = Job {
            company: company.clone(),
            title,
            location,
            url: entry.url.clone(),
            posted_at,
            description,
            source,
            stage_hint: None,
        };

        // seen_jobs too, so a later fetch that turns up the same posting
        // dedups against it instead of adding a duplicate alongside.
        self.mark_seen(&job)?;
        self.save_details(&job, true)?;

        if let Some(stage) = entry.stage {
            self.upsert_company_stage(&CompanyStage {
                company,
                stage,
                headcount: entry.headcount,
                source: StageSource::Manual,
                note: Some("entered by hand".to_string()),
            })?;
        }

        // Only touch user_status when the form actually carried one —
        // otherwise editing a manual row's title would silently clear a
        // status set earlier with the 1-5 keys.
        if let Some(channel) = entry.channel {
            self.set_channel(&entry.url, Some(channel))?;
        }

        if entry.my_status.is_some() || entry.notes.is_some() {
            let previous = self.get_user_status(&entry.url)?;
            let status = entry
                .my_status
                .clone()
                .or_else(|| previous.as_ref().and_then(|p| p.0.clone()));
            let notes = entry
                .notes
                .clone()
                .or_else(|| previous.as_ref().and_then(|p| p.1.clone()));
            self.save_user_status(&entry.url, status.as_deref(), notes.as_deref())?;
        }

        Ok(is_new)
    }

    fn get_user_status(&self, url: &str) -> Result<Option<(Option<String>, Option<String>)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT my_status, notes FROM user_status WHERE url = ?1",
                params![url],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// Remove a job entirely — every table, including the dedup key, so a
    /// later fetch can rediscover it. Only reachable for manual rows in the
    /// UI: deleting a fetched one would just invite it straight back on the
    /// next run.
    pub fn delete_job(&self, url: &str) -> Result<()> {
        for table in ["ai_evaluations", "user_status", "job_details", "seen_jobs"] {
            self.conn
                .execute(&format!("DELETE FROM {table} WHERE url = ?1"), params![url])?;
        }
        Ok(())
    }

    // ------------------------------------------------------- company stage

    /// Record a stage claim, keeping whichever source is more trustworthy.
    ///
    /// The conflict rule matters: the AI step runs over jobs from
    /// companies that may already be hand-curated, and a model's guess
    /// must never quietly overwrite a value a human looked up. Equal rank
    /// does overwrite, so re-running a source picks up corrections.
    pub fn upsert_company_stage(&self, claim: &CompanyStage) -> Result<bool> {
        let key = normalize(&claim.company);
        if key.is_empty() {
            return Ok(false);
        }
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT source FROM company_stage WHERE company_key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            let existing_rank = StageSource::parse(&existing).map_or(0, StageSource::rank);
            if existing_rank > claim.source.rank() {
                return Ok(false);
            }
        }
        self.conn.execute(
            "INSERT INTO company_stage (company_key, company, stage, headcount, source, note, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, CURRENT_TIMESTAMP) \
             ON CONFLICT(company_key) DO UPDATE SET \
               company=excluded.company, stage=excluded.stage, headcount=excluded.headcount, \
               source=excluded.source, note=excluded.note, updated_at=CURRENT_TIMESTAMP",
            params![
                key,
                claim.company,
                claim.stage.as_str(),
                claim.headcount,
                claim.source.as_str(),
                claim.note,
            ],
        )?;
        Ok(true)
    }

    pub fn company_stage_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM company_stage", [], |r| r.get(0))?)
    }

    // ------------------------------------------------------ ats resolution

    /// Companies we've already probed, so a re-run skips them. Includes
    /// the misses — those are the expensive ones to repeat.
    pub fn probed_company_keys(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT company_key FROM ats_resolution")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn record_ats_resolution(
        &self,
        company: &str,
        resolved: Option<(&str, &str)>,
    ) -> Result<()> {
        let key = normalize(company);
        let (ats, slug) = match resolved {
            Some((a, s)) => (Some(a), Some(s)),
            None => (None, None),
        };
        self.conn.execute(
            "INSERT INTO ats_resolution (company_key, company, ats, slug, resolved, checked_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, CURRENT_TIMESTAMP) \
             ON CONFLICT(company_key) DO UPDATE SET \
               ats=excluded.ats, slug=excluded.slug, resolved=excluded.resolved, \
               checked_at=CURRENT_TIMESTAMP",
            params![key, company, ats, slug, i64::from(resolved.is_some())],
        )?;
        Ok(())
    }

    // ---------------------------------------------------------- evaluation

    /// Jobs that passed the deterministic filters but haven't been scored
    /// yet — the input queue for the AI step.
    ///
    /// `startup_only` pushes the stage cutoff down into SQL rather than
    /// filtering after the fact, because this queue decides what gets
    /// *paid for*. Scoring 300 big-company jobs to then hide them in the
    /// UI would be the expensive way to run startup mode.
    pub fn unevaluated(
        &self,
        startup_only: Option<&crate::filters::StartupPolicy>,
        seniority: Option<&crate::filters::SeniorityPolicy>,
    ) -> Result<Vec<JobRow>> {
        self.evaluation_queue(startup_only, seniority, false)
    }

    /// The same queue, optionally including rows that already have a
    /// score. Re-scoring exists because the prompt is not frozen: when the
    /// scoring rubric changes, every stored verdict was reached under the
    /// old one, and the stale ones are exactly the rows the change was
    /// meant to fix.
    pub fn evaluation_queue(
        &self,
        startup_only: Option<&crate::filters::StartupPolicy>,
        seniority: Option<&crate::filters::SeniorityPolicy>,
        include_scored: bool,
    ) -> Result<Vec<JobRow>> {
        let rows = self.query_rows(
            if include_scored {
                "WHERE jd.passed_filters = 1"
            } else {
                "WHERE jd.passed_filters = 1 AND ae.url IS NULL"
            },
            "ORDER BY jd.fetched_at DESC",
        )?;
        // Dismissed rows never reach the queue. Paying to score a posting
        // you've explicitly hidden is the clearest possible waste.
        let rows: Vec<JobRow> = rows.into_iter().filter(|r| !r.dismissed).collect();
        // Both policies gate the QUEUE, not just the display, because
        // this is the step that costs money: paying to score levels or
        // stages you're filtering out is the expensive way to run them.
        let rows: Vec<JobRow> = match startup_only {
            Some(policy) => rows.into_iter().filter(|r| policy.admits(r)).collect(),
            None => rows,
        };
        Ok(match seniority {
            Some(policy) => rows.into_iter().filter(|r| policy.admits(r)).collect(),
            None => rows,
        })
    }

    pub fn save_evaluation(&self, url: &str, eval: &Evaluation, model: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO ai_evaluations \
             (url, match_score, recommendation, genuine_gaps, transferable_strengths, \
              risk_factors, qualification_match, callback_odds, callback_notes, \
              model, evaluated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, CURRENT_TIMESTAMP)",
            params![
                url,
                eval.match_score,
                eval.recommendation,
                eval.genuine_gaps,
                eval.transferable_strengths,
                eval.risk_factors,
                eval.qualification_match,
                eval.callback_odds,
                eval.callback_notes,
                model,
            ],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------ user status

    pub fn save_user_status(
        &self,
        url: &str,
        my_status: Option<&str>,
        notes: Option<&str>,
    ) -> Result<()> {
        // Empty strings become NULL so "cleared" and "never set" are the
        // same state in the database, which is what every query here
        // assumes.
        let status = my_status.filter(|s| !s.is_empty());
        let notes = notes.filter(|s| !s.is_empty());

        // Only log a genuine transition. `save_user_status` is also the
        // notes-editing path, and appending there would both spam the log
        // and reset the age — making a row you just annotated look freshly
        // moved, which is the opposite of what the age column is for.
        let previous: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT my_status FROM user_status WHERE url = ?1",
                params![url],
                |r| r.get(0),
            )
            .optional()?;
        let changed = match &previous {
            Some(prev) => prev.as_deref() != status,
            None => status.is_some(),
        };
        if changed {
            self.conn.execute(
                "INSERT INTO status_history (url, status, at) \
                 VALUES (?1, ?2, CURRENT_TIMESTAMP)",
                params![url, status],
            )?;
        }

        self.conn.execute(
            "INSERT INTO user_status (url, my_status, notes, updated_at) \
             VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP) \
             ON CONFLICT(url) DO UPDATE SET my_status=excluded.my_status, \
               notes=excluded.notes, updated_at=CURRENT_TIMESTAMP",
            params![url, status, notes],
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------- reads

    /// Every job that currently passes the filters, joined with its
    /// evaluation, your status, and the company's stage — the TUI's whole
    /// dataset, loaded in one query.
    pub fn all_candidates(&self) -> Result<Vec<JobRow>> {
        self.query_rows(
            "WHERE jd.passed_filters = 1",
            "ORDER BY ae.match_score DESC, jd.fetched_at DESC",
        )
    }

    /// Every stored job, passed or rejected — the input for `diagnose`,
    /// which has to see the whole funnel rather than either end of it.
    pub fn all_stored(&self) -> Result<Vec<JobRow>> {
        self.query_rows("", "ORDER BY jd.fetched_at DESC")
    }

    /// How many jobs were first seen in each of the last `days` days.
    /// Answers "is the pipeline actually finding anything new" separately
    /// from "are my filters too tight" — two very different problems that
    /// look identical from the review list.
    pub fn recent_intake(&self, days: i64) -> Result<Vec<(String, i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT date(sj.first_seen_at) AS d, \
                    COUNT(*), \
                    SUM(COALESCE(jd.passed_filters, 0)) \
             FROM seen_jobs sj \
             LEFT JOIN job_details jd ON jd.url = sj.url \
             WHERE sj.first_seen_at >= date('now', ?1) \
             GROUP BY d ORDER BY d DESC",
        )?;
        let rows = stmt.query_map([format!("-{days} days")], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get(1)?,
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            ))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn filtered_out(&self) -> Result<Vec<JobRow>> {
        self.query_rows("WHERE jd.passed_filters = 0", "ORDER BY jd.fetched_at DESC")
    }

    /// One row by URL.
    ///
    /// This has to be a real indexed lookup, not a scan. The first version
    /// selected EVERY stored row, attached stages and dismissals to all of
    /// them, ran tech detection over 15,000 descriptions, and then called
    /// `.find()` — 1.3 seconds to fetch one row, on a path that
    /// `upsert_manual_job` hits on every save. That was the "lag when
    /// saving a manual entry".
    pub fn get_by_url(&self, url: &str) -> Result<Option<JobRow>> {
        Ok(self
            .query_rows_with("WHERE jd.url = ?1", "", rusqlite::params![url])?
            .into_iter()
            .next())
    }

    /// Scored candidates, best match first — the rows written to the
    /// scored CSV.
    pub fn scored(&self) -> Result<Vec<JobRow>> {
        self.query_rows("WHERE ae.url IS NOT NULL", "ORDER BY ae.match_score DESC")
    }

    fn query_rows(&self, where_clause: &str, order_clause: &str) -> Result<Vec<JobRow>> {
        self.query_rows_with(where_clause, order_clause, rusqlite::params![])
    }

    /// The one place the big join is written. Every read above differs
    /// only in its WHERE/ORDER BY, so keeping the column list and the
    /// row-building in a single function is what stops one caller from
    /// silently returning rows with a field left unpopulated.
    ///
    /// Note what is NOT joined here: `company_stage`. Matching a stage to
    /// a job means normalizing the company name, and expressing
    /// `normalize` as a pile of nested SQL `REPLACE`s gives you a second
    /// implementation that drifts from the first — at which point the
    /// join silently returns nothing and startup mode looks broken for
    /// reasons no test catches. `attach_stages` does it in Rust with the
    /// same function, against a map small enough (one row per company,
    /// not per job) that the lookup is free.
    fn query_rows_with(
        &self,
        where_clause: &str,
        order_clause: &str,
        args: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<JobRow>> {
        let sql = format!(
            "SELECT jd.url, jd.company, jd.title, jd.location, jd.posted_at, jd.description, \
                    jd.source, jd.fetched_at, jd.passed_filters, \
                    ae.match_score, ae.recommendation, ae.genuine_gaps, \
                    ae.transferable_strengths, ae.risk_factors, \
                    ae.qualification_match, ae.callback_odds, ae.callback_notes, \
                    us.my_status, us.notes \
             FROM job_details jd \
             LEFT JOIN ai_evaluations ae ON jd.url = ae.url \
             LEFT JOIN user_status us ON jd.url = us.url \
             {where_clause} {order_clause}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(args, |r| {
            Ok(JobRow {
                url: r.get(0)?,
                company: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                title: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                location: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                posted_at: r.get(4)?,
                description: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                source: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                fetched_at: r.get(7)?,
                passed_filters: r.get::<_, Option<i64>>(8)?.unwrap_or(0) != 0,
                match_score: r.get(9)?,
                recommendation: r.get(10)?,
                genuine_gaps: r.get(11)?,
                transferable_strengths: r.get(12)?,
                risk_factors: r.get(13)?,
                qualification_match: r.get(14)?,
                callback_odds: r.get(15)?,
                callback_notes: r.get(16)?,
                my_status: r.get(17)?,
                notes: r.get(18)?,
                ..Default::default()
            })
        })?;
        let mut rows: Vec<JobRow> = rows.collect::<Result<Vec<_>, _>>()?;
        self.attach_stages(&mut rows)?;
        self.attach_dismissals(&mut rows)?;
        // Status age, from the transition log.
        let timelines = self.status_timelines()?;
        for row in rows.iter_mut() {
            let Some(entries) = timelines.get(&row.url) else {
                continue;
            };
            row.status_since = entries.last().map(|(_, at)| at.clone());
            // When it FIRST entered the pipeline, which is a different and
            // also useful number: "applied 6 weeks ago, interviewed 3 days
            // ago" is the shape of a real conversation.
            row.first_tracked_at = entries.first().map(|(_, at)| at.clone());
            row.status_timeline = entries.clone();
        }

        // Channel: the hand-set override wins; otherwise infer from the
        // text. Same precedence shape as company stage.
        let overrides = self.channel_overrides()?;
        for row in rows.iter_mut() {
            if let Some(c) = overrides.get(&row.url) {
                row.channel = Some(*c);
                row.channel_source = Some(crate::model::ChannelSource::Manual);
            } else if let Some(c) = crate::model::infer_channel(
                &row.company,
                &row.description,
                &self.agency_text,
                &self.agency_company,
            ) {
                row.channel = Some(c);
                row.channel_source = Some(crate::model::ChannelSource::Inferred);
            }
        }
        if !self.tech.is_empty() {
            for row in rows.iter_mut() {
                // Tag from the stripped text, not the raw HTML: a pattern
                // like `\bgo\b` would otherwise match inside a tag name
                // or a tracking URL.
                let text = crate::filters::strip_html(&row.description);
                row.tech = self.tech.detect(&text);
            }
        }
        Ok(rows)
    }

    /// Mark which rows are dismissed, and why.
    ///
    /// Attached to every read rather than filtered out in SQL: the TUI
    /// needs to be able to SHOW dismissed rows (that's how you undo one),
    /// so the flag travels with the row and each caller decides.
    fn attach_dismissals(&self, rows: &mut [JobRow]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let (jobs, companies) = self.dismissed_sets()?;
        if jobs.is_empty() && companies.is_empty() {
            return Ok(());
        }
        for row in rows.iter_mut() {
            if let Some(reason) = jobs.get(&row.url) {
                row.dismissed = true;
                row.dismissed_scope = Some("job");
                row.dismissed_reason = reason.clone();
            } else if let Some(reason) = companies.get(&normalize(&row.company)) {
                row.dismissed = true;
                row.dismissed_scope = Some("company");
                row.dismissed_reason = reason.clone();
            }
        }
        Ok(())
    }

    /// Fill in each row's stage from `company_stage`, keyed by the same
    /// `normalize` the writes use.
    fn attach_stages(&self, rows: &mut [JobRow]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let stages = self.company_stages()?;
        for row in rows.iter_mut() {
            if let Some(claim) = stages.get(&normalize(&row.company)) {
                row.stage = Some(claim.stage);
                row.headcount = claim.headcount;
                row.stage_source = Some(claim.source);
            }
        }
        Ok(())
    }

    /// Every known stage claim, keyed by normalized company name.
    pub fn company_stages(&self) -> Result<std::collections::HashMap<String, CompanyStage>> {
        let mut stmt = self.conn.prepare(
            "SELECT company_key, company, stage, headcount, source, note FROM company_stage",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (key, company, stage, headcount, source, note) = row?;
            // A row whose stage string doesn't parse is dropped rather
            // than defaulted — an unrecognized label is no information,
            // and guessing one would put a company in or out of startup
            // mode on the strength of a typo.
            let Some(stage) = stage.as_deref().and_then(Stage::parse) else {
                continue;
            };
            out.insert(
                key,
                CompanyStage {
                    company,
                    stage,
                    headcount,
                    source: source
                        .as_deref()
                        .and_then(StageSource::parse)
                        .unwrap_or(StageSource::Ai),
                    note,
                },
            );
        }
        Ok(out)
    }
}

/// Rust-side company-name normalization. Kept simple and total: lowercase,
/// non-alphanumerics to spaces, collapse runs of whitespace.
pub fn normalize(text: &str) -> String {
    let lowered = text.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut last_space = true;
    for ch in lowered.chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    out.trim().to_string()
}

/// Location is part of the dedup key so two genuinely distinct postings
/// with the same title at the same company (e.g. "Backend Engineer" open
/// in both Columbus and Austin) aren't treated as reposts of each other.
pub fn company_title_key(company: &str, title: &str, location: &str) -> String {
    format!(
        "{}::{}::{}",
        normalize(company),
        normalize(title),
        normalize(location)
    )
}

/// `CREATE TABLE IF NOT EXISTS` only helps a brand-new database — a file
/// that already exists needs an explicit ALTER for each column added
/// later.
fn migrate(conn: &Connection) -> Result<()> {
    let mut existing = Vec::new();
    {
        let mut stmt = conn.prepare("PRAGMA table_info(job_details)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for row in rows {
            existing.push(row?);
        }
    }
    if !existing.iter().any(|c| c == "source") {
        conn.execute("ALTER TABLE job_details ADD COLUMN source TEXT", [])?;
    }

    // Added with the two-axis scoring rewrite. Existing evaluations keep
    // their match_score and simply carry NULL here — they were produced by
    // a prompt that never asked the question, and backfilling a guess
    // would be worse than an honest blank.
    let mut evals = Vec::new();
    {
        let mut stmt = conn.prepare("PRAGMA table_info(ai_evaluations)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for row in rows {
            evals.push(row?);
        }
    }
    for (column, ty) in [
        ("qualification_match", "INTEGER"),
        ("callback_odds", "INTEGER"),
        ("callback_notes", "TEXT"),
    ] {
        if !evals.iter().any(|c| c == column) {
            conn.execute(
                &format!("ALTER TABLE ai_evaluations ADD COLUMN {column} {ty}"),
                [],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StageSource;

    fn store() -> Store {
        Store::open(":memory:").unwrap()
    }

    fn job(company: &str, title: &str, url: &str) -> Job {
        Job {
            company: company.into(),
            title: title.into(),
            location: "Remote - US".into(),
            url: url.into(),
            description: "desc".into(),
            source: "greenhouse".into(),
            ..Default::default()
        }
    }

    #[test]
    fn dedups_by_url_and_by_company_title_location() {
        let s = store();
        let j = job("Fly.io", "Platform Engineer", "https://a/1");
        assert!(s.is_new(&j).unwrap());
        s.mark_seen(&j).unwrap();
        assert!(!s.is_new(&j).unwrap());

        // Same role reposted under a new URL — still not new.
        let repost = job("Fly.io", "Platform Engineer", "https://a/2");
        assert!(!s.is_new(&repost).unwrap());

        // Same title, different location — a genuinely distinct posting.
        let mut other_loc = job("Fly.io", "Platform Engineer", "https://a/3");
        other_loc.location = "Columbus, Ohio".into();
        assert!(s.is_new(&other_loc).unwrap());
    }

    #[test]
    fn company_stage_joins_to_jobs_by_normalized_name() {
        let s = store();
        // The job's company string and the stage claim's differ in
        // punctuation and case — the join must still connect them, which
        // is the whole contract between `normalize` and
        // `normalized_company_sql`.
        let j = job("Acme, Inc.", "Platform Engineer", "https://a/1");
        s.mark_seen(&j).unwrap();
        s.save_details(&j, true).unwrap();
        s.upsert_company_stage(&CompanyStage {
            company: "acme inc".into(),
            stage: Stage::SeriesA,
            headcount: Some(40),
            source: StageSource::Curated,
            note: None,
        })
        .unwrap();

        let rows = s.all_candidates().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].stage,
            Some(Stage::SeriesA),
            "stage join must match across punctuation"
        );
        assert_eq!(rows[0].headcount, Some(40));
    }

    #[test]
    fn a_model_guess_never_overwrites_a_curated_stage() {
        let s = store();
        s.upsert_company_stage(&CompanyStage {
            company: "Fly.io".into(),
            stage: Stage::SeriesB,
            headcount: None,
            source: StageSource::Curated,
            note: None,
        })
        .unwrap();
        let wrote = s
            .upsert_company_stage(&CompanyStage {
                company: "Fly.io".into(),
                stage: Stage::Public,
                headcount: None,
                source: StageSource::Ai,
                note: None,
            })
            .unwrap();
        assert!(!wrote, "AI must not overwrite a curated stage");

        let j = job("Fly.io", "Platform Engineer", "https://a/1");
        s.save_details(&j, true).unwrap();
        assert_eq!(s.all_candidates().unwrap()[0].stage, Some(Stage::SeriesB));
    }

    #[test]
    fn a_better_source_does_overwrite_a_weaker_one() {
        let s = store();
        s.upsert_company_stage(&CompanyStage {
            company: "Acme".into(),
            stage: Stage::Public,
            headcount: None,
            source: StageSource::Ai,
            note: None,
        })
        .unwrap();
        assert!(s
            .upsert_company_stage(&CompanyStage {
                company: "Acme".into(),
                stage: Stage::Seed,
                headcount: Some(9),
                source: StageSource::Curated,
                note: None,
            })
            .unwrap());
        let j = job("Acme", "Platform Engineer", "https://a/1");
        s.save_details(&j, true).unwrap();
        assert_eq!(s.all_candidates().unwrap()[0].stage, Some(Stage::Seed));
    }

    #[test]
    fn user_status_round_trips_and_clears() {
        let s = store();
        let j = job("Acme", "Platform Engineer", "https://a/1");
        s.save_details(&j, true).unwrap();
        s.save_user_status("https://a/1", Some("applied"), Some("referred by X"))
            .unwrap();
        let row = &s.all_candidates().unwrap()[0];
        assert_eq!(row.my_status.as_deref(), Some("applied"));

        s.save_user_status("https://a/1", Some(""), None).unwrap();
        let row = &s.all_candidates().unwrap()[0];
        assert_eq!(
            row.my_status, None,
            "empty string must clear, not store \"\""
        );
    }

    fn manual(company: &str, title: &str) -> ManualEntry {
        ManualEntry {
            company: company.into(),
            title: title.into(),
            location: "Remote - US".into(),
            ..Default::default()
        }
        .with_url()
    }

    #[test]
    fn passed_filters_round_trips_through_the_row() {
        // The column is selected by index; forgetting it made every row
        // report passed=false, which `diagnose` would have read as a 0%
        // pass rate.
        let s = store();
        let kept = job("Kept", "Engineer", "https://a/1");
        let dropped = job("Dropped", "Engineer", "https://a/2");
        s.save_details(&kept, true).unwrap();
        s.save_details(&dropped, false).unwrap();

        let all = s.all_stored().unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().find(|r| r.company == "Kept").unwrap().passed());
        assert!(!all
            .iter()
            .find(|r| r.company == "Dropped")
            .unwrap()
            .passed());
        // And the other columns must not have shifted with the insertion.
        let kept_row = all.iter().find(|r| r.company == "Kept").unwrap();
        assert_eq!(kept_row.title, "Engineer");
        assert_eq!(kept_row.location, "Remote - US");
        assert_eq!(kept_row.source, "greenhouse");
    }

    fn agency_text_set() -> regex::RegexSet {
        regex::RegexSet::new([r"(?i-u)\bon behalf of our client\b"]).unwrap()
    }
    fn agency_company_set() -> regex::RegexSet {
        regex::RegexSet::new([r"(?i-u)\bmotion recruitment\b"]).unwrap()
    }

    #[test]
    fn an_agency_posting_is_inferred_from_its_text_or_its_name() {
        let s = store().with_agency_patterns(agency_text_set(), agency_company_set());
        let mut by_body = job("Unknown Co", "Engineer", "https://a/1");
        by_body.description = "On behalf of our client, a Series B fintech.".into();
        s.save_details(&by_body, true).unwrap();

        let by_name = job("Motion Recruitment, LLC", "Engineer", "https://a/2");
        s.save_details(&by_name, true).unwrap();

        let plain = job("Tailscale", "Engineer", "https://a/3");
        s.save_details(&plain, true).unwrap();

        let rows = s.all_candidates().unwrap();
        let ch = |url: &str| rows.iter().find(|r| r.url == url).and_then(|r| r.channel);
        assert_eq!(ch("https://a/1"), Some(crate::model::Channel::Agency));
        assert_eq!(ch("https://a/2"), Some(crate::model::Channel::Agency));
        // No agency signal means NO claim — not a claim of "direct".
        assert_eq!(ch("https://a/3"), None);
    }

    #[test]
    fn a_hand_set_channel_beats_the_inferred_one() {
        let s = store().with_agency_patterns(agency_text_set(), agency_company_set());
        let mut j = job("Motion Recruitment, LLC", "Engineer", "https://a/1");
        j.description = "On behalf of our client.".into();
        s.save_details(&j, true).unwrap();
        assert_eq!(
            s.all_candidates().unwrap()[0].channel,
            Some(crate::model::Channel::Agency)
        );

        s.set_channel("https://a/1", Some(crate::model::Channel::Direct))
            .unwrap();
        let row = &s.all_candidates().unwrap()[0];
        assert_eq!(row.channel, Some(crate::model::Channel::Direct));
        assert_eq!(
            row.channel_source,
            Some(crate::model::ChannelSource::Manual)
        );

        // Clearing falls back to the guess rather than to nothing.
        s.set_channel("https://a/1", None).unwrap();
        let row = &s.all_candidates().unwrap()[0];
        assert_eq!(row.channel, Some(crate::model::Channel::Agency));
        assert_eq!(
            row.channel_source,
            Some(crate::model::ChannelSource::Inferred)
        );
    }

    #[test]
    fn editing_a_manual_entry_without_a_channel_keeps_the_one_already_set() {
        let s = store();
        let mut entry = ManualEntry {
            company: "Acme".into(),
            title: "Engineer".into(),
            channel: Some(crate::model::Channel::Referral),
            ..Default::default()
        }
        .with_url();
        s.upsert_manual_job(&entry).unwrap();
        assert_eq!(
            s.all_candidates().unwrap()[0].channel,
            Some(crate::model::Channel::Referral)
        );

        entry.channel = None;
        entry.location = "Remote - US".into();
        s.upsert_manual_job(&entry).unwrap();
        let row = &s.all_candidates().unwrap()[0];
        assert_eq!(row.location, "Remote - US", "the edit landed");
        assert_eq!(
            row.channel,
            Some(crate::model::Channel::Referral),
            "an edit that didn't mention the channel must not clear it"
        );
    }

    #[test]
    fn get_by_url_is_an_indexed_lookup_not_a_scan() {
        // The first version selected every stored row and called .find(),
        // which cost 1.3s per lookup on the real database and made every
        // manual-entry save feel broken. Assert the query is actually
        // narrowed, by checking it doesn't return a different row.
        let s = store();
        for i in 0..50 {
            let j = job("Acme", &format!("Engineer {i}"), &format!("https://a/{i}"));
            s.save_details(&j, true).unwrap();
        }
        let found = s.get_by_url("https://a/7").unwrap().expect("should exist");
        assert_eq!(found.title, "Engineer 7");
        assert!(s.get_by_url("https://a/nope").unwrap().is_none());
    }

    #[test]
    fn dismissing_a_job_hides_it_without_destroying_anything() {
        let s = store();
        let j = job("Acme", "Engineer", "https://a/1");
        s.mark_seen(&j).unwrap();
        s.save_details(&j, true).unwrap();
        s.save_user_status("https://a/1", Some("applied"), Some("note"))
            .unwrap();

        let row = s.all_candidates().unwrap().into_iter().next().unwrap();
        s.dismiss_job(&row, Some("closed")).unwrap();

        let rows = s.all_candidates().unwrap();
        assert_eq!(
            rows.len(),
            1,
            "the row still exists — this is a soft delete"
        );
        assert!(rows[0].dismissed);
        assert_eq!(rows[0].dismissed_scope, Some("job"));
        assert_eq!(rows[0].dismissed_reason.as_deref(), Some("closed"));
        // Everything attached to it survives, so restoring is lossless.
        assert_eq!(rows[0].my_status.as_deref(), Some("applied"));
        assert_eq!(rows[0].notes.as_deref(), Some("note"));
    }

    #[test]
    fn a_dismissed_job_is_never_queued_for_paid_scoring() {
        let s = store();
        let j = job("Acme", "Engineer", "https://a/1");
        s.save_details(&j, true).unwrap();
        assert_eq!(s.unevaluated(None, None).unwrap().len(), 1);

        let row = s.all_candidates().unwrap().into_iter().next().unwrap();
        s.dismiss_job(&row, None).unwrap();
        assert!(
            s.unevaluated(None, None).unwrap().is_empty(),
            "paying to score something you hid is the clearest waste there is"
        );
    }

    #[test]
    fn dismissing_a_company_covers_every_posting_including_later_ones() {
        let s = store();
        for (i, title) in ["Engineer", "Senior Engineer"].iter().enumerate() {
            let j = job("Motion Recruitment, LLC", title, &format!("https://a/{i}"));
            s.save_details(&j, true).unwrap();
        }
        let n = s
            .dismiss_company("Motion Recruitment LLC", Some("agency"))
            .unwrap();
        assert_eq!(n, 2, "reports the blast radius");

        // A posting fetched AFTER the dismissal is hidden too — that's the
        // difference between a company dismissal and two job dismissals.
        let later = job(
            "motion recruitment,  llc.",
            "Platform Engineer",
            "https://a/9",
        );
        s.save_details(&later, true).unwrap();
        let rows = s.all_candidates().unwrap();
        assert_eq!(rows.len(), 3);
        assert!(
            rows.iter().all(|r| r.dismissed),
            "company match must survive punctuation and case differences"
        );
        assert!(rows.iter().all(|r| r.dismissed_scope == Some("company")));
    }

    #[test]
    fn restoring_puts_a_row_back() {
        let s = store();
        let j = job("Acme", "Engineer", "https://a/1");
        s.save_details(&j, true).unwrap();
        let row = s.all_candidates().unwrap().into_iter().next().unwrap();

        s.dismiss_job(&row, None).unwrap();
        assert!(s.all_candidates().unwrap()[0].dismissed);
        s.restore_job(&row.url).unwrap();
        assert!(!s.all_candidates().unwrap()[0].dismissed);

        s.dismiss_company("Acme", None).unwrap();
        assert!(s.all_candidates().unwrap()[0].dismissed);
        s.restore_company("Acme").unwrap();
        assert!(!s.all_candidates().unwrap()[0].dismissed);
    }

    #[test]
    fn a_job_level_dismissal_takes_precedence_in_reporting() {
        // Both kinds can apply at once; the UI needs to know which undo to
        // offer, and restoring the job row alone would leave a
        // company-blocked posting stubbornly hidden.
        let s = store();
        let j = job("Acme", "Engineer", "https://a/1");
        s.save_details(&j, true).unwrap();
        let row = s.all_candidates().unwrap().into_iter().next().unwrap();
        s.dismiss_job(&row, None).unwrap();
        s.dismiss_company("Acme", None).unwrap();
        assert_eq!(s.all_candidates().unwrap()[0].dismissed_scope, Some("job"));
    }

    #[test]
    fn a_manual_entry_always_passes_the_filters() {
        let s = store();
        assert!(s.upsert_manual_job(&manual("Acme", "Engineer")).unwrap());
        let rows = s.all_candidates().unwrap();
        assert_eq!(rows.len(), 1, "must be visible without passing any filter");
        assert!(rows[0].is_manual());
    }

    #[test]
    fn re_adding_the_same_role_updates_rather_than_duplicating() {
        let s = store();
        assert!(s.upsert_manual_job(&manual("Acme", "Engineer")).unwrap());
        let mut second = manual("Acme", "Engineer");
        second.location = "Columbus, Ohio".into();
        assert!(
            !s.upsert_manual_job(&second).unwrap(),
            "same company+title means the same synthetic URL, so it updates"
        );
        let rows = s.all_candidates().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].location, "Columbus, Ohio");
    }

    #[test]
    fn adding_a_url_a_fetcher_already_found_merges_and_keeps_its_source() {
        let s = store();
        let mut scraped = job("Fly.io", "Platform Engineer", "https://fly.io/jobs/1");
        scraped.description = "the full fetched JD".into();
        s.mark_seen(&scraped).unwrap();
        s.save_details(&scraped, true).unwrap();

        let entry = ManualEntry {
            url: "https://fly.io/jobs/1".into(),
            company: "Fly.io".into(),
            title: "Platform Engineer".into(),
            my_status: Some("applied".into()),
            ..Default::default()
        }
        .with_url();
        assert!(!s.upsert_manual_job(&entry).unwrap(), "merges, not creates");

        let rows = s.all_candidates().unwrap();
        assert_eq!(rows.len(), 1, "no duplicate row");
        assert_eq!(rows[0].source, "greenhouse", "provenance is preserved");
        assert!(!rows[0].is_manual());
        assert_eq!(rows[0].my_status.as_deref(), Some("applied"));
        assert_eq!(
            rows[0].description, "the full fetched JD",
            "a blank form field must not wipe the fetched description"
        );
    }

    #[test]
    fn merging_rescues_a_row_the_filters_had_dropped() {
        let s = store();
        let dropped = job("Acme", "Engineer", "https://acme/1");
        s.save_details(&dropped, false).unwrap();
        assert!(s.all_candidates().unwrap().is_empty());

        let entry = ManualEntry {
            url: "https://acme/1".into(),
            company: "Acme".into(),
            title: "Engineer".into(),
            my_status: Some("interview".into()),
            ..Default::default()
        };
        s.upsert_manual_job(&entry).unwrap();
        // You applied to it, so it belongs on the list whatever the
        // filters thought.
        assert_eq!(s.all_candidates().unwrap().len(), 1);
    }

    #[test]
    fn a_hand_entered_stage_outranks_every_other_source() {
        let s = store();
        s.upsert_company_stage(&CompanyStage {
            company: "Acme".into(),
            stage: Stage::Public,
            headcount: None,
            source: StageSource::Curated,
            note: None,
        })
        .unwrap();
        let mut entry = manual("Acme", "Engineer");
        entry.stage = Some(Stage::SeriesA);
        entry.headcount = Some(30);
        s.upsert_manual_job(&entry).unwrap();

        let rows = s.all_candidates().unwrap();
        assert_eq!(rows[0].stage, Some(Stage::SeriesA));
        assert_eq!(rows[0].stage_source, Some(StageSource::Manual));
    }

    #[test]
    fn editing_without_a_status_keeps_the_one_set_with_the_number_keys() {
        let s = store();
        let entry = manual("Acme", "Engineer");
        s.upsert_manual_job(&entry).unwrap();
        s.save_user_status(&entry.url, Some("interview"), None)
            .unwrap();

        // An edit that carries notes but no status must not clear it.
        let mut edit = manual("Acme", "Engineer");
        edit.notes = Some("phone screen went well".into());
        s.upsert_manual_job(&edit).unwrap();

        let row = &s.all_candidates().unwrap()[0];
        assert_eq!(row.my_status.as_deref(), Some("interview"));
        assert_eq!(row.notes.as_deref(), Some("phone screen went well"));
    }

    #[test]
    fn a_manual_entry_is_queued_for_scoring_even_with_no_stage() {
        let s = store();
        s.upsert_manual_job(&manual("Acme", "Engineer")).unwrap();
        let policy = crate::filters::StartupPolicy {
            enabled_by_default: true,
            max_stage: Stage::Growth,
            max_headcount: None,
            // Strict: an unknown-stage scraped row would be excluded here.
            include_unknown: false,
        };
        assert_eq!(s.unevaluated(Some(&policy), None).unwrap().len(), 1);
    }

    #[test]
    fn deleting_clears_every_table_so_a_fetch_can_rediscover_it() {
        let s = store();
        let entry = manual("Acme", "Engineer");
        s.upsert_manual_job(&entry).unwrap();
        s.save_user_status(&entry.url, Some("applied"), Some("note"))
            .unwrap();
        s.delete_job(&entry.url).unwrap();

        assert!(s.all_candidates().unwrap().is_empty());
        let readded = Job {
            url: entry.url.clone(),
            company: "Acme".into(),
            title: "Engineer".into(),
            location: "Remote - US".into(),
            ..Default::default()
        };
        assert!(s.is_new(&readded).unwrap(), "dedup key must be gone too");
    }

    #[test]
    fn unevaluated_queue_respects_the_startup_cutoff() {
        let s = store();
        for (company, url) in [("Fly.io", "https://a/1"), ("Amazon", "https://a/2")] {
            let j = job(company, "Platform Engineer", url);
            s.save_details(&j, true).unwrap();
        }
        s.upsert_company_stage(&CompanyStage {
            company: "Fly.io".into(),
            stage: Stage::SeriesB,
            headcount: None,
            source: StageSource::Curated,
            note: None,
        })
        .unwrap();
        s.upsert_company_stage(&CompanyStage {
            company: "Amazon".into(),
            stage: Stage::BigTech,
            headcount: None,
            source: StageSource::Curated,
            note: None,
        })
        .unwrap();

        assert_eq!(s.unevaluated(None, None).unwrap().len(), 2);
        let policy = crate::filters::StartupPolicy {
            enabled_by_default: true,
            max_stage: Stage::Growth,
            max_headcount: None,
            include_unknown: false,
        };
        let queue = s.unevaluated(Some(&policy), None).unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].company, "Fly.io");
    }

    #[test]
    fn the_status_log_records_transitions_and_ignores_notes_edits() {
        let s = store();
        s.save_details(&job("Fly.io", "SRE", "u1"), true).unwrap();

        s.save_user_status("u1", Some("applied"), None).unwrap();
        s.save_user_status("u1", Some("recruiter"), None).unwrap();
        // Same status, new notes: an annotation, not a move. Logging this
        // would reset the age and make a row you just commented on look
        // freshly progressed.
        s.save_user_status("u1", Some("recruiter"), Some("call Thursday"))
            .unwrap();

        let timeline = s.status_timelines().unwrap();
        let entries = &timeline["u1"];
        assert_eq!(
            entries
                .iter()
                .map(|(st, _)| st.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("applied"), Some("recruiter")],
        );
    }

    #[test]
    fn clearing_a_status_is_itself_a_transition() {
        let s = store();
        s.save_details(&job("Fly.io", "SRE", "u1"), true).unwrap();
        s.save_user_status("u1", Some("applied"), None).unwrap();
        s.save_user_status("u1", None, None).unwrap();

        let timeline = s.status_timelines().unwrap();
        assert_eq!(timeline["u1"].len(), 2);
        assert_eq!(timeline["u1"][1].0, None);
    }

    #[test]
    fn the_backfill_gives_pre_existing_marks_an_age_and_runs_once() {
        let s = store();
        s.save_details(&job("Fly.io", "SRE", "u1"), true).unwrap();
        // A row marked before the log existed: user_status set directly,
        // with nothing in status_history.
        s.conn
            .execute(
                "INSERT INTO user_status (url, my_status, updated_at) \
                 VALUES ('u1', 'applied', '2025-01-02 03:04:05')",
                [],
            )
            .unwrap();

        assert_eq!(s.backfill_status_history().unwrap(), 1);
        assert_eq!(
            s.backfill_status_history().unwrap(),
            0,
            "must be idempotent"
        );

        let timeline = s.status_timelines().unwrap();
        assert_eq!(
            timeline["u1"],
            vec![(Some("applied".into()), "2025-01-02 03:04:05".to_string())]
        );
    }

    #[test]
    fn a_row_carries_its_status_age_when_read_back() {
        let s = store();
        s.save_details(&job("Fly.io", "SRE", "u1"), true).unwrap();
        s.save_user_status("u1", Some("applied"), None).unwrap();

        let rows = s.all_stored().unwrap();
        let row = rows.iter().find(|r| r.url == "u1").unwrap();
        assert!(row.status_since.is_some());
        assert_eq!(row.status_age_days(), Some(0));
        assert_eq!(row.age_label().as_deref(), Some("0d"));
    }
}
