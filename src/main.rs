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
            tui::run(paths, filters).await
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
        Command::Check => cmd_check(&paths),
    }
}

fn load_filters(paths: &Paths) -> Result<Filters> {
    let cfg = config::load_filters(paths.filters())?;
    Filters::compile(&cfg)
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
    let store = Store::open(paths.db())?;

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
    let store = Store::open(paths.db())?;
    let startup_only = !all_stages && filters.startup.enabled_by_default;

    let policy = startup_only.then_some(&filters.startup);
    let mut queue = store.unevaluated(policy)?;
    if let Some(limit) = limit {
        queue.truncate(limit);
    }

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
                "WOULD EVALUATE | {:<8} | {:<24} | {}",
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
    let store = Store::open(paths.db())?;
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
    let store = Store::open(paths.db())?;

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
    let store = Store::open(paths.db())?;

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
        "                  startup policy: {}",
        filters.startup.describe()
    );
    println!(
        "                  default: {}",
        if filters.startup.enabled_by_default {
            "startups only"
        } else {
            "all stages"
        }
    );

    match config::load_profile(paths.profile()) {
        Ok(_) => println!("profile.yaml      OK"),
        Err(e) => {
            println!("profile.yaml      ! {e}");
            println!("                    (only needed for `jobsearch evaluate`)");
        }
    }

    match Store::open(paths.db()) {
        Ok(store) => {
            let all = store.all_candidates()?;
            let startup = all.iter().filter(|r| filters.startup.admits(r)).count();
            let scored = all.iter().filter(|r| r.match_score.is_some()).count();
            println!("database          {}", paths.db().display());
            println!(
                "                  {} candidates, {startup} startup, {scored} scored, \
                 {} companies with a known stage",
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
