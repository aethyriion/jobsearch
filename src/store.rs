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
//! This is the same file and the same schema the (now frozen) Next.js
//! board in `web/` reads, so both UIs stay usable against one database.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

use crate::model::{CompanyStage, Evaluation, Job, JobRow, Stage, StageSource};

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

pub struct Store {
    conn: Connection,
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
        // WAL lets the frozen web/ board read while a fetch run writes,
        // instead of one blocking the other.
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Self { conn })
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
        let rows = self.query_rows(
            "WHERE jd.passed_filters = 1 AND ae.url IS NULL",
            "ORDER BY jd.fetched_at DESC",
        )?;
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
              risk_factors, model, evaluated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, CURRENT_TIMESTAMP)",
            params![
                url,
                eval.match_score,
                eval.recommendation,
                eval.genuine_gaps,
                eval.transferable_strengths,
                eval.risk_factors,
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

    pub fn get_by_url(&self, url: &str) -> Result<Option<JobRow>> {
        Ok(self
            .query_rows_with("", "", rusqlite::params![])?
            .into_iter()
            .find(|r| r.url == url))
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
                my_status: r.get(14)?,
                notes: r.get(15)?,
                ..Default::default()
            })
        })?;
        let mut rows: Vec<JobRow> = rows.collect::<Result<Vec<_>, _>>()?;
        self.attach_stages(&mut rows)?;
        Ok(rows)
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
}
