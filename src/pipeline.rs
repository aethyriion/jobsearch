//! Orchestration: fetch -> filter -> dedup -> store, and the AI scoring
//! pass over what survives.
//!
//! Both long-running operations report progress through a `Progress`
//! callback rather than printing. That's what lets the same code drive
//! both the CLI (which prints each event) and the TUI (which renders them
//! into a status line) without either one reimplementing the pipeline.
//!
//! Company boards are fetched concurrently — a run over ~130 boards is
//! almost entirely spent waiting on the network, so this is the single
//! biggest difference in wall-clock time from the serial implementation
//! this replaces.

use anyhow::Result;
use futures::stream::{self, StreamExt};
use std::path::Path;

use crate::config::{Aggregator, Company};
use crate::discover::NewCompany;
use crate::filters::Filters;
use crate::model::{CompanyStage, Job, StageSource};
use crate::sources::{aggregators, ats};
use crate::store::Store;

/// How many company boards to fetch at once. Enough to hide the latency of
/// ~130 sequential requests, low enough to stay a polite guest on ATS APIs
/// that are doing this for free.
pub const DEFAULT_CONCURRENCY: usize = 8;

/// Everything the UI layers want to hear about while a long job runs.
#[derive(Debug, Clone)]
pub enum Progress {
    /// One source finished. `fetched` is everything it returned; `kept` is
    /// what was new AND passed the filters.
    SourceDone {
        name: String,
        fetched: usize,
        kept: usize,
    },
    /// A source failed. One bad board never aborts a run — 129 others are
    /// still worth having.
    SourceFailed { name: String, error: String },
    /// One job scored by the AI step.
    Scored {
        index: usize,
        total: usize,
        company: String,
        title: String,
        score: i64,
        recommendation: String,
    },
    ScoreFailed {
        company: String,
        title: String,
        error: String,
    },
    /// Free-text status for phases with no natural per-item unit.
    Note(String),
}

#[derive(Debug, Default, Clone)]
pub struct RunSummary {
    pub fetched: usize,
    pub new_candidates: usize,
    pub sources_ok: usize,
    pub sources_failed: usize,
    pub discovered: Vec<NewCompany>,
}

/// Fetch everything, filter and dedup it, and store the result.
///
/// Note the ordering inside `ingest`: every job is marked seen and stored
/// whether or not it passed, so a future run never re-considers it and so
/// a filter change can be replayed against it offline (see `refilter`).
pub async fn run_fetch<F>(
    store: &Store,
    filters: &Filters,
    companies: &[Company],
    aggs: &[Aggregator],
    concurrency: usize,
    mut progress: F,
) -> Result<RunSummary>
where
    F: FnMut(Progress),
{
    let mut summary = RunSummary::default();

    // --- companies, concurrently -------------------------------------
    //
    // Results are collected and written on this task rather than from
    // inside the concurrent stream: the SQLite connection isn't shared
    // across tasks, and serializing the writes here is both simpler and
    // faster than contending for a lock on every row.
    let mut stream = stream::iter(companies.iter())
        .map(|c| async move { (c, ats::fetch_company(c, filters).await) })
        .buffer_unordered(concurrency.clamp(1, 32));

    while let Some((company, result)) = stream.next().await {
        match result {
            Ok(jobs) => {
                let fetched = jobs.len();
                let kept = ingest(store, filters, jobs)?;
                summary.fetched += fetched;
                summary.new_candidates += kept;
                summary.sources_ok += 1;
                progress(Progress::SourceDone {
                    name: company.name.clone(),
                    fetched,
                    kept,
                });
            }
            Err(e) => {
                summary.sources_failed += 1;
                progress(Progress::SourceFailed {
                    name: company.name.clone(),
                    error: format!("{e:#}"),
                });
            }
        }
    }

    // --- aggregators, serially ---------------------------------------
    //
    // Deliberately not concurrent. Each of these is itself a paginated
    // walk that can fan out into per-job full-JD fetches, and two of the
    // three have explicit politeness limits (Remotive asks for a handful
    // of requests a day). Running them one at a time keeps a run inside
    // those limits without a second rate-limiter to tune.
    let mut discovered = Vec::new();
    for agg in aggs {
        if !agg.enabled {
            progress(Progress::Note(format!("{} — skipped (disabled)", agg.name)));
            continue;
        }
        match aggregators::fetch_aggregator(agg, filters, &mut discovered).await {
            Ok(jobs) => {
                let fetched = jobs.len();
                let kept = ingest(store, filters, jobs)?;
                summary.fetched += fetched;
                summary.new_candidates += kept;
                summary.sources_ok += 1;
                progress(Progress::SourceDone {
                    name: agg.name.clone(),
                    fetched,
                    kept,
                });
            }
            Err(e) => {
                summary.sources_failed += 1;
                progress(Progress::SourceFailed {
                    name: agg.name.clone(),
                    error: format!("{e:#}"),
                });
            }
        }
    }

    summary.discovered = discovered
        .into_iter()
        .map(|d| NewCompany {
            name: d.name,
            ats: d.ats,
            slug: d.slug,
            stage: None,
            headcount: None,
            note: None,
        })
        .collect();

    Ok(summary)
}

