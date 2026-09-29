//! Deterministic pre-filters: title allowlist + exclusion keywords +
//! location allowlist + JD stack dealbreakers, plus the startup-stage
//! policy.
//!
//! These run BEFORE any AI call — the whole point is to keep the
//! AI-scored volume small and cheap. The keyword and pattern lists live in
//! `filters.yaml` so they can be edited without a recompile.
//!
//! Title filtering is allowlist-first. An exclusion-only approach doesn't
//! scale to a company that posts hundreds of non-engineering roles
//! (Analyst, Compliance, Marketing, Customer Success, Talent Brand...) —
//! you can't blacklist your way out of that. So a title must look like an
//! engineering role at all before we consider it, and only then does a
//! smaller exclusion list catch engineering-adjacent titles that aren't a
//! fit (sales engineer, hardware engineer).
//!
//! The startup policy is deliberately NOT part of `passes_filters`. Stage
//! is a view-level concern: everything gets fetched and stored whatever
//! its stage, so changing the cutoff is instant and reversible instead of
//! meaning a re-fetch. See `StartupPolicy::admits`.

use anyhow::{anyhow, Result};
use regex::Regex;

use crate::config::{FiltersConfig, SeniorityPolicyConfig, StartupPolicyConfig};
use crate::model::{Job, JobRow, Seniority, Stage};

/// Compiled, ready-to-apply form of `filters.yaml`.
pub struct Filters {
    hard_exclusion: Option<Regex>,
    priority: Regex,
    title_allow: Regex,
    exclusion: Regex,
    locations: Vec<Regex>,
    dealbreakers: Vec<Regex>,
    core_stack: Vec<Regex>,
    truncated_min_chars: usize,
    pub startup: StartupPolicy,
    pub seniority: SeniorityPolicy,
    /// Phrases matched against the posting text.
    pub agency_text: regex::RegexSet,
    /// Names matched against the company field only.
    pub agency_company: regex::RegexSet,
    /// Days-in-status before a tracked application counts as stale.
    pub staleness: std::collections::BTreeMap<String, i64>,
}

impl Filters {
    pub fn compile(cfg: &FiltersConfig) -> Result<Self> {
        Ok(Self {
            hard_exclusion: compile_word_boundary_alternation(&cfg.hard_exclusion_keywords),
            priority: compile_keyword_alternation(
                &cfg.priority_title_keywords,
                "priority_title_keywords",
            )?,
            title_allow: compile_keyword_alternation(
                &cfg.title_allow_keywords,
                "title_allow_keywords",
            )?,
            exclusion: compile_keyword_alternation(&cfg.exclusion_keywords, "exclusion_keywords")?,
            locations: compile_patterns(&cfg.location_allow_patterns, "location_allow_patterns")?,
            dealbreakers: compile_patterns(&cfg.stack_dealbreakers, "stack_dealbreakers")?,
            core_stack: compile_patterns(&cfg.stack_core, "stack_core")?,
            truncated_min_chars: cfg.truncated_description_min_chars,
            startup: StartupPolicy::from_config(&cfg.startup)?,
            seniority: SeniorityPolicy::from_config(&cfg.seniority)?,
            agency_text: compile_set(&cfg.agency_text_patterns, "agency_text_patterns")?,
            agency_company: compile_set(&cfg.agency_company_patterns, "agency_company_patterns")?,
            staleness: cfg.staleness.clone(),
        })
    }

    /// A title passes if it looks like engineering work at the right level.
    pub fn title_is_relevant(&self, title: &str) -> bool {
        if let Some(hard) = &self.hard_exclusion {
            if hard.is_match(title) {
                // Wrong level or wrong track — deliberately checked before
                // the priority list, because "Junior DevOps Engineer" and
                // "Software Engineering Manager" both contain a priority
                // keyword as a substring and would otherwise be rescued.
                return false;
            }
        }
        if self.priority.is_match(title) {
            // Unambiguous engineering title — e.g. "Software Engineer,
            // Automated Marketing". Skip the exclusion list entirely so a
            // word like "marketing" elsewhere in the title can't veto it.
            return true;
        }
        if !self.title_allow.is_match(title) {
            return false;
        }
        !self.exclusion.is_match(title)
    }

