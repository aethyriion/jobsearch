//! jobsearch — a startup-focused job search pipeline and review TUI.
//!
//! Running it with no arguments opens the TUI, which is where the work
//! actually happens: fetching, scoring and triage are all one keypress
//! from the review list. The subcommands exist for the cases a TUI is the
//! wrong shape — cron, scripts, and debugging one source in isolation.

mod config;
mod discover;
mod evaluate;
mod filters;
mod model;
mod pipeline;
mod sources;
mod store;
mod tech;
mod tui;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use config::Paths;
use filters::Filters;
use pipeline::Progress;
use store::Store;

#[derive(Parser)]
#[command(
    name = "jobsearch",
    version,
    about = "Startup-focused job search: fetch, filter, score and triage roles in a TUI",
    long_about = None
)]
struct Cli {
    /// Directory holding companies.yaml, aggregators.yaml, filters.yaml
    /// and data/ (default: the current directory).
    #[arg(long, global = true, value_name = "DIR")]
    root: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Open the review TUI (the default when no subcommand is given).
    Tui,

    /// Fetch from every configured source, filter, dedup and store.
    Fetch {
        /// Only this company slug — for debugging one fetcher.
        #[arg(long, value_name = "SLUG")]
        company: Option<String>,
        /// Only aggregator entries whose name contains this (case
        /// insensitive) — the aggregator-side counterpart to --company,
        /// for trying one source without waiting on the other seven.
        #[arg(long, value_name = "SUBSTRING")]
        aggregator: Option<String>,
        #[arg(long)]
        skip_companies: bool,
        #[arg(long)]
        skip_aggregators: bool,
        /// Don't append newly-resolved companies to companies.yaml.
        #[arg(long)]
        skip_discovery: bool,
        /// Simultaneous company-board fetches.
        #[arg(long, default_value_t = pipeline::DEFAULT_CONCURRENCY)]
        concurrency: usize,
    },

    /// Score filtered candidates with Claude. This is the only step that
    /// costs money.
    Evaluate {
        /// Show what would be scored and stop, without calling the API.
        #[arg(long)]
        dry_run: bool,
        /// Cap how many to score this run.
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
        /// Score every stage, not just startups.
        #[arg(long)]
        all_stages: bool,
    },

    /// Re-apply the current filters.yaml to everything already stored,
    /// without re-fetching. Run this after editing filters.
    Refilter {
        #[arg(long)]
        dry_run: bool,
    },

    /// Pull the Y Combinator directory, resolve each company's ATS board,
    /// and record stage + headcount.
    DiscoverYc {
        /// Stop after this many companies — start small to see the hit
        /// rate before committing to a full run.
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
        /// Simultaneous companies being probed.
        #[arg(long, default_value_t = 12)]
        concurrency: usize,
        /// Record stages but don't append anything to companies.yaml.
        #[arg(long)]
        stages_only: bool,
        /// Re-probe companies already checked in a previous run.
        #[arg(long)]
        recheck: bool,
    },