/// Filter + dedup one source's batch, returning how many were kept.
fn ingest(store: &Store, filters: &Filters, jobs: Vec<Job>) -> Result<usize> {
    let mut kept = 0;
    for job in jobs {
        if job.url.is_empty() {
            // No URL means no dedup key — storing it would make every
            // subsequent url-less job look like a duplicate of the first.
            continue;
        }
        if !store.is_new(&job)? {
            continue;
        }
        store.mark_seen(&job)?;
        let passed = filters.passes(&job);
        store.save_details(&job, passed)?;
        // A stage the source knew first-hand is worth recording even for a
        // job that failed the filters: the claim is about the company, and
        // a different posting from the same company may well pass later.
        if let Some(hint) = &job.stage_hint {
            store.upsert_company_stage(&CompanyStage {
                company: job.company.clone(),
                stage: hint.stage,
                headcount: hint.headcount,
                source: hint.source,
                note: None,
            })?;
        }
        if passed {
            kept += 1;
        }
    }
    Ok(kept)
}

/// Score every unevaluated candidate.
///
/// `startup_only` gates the *queue*, not the display, because this is the
/// step that costs money — paying to score jobs that startup mode will
/// then hide is the expensive way to run it.
pub async fn run_evaluate<F>(
    store: &Store,
    filters: &Filters,
    evaluator: &crate::evaluate::Evaluator,
    startup_only: bool,
    limit: Option<usize>,
    mut progress: F,
) -> Result<usize>
where
    F: FnMut(Progress),
{
    let policy = startup_only.then_some(&filters.startup);
    let mut queue = store.unevaluated(policy)?;
    if let Some(limit) = limit {
        queue.truncate(limit);
    }
    let total = queue.len();

    let mut scored = 0;
    for (i, job) in queue.iter().enumerate() {
        let truncated = filters.looks_truncated(&job.description);
        match evaluator.evaluate(job, truncated).await {
            Ok(eval) => {
                store.save_evaluation(&job.url, &eval, evaluator.model())?;
                // The AI's stage read is the weakest of the three sources,
                // so it's offered rather than imposed: upsert_company_stage
                // drops it if a curated or directory-sourced claim already
                // exists. Low-confidence guesses are discarded outright —
                // a coin-flip stage is worse than no stage, because
                // startup mode would act on it.
                if let Some(stage) = eval.company_stage {
                    if eval.stage_confidence.unwrap_or(0) >= 50 {
                        store.upsert_company_stage(&CompanyStage {
                            company: job.company.clone(),
                            stage,
                            headcount: eval.company_headcount,
                            source: StageSource::Ai,
                            note: Some(format!(
                                "inferred from JD (confidence {})",
                                eval.stage_confidence.unwrap_or(0)
                            )),
                        })?;
                    }
                }
                scored += 1;
                progress(Progress::Scored {
                    index: i + 1,
                    total,
                    company: job.company.clone(),
                    title: job.title.clone(),
                    score: eval.match_score,
                    recommendation: eval.recommendation.clone(),
                });
            }
            Err(e) => progress(Progress::ScoreFailed {
                company: job.company.clone(),
                title: job.title.clone(),
                error: format!("{e:#}"),
            }),
        }
    }
    Ok(scored)
}

