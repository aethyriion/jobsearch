//! Thin clients for each ATS's public job-board API — one board per
//! company, precise and low-noise, as opposed to the keyword search in
//! `aggregators.rs`.
//!
//! Every fetcher returns `Vec<Job>`. `description` is whatever the ATS
//! gives us (HTML or plain text) and is fetched from the SAME request as
//! the listing wherever the API supports that (Greenhouse `?content=true`,
//! Ashby and Lever include it by default), so this doesn't multiply call
//! volume. Workable's widget API may or may not include it depending on
//! the account; when it's absent the description is empty and the stack
//! check is skipped for that job, with title and location filters still
//! applying.
//!
//! SmartRecruiters is the exception: its list endpoint omits the JD, so
//! the description needs one extra request per posting. That request is
//! gated on the posting already passing title and location, for the same
//! reason the Adzuna full-JD fetch is — otherwise a company with hundreds
//! of postings becomes hundreds of extra requests for roles that would be
//! dropped on title alone anyway.

use anyhow::Result;
use serde::Deserialize;

use super::http;
use crate::config::Company;
use crate::filters::Filters;
use crate::model::{Job, StageHint};

/// Every ATS this binary can talk to. Public so `config check` and the
/// error message for an unknown `ats:` value can list them without
/// duplicating the list.
pub const SUPPORTED: [&str; 5] = [
    "greenhouse",
    "ashby",
    "workable",
    "lever",
    "smartrecruiters",
];

pub async fn fetch_company(company: &Company, filters: &Filters) -> Result<Vec<Job>> {
    let name = &company.name;
    let slug = &company.slug;
    let mut jobs = match company.ats.as_str() {
        "greenhouse" => fetch_greenhouse(name, slug).await?,
        "ashby" => fetch_ashby(name, slug).await?,
        "workable" => fetch_workable(name, slug).await?,
        "lever" => fetch_lever(name, slug).await?,
        "smartrecruiters" => fetch_smartrecruiters(name, slug, filters).await?,
        other => anyhow::bail!(
            "no fetcher for ATS type {other:?} (supported: {})",
            SUPPORTED.join(", ")
        ),
    };
    // A curated stage in companies.yaml rides along with every job from
    // that company, so startup mode works on the first run without the AI
    // step having been paid for yet.
    let hint = company.parsed_stage().map(|stage| StageHint {
        stage,
        headcount: company.headcount,
        source: crate::model::StageSource::Curated,
    });
    for job in &mut jobs {
        job.source = company.ats.clone();
        job.stage_hint = hint.clone();
    }
    Ok(jobs)
}

// ------------------------------------------------------------- greenhouse

#[derive(Deserialize)]
struct GhResponse {
    #[serde(default)]
    jobs: Vec<GhJob>,
}

