//! Loaders for the four YAML files that decide what a run does:
//! `companies.yaml` (who to query directly), `aggregators.yaml` (broad
//! keyword search), `filters.yaml` (what counts as in-scope, including the
//! startup policy), and `profile.yaml` (your experience, fed to the AI).
//!
//! Everything tunable lives in these files rather than in code, so
//! retargeting the search never means a recompile.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::model::Stage;

pub const COMPANIES_PATH: &str = "companies.yaml";
pub const AGGREGATORS_PATH: &str = "aggregators.yaml";
pub const FILTERS_PATH: &str = "filters.yaml";
pub const PROFILE_PATH: &str = "profile.yaml";
pub const TECH_PATH: &str = "tech.yaml";
pub const DB_PATH: &str = "data/seen_jobs.sqlite3";

// ---------------------------------------------------------------- companies

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Company {
    pub name: String,
    /// One of the keys in `sources::ats::FETCHERS`.
    pub ats: String,
    /// The identifier in that ATS's public API URL.
    pub slug: String,
    /// Hand-curated stage. Optional — absent just means "nobody has looked
    /// this up", and the startup filter falls back to whatever the YC
    /// directory or the AI step knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headcount: Option<i64>,
}

impl Company {
    /// Parsed form of the `stage:` field, or None if unset/unparseable.
    /// An unparseable value is treated as absent rather than as an error:
    /// a typo in one of 129 entries shouldn't fail the whole run, and
    /// `config check` reports them separately.
    pub fn parsed_stage(&self) -> Option<Stage> {
        self.stage.as_deref().and_then(Stage::parse)
    }
}

#[derive(Debug, Deserialize)]
struct CompaniesFile {
    companies: Vec<Company>,
}

pub fn load_companies(path: impl AsRef<Path>) -> Result<Vec<Company>> {
    let path = path.as_ref();
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let parsed: CompaniesFile =
        serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(parsed.companies)
}

// -------------------------------------------------------------- aggregators

#[derive(Debug, Clone, Deserialize)]
pub struct Aggregator {
    pub name: String,
    /// One of the keys in `sources::aggregators::FETCHERS` — "adzuna",
    /// "remotive", "hn", "yc".
    #[serde(rename = "type")]
    pub kind: String,
    /// Passed through to the source. Deliberately untyped: each source
    /// reads the keys it cares about, so adding a source doesn't mean
    /// changing this struct.
    #[serde(default)]
    pub params: BTreeMap<String, serde_yaml::Value>,
    #[serde(default)]
    pub notes: Option<String>,
    /// Sources that are known-blocked or cost money can ship in the file
    /// switched off, documented in place rather than deleted.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl Aggregator {
    pub fn param_str(&self, key: &str) -> Option<String> {
        self.params.get(key).and_then(|v| match v {
            serde_yaml::Value::String(s) => Some(s.clone()),
            serde_yaml::Value::Number(n) => Some(n.to_string()),
            serde_yaml::Value::Bool(b) => Some(b.to_string()),
            _ => None,
        })
    }

    pub fn param_u64(&self, key: &str) -> Option<u64> {
        self.params.get(key).and_then(|v| match v {
            serde_yaml::Value::Number(n) => n.as_u64(),
            serde_yaml::Value::String(s) => s.parse().ok(),
            _ => None,
        })
    }

