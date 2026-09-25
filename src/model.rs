//! The shared vocabulary of the pipeline: a fetched `Job`, an AI
//! `Evaluation`, the company `Stage` axis that startup mode filters on,
//! and the joined `JobRow` the TUI renders.

use serde::{Deserialize, Serialize};
use std::fmt;

/// One posting, as returned by any source. Every fetcher — ATS,
/// aggregator, or startup-native — normalizes to exactly this shape, so
/// filtering, dedup and storage never need to know where a job came from.
///
/// `description` is HTML or plain text, whatever the source gave us;
/// `filters::strip_html` handles both.
#[derive(Debug, Clone, Default)]
pub struct Job {
    pub company: String,
    pub title: String,
    pub location: String,
    pub url: String,
    pub posted_at: Option<String>,
    pub description: String,
    /// Which fetcher produced this — "greenhouse", "adzuna", "hn", ...
    pub source: String,
    /// Set only by sources that know the company's stage first-hand (YC
    /// knows every company it returns is a startup, and its batch/team
    /// size pin the stage down further). `None` means "this source has no
    /// opinion" — the curated registry or the AI step fills it in later.
    pub stage_hint: Option<StageHint>,
}

/// A stage claim plus where it came from. Provenance matters because the
/// three sources disagree in confidence: a hand-curated entry in
/// companies.yaml beats YC's dataset beats a model's guess from a JD, and
/// `StageSource::rank` is what resolves the conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageHint {
    pub stage: Stage,
    pub headcount: Option<i64>,
    pub source: StageSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StageSource {
    /// Typed into the manual-entry form. Highest trust of all: you were
    /// looking at the company when you wrote it, which is more than can be
    /// said for a list edited months ago.
    Manual,
    /// Hand-written `stage:` in companies.yaml. Highest trust: a human
    /// looked it up.
    Curated,
    /// Intrinsic to the source — a YC company is a startup by
    /// construction, and its batch year + team size place it on the axis.
    Source,
    /// Inferred by the AI evaluation step from the JD and company name.
    /// Cheapest to obtain, easiest to be wrong; only used where nothing
    /// better exists.
    Ai,
}

impl StageSource {
    /// Higher wins when two sources claim different stages for the same
    /// company.
    pub fn rank(self) -> u8 {
        match self {
            StageSource::Manual => 4,
            StageSource::Curated => 3,
            StageSource::Source => 2,
            StageSource::Ai => 1,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StageSource::Manual => "manual",
            StageSource::Curated => "curated",
            StageSource::Source => "source",
            StageSource::Ai => "ai",
        }
    }

    /// How the TUI labels it next to a stage. "you" reads better than
    /// "manual" in `series-a (you)`.
    pub fn label(self) -> &'static str {
        match self {
            StageSource::Manual => "you",
            StageSource::Curated => "curated",
            StageSource::Source => "yc",
            StageSource::Ai => "ai",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "manual" => Some(StageSource::Manual),
            "curated" => Some(StageSource::Curated),
            "source" => Some(StageSource::Source),
            "ai" => Some(StageSource::Ai),
            _ => None,
        }
    }
}