#[derive(Deserialize)]
struct GhJob {
    #[serde(default, deserialize_with = "http::null_to_default")]
    title: String,
    #[serde(default)]
    location: Option<GhLocation>,
    #[serde(default, deserialize_with = "http::null_to_default")]
    absolute_url: String,
    #[serde(default)]
    first_published: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct GhLocation {
    #[serde(default, deserialize_with = "http::null_to_default")]
    name: String,
}

pub async fn fetch_greenhouse(company: &str, slug: &str) -> Result<Vec<Job>> {
    let url = format!("https://boards-api.greenhouse.io/v1/boards/{slug}/jobs");
    let data: GhResponse = http::get_json_with(&url, &[("content".into(), "true".into())]).await?;
    Ok(greenhouse_jobs(company, data))
}

/// Split from the fetch so the shape each ATS returns can be tested
/// without a network call. Every one of these providers has its own idea
/// of where the location, the URL and the date live, and those mappings
/// are the part that silently breaks when a provider changes.
fn greenhouse_jobs(company: &str, data: GhResponse) -> Vec<Job> {
    data.jobs
        .into_iter()
        .map(|j| Job {
            company: company.to_string(),
            title: j.title,
            location: j.location.map(|l| l.name).unwrap_or_default(),
            url: j.absolute_url,
            posted_at: j.first_published.or(j.updated_at),
            description: j.content.unwrap_or_default(),
            ..Default::default()
        })
        .collect()
}

// ------------------------------------------------------------------ ashby

#[derive(Deserialize)]
struct AshbyResponse {
    #[serde(default)]
    jobs: Vec<AshbyJob>,
}

#[derive(Deserialize)]
struct AshbyJob {
    #[serde(default, deserialize_with = "http::null_to_default")]
    title: String,
    #[serde(default)]
    location: Option<String>,
    #[serde(default, rename = "locationName")]
    location_name: Option<String>,
    #[serde(default, rename = "jobUrl")]
    job_url: Option<String>,
    #[serde(default, rename = "applyUrl")]
    apply_url: Option<String>,
    #[serde(default, rename = "publishedAt")]
    published_at: Option<String>,
    #[serde(default, rename = "descriptionPlain")]
    description_plain: Option<String>,
    #[serde(default, rename = "descriptionHtml")]
    description_html: Option<String>,
}

pub async fn fetch_ashby(company: &str, slug: &str) -> Result<Vec<Job>> {
    let url = format!("https://api.ashbyhq.com/posting-api/job-board/{slug}");
    let data: AshbyResponse = http::get_json(&url).await?;
    Ok(ashby_jobs(company, data))
}

fn ashby_jobs(company: &str, data: AshbyResponse) -> Vec<Job> {
    data.jobs
        .into_iter()
        .map(|j| Job {
            company: company.to_string(),
            title: j.title,
            location: j.location.or(j.location_name).unwrap_or_default(),
            url: j.job_url.or(j.apply_url).unwrap_or_default(),
            posted_at: j.published_at,
            description: j
                .description_plain
                .or(j.description_html)
                .unwrap_or_default(),
            ..Default::default()
        })
        .collect()
}

// --------------------------------------------------------------- workable

#[derive(Deserialize)]
struct WorkableResponse {
    #[serde(default)]
    jobs: Vec<WorkableJob>,
}

#[derive(Deserialize)]
struct WorkableJob {
    #[serde(default, deserialize_with = "http::null_to_default")]
    title: String,
    #[serde(default)]
    location: Option<WorkableLocation>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    shortlink: Option<String>,
    #[serde(default)]
    published_on: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct WorkableLocation {
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    workplace: Option<String>,
}

pub async fn fetch_workable(company: &str, slug: &str) -> Result<Vec<Job>> {
    let url = format!("https://apply.workable.com/api/v1/widget/accounts/{slug}?details=true");
    let data: WorkableResponse = http::get_json(&url).await?;
    Ok(workable_jobs(company, data))
}

fn workable_jobs(company: &str, data: WorkableResponse) -> Vec<Job> {
    data.jobs
        .into_iter()
        .map(|j| {
            let loc = j.location.unwrap_or(WorkableLocation {
                city: None,
                region: None,
                country: None,
                workplace: None,
            });
            let mut loc_str = [loc.city, loc.region, loc.country]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            if loc.workplace.as_deref() == Some("remote") {
                loc_str = if loc_str.is_empty() {
                    "Remote".to_string()
                } else {
                    format!("Remote ({loc_str})")
                };
            }
            Job {
                company: company.to_string(),
                title: j.title,
                location: loc_str,
                url: j.url.or(j.shortlink).unwrap_or_default(),
                posted_at: j.published_on.or(j.created_at),
                description: j.description.unwrap_or_default(),
                ..Default::default()
            }
        })
        .collect()
}

// ------------------------------------------------------------------ lever

#[derive(Deserialize)]
struct LeverJob {
    #[serde(default, deserialize_with = "http::null_to_default")]
    text: String,
    #[serde(default)]
    categories: Option<LeverCategories>,
    #[serde(
        default,
        rename = "hostedUrl",
        deserialize_with = "http::null_to_default"
    )]
    hosted_url: String,
    #[serde(default, rename = "createdAt")]
    created_at: Option<i64>,
    #[serde(default, rename = "descriptionPlain")]
    description_plain: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct LeverCategories {
    #[serde(default)]
    location: Option<String>,
    #[serde(default, rename = "allLocations")]
    all_locations: Option<Vec<String>>,
}

pub async fn fetch_lever(company: &str, slug: &str) -> Result<Vec<Job>> {
    let url = format!("https://api.lever.co/v0/postings/{slug}?mode=json");
    let data: Vec<LeverJob> = http::get_json(&url).await?;
    Ok(lever_jobs(company, data))
}

fn lever_jobs(company: &str, data: Vec<LeverJob>) -> Vec<Job> {
    data.into_iter()
        .map(|j| {
            let cats = j.categories.unwrap_or(LeverCategories {
                location: None,
                all_locations: None,
            });
            let location = match cats.all_locations {
                Some(all) if !all.is_empty() => all.join(", "),
                _ => cats.location.unwrap_or_default(),
            };
            Job {
                company: company.to_string(),
                title: j.text,
                location,
                url: j.hosted_url,
                // Lever gives epoch milliseconds; everything downstream
                // treats posted_at as an opaque display string, so render
                // it as an ISO date here rather than leaving a bare
                // 13-digit number in the UI.
                posted_at: j.created_at.and_then(epoch_millis_to_iso),
                description: j.description_plain.or(j.description).unwrap_or_default(),
                ..Default::default()
            }
        })
        .collect()
}

fn epoch_millis_to_iso(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms).map(|dt| dt.format("%Y-%m-%d").to_string())
}

