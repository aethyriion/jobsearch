//! Deterministic tech-stack detection from a job description.
//!
//! No model, no cost, no network — `tech.yaml`'s patterns run over stored
//! JD text. Enough to answer which postings are AWS and which are GCP, and
//! who really uses Rust, without spending a token per posting.
//!
//! Derived, never stored: it's a pure function of text already in the
//! database, so improving a pattern takes effect on the next read instead
//! of needing a migration and a re-scan.
//!
//! Two decisions make that affordable. All ~160 patterns compile into one
//! `RegexSet`, answering "which of these match" in a single pass. And each
//! is compiled `(?-u)`: the crate's Unicode `\b` and case folding cost
//! ~28x here for byte-identical results, since every pattern is ASCII.

use anyhow::{Context, Result};
use regex::RegexSet;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct TechFile {
    categories: Vec<CategoryConfig>,
}

#[derive(Debug, Deserialize)]
struct CategoryConfig {
    name: String,
    tags: Vec<TagConfig>,
}

#[derive(Debug, Deserialize)]
struct TagConfig {
    name: String,
    /// Match with case sensitivity. The flag exists for tags whose name is
    /// also an ordinary English word: `\bgo\b` matches "go to market",
    /// while `\bGo\b` matches the language and little else.
    #[serde(default)]
    case_sensitive: bool,
    patterns: Vec<String>,
    /// Phrases that veto the tag for the whole posting.
    ///
    /// Blunt by design — one match suppresses the tag even if a genuine
    /// mention also appears. That's the right trade for a phrase which is
    /// strong evidence of a false positive and weak evidence of anything
    /// else: "Spark Capital" is a VC in one company's boilerplate, and
    /// counting it as Apache Spark was wrong on every posting they have.
    #[serde(default)]
    exclude: Vec<String>,
}

/// One detectable technology.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tag {
    pub category: String,
    pub name: String,
}

impl Tag {
    /// `cloud:aws` — the form the CLI accepts and the picker displays.
    pub fn qualified(&self) -> String {
        format!("{}:{}", self.category, self.name)
    }
}

/// Compiled detector: every pattern in one automaton, plus the mapping
/// from pattern index back to the tag that owns it.
pub struct TechDetector {
    /// Patterns that ignore case (the great majority).
    insensitive: RegexSet,
    insensitive_owner: Vec<usize>,
    /// Patterns that don't (Go, R, and friends).
    sensitive: RegexSet,
    sensitive_owner: Vec<usize>,
    /// Veto patterns, same index mapping.
    excludes: RegexSet,
    exclude_owner: Vec<usize>,
    tags: Vec<Tag>,
}