/// Company maturity, ordered earliest-to-latest. Startup mode is a cutoff
/// on this axis (see `filters::StartupPolicy`), which is why the ordering
/// is explicit and total rather than a free-text label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    PreSeed,
    Seed,
    SeriesA,
    SeriesB,
    SeriesC,
    /// Late-stage private — still scaling, no longer early.
    Growth,
    /// Publicly traded, or a private company at comparable scale.
    Public,
    /// FAANG-scale. Kept distinct from `Public` so "not a startup" can be
    /// said with different force for Datadog than for Amazon.
    BigTech,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::PreSeed => "pre-seed",
            Stage::Seed => "seed",
            Stage::SeriesA => "series-a",
            Stage::SeriesB => "series-b",
            Stage::SeriesC => "series-c",
            Stage::Growth => "growth",
            Stage::Public => "public",
            Stage::BigTech => "big-tech",
        }
    }

    /// Short form for the TUI's stage column, where horizontal space is
    /// the scarce resource.
    pub fn short(self) -> &'static str {
        match self {
            Stage::PreSeed => "pre",
            Stage::Seed => "seed",
            Stage::SeriesA => "A",
            Stage::SeriesB => "B",
            Stage::SeriesC => "C",
            Stage::Growth => "grow",
            Stage::Public => "pub",
            Stage::BigTech => "big",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        // Tolerant of the spellings a human actually writes in YAML, and
        // of what a model returns when asked for a stage: "Series A",
        // "series_a", "seriesA", "a" all land on SeriesA.
        let norm: String = s
            .trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        match norm.as_str() {
            "preseed" | "pre" => Some(Stage::PreSeed),
            "seed" => Some(Stage::Seed),
            "seriesa" | "a" => Some(Stage::SeriesA),
            "seriesb" | "b" => Some(Stage::SeriesB),
            "seriesc" | "c" | "seriesd" | "d" => Some(Stage::SeriesC),
            "growth" | "grow" | "latestage" | "late" => Some(Stage::Growth),
            "public" | "ipo" => Some(Stage::Public),
            "bigtech" | "faang" | "megacap" => Some(Stage::BigTech),
            _ => None,
        }
    }

    /// Every stage, earliest first — the cycle order for the TUI's stage
    /// filter and the list rendered in `--help` text.
    pub const ALL: [Stage; 8] = [
        Stage::PreSeed,
        Stage::Seed,
        Stage::SeriesA,
        Stage::SeriesB,
        Stage::SeriesC,
        Stage::Growth,
        Stage::Public,
        Stage::BigTech,
    ];
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the AI evaluation step returns for one job. The first two fields
/// are the decision-critical ones; the rest are the reasoning behind it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Evaluation {
    pub match_score: i64,
    pub recommendation: String,
    pub genuine_gaps: String,
    pub transferable_strengths: String,
    pub risk_factors: String,
    /// Stage the model inferred for the company, if it could. This is the
    /// AI half of the startup signal — it covers the long tail of
    /// aggregator-sourced companies that will never be hand-curated.
    pub company_stage: Option<Stage>,
    pub company_headcount: Option<i64>,
    /// The model's own confidence in the stage call, 0-100. Low values are
    /// worth surfacing rather than hiding: an uncertain "seed" is still
    /// better than no signal, but shouldn't read as fact.
    pub stage_confidence: Option<i64>,
}

/// A stage claim about a company, independent of any one posting. Stored
/// keyed by normalized company name so that a stage learned from one job
/// (or from the YC directory) applies to every other posting from the same
/// employer, including ones fetched months later from a different source.
#[derive(Debug, Clone)]
pub struct CompanyStage {
    pub company: String,
    pub stage: Stage,
    pub headcount: Option<i64>,
    pub source: StageSource,
    pub note: Option<String>,
}

/// One row of the review UI: a stored posting joined with its AI
/// evaluation, the user's own tracking, and whatever is known about the
/// company's stage.
#[derive(Debug, Clone, Default)]
pub struct JobRow {
    pub url: String,
    pub company: String,
    pub title: String,
    pub location: String,
    pub posted_at: Option<String>,
    pub description: String,
    pub source: String,
    pub fetched_at: Option<String>,

    pub match_score: Option<i64>,
    pub recommendation: Option<String>,
    pub genuine_gaps: Option<String>,
    pub transferable_strengths: Option<String>,
    pub risk_factors: Option<String>,

    pub my_status: Option<String>,
    pub notes: Option<String>,

    pub stage: Option<Stage>,
    pub headcount: Option<i64>,
    pub stage_source: Option<StageSource>,
}

impl JobRow {
    /// The AI's verdict, with "unscored" standing in for a job the
    /// evaluation step hasn't reached yet — a real state worth filtering
    /// on, not a missing value.
    pub fn ai_status(&self) -> &str {
        self.recommendation.as_deref().unwrap_or("unscored")
    }

    /// Whether you entered this by hand rather than a fetcher finding it.
    pub fn is_manual(&self) -> bool {
        self.source == MANUAL_SOURCE
    }
}

/// `job_details.source` for a hand-entered row. Every fetcher writes its
/// own name here, so this just claims one more value in that space — no
/// extra column, and the frozen web board shows it without changes.
pub const MANUAL_SOURCE: &str = "manual";

/// The statuses you assign yourself, deliberately a different vocabulary
/// from the AI's apply/consider/skip so the two can never be confused in
/// the UI: the AI recommends before you apply, this records what actually
/// happened after.
pub const MY_STATUS_VALUES: [&str; 5] = ["applied", "interview", "rejected", "skipped", "silence"];