    pub fn location_is_allowed(&self, location: &str) -> bool {
        self.locations.iter().any(|p| p.is_match(location))
    }

    /// True if the JD looks like a dealbreaker-language role with no
    /// mention of your own core stack. An empty description means
    /// "unknown" — never a rejection, since most of the sources that omit
    /// a description omit it for every job equally.
    pub fn jd_stack_mismatch(&self, description: &str) -> bool {
        let text = strip_html(description);
        if text.trim().is_empty() {
            return false;
        }
        let has_dealbreaker = self.dealbreakers.iter().any(|p| p.is_match(&text));
        let has_core = self.core_stack.iter().any(|p| p.is_match(&text));
        has_dealbreaker && !has_core
    }

    /// True if `description` reads like a short aggregator teaser rather
    /// than a full posting — either it visibly cuts off mid-sentence, or
    /// it is too short to contain real requirements.
    ///
    /// Two callers depend on this: the aggregator fetchers, to decide
    /// whether a full-JD fetch is worth an extra request, and the AI step,
    /// to tell the model the description is partial so a thin snippet
    /// doesn't quietly read as "no responsibilities listed".
    pub fn looks_truncated(&self, description: &str) -> bool {
        let text = strip_html(description);
        let text = text.trim();
        if text.is_empty() {
            return true;
        }
        if text.ends_with('…') || text.ends_with("...") || text.ends_with("..") {
            return true;
        }
        text.chars().count() < self.truncated_min_chars
    }

    pub fn passes(&self, job: &Job) -> bool {
        self.title_is_relevant(&job.title)
            && self.location_is_allowed(&job.location)
            && !self.jd_stack_mismatch(&job.description)
    }

    /// How many days past its threshold a tracked row is, or `None` if it
    /// isn't stale.
    ///
    /// Only statuses with a configured threshold can go stale. `rejected`,
    /// `skipped` and `silence` are terminal — there's nothing left to
    /// chase, so flagging them would be noise.
    pub fn staleness_days(&self, row: &JobRow) -> Option<i64> {
        let status = row.my_status.as_deref()?;
        let threshold = *self.staleness.get(status)?;
        let age = row.status_age_days()?;
        (age >= threshold).then_some(age - threshold)
    }

    pub fn is_stale(&self, row: &JobRow) -> bool {
        self.staleness_days(row).is_some()
    }

    /// Which checks a job fails, for explaining a rejection. Empty means
    /// it passes.
    pub fn rejection_reasons(&self, job: &Job) -> Vec<&'static str> {
        let mut out = Vec::new();
        if !self.title_is_relevant(&job.title) {
            out.push("title");
        }
        if !self.location_is_allowed(&job.location) {
            out.push("location");
        }
        if self.jd_stack_mismatch(&job.description) {
            out.push("stack");
        }
        out
    }
}

// ------------------------------------------------------------ startup mode

/// The stage cutoff that "startup" means for this search.
#[derive(Debug, Clone)]
pub struct StartupPolicy {
    /// Whether startup-only is on by default. Runtime state lives in the
    /// TUI, which can flip it without touching this.
    pub enabled_by_default: bool,
    pub max_stage: Stage,
    pub max_headcount: Option<i64>,
    pub include_unknown: bool,
}