// -------------------------------------------------------- smartrecruiters

#[derive(Deserialize)]
struct SrResponse {
    #[serde(default)]
    content: Vec<SrPosting>,
}

#[derive(Deserialize)]
struct SrPosting {
    #[serde(default)]
    id: Option<String>,
    #[serde(default, deserialize_with = "http::null_to_default")]
    name: String,
    #[serde(default)]
    location: Option<SrLocation>,
    #[serde(default, rename = "applyUrl")]
    apply_url: Option<String>,
    #[serde(default, rename = "ref")]
    reference: Option<String>,
    #[serde(default, rename = "releasedDate")]
    released_date: Option<String>,
}

#[derive(Deserialize)]
struct SrLocation {
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    remote: Option<bool>,
}

pub async fn fetch_smartrecruiters(
    company: &str,
    slug: &str,
    filters: &Filters,
) -> Result<Vec<Job>> {
    let mut jobs = Vec::new();
    let mut offset = 0usize;
    let limit = 100usize;
    loop {
        let url = format!("https://api.smartrecruiters.com/v1/companies/{slug}/postings");
        let data: SrResponse = http::get_json_with(
            &url,
            &[
                ("limit".into(), limit.to_string()),
                ("offset".into(), offset.to_string()),
            ],
        )
        .await?;
        let page_len = data.content.len();
        if page_len == 0 {
            break;
        }

        for p in data.content {
            let loc_str = smartrecruiters_location(p.location);
            let posting_id = p.id.unwrap_or_default();
            let job_url = smartrecruiters_url(slug, &posting_id, p.apply_url, p.reference);
            // No applyUrl, ref or id to build any identifier from — skip
            // rather than store an empty URL, which would make dedup treat
            // every subsequent url-less posting as a duplicate of the
            // first.
            let Some(job_url) = job_url else { continue };

            let mut description = String::new();
            if !posting_id.is_empty()
                && filters.title_is_relevant(&p.name)
                && filters.location_is_allowed(&loc_str)
            {
                description = fetch_smartrecruiters_description(slug, &posting_id).await;
            }

            jobs.push(Job {
                company: company.to_string(),
                title: p.name,
                location: loc_str,
                url: job_url,
                posted_at: p.released_date,
                description,
                ..Default::default()
            });
        }

        if page_len < limit {
            break;
        }
        offset += limit;
    }
    Ok(jobs)
}

/// Best-effort: any failure (private board, 404, timeout) means no
/// description, not a failed run — the filters treat an empty description
/// as "unknown, don't reject on stack alone".
/// City, region and country joined, with the remote flag folded in.
/// SmartRecruiters splits the location across four optional fields and
/// signals remote separately, so a posting can be remote with no place
/// name at all.
fn smartrecruiters_location(location: Option<SrLocation>) -> String {
    let loc = location.unwrap_or(SrLocation {
        city: None,
        region: None,
        country: None,
        remote: None,
    });
    let joined = [loc.city, loc.region, loc.country]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    match (loc.remote, joined.is_empty()) {
        (Some(true), true) => "Remote".to_string(),
        (Some(true), false) => format!("Remote ({joined})"),
        _ => joined,
    }
}

/// `applyUrl`, then `ref`, then one built from the posting id.
///
/// Returns None when there is nothing to build an identifier from. That
/// posting gets skipped rather than stored with an empty URL, because the
/// URL is half the dedup key — one blank would make every later url-less
/// posting look like a duplicate of the first.
fn smartrecruiters_url(
    slug: &str,
    posting_id: &str,
    apply_url: Option<String>,
    reference: Option<String>,
) -> Option<String> {
    apply_url
        .or(reference)
        .filter(|u| !u.is_empty())
        .or_else(|| {
            (!posting_id.is_empty())
                .then(|| format!("https://jobs.smartrecruiters.com/{slug}/{posting_id}"))
        })
}

