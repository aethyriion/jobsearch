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
    /// Whether this row currently passes the deterministic filters. Most
    /// reads select only passing rows so this is redundant there, but
    /// `diagnose` needs the whole funnel in one pass.
    pub passed_filters: bool,

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

    pub fn passed(&self) -> bool {
        self.passed_filters
    }

    /// Whether you entered this by hand rather than a fetcher finding it.
    pub fn is_manual(&self) -> bool {
        self.source == MANUAL_SOURCE
    }

    /// The role's level, read from its title.
    ///
    /// Derived rather than stored: it's a pure function of the title, so
    /// computing it on load means improving the heuristic takes effect
    /// immediately instead of needing a migration and a re-scan.
    pub fn seniority(&self) -> Option<Seniority> {
        infer_seniority(&self.title)
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

/// Where a posting sits on the seniority ladder, earliest to latest.
///
/// A separate axis from `Stage`: stage is about the company, this is about
/// the role. Both are view-level filters for the same reason — everything
/// is stored regardless, so changing your mind about either is free.
///
/// `Manager` is deliberately last and deliberately *on* the scale even
/// though it isn't really a level: it's a different track, and having a
/// name for it is what lets the policy exclude it explicitly rather than
/// by a keyword veto buried elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Seniority {
    Intern,
    Junior,
    Mid,
    Senior,
    Staff,
    Manager,
}

impl Seniority {
    pub fn as_str(self) -> &'static str {
        match self {
            Seniority::Intern => "intern",
            Seniority::Junior => "junior",
            Seniority::Mid => "mid",
            Seniority::Senior => "senior",
            Seniority::Staff => "staff",
            Seniority::Manager => "manager",
        }
    }

    /// Short form for the list's level column.
    pub fn short(self) -> &'static str {
        match self {
            Seniority::Intern => "int",
            Seniority::Junior => "jr",
            Seniority::Mid => "mid",
            Seniority::Senior => "snr",
            Seniority::Staff => "stf",
            Seniority::Manager => "mgr",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let norm: String = s
            .trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        match norm.as_str() {
            "intern" | "internship" | "int" => Some(Seniority::Intern),
            "junior" | "jr" | "entry" | "entrylevel" | "newgrad" | "grad" => {
                Some(Seniority::Junior)
            }
            "mid" | "midlevel" | "intermediate" => Some(Seniority::Mid),
            "senior" | "snr" | "sr" => Some(Seniority::Senior),
            "staff" | "stf" | "principal" | "lead" => Some(Seniority::Staff),
            "manager" | "mgr" | "management" | "director" => Some(Seniority::Manager),
            _ => None,
        }
    }

    pub const ALL: [Seniority; 6] = [
        Seniority::Intern,
        Seniority::Junior,
        Seniority::Mid,
        Seniority::Senior,
        Seniority::Staff,
        Seniority::Manager,
    ];
}

impl fmt::Display for Seniority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Infer seniority from a job title, or None when the title doesn't say.
///
/// None is a real and common answer — a bare "Software Engineer" genuinely
/// carries no level, and guessing "mid" for it would put invented data
/// behind a filter. `include_unknown` in the policy is what decides
/// whether those show, rather than this function pretending to know.
///
/// Ordered most-specific-first, because titles stack modifiers
/// ("Senior Staff Engineer" is staff, "Associate Director" is management).
pub fn infer_seniority(title: &str) -> Option<Seniority> {
    // Punctuation becomes spaces so the word-boundary checks below work on
    // real titles: "Sr. Software Engineer" and "Engineer, Level II" both
    // need their markers to read as standalone words.
    let t = format!(
        " {} ",
        title
            .to_lowercase()
            .replace(['/', ',', '(', ')', '-', '.', ':', ';', '|'], " ")
    );
    let has = |needle: &str| t.contains(&format!(" {needle} "));
    let contains = |needle: &str| t.contains(needle);

    // Management track first: "Engineering Manager" also contains
    // "engineer", and "Director of Platform" would otherwise read as a
    // plain platform role.
    if contains("manager")
        || contains("director")
        || has("vp")
        || contains("vice president")
        || contains("head of")
    {
        return Some(Seniority::Manager);
    }

    if contains("intern") && !contains("internal") || contains("internship") {
        return Some(Seniority::Intern);
    }

    // "Member of Technical Staff" is a flat title used at every level by
    // several AI labs — reading its "staff" as staff-level would promote a
    // new-grad posting to the top of the ladder. Check it before the staff
    // markers below. New-grad MTS postings say so explicitly, so the
    // junior check below still catches those.
    let technical_staff = contains("technical staff") || contains("member of staff");

    // Explicit level WORDS come before roman numerals, because a bare "I"
    // is a weak signal that must not outrank a spelled-out rung. Without
    // this split, "Principal Software Engineer I" read as junior — that
    // exact title is in the database.
    if contains("new grad")
        || contains("new graduate")
        || contains("university grad")
        || contains("entry level")
        || has("junior")
        || has("jr")
        || contains("apprentice")
        || contains("trainee")
        || contains("co op")
        || contains("associate engineer")
    {
        return Some(Seniority::Junior);
    }

    if !technical_staff
        && (has("staff") || contains("principal") || has("lead") || has("distinguished"))
    {
        return Some(Seniority::Staff);
    }

    if has("senior") || has("sr") {
        return Some(Seniority::Senior);
    }

    // Roman numerals last, and longest-first so "III" isn't read as "II".
    if contains("engineer iii") || has("iii") {
        return Some(Seniority::Senior);
    }
    if contains("engineer ii") || has("ii") {
        return Some(Seniority::Mid);
    }
    if has("mid") || contains("mid level") {
        return Some(Seniority::Mid);
    }
    if contains("engineer i ") || has("i") {
        return Some(Seniority::Junior);
    }

    None
}