impl TechDetector {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let parsed: TechFile =
            serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Self::compile(&parsed)
    }

    /// An empty detector, for when `tech.yaml` is absent. Tech detection is
    /// an enhancement, not a dependency — a missing config file leaves
    /// every posting untagged rather than stopping the run.
    pub fn empty() -> Self {
        Self {
            insensitive: RegexSet::empty(),
            insensitive_owner: Vec::new(),
            sensitive: RegexSet::empty(),
            sensitive_owner: Vec::new(),
            excludes: RegexSet::empty(),
            exclude_owner: Vec::new(),
            tags: Vec::new(),
        }
    }

    fn compile(file: &TechFile) -> Result<Self> {
        let mut tags = Vec::new();
        let (mut ins, mut ins_owner) = (Vec::new(), Vec::new());
        let (mut sen, mut sen_owner) = (Vec::new(), Vec::new());
        let (mut exc, mut exc_owner) = (Vec::new(), Vec::new());

        for category in &file.categories {
            for tag in &category.tags {
                let index = tags.len();
                tags.push(Tag {
                    category: category.name.clone(),
                    name: tag.name.clone(),
                });
                for pattern in &tag.patterns {
                    // Validate each pattern individually so the error names
                    // the offending one; RegexSet's own error would only
                    // say that the set failed to build.
                    regex::Regex::new(pattern).with_context(|| {
                        format!(
                            "tech.yaml: {}:{} has an invalid pattern {pattern:?}",
                            category.name, tag.name
                        )
                    })?;
                    if tag.case_sensitive {
                        sen.push(format!("(?-u){pattern}"));
                        sen_owner.push(index);
                    } else {
                        ins.push(format!("(?i-u){pattern}"));
                        ins_owner.push(index);
                    }
                }
                for pattern in &tag.exclude {
                    regex::Regex::new(pattern).with_context(|| {
                        format!(
                            "tech.yaml: {}:{} has an invalid exclude pattern {pattern:?}",
                            category.name, tag.name
                        )
                    })?;
                    exc.push(format!("(?i-u){pattern}"));
                    exc_owner.push(index);
                }
            }
        }

        Ok(Self {
            insensitive: RegexSet::new(&ins).context("compiling tech.yaml patterns")?,
            insensitive_owner: ins_owner,
            sensitive: RegexSet::new(&sen).context("compiling tech.yaml patterns")?,
            sensitive_owner: sen_owner,
            excludes: RegexSet::new(&exc).context("compiling tech.yaml exclude patterns")?,
            exclude_owner: exc_owner,
            tags,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    pub fn tag_count(&self) -> usize {
        self.tags.len()
    }

    /// Every tag the text matches, in config order (so categories stay
    /// grouped) and deduplicated.
    ///
    /// The caller passes the ALREADY case-folded-or-not original text: the
    /// case-sensitive patterns need the real casing, and the insensitive
    /// ones carry `(?i)` themselves, so one string serves both.
    pub fn detect(&self, text: &str) -> Vec<Tag> {
        if self.tags.is_empty() || text.is_empty() {
            return Vec::new();
        }
        let mut hit = vec![false; self.tags.len()];
        for i in self.insensitive.matches(text) {
            hit[self.insensitive_owner[i]] = true;
        }
        for i in self.sensitive.matches(text) {
            hit[self.sensitive_owner[i]] = true;
        }
        // Vetoes last, so an exclude always wins over a positive match.
        for i in self.excludes.matches(text) {
            hit[self.exclude_owner[i]] = false;
        }
        self.tags
            .iter()
            .enumerate()
            .filter(|(i, _)| hit[*i])
            .map(|(_, t)| t.clone())
            .collect()
    }

    /// Resolve a user-typed tag name. Accepts either `aws` or `cloud:aws`,
    /// so the CLI doesn't force you to remember which category something
    /// is in.
    pub fn resolve(&self, needle: &str) -> Option<&Tag> {
        let needle = needle.trim().to_lowercase();
        self.tags
            .iter()
            .find(|t| t.qualified() == needle || t.name == needle)
    }

    pub fn all_tags(&self) -> &[Tag] {
        &self.tags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detector() -> TechDetector {
        TechDetector::load("tech.yaml").expect("tech.yaml should load from the repo root")
    }

    fn names(tags: &[Tag]) -> Vec<String> {
        tags.iter().map(|t| t.name.clone()).collect()
    }

    #[test]
    fn detects_the_obvious_cases() {
        let d = detector();
        let tags = names(&d.detect(
            "We run Kubernetes on AWS, infrastructure as code with Terraform, \
             services written in Rust and Python.",
        ));
        for expected in ["rust", "python", "aws", "kubernetes", "terraform"] {
            assert!(
                tags.contains(&expected.to_string()),
                "missing {expected} in {tags:?}"
            );
        }
    }

    #[test]
    fn separates_aws_from_gcp() {
        let d = detector();
        let aws = names(&d.detect("Our stack is EKS, S3 and DynamoDB."));
        assert!(aws.contains(&"aws".into()));
        assert!(!aws.contains(&"gcp".into()));

        let gcp = names(&d.detect("We use GKE and BigQuery on Google Cloud."));
        assert!(gcp.contains(&"gcp".into()));
        assert!(!gcp.contains(&"aws".into()));
    }

    #[test]
    fn the_word_go_does_not_become_the_language_go() {
        let d = detector();
        // The single most likely false positive in the whole file: "go" is
        // an extremely common English verb.
        let prose = d.detect("You will go to market fast and help us go from zero to one.");
        assert!(
            !names(&prose).contains(&"go".to_string()),
            "lowercase 'go' is a verb, not a language: {:?}",
            names(&prose)
        );
        // Capitalized mid-sentence, it's the language.
        assert!(names(&d.detect("Backend services in Go and Rust.")).contains(&"go".to_string()));
        assert!(names(&d.detect("Strong golang experience.")).contains(&"go".to_string()));
    }

    #[test]
    fn java_does_not_match_javascript() {
        let d = detector();
        let tags = names(&d.detect("Frontend work in JavaScript and TypeScript."));
        assert!(!tags.contains(&"java".to_string()), "got {tags:?}");
        assert!(tags.contains(&"javascript".to_string()));
    }

    #[test]
    fn punctuation_languages_match_despite_the_word_boundary_trap() {
        // '+' and '#' are not word characters, so a TRAILING \b can never
        // match after them — the same bug that made filters.yaml's C#/C++
        // dealbreakers silently inert. These patterns must not repeat it.
        let d = detector();
        assert!(names(&d.detect("Systems programming in C++.")).contains(&"cpp".to_string()));
        assert!(names(&d.detect("A C# and .NET shop.")).contains(&"c-sharp".to_string()));
    }

    #[test]
    fn exclude_patterns_veto_a_false_positive() {
        let d = detector();
        // "Spark Capital" is a VC in one company's boilerplate; it appeared
        // on every one of their postings and counted as Apache Spark.
        let tags = names(&d.detect(
            "Backed by Sequoia Capital and Spark Capital. We build container security tools.",
        ));
        assert!(!tags.contains(&"spark".to_string()), "got {tags:?}");
        // A real mention still lands.
        assert!(
            names(&d.detect("Deep experience with Apache Spark internals."))
                .contains(&"spark".to_string())
        );
    }

    #[test]
    fn ordinary_english_words_are_not_read_as_technologies() {
        let d = detector();
        // Each of these was a real false positive in the live database.
        let workers = names(&d.detect(
            "For unincorporated Los Angeles County workers: we ensure workers return home safely.",
        ));
        assert!(
            !workers.contains(&"cloudflare".to_string()),
            "got {workers:?}"
        );

        let clearance = names(&d.detect("Candidates must hold an active TS/SCI clearance."));
        assert!(
            !clearance.contains(&"typescript".to_string()),
            "got {clearance:?}"
        );

        // And the real things still register.
        assert!(names(&d.detect("We run Cloudflare Workers at the edge."))
            .contains(&"cloudflare".to_string()));
        assert!(
            names(&d.detect("Strong TypeScript experience.")).contains(&"typescript".to_string())
        );
    }

    #[test]
    fn an_empty_or_untagged_description_yields_nothing() {
        let d = detector();
        assert!(d.detect("").is_empty());
        assert!(d
            .detect("We are looking for a thoughtful, curious person.")
            .is_empty());
    }

    #[test]
    fn tags_resolve_by_bare_or_qualified_name() {
        let d = detector();
        assert_eq!(
            d.resolve("aws").map(Tag::qualified),
            Some("cloud:aws".into())
        );
        assert_eq!(
            d.resolve("cloud:aws").map(Tag::qualified),
            Some("cloud:aws".into())
        );
        assert_eq!(
            d.resolve("AWS").map(Tag::qualified),
            Some("cloud:aws".into())
        );
        assert!(d.resolve("cobol").is_none());
    }

    #[test]
    fn a_missing_config_is_not_an_error() {
        let d = TechDetector::empty();
        assert!(d.is_empty());
        assert!(d.detect("Rust and AWS everywhere").is_empty());
    }

    #[test]
    fn every_shipped_pattern_compiles_and_has_an_owner() {
        let d = detector();
        assert!(d.tag_count() > 30, "expected a substantial tag set");
        assert_eq!(d.insensitive.len(), d.insensitive_owner.len());
        assert_eq!(d.sensitive.len(), d.sensitive_owner.len());
    }
}
