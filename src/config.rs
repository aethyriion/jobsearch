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
    /// one typo in a long companies.yaml shouldn't fail the whole run, and
    /// `jobsearch check` reports them separately.
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
///
/// Config and data are deliberately separate directories. A binary
/// installed from crates.io, Homebrew or Scoop lives on `PATH` with
/// nothing beside it, and the directory it was installed into is usually
/// not writable — so the YAML it reads and the SQLite database it writes
/// cannot be the same place. Keeping them apart is what lets the same
/// binary work from a package install and from a git checkout.
#[derive(Debug, Clone)]
pub struct Paths {
    /// Read-only configuration: the four YAML files and `.env`.
    pub root: PathBuf,
    /// Writable state: the SQLite database and the CSV exports.
    pub data: PathBuf,
}

/// How `Paths` was resolved, so `check` and the first-run message can tell
/// the user where the tool is actually reading from. Guessing wrong about
/// this is the single most confusing failure a packaged CLI has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootSource {
    /// `--root DIR`.
    Flag,
    /// The `JOBSEARCH_ROOT` environment variable.
    Env,
    /// The working directory, because it holds a `filters.yaml`.
    WorkingDir,
    /// The per-user config directory.
    ConfigDir,
}

impl RootSource {
    pub fn describe(self) -> &'static str {
        match self {
            RootSource::Flag => "--root",
            RootSource::Env => "JOBSEARCH_ROOT",
            RootSource::WorkingDir => "the working directory",
            RootSource::ConfigDir => "your config directory",
        }
    }
}

/// The per-user config directory: `~/.config/jobsearch` on Linux,
/// `~/Library/Application Support/jobsearch` on macOS,
/// `%APPDATA%\jobsearch` on Windows.
pub fn config_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "jobsearch").map(|d| d.config_dir().to_path_buf())
}

/// The per-user data directory. Separate from the config directory because
/// one is hand-edited and worth backing up, and the other is a cache that
/// can be rebuilt by re-running `fetch`.
pub fn data_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "jobsearch").map(|d| d.data_dir().to_path_buf())
}