impl StartupPolicy {
    pub fn from_config(cfg: &StartupPolicyConfig) -> Result<Self> {
        let max_stage = Stage::parse(&cfg.max_stage).ok_or_else(|| {
            anyhow!(
                "filters.yaml: startup.max_stage '{}' isn't a known stage. \
                 Valid values, earliest first: {}",
                cfg.max_stage,
                Stage::ALL
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        Ok(Self {
            enabled_by_default: cfg.enabled,
            max_stage,
            max_headcount: cfg.max_headcount,
            include_unknown: cfg.include_unknown,
        })
    }

    /// Whether a row survives startup mode.
    ///
    /// Headcount vetoes independently of stage: a company labelled
    /// "series-c" with 4000 people is not the kind of place this mode
    /// exists to surface, and headcount is the less gameable of the two
    /// signals. A company with a stage but no headcount is judged on
    /// stage alone rather than being dropped for missing data.
    pub fn admits(&self, row: &JobRow) -> bool {
        // A hand-entered row is never filtered out. Startup mode is a
        // discovery aid — it decides which of thousands of scraped
        // postings are worth your attention — and an application you
        // typed in yourself was never discovered. Hiding a job you
        // actually applied to because the employer turned out to be
        // public would defeat the point of tracking it.
        if row.is_manual() {
            return true;
        }
        if let Some(max) = self.max_headcount {
            if row.headcount.is_some_and(|h| h > max) {
                return false;
            }
        }
        match row.stage {
            Some(stage) => stage <= self.max_stage,
            None => self.include_unknown,
        }
    }

    /// One-line description of the current cutoff, for the TUI header and
    /// the CLI's run summary.
    pub fn describe(&self) -> String {
        let mut s = format!("<= {}", self.max_stage.as_str());
        if let Some(max) = self.max_headcount {
            s.push_str(&format!(", <= {max} people"));
        }
        if self.include_unknown {
            s.push_str(", incl. unknown");
        }
        s
    }
}

// -------------------------------------------------------- seniority mode

/// Which rungs of the ladder count as in scope.
///
/// A separate axis from the startup policy: that one is about the company,
/// this one is about the role. Both are view-level for the same reason —
/// everything is stored regardless, so changing either is instant and
/// needs no re-fetch.
#[derive(Debug, Clone)]
pub struct SeniorityPolicy {
    pub min: Seniority,
    pub max: Seniority,
    pub include_unknown: bool,
}

impl SeniorityPolicy {
    pub fn from_config(cfg: &SeniorityPolicyConfig) -> Result<Self> {
        let parse = |raw: &str, field: &str| {
            Seniority::parse(raw).ok_or_else(|| {
                anyhow!(
                    "filters.yaml: seniority.{field} '{raw}' isn't a known level. \
                     Valid values, lowest first: {}",
                    Seniority::ALL
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
        };
        let min = parse(&cfg.min, "min")?;
        let max = parse(&cfg.max, "max")?;
        if min > max {
            return Err(anyhow!(
                "filters.yaml: seniority.min ({}) is above seniority.max ({}), \
                 which admits nothing at all",
                min.as_str(),
                max.as_str()
            ));
        }
        Ok(Self {
            min,
            max,
            include_unknown: cfg.include_unknown,
        })
    }

    pub fn admits(&self, row: &JobRow) -> bool {
        // Same exemption as startup mode, for the same reason: a job you
        // entered by hand was never discovered, so a discovery filter has
        // no business hiding it.
        if row.is_manual() {
            return true;
        }
        match row.seniority() {
            Some(level) => level >= self.min && level <= self.max,
            None => self.include_unknown,
        }
    }

    pub fn describe(&self) -> String {
        let mut s = if self.min == self.max {
            self.min.as_str().to_string()
        } else {
            format!("{}..{}", self.min.as_str(), self.max.as_str())
        };
        if self.include_unknown {
            s.push_str(", incl. unlabelled");
        }
        s
    }
}

// ------------------------------------------------------------------ helpers

/// An empty keyword list joins to "" and compiles to "()", which matches
/// the empty string at every position — `is_match` would then match ANY
/// title, silently turning an allowlist into "allow everything" and an
/// exclusion list into "exclude everything". Fail loudly instead.
fn compile_keyword_alternation(keywords: &[String], config_key: &str) -> Result<Regex> {
    if keywords.is_empty() {
        return Err(anyhow!(
            "filters.yaml's '{config_key}' is empty — this would silently match every \
             title instead of none. Add at least one keyword."
        ));
    }
    let joined = keywords
        .iter()
        .map(|k| regex::escape(k))
        .collect::<Vec<_>>()
        .join("|");
    Ok(Regex::new(&format!("(?i)({joined})"))?)
}

/// Like `compile_keyword_alternation` but word-boundary anchored, and None
/// for an empty list rather than an error — an absent hard-exclusion list
/// just means "don't hard-exclude anything", which is a safe default
/// (unlike an empty allowlist). Boundaries matter here: plain substring
/// matching on "intern" would also drop "Software Engineer, Internal
/// Tools".
fn compile_word_boundary_alternation(keywords: &[String]) -> Option<Regex> {
    if keywords.is_empty() {
        return None;
    }
    let joined = keywords
        .iter()
        .map(|k| regex::escape(k))
        .collect::<Vec<_>>()
        .join("|");
    Regex::new(&format!(r"(?i)\b({joined})\b")).ok()
}

/// Compile many patterns into one `RegexSet`, ASCII-mode for the same
/// 28x reason documented in `tech.rs`. An empty list is fine and yields a
/// set that matches nothing.
fn compile_set(patterns: &[String], config_key: &str) -> Result<regex::RegexSet> {
    let prefixed: Vec<String> = patterns.iter().map(|p| format!("(?i-u){p}")).collect();
    for (raw, p) in patterns.iter().zip(&prefixed) {
        Regex::new(p).map_err(|e| {
            anyhow!("filters.yaml: '{config_key}' pattern {raw:?} is not valid regex: {e}")
        })?;
    }
    regex::RegexSet::new(&prefixed)
        .map_err(|e| anyhow!("filters.yaml: compiling '{config_key}': {e}"))
}

fn compile_patterns(patterns: &[String], config_key: &str) -> Result<Vec<Regex>> {
    if patterns.is_empty() {
        return Err(anyhow!("filters.yaml's '{config_key}' is empty"));
    }
    patterns
        .iter()
        .map(|p| {
            Regex::new(&format!("(?i){p}")).map_err(|e| {
                anyhow!("filters.yaml: '{config_key}' pattern {p:?} is not valid regex: {e}")
            })
        })
        .collect()
}

/// Cheap HTML-to-text: good enough for keyword matching, not for display.
/// Entities are decoded because a dealbreaker keyword can otherwise hide
/// behind one (`C&#43;&#43;`), and tags become spaces rather than nothing
/// so `<b>Go</b>lang` doesn't silently become the word "Golang".
pub fn strip_html(html_or_text: &str) -> String {
    if html_or_text.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(html_or_text.len());
    let mut in_tag = false;
    for ch in html_or_text.chars() {
        match ch {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    html_escape::decode_html_entities(&out).into_owned()
}

/// Scraped page text turns every stripped block-level tag into its own
/// line, so a page with nav/sidebar chrome comes back as mostly blank
/// lines around a handful of real ones. Strip each line and collapse RUNS
/// of blank lines to one — not zero, since a single blank line is often a
/// real paragraph break worth keeping.
pub fn collapse_whitespace(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.is_empty() || out.last().is_some_and(|l: &&str| !l.is_empty()) {
            out.push(line);
        }
    }
    out.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SeniorityPolicyConfig, StartupPolicyConfig};
    use crate::model::Seniority;

    fn test_filters() -> Filters {
        let cfg = FiltersConfig {
            hard_exclusion_keywords: vec!["intern".into(), "manager".into(), "junior".into()],
            priority_title_keywords: vec!["software engineer".into(), "platform engineer".into()],
            title_allow_keywords: vec!["engineer".into(), "developer".into(), "sre".into()],
            exclusion_keywords: vec!["sales".into(), "hardware".into()],
            location_allow_patterns: vec![
                r"remote.*\b(us|usa|united states)\b".into(),
                r"\bohio\b".into(),
            ],
            stack_dealbreakers: vec![r"\bc#".into(), r"\.net\b".into()],
            stack_core: vec![r"\bgo\b".into(), r"\bkubernetes\b".into()],
            truncated_description_min_chars: 400,
            startup: StartupPolicyConfig::default(),
            seniority: SeniorityPolicyConfig::default(),
            staleness: Default::default(),
            agency_text_patterns: vec![r"\bon behalf of our client\b".into()],
            agency_company_patterns: vec![r"\bmotion recruitment\b".into()],
        };
        Filters::compile(&cfg).unwrap()
    }

    #[test]
    fn hard_exclusion_beats_priority_rescue() {
        let f = test_filters();
        // Contains the priority keyword "software engineer" as a
        // substring, but the level is wrong — the whole reason hard
        // exclusions are checked first.
        assert!(!f.title_is_relevant("Junior Software Engineer"));
        assert!(!f.title_is_relevant("Software Engineering Manager"));
        assert!(f.title_is_relevant("Senior Software Engineer"));
    }

    #[test]
    fn hard_exclusion_is_word_boundary_anchored() {
        let f = test_filters();
        // "Internal" contains "intern" — substring matching would drop it.
        assert!(f.title_is_relevant("Software Engineer, Internal Tools"));
    }

    #[test]
    fn priority_title_skips_the_exclusion_list() {
        let f = test_filters();
        assert!(f.title_is_relevant("Software Engineer, Sales Platform"));
        // No priority keyword, so the exclusion list applies.
        assert!(!f.title_is_relevant("Sales Engineer"));
        assert!(!f.title_is_relevant("Hardware Engineer"));
    }

    #[test]
    fn non_engineering_titles_fail_the_allowlist() {
        let f = test_filters();
        assert!(!f.title_is_relevant("Compliance Analyst"));
        assert!(!f.title_is_relevant("Talent Brand Partner"));
    }

    #[test]
    fn location_patterns_are_anchored_enough_to_exclude_other_countries() {
        let f = test_filters();
        assert!(f.location_is_allowed("Remote - US"));
        assert!(f.location_is_allowed("Columbus, Ohio"));
        assert!(!f.location_is_allowed("Remote Poland"));
        assert!(!f.location_is_allowed("Berlin, Germany"));
    }

    #[test]
    fn stack_mismatch_needs_a_dealbreaker_and_no_core() {
        let f = test_filters();
        assert!(f.jd_stack_mismatch("We are a C# and .NET shop."));
        // Dealbreaker present but so is the core stack — not a mismatch.
        assert!(!f.jd_stack_mismatch("C# services orchestrated with Kubernetes."));
        // Empty description is "unknown", never a rejection.
        assert!(!f.jd_stack_mismatch(""));
    }

    #[test]
    fn a_trailing_word_boundary_after_punctuation_never_matches() {
        // Regression guard for a real bug in filters.yaml: '#' and '+'
        // are not word characters, so a trailing \b demands a word
        // character right after them and the pattern matches nothing at
        // all. Both spellings lived in stack_dealbreakers until
        // 2026-09-25 and had silently never fired.
        let broken = Regex::new(r"(?i)\bc#\b").unwrap();
        assert!(!broken.is_match("Strong C# and .NET experience"));
        let fixed = Regex::new(r"(?i)\bc#").unwrap();
        assert!(fixed.is_match("Strong C# and .NET experience"));
        assert!(fixed.is_match("We use C#."));

        assert!(!Regex::new(r"(?i)\bc\+\+\b")
            .unwrap()
            .is_match("Modern C++ work"));
        assert!(Regex::new(r"(?i)\bc\+\+")
            .unwrap()
            .is_match("Modern C++ work"));
    }

    #[test]
    fn the_shipped_filters_yaml_compiles_and_its_patterns_can_match() {
        // Compiling is not enough — a regex that compiles fine can still
        // be incapable of matching anything (see the test above). Walk the
        // real file's dealbreakers and fail on any that can never fire,
        // which is the only automatic check that catches this class.
        let Ok(cfg) = crate::config::load_filters("filters.yaml") else {
            return; // not run from the repo root; nothing to check
        };
        for pattern in &cfg.stack_dealbreakers {
            let re = Regex::new(&format!("(?i){pattern}"))
                .unwrap_or_else(|e| panic!("filters.yaml pattern {pattern:?}: {e}"));
            // A pattern ending in \b whose preceding literal char is
            // non-word can never match at end-of-token.
            let trimmed = pattern.trim_end_matches("\\b");
            if trimmed.len() < pattern.len() {
                let last = trimmed.trim_end_matches('\\').chars().last();
                if let Some(c) = last {
                    assert!(
                        c.is_alphanumeric() || c == '_',
                        "filters.yaml: {pattern:?} ends with \\b after the non-word \
                         character {c:?}, so it can never match. Drop the trailing \\b."
                    );
                }
            }
            let _ = re;
        }
    }

    #[test]
    fn strip_html_decodes_entities_and_spaces_tags() {
        assert_eq!(strip_html("<p>Go &amp; Rust</p>").trim(), "Go & Rust");
        // Tags become spaces so adjacent words don't fuse.
        assert!(strip_html("<b>Go</b>lang").contains("Go lang"));
    }

    #[test]
    fn truncation_detection() {
        let f = test_filters();
        assert!(f.looks_truncated("Short teaser that cuts off…"));
        assert!(f.looks_truncated(""));
        assert!(!f.looks_truncated(&"x".repeat(500)));
    }

    fn row_with(stage: Option<Stage>, headcount: Option<i64>) -> JobRow {
        JobRow {
            stage,
            headcount,
            ..Default::default()
        }
    }

    #[test]
    fn manual_entries_are_never_filtered_out_by_startup_mode() {
        let p = StartupPolicy {
            enabled_by_default: true,
            max_stage: Stage::Growth,
            max_headcount: Some(500),
            include_unknown: false,
        };
        // Big, public, huge headcount, no stage — every reason to hide it,
        // except that the user entered it by hand.
        let manual = JobRow {
            source: crate::model::MANUAL_SOURCE.to_string(),
            stage: Some(Stage::BigTech),
            headcount: Some(90_000),
            ..Default::default()
        };
        assert!(p.admits(&manual));

        // The same row from a fetcher is filtered as usual.
        let scraped = JobRow {
            source: "greenhouse".to_string(),
            ..manual.clone()
        };
        assert!(!p.admits(&scraped));
    }

    fn level_row(title: &str) -> JobRow {
        JobRow {
            title: title.into(),
            source: "greenhouse".into(),
            ..Default::default()
        }
    }

    fn level_policy(min: Seniority, max: Seniority, include_unknown: bool) -> SeniorityPolicy {
        SeniorityPolicy {
            min,
            max,
            include_unknown,
        }
    }

    #[test]
    fn the_level_range_is_inclusive_at_both_ends() {
        let p = level_policy(Seniority::Junior, Seniority::Senior, false);
        assert!(p.admits(&level_row("Junior Platform Engineer")));
        assert!(p.admits(&level_row("Software Engineer II")));
        assert!(p.admits(&level_row("Senior Platform Engineer")));
        assert!(!p.admits(&level_row("Engineering Intern")), "below min");
        assert!(
            !p.admits(&level_row("Staff Software Engineer")),
            "above max"
        );
    }

    #[test]
    fn unlabelled_titles_are_governed_by_include_unknown() {
        let row = level_row("Software Engineer");
        assert!(level_policy(Seniority::Junior, Seniority::Staff, true).admits(&row));
        assert!(!level_policy(Seniority::Junior, Seniority::Staff, false).admits(&row));
    }

    #[test]
    fn manual_entries_ignore_the_level_policy_too() {
        // Same reasoning as startup mode: you entered it, so a discovery
        // filter has no business hiding it.
        let mut row = level_row("Engineering Manager");
        row.source = crate::model::MANUAL_SOURCE.into();
        assert!(level_policy(Seniority::Junior, Seniority::Senior, false).admits(&row));
    }

    #[test]
    fn an_inverted_range_is_rejected_rather_than_silently_empty() {
        let cfg = SeniorityPolicyConfig {
            min: "staff".into(),
            max: "junior".into(),
            include_unknown: true,
        };
        let err = SeniorityPolicy::from_config(&cfg).unwrap_err().to_string();
        assert!(err.contains("admits nothing at all"), "got: {err}");
    }

    #[test]
    fn an_unknown_level_name_names_the_valid_ones() {
        let cfg = SeniorityPolicyConfig {
            min: "midlevelish".into(),
            max: "staff".into(),
            include_unknown: true,
        };
        let err = SeniorityPolicy::from_config(&cfg).unwrap_err().to_string();
        assert!(
            err.contains("junior"),
            "error should list the options: {err}"
        );
    }

    #[test]
    fn the_shipped_filters_yaml_still_admits_junior() {
        let Ok(cfg) = crate::config::load_filters("filters.yaml") else {
            return; // not run from the repo root
        };
        let policy = SeniorityPolicy::from_config(&cfg.seniority).unwrap();
        assert!(policy.admits(&level_row("Junior Platform Engineer")));
        // And the level vetoes must have left hard_exclusion_keywords —
        // leaving them there would drop junior roles at fetch time, where
        // no view policy can ever get them back.
        let joined = cfg.hard_exclusion_keywords.join(" ").to_lowercase();
        for veto in ["junior", "new grad", "entry level"] {
            assert!(
                !joined.contains(veto),
                "{veto:?} is still a fetch-time veto, so the level policy can never see it"
            );
        }
    }

    #[test]
    fn startup_policy_cutoff() {
        let p = StartupPolicy {
            enabled_by_default: true,
            max_stage: Stage::Growth,
            max_headcount: Some(500),
            include_unknown: false,
        };
        assert!(p.admits(&row_with(Some(Stage::Seed), Some(12))));
        assert!(p.admits(&row_with(Some(Stage::Growth), None)));
        assert!(!p.admits(&row_with(Some(Stage::Public), None)));
        // Headcount vetoes independently of the stage label.
        assert!(!p.admits(&row_with(Some(Stage::SeriesC), Some(4000))));
        // Unknown stage excluded by default...
        assert!(!p.admits(&row_with(None, None)));
        // ...unless explicitly included.
        let lenient = StartupPolicy {
            include_unknown: true,
            ..p
        };
        assert!(lenient.admits(&row_with(None, None)));
    }

    /// The threshold is a floor, not a fence: a row exactly at it is
    /// stale, because "14 days and nothing" is the day you follow up.
    #[test]
    fn staleness_counts_from_the_last_transition_and_only_for_configured_statuses() {
        let mut f = test_filters();
        f.staleness = [("recruiter".to_string(), 14), ("applied".to_string(), 28)]
            .into_iter()
            .collect();

        let row = |status: &str, days_ago: i64| {
            let at = chrono::Utc::now() - chrono::Duration::days(days_ago);
            JobRow {
                my_status: Some(status.to_string()),
                status_since: Some(at.format("%Y-%m-%d %H:%M:%S").to_string()),
                ..Default::default()
            }
        };

        assert_eq!(f.staleness_days(&row("recruiter", 13)), None);
        assert_eq!(f.staleness_days(&row("recruiter", 14)), Some(0));
        assert_eq!(f.staleness_days(&row("recruiter", 20)), Some(6));
        assert_eq!(f.staleness_days(&row("applied", 20)), None);

        // Terminal statuses have no threshold, so they never go stale: a
        // rejection isn't waiting on anything.
        assert_eq!(f.staleness_days(&row("rejected", 400)), None);
        assert_eq!(f.staleness_days(&row("silence", 400)), None);

        // No timestamp at all reads as "age unknown", never as stale.
        let untimed = JobRow {
            my_status: Some("recruiter".into()),
            ..Default::default()
        };
        assert!(!f.is_stale(&untimed));
    }
}
