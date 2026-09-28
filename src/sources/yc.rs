//! Y Combinator company directory — the startup-native *company* source.
//!
//! This one doesn't return jobs. It returns companies, with the two facts
//! startup mode actually needs: a stage and a headcount, for ~1,500
//! companies that are hiring right now and are startups by construction.
//!
//! That shape is deliberate. YC's own job board (workatastartup.com)
//! requires an authenticated session, but the great majority of YC
//! companies host their real board on Greenhouse, Ashby or Lever — which
//! this binary already knows how to read, and which give the *whole*
//! board rather than whatever a search surfaced. So the useful move is to
//! resolve a YC company to its ATS once, write it into companies.yaml, and
//! let the normal company path fetch it forever after. Stage and headcount
//! go into the `company_stage` table at the same time.
//!
//! Data source: `yc-oss.github.io/api`, a daily-rebuilt static JSON mirror
//! of YC's public company directory (no key, no rate limit, no scraping).
//! If it ever goes away, `COMPANIES_URL` is the only thing to repoint.

use anyhow::Result;
use futures::stream::{self, StreamExt};
use serde::Deserialize;

use super::http;
use crate::model::{CompanyStage, Stage, StageSource};

const COMPANIES_URL: &str = "https://yc-oss.github.io/api/companies/hiring.json";

#[derive(Debug, Deserialize)]
pub struct YcCompany {
    #[serde(default, deserialize_with = "http::null_to_default")]
    pub name: String,
    #[serde(default, deserialize_with = "http::null_to_default")]
    pub slug: String,
    #[serde(default, deserialize_with = "http::null_to_default")]
    pub website: String,
    #[serde(default)]
    pub team_size: Option<i64>,
    #[serde(default, deserialize_with = "http::null_to_default")]
    pub batch: String,
    #[serde(default, deserialize_with = "http::null_to_default")]
    pub stage: String,
    #[serde(default, deserialize_with = "http::null_to_default")]
    pub status: String,
    #[serde(default, deserialize_with = "http::null_to_default")]
    pub regions: Vec<String>,
    #[serde(default, deserialize_with = "http::null_to_default")]
    pub one_liner: String,
    #[serde(default, rename = "isHiring")]
    pub is_hiring: bool,
}

impl YcCompany {
    /// Map YC's coarse label plus team size onto this crate's stage axis.
    ///
    /// YC only publishes "Early" / "Growth", which is too blunt to filter
    /// on — nearly everything is "Early". Team size is the sharper signal
    /// and is what actually separates a 4-person seed company from a
    /// 200-person Series B, so it refines the label rather than the label
    /// standing alone. `status: "Public"` overrides both: a handful of YC
    /// companies have IPO'd and are emphatically not startups.
    pub fn to_stage(&self) -> Stage {
        if self.status.eq_ignore_ascii_case("public") {
            return Stage::Public;
        }
        let size = self.team_size.unwrap_or(0);
        if self.stage.eq_ignore_ascii_case("growth") {
            return if size > 500 {
                Stage::Public
            } else {
                Stage::Growth
            };
        }
        match size {
            0..=10 => Stage::Seed,
            11..=35 => Stage::SeriesA,
            36..=120 => Stage::SeriesB,
            121..=400 => Stage::SeriesC,
            _ => Stage::Growth,
        }
    }

    pub fn stage_claim(&self) -> CompanyStage {
        CompanyStage {
            company: self.name.clone(),
            stage: self.to_stage(),
            headcount: self.team_size,
            source: StageSource::Source,
            note: Some(format!(
                "YC {} · {} · {}",
                self.batch,
                self.stage,
                if self.one_liner.is_empty() {
                    "—"
                } else {
                    &self.one_liner
                }
            )),
        }
    }

    /// Whether this company is worth probing for a US-reachable board.
    /// The directory is global; fetching a Bangalore-only company's board
    /// just to have every posting rejected on location later is wasted
    /// requests.
    pub fn is_us_reachable(&self) -> bool {
        const US_REGIONS: [&str; 5] = [
            "United States of America",
            "America / Canada",
            "Remote",
            "Fully Remote",
            "Partly Remote",
        ];
        self.regions
            .iter()
            .any(|r| US_REGIONS.iter().any(|u| r.eq_ignore_ascii_case(u)))
    }
}

pub async fn fetch_directory() -> Result<Vec<YcCompany>> {
    let companies: Vec<YcCompany> = http::get_json(COMPANIES_URL).await?;
    Ok(companies.into_iter().filter(|c| c.is_hiring).collect())
}

/// An ATS board found for a YC company.
#[derive(Debug, Clone)]
pub struct ResolvedBoard {
    pub ats: String,
    pub slug: String,
    pub open_roles: usize,
}

/// Candidate slugs to try for a company, best guess first.
///
/// YC's own slug is right surprisingly often (both it and the ATS slug
/// usually derive from the company name), but not always — so also try
/// the company name squashed, and the website's domain label, which is
/// what a company that renamed itself tends to have kept.
fn candidate_slugs(c: &YcCompany) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |s: String| {
        if !s.is_empty() && s.len() >= 2 && !out.contains(&s) {
            out.push(s);
        }
    };
    push(c.slug.clone());
    push(
        c.name
            .to_lowercase()
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .collect(),
    );
    if let Some(domain) = c
        .website
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.")
        .split('/')
        .next()
    {
        if let Some(label) = domain.split('.').next() {
            push(label.to_lowercase());
        }
    }
    out
}