impl Paths {
    /// Config and data in the same directory — the git-checkout layout, and
    /// what the test suite uses against a temp dir.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let data = root.join("data");
        Self { root, data }
    }

    pub fn with_data(root: impl Into<PathBuf>, data: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            data: data.into(),
        }
    }

    /// Work out where config lives, in priority order:
    ///
    /// 1. `--root DIR`, which wins over everything.
    /// 2. `JOBSEARCH_ROOT`.
    /// 3. The working directory, but ONLY if it actually holds a
    ///    `filters.yaml`. The bare "." default used to apply everywhere,
    ///    which meant a packaged binary run from `/tmp` looked for config
    ///    in `/tmp` and died. Requiring the marker file keeps the
    ///    run-from-the-repo workflow working without claiming every
    ///    directory on the system.
    /// 4. The per-user config directory, which is where `init` writes.
    ///
    /// Only case 3 keeps data beside config; everywhere else the database
    /// goes to the per-user data directory, which is always writable.
    pub fn resolve(flag: Option<PathBuf>) -> (Self, RootSource) {
        if let Some(root) = flag {
            return (Self::new(root), RootSource::Flag);
        }
        if let Some(root) = std::env::var_os("JOBSEARCH_ROOT").filter(|v| !v.is_empty()) {
            return (Self::new(PathBuf::from(root)), RootSource::Env);
        }
        let cwd = PathBuf::from(".");
        if cwd.join(FILTERS_PATH).is_file() {
            return (Self::new(cwd), RootSource::WorkingDir);
        }
        let root = config_dir().unwrap_or_else(|| PathBuf::from("."));
        let data = data_dir().unwrap_or_else(|| root.join("data"));
        (Self::with_data(root, data), RootSource::ConfigDir)
    }

    /// Whether this looks like a set-up config directory. `init` has not
    /// been run if the file every subcommand needs isn't there.
    pub fn is_initialized(&self) -> bool {
        self.filters().is_file()
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
    pub fn env_file(&self) -> PathBuf {
        self.root.join(".env")
    }
    pub fn db(&self) -> PathBuf {
        // Namespaced: a bare DB_PATH is far too generic for a binary that
        // now lives on the global PATH alongside everything else.
        std::env::var("JOBSEARCH_DB_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| self.data.join("seen_jobs.sqlite3"))
    }
    pub fn candidates_csv(&self) -> PathBuf {
        self.data.join("candidates.csv")
    }
    pub fn scored_csv(&self) -> PathBuf {
        self.data.join("scored_candidates.csv")
    }
}

/// The config files compiled into the binary, written out by `init`.
///
/// Embedding rather than installing them to `/usr/share` is what makes
/// every distribution channel a single file: crates.io, Homebrew and Scoop
/// all ship one binary and nothing else, and none of them has a good place
/// to put auxiliary data.
pub const BUNDLED_CONFIG: &[(&str, &str)] = &[
    (FILTERS_PATH, include_str!("../filters.yaml")),
    (COMPANIES_PATH, include_str!("../companies.yaml")),
    (AGGREGATORS_PATH, include_str!("../aggregators.yaml")),
    (TECH_PATH, include_str!("../tech.yaml")),
    (
        "profile.example.yaml",
        include_str!("../profile.example.yaml"),
    ),
    (".env.example", include_str!("../.env.example")),
];

impl Default for Paths {
    fn default() -> Self {
        Self::new(".")
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    /// A directory only counts as a config root when it actually holds a
    /// `filters.yaml`. The old default treated EVERY working directory as
    /// one, which is why a packaged binary run from /tmp looked for config
    /// in /tmp and died with "reading ./companies.yaml".
    #[test]
    fn the_working_directory_wins_only_when_it_holds_config() {
        let dir = std::env::temp_dir().join(format!("jobsearch-pt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let empty = Paths::new(&dir);
        assert!(!empty.is_initialized());

        std::fs::write(dir.join(FILTERS_PATH), "# marker\n").unwrap();
        assert!(Paths::new(&dir).is_initialized());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `--root` must beat everything, including a config directory that
    /// exists. It is the escape hatch for running several searches side by
    /// side out of different checkouts.
    #[test]
    fn an_explicit_root_wins_over_every_other_source() {
        let (paths, source) = Paths::resolve(Some(PathBuf::from("/nowhere/in/particular")));
        assert_eq!(source, RootSource::Flag);
        assert_eq!(paths.root, PathBuf::from("/nowhere/in/particular"));
        assert_eq!(
            paths.filters(),
            PathBuf::from("/nowhere/in/particular/filters.yaml")
        );
    }

    /// Config is read-only and hand-edited; data is a rebuildable cache.
    /// They must not be the same directory when the binary is installed to
    /// somewhere unwritable, which is every packaged install.
    #[test]
    fn config_and_data_can_live_apart() {
        let p = Paths::with_data("/etc/jobsearch", "/var/lib/jobsearch");
        assert_eq!(p.filters(), PathBuf::from("/etc/jobsearch/filters.yaml"));
        assert_eq!(
            p.db(),
            PathBuf::from("/var/lib/jobsearch/seen_jobs.sqlite3")
        );
        assert_eq!(
            p.candidates_csv(),
            PathBuf::from("/var/lib/jobsearch/candidates.csv")
        );
    }

    /// In a git checkout the two coincide, which is what keeps the
    /// run-from-the-repo workflow working exactly as before.
    #[test]
    fn a_checkout_keeps_data_beside_config() {
        let p = Paths::new("/home/u/jobsearch");
        assert_eq!(p.filters(), PathBuf::from("/home/u/jobsearch/filters.yaml"));
        assert_eq!(
            p.db(),
            PathBuf::from("/home/u/jobsearch/data/seen_jobs.sqlite3")
        );
    }

    /// Every file `init` writes must be one the loaders actually look for,
    /// or the binary ships defaults nothing reads.
    #[test]
    fn the_bundled_config_covers_what_the_loaders_require() {
        let names: Vec<&str> = BUNDLED_CONFIG.iter().map(|(n, _)| *n).collect();
        for required in [FILTERS_PATH, COMPANIES_PATH, AGGREGATORS_PATH, TECH_PATH] {
            assert!(names.contains(&required), "init does not write {required}");
        }
        for (name, contents) in BUNDLED_CONFIG {
            assert!(!contents.is_empty(), "{name} embedded empty");
        }
    }

    /// The shipped filters.yaml has to survive its own parser, or `init`
    /// hands every new user a config that cannot load.
    #[test]
    fn the_embedded_filters_actually_parse() {
        let (_, text) = BUNDLED_CONFIG
            .iter()
            .find(|(n, _)| *n == FILTERS_PATH)
            .expect("filters.yaml is bundled");
        let cfg: FiltersConfig = serde_yaml::from_str(text).expect("embedded filters.yaml parses");
        assert!(!cfg.location_allow_patterns.is_empty());
    }
}