    /// Print a stored job, or explain why one was filtered out.
    Inspect {
        /// Job URL. Omit to list recent rejections instead.
        url: Option<String>,
        /// With no URL: how many recent rejections to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },

    /// Record an application by hand — a role the scrapers never found,
    /// or one you applied to through a referral. Shows inline with the
    /// fetched jobs and is scored by `evaluate` like any other.
    Add {
        #[arg(long)]
        company: String,
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        location: String,
        /// The posting URL. Omitted, a stable `manual:` one is generated
        /// from company and title.
        #[arg(long, default_value = "")]
        url: String,
        /// YYYY-MM-DD.
        #[arg(long, default_value = "")]
        posted: String,
        /// Job description. Prefer --description-file for a real posting.
        #[arg(long, default_value = "")]
        description: String,
        /// Read the description from a file, or from stdin with `-`.
        #[arg(long, value_name = "PATH")]
        description_file: Option<String>,
        /// pre-seed | seed | series-a | series-b | series-c | growth |
        /// public | big-tech
        #[arg(long)]
        stage: Option<String>,
        #[arg(long)]
        headcount: Option<i64>,
        /// applied | interview | rejected | skipped | silence
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        notes: Option<String>,
    },

    /// Hide a posting, or an entire employer, from the list. A soft
    /// delete: nothing is destroyed, it survives refreshes, and `restore`
    /// undoes it.
    Dismiss {
        /// The posting URL to hide.
        #[arg(long, conflicts_with = "company")]
        url: Option<String>,
        /// Hide every posting from this employer, including future ones.
        #[arg(long, conflicts_with = "url")]
        company: Option<String>,
        /// Why — shown in the dismissed list and the detail pane.
        #[arg(long)]
        reason: Option<String>,
    },

    /// Put a dismissed posting or employer back.
    Restore {
        #[arg(long, conflicts_with = "company")]
        url: Option<String>,
        #[arg(long, conflicts_with = "url")]
        company: Option<String>,
    },

    /// List everything currently hidden.
    Dismissed,

    /// What technologies your candidates actually mention — the split
    /// between AWS and GCP, who uses Rust, and so on. Deterministic: this
    /// reads `tech.yaml`'s patterns over stored descriptions, with no
    /// model and no network.
    Tech {
        /// List the postings matching this tag instead of the summary.
        /// Accepts `aws` or `cloud:aws`.
        #[arg(long, value_name = "TAG")]
        tag: Option<String>,
        /// With --tag: how many postings to list.
        #[arg(long, default_value_t = 30)]
        limit: usize,
        /// Include dismissed postings.
        #[arg(long)]
        include_dismissed: bool,
    },

    /// Show where jobs die in the filter funnel — the answer to "are my
    /// filters too tight". Reads only what's already stored; no network.
    Diagnose {
        /// How many examples to show per category.
        #[arg(long, default_value_t = 12)]
        examples: usize,
        /// How many days of intake history to summarize.
        #[arg(long, default_value_t = 14)]
        days: i64,
    },

    /// Validate every config file and report what's known about stages.
    Check,
}

fn main() -> Result<()> {
    // A single-threaded runtime would be enough for the TUI but not for
    // the concurrent fetch, which is the whole reason a run finishes in
    // seconds rather than minutes.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::new(cli.root.clone().unwrap_or_else(|| PathBuf::from(".")));
    // Load .env from the config root, not the process's cwd, so --root
    // works from anywhere.
    let _ = dotenvy::from_path(paths.root.join(".env"));

    match cli.command.unwrap_or(Command::Tui) {
        Command::Tui => {
            let filters = load_filters(&paths)?;
            let tech = tech::TechDetector::load(paths.tech()).unwrap_or_else(|e| {
                if paths.tech().exists() {
                    eprintln!("[WARN] tech.yaml: {e:#} — continuing without tech detection.");
                }
                tech::TechDetector::empty()
            });
            tui::run(paths, filters, tech).await
        }
        Command::Fetch {
            company,
            aggregator,
            skip_companies,
            skip_aggregators,
            skip_discovery,
            concurrency,
        } => {
            cmd_fetch(
                &paths,
                company,
                aggregator,
                skip_companies,
                skip_aggregators,
                skip_discovery,
                concurrency,
            )
            .await
        }
        Command::Evaluate {
            dry_run,
            limit,
            all_stages,
        } => cmd_evaluate(&paths, dry_run, limit, all_stages).await,
        Command::Refilter { dry_run } => cmd_refilter(&paths, dry_run),
        Command::DiscoverYc {
            limit,
            concurrency,
            stages_only,
            recheck,
        } => cmd_discover_yc(&paths, limit, concurrency, stages_only, recheck).await,
        Command::Inspect { url, limit } => cmd_inspect(&paths, url, limit),
        Command::Add {
            company,
            title,
            location,
            url,
            posted,
            description,
            description_file,
            stage,
            headcount,
            status,
            notes,
        } => cmd_add(
            &paths,
            AddArgs {
                company,
                title,
                location,
                url,
                posted,
                description,
                description_file,
                stage,
                headcount,
                status,
                notes,
            },
        ),
        Command::Dismiss {
            url,
            company,
            reason,
        } => cmd_dismiss(&paths, url, company, reason),
        Command::Restore { url, company } => cmd_restore(&paths, url, company),
        Command::Dismissed => cmd_dismissed(&paths),
        Command::Tech {
            tag,
            limit,
            include_dismissed,
        } => cmd_tech(&paths, tag, limit, include_dismissed),
        Command::Diagnose { examples, days } => cmd_diagnose(&paths, examples, days),
        Command::Check => cmd_check(&paths),
    }
}

fn load_filters(paths: &Paths) -> Result<Filters> {
    let cfg = config::load_filters(paths.filters())?;
    Filters::compile(&cfg)
}

/// Open the store with tech detection wired in.
///
/// Every caller goes through this rather than `Store::open` directly, so
/// there's no way to end up with a store whose rows are silently untagged.
/// A missing or broken `tech.yaml` degrades to no tags with a warning
/// rather than failing the command — detection is an enhancement, and
/// losing it shouldn't stop you fetching jobs.
fn open_store(paths: &Paths) -> Result<Store> {
    let tech = match tech::TechDetector::load(paths.tech()) {
        Ok(t) => t,
        Err(e) => {
            if paths.tech().exists() {
                eprintln!("[WARN] tech.yaml: {e:#}");
                eprintln!("       continuing without tech detection.");
            }
            tech::TechDetector::empty()
        }
    };
    Ok(Store::open(paths.db())?.with_tech(tech))
}

// ------------------------------------------------------------------ fetch

#[allow(clippy::too_many_arguments)]
async fn cmd_fetch(
    paths: &Paths,
    company: Option<String>,
    aggregator: Option<String>,
    skip_companies: bool,
    skip_aggregators: bool,
    skip_discovery: bool,
    concurrency: usize,
) -> Result<()> {
    let filters = load_filters(paths)?;
    let store = open_store(paths)?;

    let mut companies = if skip_companies {
        Vec::new()
    } else {
        config::load_companies(paths.companies())?
    };
    let mut aggs = if skip_aggregators {
        Vec::new()
    } else {
        config::load_aggregators(paths.aggregators())?
    };

    if let Some(slug) = &company {
        companies.retain(|c| &c.slug == slug);
        aggs.clear();
        if companies.is_empty() {
            anyhow::bail!(
                "no company with slug {slug:?} in {}",
                paths.companies().display()
            );
        }
    }

    if let Some(needle) = &aggregator {
        let needle = needle.to_lowercase();
        aggs.retain(|a| a.name.to_lowercase().contains(&needle));
        companies.clear();
        if aggs.is_empty() {
            anyhow::bail!(
                "no aggregator in {} whose name contains {needle:?}",
                paths.aggregators().display()
            );
        }
        // An explicit --aggregator overrides `enabled: false`, so a source
        // that is switched off in the file can still be tried by hand
        // without editing it.
        for a in &mut aggs {
            a.enabled = true;
        }
    }

    // Curated stages in companies.yaml are the most trustworthy source, so
    // sync them before fetching rather than only when a job arrives —
    // that way a stage added by hand applies to jobs already stored from
    // an earlier run, not just new ones.
    let mut curated = 0;
    for c in &companies {
        if let Some(stage) = c.parsed_stage() {
            if store.upsert_company_stage(&model::CompanyStage {
                company: c.name.clone(),
                stage,
                headcount: c.headcount,
                source: model::StageSource::Curated,
                note: None,
            })? {
                curated += 1;
            }
        }
    }
    if curated > 0 {
        println!("Synced {curated} curated company stage(s) from companies.yaml.");
    }

    println!(
        "Fetching {} company board(s) and {} aggregator(s), {concurrency} at a time…\n",
        companies.len(),
        aggs.iter().filter(|a| a.enabled).count()
    );

    let summary = pipeline::run_fetch(
        &store,
        &filters,
        &companies,
        &aggs,
        concurrency,
        |p| match p {
            Progress::SourceDone {
                name,
                fetched,
                kept,
            } => {
                println!("{name:<38} fetched={fetched:>4}  new={kept:>3}")
            }
            Progress::SourceFailed { name, error } => {
                eprintln!("[WARN] {name}: {error}")
            }
            Progress::Note(n) => println!("{n}"),
            _ => {}
        },
    )
    .await?;

    println!(
        "\nTotal fetched: {}  |  new candidates after filters+dedup: {}  |  sources failed: {}",
        summary.fetched, summary.new_candidates, summary.sources_failed
    );

    // Reprinted at the end because the inline warnings scroll past in a
    // 145-board run, and a board that has quietly 404'd costs you that
    // company on every future run until someone notices.
    if !summary.failures.is_empty() {
        println!("\nFAILED SOURCES — these returned nothing this run:");
        for (name, error) in &summary.failures {
            println!("  {name}: {}", truncate(error, 110));
        }
        println!("  A 404 usually means the slug moved or the board closed. Check one with");
        println!(
            "  `jobsearch fetch --company <slug>`, then fix or remove its companies.yaml entry."
        );
    }

    if summary.new_candidates == 0 && summary.fetched > 0 {
        // The single most common confusion with this tool: a re-run that
        // finds nothing looks broken and almost never is.
        println!("\nNothing new this run.");
        println!("Boards only change when companies post, so a re-run within a day of the last");
        println!("one normally finds nothing. `jobsearch diagnose` shows the daily intake rate");
        println!("and where jobs are being filtered out.");
    }

    if !skip_discovery && !summary.discovered.is_empty() {
        let added = discover::append_new_companies(
            paths.companies(),
            &summary.discovered,
            "aggregator full-JD fetch",
        )?;
        if added.is_empty() {
            println!("No new companies to add to companies.yaml.");
        } else {
            println!(
                "\nAdded {} company/companies to companies.yaml:",
                added.len()
            );
            for c in &added {
                println!("  + {} ({}: {})", c.name, c.ats, c.slug);
            }
        }
    }

    let written = pipeline::write_candidates_csv(&store, paths.candidates_csv())?;
    println!(
        "\nWrote {written} candidates to {}",
        paths.candidates_csv().display()
    );

    let startup = store
        .all_candidates()?
        .iter()
        .filter(|r| filters.startup.admits(r))
        .count();
    println!(
        "{startup} of them are startups under the current policy ({}).",
        filters.startup.describe()
    );
    Ok(())
}

// --------------------------------------------------------------- evaluate

async fn cmd_evaluate(
    paths: &Paths,
    dry_run: bool,
    limit: Option<usize>,
    all_stages: bool,
) -> Result<()> {
    let filters = load_filters(paths)?;
    let store = open_store(paths)?;
    let startup_only = !all_stages && filters.startup.enabled_by_default;

    let policy = startup_only.then_some(&filters.startup);
    let mut queue = store.unevaluated(policy, Some(&filters.seniority))?;
    if let Some(limit) = limit {
        queue.truncate(limit);
    }

    println!("Levels in scope: {}", filters.seniority.describe());
    println!(
        "{} candidate(s) queued for AI evaluation{}.",
        queue.len(),
        if startup_only {
            format!(" (startups only: {})", filters.startup.describe())
        } else {
            " (all stages)".to_string()
        }
    );
    if queue.is_empty() {
        return Ok(());
    }

    if dry_run {
        for job in &queue {
            println!(
                "WOULD EVALUATE | {:<5} | {:<8} | {:<24} | {}",
                job.seniority().map(model::Seniority::short).unwrap_or("?"),
                job.stage.map(model::Stage::short).unwrap_or("?"),
                truncate(&job.company, 24),
                job.title
            );
        }
        println!("\n(dry run — nothing was sent, nothing was charged)");
        return Ok(());
    }

    let profile = config::load_profile(paths.profile())?;
    let evaluator = evaluate::Evaluator::new(&profile, evaluate::model_from_env())?;
    println!("Scoring with {}…\n", evaluator.model());

    let scored =
        pipeline::run_evaluate(
            &store,
            &filters,
            &evaluator,
            startup_only,
            limit,
            |p| match p {
                Progress::Scored {
                    index,
                    total,
                    company,
                    title,
                    score,
                    recommendation,
                } => println!(
                    "[{index}/{total}] {score:3} {recommendation:<9} | {:<24} | {title}",
                    truncate(&company, 24)
                ),
                Progress::ScoreFailed {
                    company,
                    title,
                    error,
                } => {
                    eprintln!("[WARN] {company} — {title}: {error}")
                }
                _ => {}
            },
        )
        .await?;

    let written = pipeline::write_scored_csv(&store, paths.scored_csv())?;
    println!(
        "\nScored {scored}. Wrote {written} rows to {}",
        paths.scored_csv().display()
    );
    Ok(())
}

// --------------------------------------------------------------- refilter

fn cmd_refilter(paths: &Paths, dry_run: bool) -> Result<()> {
    let filters = load_filters(paths)?;
    let store = open_store(paths)?;
    let summary = pipeline::run_refilter(&store, &filters, dry_run)?;

    for (company, title, url) in &summary.rescued {
        println!(
            "RESCUED | {:<24} | {:<52} | {url}",
            truncate(company, 24),
            truncate(title, 52)
        );
    }
    for (company, title, url) in &summary.demoted {
        println!(
            "DEMOTED | {:<24} | {:<52} | {url}",
            truncate(company, 24),
            truncate(title, 52)
        );
    }
    println!(
        "\nChecked {}: {} rescued, {} demoted.",
        summary.checked,
        summary.rescued.len(),
        summary.demoted.len()
    );
    if dry_run {
        println!("(dry run — nothing written; drop --dry-run to apply)");
    }
    Ok(())
}

// ------------------------------------------------------------ discover-yc

async fn cmd_discover_yc(
    paths: &Paths,
    limit: Option<usize>,
    concurrency: usize,
    stages_only: bool,
    recheck: bool,
) -> Result<()> {
    let store = open_store(paths)?;

    println!("Fetching the YC company directory…");
    let all = sources::yc::fetch_directory().await?;
    println!("{} YC companies are currently hiring.", all.len());

    // Record stage and headcount for EVERY hiring company, not just the
    // ones we go on to probe. That data is the cheap half of this command
    // and is useful on its own: any of these companies can show up later
    // through an aggregator or the HN thread, and when it does it arrives
    // already placed on the stage axis.
    let mut stages = 0;
    for c in &all {
        if store.upsert_company_stage(&c.stage_claim())? {
            stages += 1;
        }
    }
    println!("Recorded {stages} company stage(s).");
    if stages_only {
        return Ok(());
    }

    let known = discover::known_keys(paths.companies())?;
    let already_probed = if recheck {
        Default::default()
    } else {
        store.probed_company_keys()?
    };

    let mut candidates: Vec<_> = all
        .into_iter()
        .filter(|c| c.is_us_reachable())
        .filter(|c| !already_probed.contains(&store::normalize(&c.name)))
        .collect();
    if let Some(limit) = limit {
        candidates.truncate(limit);
    }

    if candidates.is_empty() {
        println!("Nothing new to probe (pass --recheck to re-probe known companies).");
        return Ok(());
    }

    println!(
        "\nProbing {} US-reachable compan{} for a Greenhouse/Ashby/Lever board, \
         {concurrency} at a time.\nThis takes a few minutes and makes several requests per \
         company — it's cached, so a re-run skips everything checked here.\n",
        candidates.len(),
        if candidates.len() == 1 { "y" } else { "ies" }
    );

    let by_name: std::collections::HashMap<_, _> =
        candidates.iter().map(|c| (c.name.clone(), c)).collect();

    let results = sources::yc::resolve_boards(&candidates, concurrency, |done, total, name| {
        if done % 25 == 0 || done == total {
            println!("  …{done}/{total} probed (latest: {name})");
        }
    })
    .await;

    let mut to_add = Vec::new();
    let mut hits = 0;
    for (name, resolved) in &results {
        match resolved {
            Some(board) => {
                hits += 1;
                store.record_ats_resolution(name, Some((&board.ats, &board.slug)))?;
                if known.contains(&(board.ats.clone(), board.slug.clone())) {
                    continue;
                }
                let yc = by_name.get(name);
                to_add.push(discover::NewCompany {
                    name: name.clone(),
                    ats: board.ats.clone(),
                    slug: board.slug.clone(),
                    stage: yc.map(|c| c.to_stage()),
                    headcount: yc.and_then(|c| c.team_size),
                    note: yc.map(|c| {
                        format!(
                            "YC {} · {} open roles at discovery",
                            c.batch, board.open_roles
                        )
                    }),
                });
            }
            None => store.record_ats_resolution(name, None)?,
        }
    }

    println!("\nResolved a board for {hits}/{} companies.", results.len());

    let added = discover::append_new_companies(paths.companies(), &to_add, "YC directory")?;
    if added.is_empty() {
        println!("All of them were already in companies.yaml.");
    } else {
        println!("Added {} to companies.yaml:", added.len());
        for c in &added {
            println!(
                "  + {:<30} {}:{}  [{}]",
                truncate(&c.name, 30),
                c.ats,
                c.slug,
                c.stage.map(model::Stage::as_str).unwrap_or("?")
            );
        }
        println!("\nRun `jobsearch fetch` to pull their boards.");
    }
    Ok(())
}

// ---------------------------------------------------------------- inspect

fn cmd_inspect(paths: &Paths, url: Option<String>, limit: usize) -> Result<()> {
    let filters = load_filters(paths)?;
    let store = open_store(paths)?;

    let Some(url) = url else {
        for row in store.filtered_out()?.into_iter().take(limit) {
            let job = model::Job {
                title: row.title.clone(),
                location: row.location.clone(),
                description: row.description.clone(),
                ..Default::default()
            };
            let reasons = filters.rejection_reasons(&job);
            let reasons = if reasons.is_empty() {
                "?".to_string()
            } else {
                reasons.join("+")
            };
            println!(
                "[{reasons:<16}] {:<24} | {:<50} | {}",
                truncate(&row.company, 24),
                truncate(&row.title, 50),
                row.location
            );
        }
        return Ok(());
    };

    let Some(row) = store.get_by_url(&url)? else {
        println!("No stored record for {url}");
        return Ok(());
    };

    println!("{:<16}: {}", "company", row.company);
    println!("{:<16}: {}", "title", row.title);
    println!("{:<16}: {}", "location", row.location);
    println!("{:<16}: {}", "source", row.source);
    println!(
        "{:<16}: {}",
        "level",
        row.seniority()
            .map(model::Seniority::as_str)
            .unwrap_or("not stated in the title")
    );
    println!(
        "{:<16}: {}{}",
        "stage",
        row.stage.map(model::Stage::as_str).unwrap_or("unknown"),
        row.stage_source
            .map(|s| format!(" ({})", s.as_str()))
            .unwrap_or_default()
    );
    if let Some(h) = row.headcount {
        println!("{:<16}: {h}", "headcount");
    }
    if let (Some(score), Some(rec)) = (row.match_score, row.recommendation.as_deref()) {
        println!("{:<16}: {score}/100 {rec}", "ai");
    }
    if let Some(status) = &row.my_status {
        println!("{:<16}: {status}", "your status");
    }
    if let Some(fetched) = &row.fetched_at {
        println!("{:<16}: {fetched}", "first fetched");
    }
    println!("{:<16}: {}", "url", row.url);

    let job = model::Job {
        title: row.title.clone(),
        location: row.location.clone(),
        description: row.description.clone(),
        ..Default::default()
    };
    let reasons = filters.rejection_reasons(&job);
    if !reasons.is_empty() {
        println!("{:<16}: {}", "filtered out by", reasons.join("+"));
    }

    println!("{}", "-".repeat(60));
    let body = filters::collapse_whitespace(&filters::strip_html(&row.description));
    println!(
        "{}",
        if body.is_empty() {
            "(no description stored)"
        } else {
            &body
        }
    );
    Ok(())
}

// -------------------------------------------------------------------- add

struct AddArgs {
    company: String,
    title: String,
    location: String,
    url: String,
    posted: String,
    description: String,
    description_file: Option<String>,
    stage: Option<String>,
    headcount: Option<i64>,
    status: Option<String>,
    notes: Option<String>,
}

fn cmd_add(paths: &Paths, args: AddArgs) -> Result<()> {
    let store = open_store(paths)?;

    let description = match args.description_file.as_deref() {
        Some("-") => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("reading the description from stdin")?;
            buf
        }
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("reading the description from {path}"))?,
        None => args.description,
    };

    // Both are validated up front rather than silently dropped: a typo'd
    // --stage that quietly became "no stage" would be invisible until you
    // noticed the row missing from startup mode.
    let stage = match args.stage.as_deref() {
        Some(raw) => Some(model::Stage::parse(raw).ok_or_else(|| {
            anyhow::anyhow!(
                "--stage {raw:?} isn't a known stage. Valid, earliest first: {}",
                model::Stage::ALL
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?),
        None => None,
    };
    if let Some(status) = args.status.as_deref() {
        if !model::MY_STATUS_VALUES.contains(&status) {
            anyhow::bail!(
                "--status {status:?} isn't valid. One of: {}",
                model::MY_STATUS_VALUES.join(", ")
            );
        }
    }

    let entry = store::ManualEntry {
        url: args.url,
        company: args.company,
        title: args.title,
        location: args.location,
        posted_at: args.posted,
        description,
        stage,
        headcount: args.headcount,
        my_status: args.status,
        notes: args.notes,
    }
    .with_url();

    let created = store.upsert_manual_job(&entry)?;
    println!(
        "{} {} — {}",
        if created { "Added" } else { "Updated" },
        entry.company,
        entry.title
    );
    println!("  url   {}", entry.url);
    if let Some(stage) = entry.stage {
        println!("  stage {}", stage.as_str());
    }
    println!("\nIt shows inline in the TUI and is queued for `jobsearch evaluate`.");
    Ok(())
}

// --------------------------------------------------------------- dismiss

fn cmd_dismiss(
    paths: &Paths,
    url: Option<String>,
    company: Option<String>,
    reason: Option<String>,
) -> Result<()> {
    let store = open_store(paths)?;
    match (url, company) {
        (Some(url), _) => {
            let row = store
                .get_by_url(&url)?
                .ok_or_else(|| anyhow::anyhow!("no stored job with url {url}"))?;
            store.dismiss_job(&row, reason.as_deref())?;
            println!("Hidden: {} — {}", row.company, row.title);
        }
        (_, Some(company)) => {
            let n = store.dismiss_company(&company, reason.as_deref())?;
            println!("Hidden: every posting from {company} ({n} currently stored)");
            println!("Future postings from them are hidden too.");
        }
        _ => anyhow::bail!("pass --url <URL> or --company <NAME>"),
    }
    println!("Nothing was deleted. `jobsearch restore` puts it back.");
    Ok(())
}

fn cmd_restore(paths: &Paths, url: Option<String>, company: Option<String>) -> Result<()> {
    let store = open_store(paths)?;
    match (url, company) {
        (Some(url), _) => {
            store.restore_job(&url)?;
            println!("Restored {url}");
        }
        (_, Some(company)) => {
            store.restore_company(&company)?;
            println!("Restored every posting from {company}");
        }
        _ => anyhow::bail!("pass --url <URL> or --company <NAME>"),
    }
    Ok(())
}

fn cmd_dismissed(paths: &Paths) -> Result<()> {
    let store = open_store(paths)?;
    let companies = store.dismissed_company_list()?;
    let rows: Vec<_> = store
        .all_stored()?
        .into_iter()
        .filter(|r| r.dismissed && r.dismissed_scope == Some("job"))
        .collect();

    if companies.is_empty() && rows.is_empty() {
        println!("Nothing is hidden.");
        return Ok(());
    }

    if !companies.is_empty() {
        println!(
            "HIDDEN EMPLOYERS ({}) — including future postings",
            companies.len()
        );
        for (company, reason, at) in &companies {
            println!(
                "  {:<34} {}  {}",
                truncate(company, 34),
                &at[..at.len().min(10)],
                reason.as_deref().unwrap_or("")
            );
        }
        println!();
    }

    if !rows.is_empty() {
        println!("HIDDEN POSTINGS ({})", rows.len());
        for r in &rows {
            println!(
                "  {:<24} | {:<52} | {}",
                truncate(&r.company, 24),
                truncate(&r.title, 52),
                r.dismissed_reason.as_deref().unwrap_or("")
            );
        }
        println!();
    }
    println!("`jobsearch restore --url <URL>` or `--company <NAME>` puts one back.");
    Ok(())
}

// ------------------------------------------------------------------ tech

fn cmd_tech(
    paths: &Paths,
    tag: Option<String>,
    limit: usize,
    include_dismissed: bool,
) -> Result<()> {
    let store = open_store(paths)?;
    if store.tech().is_empty() {
        anyhow::bail!(
            "no technologies configured — {} is missing or failed to load",
            paths.tech().display()
        );
    }
    let rows: Vec<_> = store
        .all_candidates()?
        .into_iter()
        .filter(|r| include_dismissed || !r.dismissed)
        .collect();

    // --- one tag: list the postings ------------------------------------
    if let Some(needle) = tag {
        let Some(tag) = store.tech().resolve(&needle).cloned() else {
            anyhow::bail!("unknown tag {needle:?}. `jobsearch tech` lists what's available.");
        };
        let matching: Vec<_> = rows.iter().filter(|r| r.has_tech(&tag)).collect();
        println!(
            "{} of {} candidates mention {}\n",
            matching.len(),
            rows.len(),
            tag.qualified()
        );
        for r in matching.iter().take(limit) {
            println!(
                "  {:>3}  {:<24} | {:<46} | {}",
                r.match_score
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "–".into()),
                truncate(&r.company, 24),
                truncate(&r.title, 46),
                r.tech_summary(5)
            );
        }
        if matching.len() > limit {
            println!("  … and {} more", matching.len() - limit);
        }
        return Ok(());
    }

    // --- the summary ----------------------------------------------------
    let mut counts: std::collections::HashMap<&tech::Tag, usize> = Default::default();
    for row in &rows {
        for t in &row.tech {
            *counts.entry(t).or_default() += 1;
        }
    }
    let untagged = rows.iter().filter(|r| r.tech.is_empty()).count();

    println!(
        "{} candidates · {} carry at least one tag · {untagged} untagged\n",
        rows.len(),
        rows.len() - untagged
    );

    // Grouped by category, because the comparisons that matter are
    // within one — AWS vs GCP, Rust vs Java — not across.
    let mut category = "";
    for t in store.tech().all_tags() {
        let n = counts.get(t).copied().unwrap_or(0);
        if n == 0 {
            continue;
        }
        if t.category != category {
            category = &t.category;
            println!("{}", category.to_uppercase());
        }
        let pct = n as f64 / rows.len().max(1) as f64 * 100.0;
        let bar = "█".repeat((pct / 2.0).round() as usize);
        println!("  {:<16} {n:>5}  {pct:>5.1}%  {bar}", t.name);
    }
    println!("\n`jobsearch tech --tag rust` lists the postings for one tag.");
    println!("In the TUI, `t` opens the same list as a picker.");
    Ok(())
}

// --------------------------------------------------------------- diagnose

fn cmd_diagnose(paths: &Paths, examples: usize, days: i64) -> Result<()> {
    let filters = load_filters(paths)?;
    let store = open_store(paths)?;
    let rows = store.all_stored()?;

    if rows.is_empty() {
        println!("Nothing stored yet — run `jobsearch fetch` first.");
        return Ok(());
    }

    // ---- intake: is anything NEW arriving at all? ---------------------
    //
    // Deliberately first. "I'm not seeing new results" has two completely
    // different causes — nothing is arriving, or everything arriving is
    // being filtered out — and they need opposite fixes. Every other
    // section here is about the second; this one tells you whether that's
    // even the right question.
    println!("INTAKE (jobs first seen, last {days} days)");
    let intake = store.recent_intake(days)?;
    if intake.is_empty() {
        println!("  nothing new in {days} days.");
    } else {
        for (day, seen, passed) in &intake {
            println!("  {day}   {seen:>5} new   {passed:>4} passed filters");
        }
    }
    let total_new: i64 = intake.iter().map(|(_, n, _)| n).sum();
    println!("  ---");
    println!(
        "  {total_new} new postings in {days} days, out of {} stored.",
        rows.len()
    );
    println!(
        "  Boards only change when companies post; a re-run finding nothing new is the\n           normal case, not a bug. Dedup is on URL and on company+title+location.\n"
    );

    // ---- the funnel ---------------------------------------------------
    let passed = rows.iter().filter(|r| r.passed()).count();
    let mut by_reason: std::collections::HashMap<String, usize> = Default::default();
    let mut title_only: Vec<&model::JobRow> = Vec::new();
    let mut location_only: Vec<&model::JobRow> = Vec::new();
    let mut stack_only: Vec<&model::JobRow> = Vec::new();
    let mut rejected_locations: std::collections::HashMap<String, usize> = Default::default();
    let mut rejected_titles: std::collections::HashMap<String, usize> = Default::default();

    for row in &rows {
        let job = model::Job {
            title: row.title.clone(),
            location: row.location.clone(),
            description: row.description.clone(),
            ..Default::default()
        };
        let reasons = filters.rejection_reasons(&job);
        if reasons.is_empty() {
            continue;
        }
        *by_reason.entry(reasons.join("+")).or_default() += 1;
        // A job failing exactly ONE check is a near miss: one config
        // change away from being a candidate. Those are where loosening
        // actually buys you something.
        match reasons.as_slice() {
            ["title"] => title_only.push(row),
            ["location"] => {
                location_only.push(row);
                *rejected_locations.entry(row.location.clone()).or_default() += 1;
            }
            ["stack"] => stack_only.push(row),
            _ => {}
        }
        if reasons.contains(&"title") {
            *rejected_titles.entry(row.title.clone()).or_default() += 1;
        }
    }

    println!("FUNNEL ({} stored)", rows.len());
    println!(
        "  {passed:>6} passed all filters  ({:.1}%)",
        passed as f64 / rows.len() as f64 * 100.0
    );
    let mut reasons: Vec<_> = by_reason.iter().collect();
    reasons.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (reason, n) in reasons {
        println!("  {n:>6} failed on {reason}");
    }

    // ---- near misses --------------------------------------------------
    println!("\nNEAR MISSES — failing exactly one check, so one config change reaches them");
    println!(
        "  {:>6} location only   (right role, wrong place)",
        location_only.len()
    );
    println!(
        "  {:>6} title only      (right place, title didn't look like engineering)",
        title_only.len()
    );
    println!(
        "  {:>6} stack only      (right role and place, JD tripped stack_dealbreakers)",
        stack_only.len()
    );

    if !location_only.is_empty() {
        println!("\n  Top locations that failed ONLY on location:");
        let mut locs: Vec<_> = rejected_locations.into_iter().collect();
        locs.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for (loc, n) in locs.into_iter().take(examples) {
            println!("    {n:>4}  {}", truncate(&loc, 70));
        }
        println!("    -> widen filters.yaml's location_allow_patterns to take any of these.");
    }

    if !stack_only.is_empty() {
        println!("\n  Failed ONLY on stack (a dealbreaker language, none of stack_core):");
        for row in stack_only.iter().take(examples) {
            println!(
                "    {:<24} | {}",
                truncate(&row.company, 24),
                truncate(&row.title, 58)
            );
        }
        println!(
            "    -> if these look fine, the JD mentions a stack_dealbreakers language in\n                    passing. Move that language to stack_core, or drop it."
        );
    }

    if !title_only.is_empty() {
        println!("\n  Failed ONLY on title (location was fine):");
        let mut titles: Vec<_> = rejected_titles
            .into_iter()
            .filter(|(t, _)| title_only.iter().any(|r| &r.title == t))
            .collect();
        titles.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for (title, n) in titles.into_iter().take(examples) {
            println!("    {n:>4}  {}", truncate(&title, 70));
        }
        println!(
            "    -> add a keyword to title_allow_keywords, or remove one from\n                    exclusion_keywords / hard_exclusion_keywords."
        );
    }

    // ---- what the view filters hide on top ----------------------------
    let candidates: Vec<&model::JobRow> = rows.iter().filter(|r| r.passed()).collect();
    let hidden_stage = candidates
        .iter()
        .filter(|r| !filters.startup.admits(r))
        .count();
    let hidden_level = candidates
        .iter()
        .filter(|r| !filters.seniority.admits(r))
        .count();
    let dismissed = candidates.iter().filter(|r| r.dismissed).count();
    println!("\nVIEW FILTERS (applied on top, in the TUI — nothing is deleted)");
    if dismissed > 0 {
        println!(
            "  {dismissed:>6} of the {} candidates are hidden because you dismissed them",
            candidates.len()
        );
    }
    println!(
        "  {hidden_stage:>6} of the {} candidates are hidden by startup mode ({})",
        candidates.len(),
        filters.startup.describe()
    );
    println!(
        "  {hidden_level:>6} are hidden by the level policy ({})",
        filters.seniority.describe()
    );
    println!("    -> `s` and `L` in the TUI toggle these without touching any config.");

    Ok(())
}

// ------------------------------------------------------------------ check

fn cmd_check(paths: &Paths) -> Result<()> {
    let mut problems = 0;

    let companies = config::load_companies(paths.companies())
        .with_context(|| format!("{} is not valid", paths.companies().display()))?;
    println!("companies.yaml    {} entries", companies.len());

    let mut with_stage = 0;
    for c in &companies {
        if !sources::ats::SUPPORTED.contains(&c.ats.as_str()) {
            println!(
                "  ! {}: unknown ats {:?} (supported: {})",
                c.name,
                c.ats,
                sources::ats::SUPPORTED.join(", ")
            );
            problems += 1;
        }
        match (&c.stage, c.parsed_stage()) {
            (Some(raw), None) => {
                println!("  ! {}: stage {raw:?} isn't a known stage", c.name);
                problems += 1;
            }
            (Some(_), Some(_)) => with_stage += 1,
            (None, _) => {}
        }
    }
    println!(
        "                  {with_stage} have a curated stage, {} don't",
        companies.len() - with_stage
    );

    let aggs = config::load_aggregators(paths.aggregators())?;
    println!(
        "aggregators.yaml  {} entries ({} enabled)",
        aggs.len(),
        aggs.iter().filter(|a| a.enabled).count()
    );
    for a in &aggs {
        if !a.enabled {
            // A disabled entry usually means a source that is blocked or
            // costs money, and the reason lives in its `notes:`. Surfacing
            // it here is the difference between "why is nothing coming
            // from X" being a mystery and being answered.
            let why = a
                .notes
                .as_deref()
                .map(|n| n.split_whitespace().collect::<Vec<_>>().join(" "))
                .unwrap_or_else(|| "no note given".to_string());
            println!("  - {} is disabled: {}", a.name, truncate(&why, 90));
        }
    }

    let filters = load_filters(paths)?;
    println!("filters.yaml      compiled OK");
    println!(
        "                  startup policy:   {}",
        filters.startup.describe()
    );
    println!(
        "                  seniority policy: {}",
        filters.seniority.describe()
    );
    println!(
        "                  default: {}",
        if filters.startup.enabled_by_default {
            "startups only"
        } else {
            "all stages"
        }
    );

    match tech::TechDetector::load(paths.tech()) {
        Ok(t) => println!("tech.yaml         {} technologies", t.tag_count()),
        Err(e) => {
            println!("tech.yaml         ! {e}");
            println!("                    (tech detection disabled; everything else still works)");
        }
    }

    match config::load_profile(paths.profile()) {
        Ok(_) => println!("profile.yaml      OK"),
        Err(e) => {
            println!("profile.yaml      ! {e}");
            println!("                    (only needed for `jobsearch evaluate`)");
        }
    }

    match open_store(paths) {
        Ok(store) => {
            let all = store.all_candidates()?;
            let startup = all.iter().filter(|r| filters.startup.admits(r)).count();
            let scored = all.iter().filter(|r| r.match_score.is_some()).count();
            println!("database          {}", paths.db().display());
            let manual = all.iter().filter(|r| r.is_manual()).count();
            let in_level = all.iter().filter(|r| filters.seniority.admits(r)).count();
            println!(
                "                  {} candidates, {startup} startup, {scored} scored, \
                 {manual} entered by hand,\n                  {in_level} within the level \
                 policy, {} companies with a known stage",
                all.len(),
                store.company_stage_count()?
            );
        }
        Err(e) => {
            println!("database          ! {e}");
            problems += 1;
        }
    }

    if problems > 0 {
        println!("\n{problems} problem(s) found.");
        std::process::exit(1);
    }
    println!("\nEverything checks out.");
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}