#[cfg(test)]
mod seniority_tests {
    use super::*;

    #[track_caller]
    fn assert_level(title: &str, expected: Option<Seniority>) {
        assert_eq!(infer_seniority(title), expected, "for title {title:?}");
    }

    #[test]
    fn reads_the_obvious_markers() {
        assert_level("Junior DevOps Engineer", Some(Seniority::Junior));
        assert_level("Software Engineer - New Grad", Some(Seniority::Junior));
        assert_level("Software Engineer, Entry Level", Some(Seniority::Junior));
        assert_level("Senior Software Engineer", Some(Seniority::Senior));
        assert_level("Sr. Platform Engineer", Some(Seniority::Senior));
        assert_level("Staff Software Engineer", Some(Seniority::Staff));
        assert_level("Principal Engineer", Some(Seniority::Staff));
        assert_level("Engineering Intern", Some(Seniority::Intern));
    }

    #[test]
    fn roman_numerals_land_on_the_right_rung() {
        assert_level("Software Engineer I", Some(Seniority::Junior));
        assert_level("Software Engineer II", Some(Seniority::Mid));
        assert_level("Software Engineer III", Some(Seniority::Senior));
    }

    #[test]
    fn management_is_recognized_before_the_engineering_words() {
        // "Engineering Manager" contains "engineer"; "Director of Platform"
        // would otherwise read as a plain platform role.
        assert_level("Engineering Manager", Some(Seniority::Manager));
        assert_level("Director of Security Engineering", Some(Seniority::Manager));
        assert_level("VP Engineering", Some(Seniority::Manager));
        assert_level("Head of Infrastructure", Some(Seniority::Manager));
    }

    #[test]
    fn member_of_technical_staff_is_not_staff_level() {
        // Several AI labs use MTS as a flat title at every level. Reading
        // its "staff" as staff-level would promote a new-grad posting to
        // the top of the ladder — this exact title is in the database.
        assert_level("Member of Technical Staff", None);
        assert_level(
            "Member of Technical Staff, New Grad (BS/MS)",
            Some(Seniority::Junior),
        );
        assert_level(
            "Member of Technical Staff, Systems Infrastructure (2026 PhD New Grad)",
            Some(Seniority::Junior),
        );
    }

    #[test]
    fn the_highest_marker_in_a_stacked_title_wins() {
        assert_level("Senior Staff Software Engineer", Some(Seniority::Staff));
        assert_level("Senior Engineering Manager", Some(Seniority::Manager));
    }

    #[test]
    fn a_spelled_out_rung_outranks_a_bare_roman_numeral() {
        // "Principal Software Engineer I" is a real title in the database.
        // Reading its standalone "I" as junior demoted a principal role to
        // the bottom of the ladder.
        assert_level(
            "Principal Software Engineer I - Serverless - Platform Control Plane",
            Some(Seniority::Staff),
        );
        assert_level("Senior Software Engineer I", Some(Seniority::Senior));
        assert_level("Staff Engineer II", Some(Seniority::Staff));
        // With no spelled-out rung, the numeral is still the signal.
        assert_level("Software Engineer I", Some(Seniority::Junior));
    }

    #[test]
    fn a_bare_title_has_no_level_rather_than_a_guessed_one() {
        // Extremely common, and genuinely carries no level. Guessing "mid"
        // here would put invented data behind a filter.
        assert_level("Software Engineer", None);
        assert_level("Backend Engineer, Control Plane", None);
    }

    #[test]
    fn internal_is_not_intern() {
        assert_level("Software Engineer, Internal Tools", None);
    }

    #[test]
    fn the_scale_is_ordered_so_a_range_filter_works() {
        assert!(Seniority::Intern < Seniority::Junior);
        assert!(Seniority::Junior < Seniority::Mid);
        assert!(Seniority::Mid < Seniority::Senior);
        assert!(Seniority::Senior < Seniority::Staff);
        assert!(Seniority::Staff < Seniority::Manager);
    }
}