    /// The subset of `params` that should be forwarded verbatim as query
    /// string pairs, with the pipeline-level controls removed. Those
    /// (`max_pages`, `country`, and friends) shape the request rather than
    /// being part of it, and sending them along would at best be ignored
    /// and at worst confuse the upstream API.
    pub fn query_params(&self, exclude: &[&str]) -> Vec<(String, String)> {
        self.params
            .iter()
            .filter(|(k, _)| !exclude.contains(&k.as_str()))
            .filter_map(|(k, v)| {
                let s = match v {
                    serde_yaml::Value::String(s) => s.clone(),
                    serde_yaml::Value::Number(n) => n.to_string(),
                    serde_yaml::Value::Bool(b) => b.to_string(),
                    _ => return None,
                };
                Some((k.clone(), s))
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
struct AggregatorsFile {
    aggregators: Vec<Aggregator>,
}

pub fn load_aggregators(path: impl AsRef<Path>) -> Result<Vec<Aggregator>> {
    let path = path.as_ref();
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let parsed: AggregatorsFile =
        serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(parsed.aggregators)
}

// ------------------------------------------------------------------ filters

/// The startup policy: where the cutoff on the stage axis sits, and what
/// to do about companies whose stage nobody knows.
///
/// This is a *view* policy, not a fetch policy. Everything still gets
/// fetched, filtered and stored regardless of stage — so flipping the
/// cutoff, or turning startup mode off entirely, never requires a
/// re-fetch, and a company that gets its stage filled in later
/// retroactively appears (or disappears) with no extra work.
#[derive(Debug, Clone, Deserialize)]
pub struct StartupPolicyConfig {
    /// Whether startup-only is the default view on launch. The TUI toggles
    /// it live with `s`, and `--all-stages` / `--startups-only` override
    /// it per command.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// The latest stage that still counts as a startup, inclusive.
    #[serde(default = "default_max_stage")]
    pub max_stage: String,
    /// Companies bigger than this are excluded even if their stage label
    /// says otherwise — headcount is the more honest signal when a
    /// "Series C" company has 4000 people.
    #[serde(default)]
    pub max_headcount: Option<i64>,
    /// Whether a company with no stage information at all survives the
    /// filter. Default false: startup mode should mean "known to be a
    /// startup", not "not known to be big". Set true while your curated
    /// coverage is still thin, so unknowns stay visible.
    #[serde(default)]
    pub include_unknown: bool,
}

fn default_max_stage() -> String {
    "growth".to_string()
}

impl Default for StartupPolicyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_stage: default_max_stage(),
            max_headcount: None,
            include_unknown: false,
        }
    }
}

/// Which rungs of the seniority ladder are in scope.
///
/// Like the startup policy, this is a VIEW policy, not a fetch policy:
/// every posting is stored whatever its level, so widening or narrowing
/// takes effect against jobs already in the database. Run
/// `jobsearch refilter` after changing it only if you also edited
/// `hard_exclusion_keywords` — the level policy itself needs no re-run.
#[derive(Debug, Clone, Deserialize)]
pub struct SeniorityPolicyConfig {
    /// Earliest rung to include, inclusive.
    #[serde(default = "default_min_level")]
    pub min: String,
    /// Latest rung to include, inclusive.
    #[serde(default = "default_max_level")]
    pub max: String,
    /// Whether a posting whose title carries no level marker survives.
    /// Default true, and it matters more here than for stage: a bare
    /// "Software Engineer" is one of the commonest titles there is, and
    /// excluding those would throw away most of the list.
    #[serde(default = "default_true")]
    pub include_unknown: bool,
}

fn default_min_level() -> String {
    "junior".to_string()
}

fn default_staleness() -> BTreeMap<String, i64> {
    // Mirrors the shipped filters.yaml so a config without the section
    // still flags the obvious cases rather than silently flagging nothing.
    BTreeMap::from([
        ("interview".to_string(), 21),
        ("recruiter".to_string(), 14),
        ("applied".to_string(), 28),
    ])
}

fn default_max_level() -> String {
    "staff".to_string()
}

impl Default for SeniorityPolicyConfig {
    fn default() -> Self {
        Self {
            min: default_min_level(),
            max: default_max_level(),
            include_unknown: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FiltersConfig {
    /// Level/track vetoes applied before anything else — see
    /// `filters::title_is_relevant` for why these can't be rescued.
    #[serde(default)]
    pub hard_exclusion_keywords: Vec<String>,
    pub priority_title_keywords: Vec<String>,
    pub title_allow_keywords: Vec<String>,
    pub exclusion_keywords: Vec<String>,
    pub location_allow_patterns: Vec<String>,
    pub stack_dealbreakers: Vec<String>,
    pub stack_core: Vec<String>,
    #[serde(default = "default_truncated_min")]
    pub truncated_description_min_chars: usize,
    #[serde(default)]
    pub startup: StartupPolicyConfig,
    #[serde(default)]
    pub seniority: SeniorityPolicyConfig,
    /// Phrases in the posting TEXT that mark it as an agency listing.
    #[serde(default)]
    pub agency_text_patterns: Vec<String>,
    /// Agency names, matched against the COMPANY FIELD only. Kept separate
    /// because matching a company name against the body text hits customer
    /// lists — "brands including Randstad, Motorola, Netflix" in a direct
    /// posting is not an agency listing.
    #[serde(default)]
    pub agency_company_patterns: Vec<String>,
    /// Days-in-status before a tracked application is flagged as stale,
    /// keyed by status name. Absent statuses are never flagged.
    #[serde(default = "default_staleness")]
    pub staleness: BTreeMap<String, i64>,
}

fn default_truncated_min() -> usize {
    400
}

pub fn load_filters(path: impl AsRef<Path>) -> Result<FiltersConfig> {
    let path = path.as_ref();
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let parsed: FiltersConfig =
        serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(parsed)
}

// ------------------------------------------------------------------ profile

/// The experience profile, kept as an opaque YAML value. Nothing in the
/// pipeline reads individual fields — it is serialized straight into the
/// AI prompt — so imposing a schema here would only mean rejecting
/// profiles that would have worked fine.
pub fn load_profile(path: impl AsRef<Path>) -> Result<serde_yaml::Value> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).with_context(|| {
        format!(
            "reading {} — copy profile.example.yaml to profile.yaml and fill it in",
            path.display()
        )
    })?;
    let parsed: serde_yaml::Value =
        serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(parsed)
}

// ------------------------------------------------------------------- paths

/// Where every config and data file lives, resolved once at startup.
/// Carrying this around beats reading the constants directly: the test
/// suite points it at a temp dir, and `--root` lets you run the binary
/// from anywhere rather than only from the repo root.
#[derive(Debug, Clone)]
pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn companies(&self) -> PathBuf {
        self.root.join(COMPANIES_PATH)
    }
    pub fn aggregators(&self) -> PathBuf {
        self.root.join(AGGREGATORS_PATH)
    }
    pub fn filters(&self) -> PathBuf {
        self.root.join(FILTERS_PATH)
    }
    pub fn profile(&self) -> PathBuf {
        self.root.join(PROFILE_PATH)
    }
    pub fn tech(&self) -> PathBuf {
        self.root.join(TECH_PATH)
    }
    pub fn db(&self) -> PathBuf {
        std::env::var("DB_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| self.root.join(DB_PATH))
    }
    pub fn candidates_csv(&self) -> PathBuf {
        self.root.join("data/candidates.csv")
    }
    pub fn scored_csv(&self) -> PathBuf {
        self.root.join("data/scored_candidates.csv")
    }
}

impl Default for Paths {
    fn default() -> Self {
        Self::new(".")
    }
}