/// Probe a company's candidate slugs against each ATS until one answers
/// with a real board.
///
/// "Real" means at least one open posting. A board that exists but is
/// empty is indistinguishable from a wrong slug for our purposes and
/// would just add a dead entry to companies.yaml that every future run
/// re-fetches for nothing.
pub async fn resolve_board(c: &YcCompany) -> Option<ResolvedBoard> {
    for slug in candidate_slugs(c) {
        // Ordered by how common each ATS is among YC companies, so the
        // usual case costs one request rather than three.
        for ats in ["ashby", "greenhouse", "lever"] {
            let jobs = match ats {
                "ashby" => super::ats::fetch_ashby(&c.name, &slug).await,
                "greenhouse" => super::ats::fetch_greenhouse(&c.name, &slug).await,
                "lever" => super::ats::fetch_lever(&c.name, &slug).await,
                _ => unreachable!(),
            };
            if let Ok(jobs) = jobs {
                if !jobs.is_empty() {
                    return Some(ResolvedBoard {
                        ats: ats.to_string(),
                        slug,
                        open_roles: jobs.len(),
                    });
                }
            }
        }
    }
    None
}

/// Resolve many companies concurrently, reporting progress as it goes.
///
/// Concurrency is capped rather than unbounded: this fires several
/// requests per company across three third-party APIs, and the polite
/// thing — as well as the thing that doesn't get the IP throttled
/// mid-run — is a steady trickle rather than 1,500 simultaneous
/// connections.
pub async fn resolve_boards<F>(
    companies: &[YcCompany],
    concurrency: usize,
    mut on_progress: F,
) -> Vec<(String, Option<ResolvedBoard>)>
where
    F: FnMut(usize, usize, &str),
{
    let total = companies.len();
    let mut done = 0usize;
    let mut results = Vec::with_capacity(total);

    let mut stream = stream::iter(companies.iter())
        .map(|c| async move { (c.name.clone(), resolve_board(c).await) })
        .buffer_unordered(concurrency.clamp(1, 32));

    while let Some((name, resolved)) = stream.next().await {
        done += 1;
        on_progress(done, total, &name);
        results.push((name, resolved));
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn company(stage: &str, size: i64, status: &str) -> YcCompany {
        YcCompany {
            name: "Acme".into(),
            slug: "acme".into(),
            website: "https://acme.dev".into(),
            team_size: Some(size),
            batch: "Winter 2024".into(),
            stage: stage.into(),
            status: status.into(),
            regions: vec!["United States of America".into()],
            one_liner: "Thing for things".into(),
            is_hiring: true,
        }
    }

    #[test]
    fn team_size_refines_ycs_coarse_stage_label() {
        assert_eq!(company("Early", 4, "Active").to_stage(), Stage::Seed);
        assert_eq!(company("Early", 20, "Active").to_stage(), Stage::SeriesA);
        assert_eq!(company("Early", 80, "Active").to_stage(), Stage::SeriesB);
        assert_eq!(company("Early", 200, "Active").to_stage(), Stage::SeriesC);
        assert_eq!(company("Growth", 300, "Active").to_stage(), Stage::Growth);
    }

    #[test]
    fn a_public_yc_company_is_not_a_startup() {
        // Status wins over the Early/Growth label — a handful of YC
        // companies have IPO'd and must not survive startup mode.
        assert_eq!(company("Growth", 8000, "Public").to_stage(), Stage::Public);
        assert_eq!(company("Early", 5, "Public").to_stage(), Stage::Public);
    }

    #[test]
    fn very_large_growth_companies_fall_out_of_startup_range() {
        assert_eq!(company("Growth", 2000, "Active").to_stage(), Stage::Public);
    }

    #[test]
    fn candidate_slugs_cover_name_and_domain_variants() {
        let mut c = company("Early", 10, "Active");
        c.slug = "acme-labs".into();
        c.name = "Acme Labs".into();
        c.website = "https://getacme.com".into();
        let slugs = candidate_slugs(&c);
        assert!(slugs.contains(&"acme-labs".to_string()));
        assert!(slugs.contains(&"acmelabs".to_string()));
        assert!(slugs.contains(&"getacme".to_string()));
    }

    #[test]
    fn region_gate_keeps_us_reachable_companies_only() {
        let mut c = company("Early", 10, "Active");
        assert!(c.is_us_reachable());
        c.regions = vec!["India".into(), "South Asia".into()];
        assert!(!c.is_us_reachable());
        c.regions = vec!["India".into(), "Remote".into()];
        assert!(c.is_us_reachable());
    }

    #[test]
    fn stage_claim_carries_source_provenance() {
        let claim = company("Early", 10, "Active").stage_claim();
        assert_eq!(claim.source, StageSource::Source);
        assert_eq!(claim.headcount, Some(10));
        assert!(claim.note.unwrap().contains("Winter 2024"));
    }
}