async fn fetch_smartrecruiters_description(slug: &str, posting_id: &str) -> String {
    #[derive(Deserialize)]
    struct Detail {
        #[serde(default, rename = "jobAd")]
        job_ad: Option<JobAd>,
    }
    #[derive(Deserialize)]
    struct JobAd {
        #[serde(default)]
        sections: Option<serde_json::Value>,
    }

    let url = format!("https://api.smartrecruiters.com/v1/companies/{slug}/postings/{posting_id}");
    let Ok(detail) = http::get_json::<Detail>(&url).await else {
        return String::new();
    };
    let Some(sections) = detail.job_ad.and_then(|a| a.sections) else {
        return String::new();
    };
    ["jobDescription", "qualifications", "additionalInformation"]
        .iter()
        .filter_map(|key| {
            sections
                .get(key)
                .and_then(|s| s.get("text"))
                .and_then(|t| t.as_str())
                .filter(|t| !t.is_empty())
                .map(str::to_string)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each provider puts the location, the URL and the date somewhere
    /// different, and several of them send `null` for fields the docs
    /// describe as strings. A posting whose JSON shape shifts does not
    /// fail loudly — it arrives with an empty title or no URL and then
    /// quietly poisons dedup, so the mapping is worth pinning down.
    fn parse<T: serde::de::DeserializeOwned>(json: &str) -> T {
        serde_json::from_str(json).expect("fixture should deserialize")
    }

    #[test]
    fn greenhouse_maps_a_posting() {
        let data: GhResponse = parse(
            r#"{"jobs":[{
                "title":"Platform Engineer",
                "location":{"name":"Remote - US"},
                "absolute_url":"https://boards.greenhouse.io/acme/jobs/1",
                "first_published":"2026-10-01T00:00:00Z",
                "content":"<p>Kubernetes</p>"
            }]}"#,
        );
        let jobs = greenhouse_jobs("Acme", data);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].company, "Acme");
        assert_eq!(jobs[0].title, "Platform Engineer");
        assert_eq!(jobs[0].location, "Remote - US");
        assert_eq!(jobs[0].posted_at.as_deref(), Some("2026-10-01T00:00:00Z"));
    }

    /// A real Greenhouse board sent `"location": {"name": null}`, which
    /// aborted the whole company's fetch before `null_to_default` existed.
    /// One employer's null must not cost you every posting they have.
    #[test]
    fn greenhouse_survives_nulls_where_strings_are_documented() {
        let data: GhResponse =
            parse(r#"{"jobs":[{"title":null,"location":{"name":null},"absolute_url":null}]}"#);
        let jobs = greenhouse_jobs("Acme", data);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].title, "");
        assert_eq!(jobs[0].location, "");
    }

    #[test]
    fn greenhouse_falls_back_to_updated_at_when_unpublished() {
        let data: GhResponse =
            parse(r#"{"jobs":[{"title":"SRE","absolute_url":"u","updated_at":"2026-09-09"}]}"#);
        assert_eq!(
            greenhouse_jobs("Acme", data)[0].posted_at.as_deref(),
            Some("2026-09-09")
        );
    }

    /// Ashby names the same field two ways depending on the board's age,
    /// and serves either plain text or HTML for the description.
    #[test]
    fn ashby_accepts_either_spelling_of_location_and_url() {
        let modern: AshbyResponse = parse(
            r#"{"jobs":[{"title":"Infra","location":"Remote (US)","jobUrl":"https://a/1","descriptionPlain":"text"}]}"#,
        );
        let j = &ashby_jobs("Acme", modern)[0];
        assert_eq!(j.location, "Remote (US)");
        assert_eq!(j.url, "https://a/1");
        assert_eq!(j.description, "text");

        let legacy: AshbyResponse = parse(
            r#"{"jobs":[{"title":"Infra","locationName":"Remote","applyUrl":"https://a/2","descriptionHtml":"<p>html</p>"}]}"#,
        );
        let j = &ashby_jobs("Acme", legacy)[0];
        assert_eq!(j.location, "Remote");
        assert_eq!(j.url, "https://a/2");
        assert_eq!(j.description, "<p>html</p>");
    }

    #[test]
    fn workable_joins_the_location_parts_it_actually_has() {
        let data: WorkableResponse = parse(
            r#"{"jobs":[{"title":"DevOps","location":{"city":"Austin","country":"United States"},"url":"https://w/1"}]}"#,
        );
        assert_eq!(
            workable_jobs("Acme", data)[0].location,
            "Austin, United States"
        );
    }

    /// Workable flags remote separately from the place name, so a role can
    /// be remote *and* city-tagged, or remote with no city at all.
    #[test]
    fn workable_folds_in_the_remote_flag() {
        let with_city: WorkableResponse = parse(
            r#"{"jobs":[{"title":"X","location":{"city":"Berlin","workplace":"remote"},"url":"u"}]}"#,
        );
        assert_eq!(
            workable_jobs("Acme", with_city)[0].location,
            "Remote (Berlin)"
        );

        let without: WorkableResponse =
            parse(r#"{"jobs":[{"title":"X","location":{"workplace":"remote"},"url":"u"}]}"#);
        assert_eq!(workable_jobs("Acme", without)[0].location, "Remote");
    }

    /// Lever sends epoch milliseconds. Everything downstream treats
    /// posted_at as a display string, so a bare 13-digit number would
    /// reach the UI unrendered.
    #[test]
    fn lever_renders_its_epoch_millis_as_a_date() {
        let data: Vec<LeverJob> = parse(
            r#"[{"text":"Backend Engineer","hostedUrl":"https://l/1","createdAt":1760000000000,
                 "categories":{"location":"Remote"},"descriptionPlain":"plain"}]"#,
        );
        let j = &lever_jobs("Acme", data)[0];
        assert_eq!(j.title, "Backend Engineer");
        assert_eq!(j.posted_at.as_deref(), Some("2025-10-09"));
        assert_eq!(j.description, "plain");
    }

    #[test]
    fn lever_prefers_the_full_location_list_over_the_single_field() {
        let data: Vec<LeverJob> = parse(
            r#"[{"text":"X","hostedUrl":"u","categories":{"location":"NYC","allLocations":["NYC","Remote"]}}]"#,
        );
        assert_eq!(lever_jobs("Acme", data)[0].location, "NYC, Remote");
    }

    #[test]
    fn smartrecruiters_assembles_its_four_part_location() {
        assert_eq!(
            smartrecruiters_location(Some(SrLocation {
                city: Some("Dublin".into()),
                region: Some("Leinster".into()),
                country: Some("Ireland".into()),
                remote: None,
            })),
            "Dublin, Leinster, Ireland"
        );
        assert_eq!(
            smartrecruiters_location(Some(SrLocation {
                city: None,
                region: None,
                country: None,
                remote: Some(true),
            })),
            "Remote"
        );
        assert_eq!(smartrecruiters_location(None), "");
    }

    /// A posting with no usable identifier is skipped, not stored blank.
    /// The URL is half the dedup key, so one empty string would make every
    /// later url-less posting look like a repeat of the first.
    #[test]
    fn smartrecruiters_url_falls_back_and_then_gives_up() {
        assert_eq!(
            smartrecruiters_url("acme", "99", Some("https://apply".into()), None),
            Some("https://apply".to_string())
        );
        assert_eq!(
            smartrecruiters_url("acme", "99", None, Some("https://ref".into())),
            Some("https://ref".to_string())
        );
        assert_eq!(
            smartrecruiters_url("acme", "99", None, None),
            Some("https://jobs.smartrecruiters.com/acme/99".to_string())
        );
        assert_eq!(
            smartrecruiters_url("acme", "", Some(String::new()), None),
            None
        );
    }

    /// An empty board is a normal answer, not a failure.
    #[test]
    fn an_empty_board_yields_no_jobs_rather_than_an_error() {
        assert!(greenhouse_jobs("Acme", parse::<GhResponse>(r#"{"jobs":[]}"#)).is_empty());
        assert!(ashby_jobs("Acme", parse::<AshbyResponse>(r#"{}"#)).is_empty());
        assert!(workable_jobs("Acme", parse::<WorkableResponse>(r#"{}"#)).is_empty());
        assert!(lever_jobs("Acme", parse::<Vec<LeverJob>>("[]")).is_empty());
    }
}