#[derive(Debug, Default)]
pub struct RefilterSummary {
    pub rescued: Vec<(String, String, String)>,
    pub demoted: Vec<(String, String, String)>,
    pub checked: usize,
}

/// Re-run the CURRENT filters against everything already stored, without
/// re-fetching anything.
///
/// This is what makes tuning `filters.yaml` cheap: `job_details` keeps the
/// full record for every job ever seen, pass or fail, so a loosened filter
/// can rescue jobs the next scheduled run would never re-discover (dedup
/// already marks them seen) and a tightened one can demote jobs that no
/// longer belong.
pub fn run_refilter(store: &Store, filters: &Filters, dry_run: bool) -> Result<RefilterSummary> {
    let mut summary = RefilterSummary::default();

    for row in store.filtered_out()? {
        summary.checked += 1;
        let job = row_as_job(&row);
        if filters.passes(&job) {
            if !dry_run {
                store.set_passed_filters(&row.url, true)?;
            }
            summary.rescued.push((row.company, row.title, row.url));
        }
    }

    for row in store.all_candidates()? {
        // Manual entries are exempt in this direction only. They were
        // never admitted by the filters in the first place, so letting a
        // filter tweak demote one would quietly delete an application from
        // your tracking list.
        if row.is_manual() {
            continue;
        }
        summary.checked += 1;
        let job = row_as_job(&row);
        if !filters.passes(&job) {
            if !dry_run {
                store.set_passed_filters(&row.url, false)?;
            }
            summary.demoted.push((row.company, row.title, row.url));
        }
    }

    Ok(summary)
}

fn row_as_job(row: &crate::model::JobRow) -> Job {
    Job {
        company: row.company.clone(),
        title: row.title.clone(),
        location: row.location.clone(),
        url: row.url.clone(),
        posted_at: row.posted_at.clone(),
        description: row.description.clone(),
        source: row.source.clone(),
        stage_hint: None,
    }
}

// ------------------------------------------------------------------- CSVs

/// Write the lean human-scannable candidate list.
///
/// Deliberately not every column: `description` (and anything else added
/// to the job schema later) stays in SQLite. Putting a multi-KB JD in a
/// CSV cell makes the file unreadable in a spreadsheet, which defeats the
/// only reason the CSV exists.
pub fn write_candidates_csv(store: &Store, path: impl AsRef<Path>) -> Result<usize> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let rows = store.all_candidates()?;
    let mut w = csv::Writer::from_path(path)?;
    w.write_record(["company", "title", "location", "stage", "url", "posted_at"])?;
    for r in &rows {
        w.write_record([
            &r.company,
            &r.title,
            &r.location,
            r.stage.map(|s| s.as_str()).unwrap_or(""),
            &r.url,
            r.posted_at.as_deref().unwrap_or(""),
        ])?;
    }
    w.flush()?;
    Ok(rows.len())
}

pub fn write_scored_csv(store: &Store, path: impl AsRef<Path>) -> Result<usize> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let rows = store.scored()?;
    let mut w = csv::Writer::from_path(path)?;
    w.write_record([
        "match_score",
        "recommendation",
        "company",
        "stage",
        "headcount",
        "title",
        "location",
        "transferable_strengths",
        "genuine_gaps",
        "risk_factors",
        "url",
        "posted_at",
    ])?;
    for r in &rows {
        w.write_record([
            r.match_score.map(|s| s.to_string()).unwrap_or_default(),
            r.recommendation.clone().unwrap_or_default(),
            r.company.clone(),
            r.stage.map(|s| s.as_str().to_string()).unwrap_or_default(),
            r.headcount.map(|h| h.to_string()).unwrap_or_default(),
            r.title.clone(),
            r.location.clone(),
            r.transferable_strengths.clone().unwrap_or_default(),
            r.genuine_gaps.clone().unwrap_or_default(),
            r.risk_factors.clone().unwrap_or_default(),
            r.url.clone(),
            r.posted_at.clone().unwrap_or_default(),
        ])?;
    }
    w.flush()?;
    Ok(rows.len())
}
