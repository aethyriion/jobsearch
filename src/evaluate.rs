//! Stage 2: AI evaluation via the Anthropic Messages API.
//!
//! Reads every job that passed the deterministic filters but hasn't been
//! scored yet, sends it to Claude alongside your profile, and asks for a
//! FUNCTIONAL FIT judgment — not a title match. Results go into the
//! `ai_evaluations` table and the scored CSV.
//!
//! This is the ONLY part of the pipeline that costs money. Everything
//! upstream is free, which is the whole point of filtering deterministically
//! first: by the time a job reaches here it has already passed title,
//! location and stack screening, so the scored volume is small.
//!
//! It also does double duty as the third source of startup signal. The
//! same call that scores fit is asked to place the company on the stage
//! axis, because the model is already reading the full JD — where funding
//! history, team size and "founding engineer" language usually sit in
//! plain text. That covers the long tail of aggregator- and HN-sourced
//! companies that will never be hand-curated and aren't in the YC
//! directory. A curated or YC-sourced stage always wins over this one
//! (see `Store::upsert_company_stage`).
//!
//! Uses the REST API directly rather than an SDK because Anthropic
//! publishes no official Rust SDK; the request shape below is the
//! documented `POST /v1/messages` contract.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::model::{Evaluation, JobRow, Stage};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";

/// Haiku is the default because this is a bulk classifier: it runs over
/// every candidate a fetch produced, and the task — judging fit against a
/// profile that's right there in the prompt — is one the small model does
/// well. Override with ANTHROPIC_MODEL for a run where you want a stronger
/// read (`claude-sonnet-5`, `claude-opus-5`).
pub const DEFAULT_MODEL: &str = "claude-haiku-4-5";

pub fn model_from_env() -> String {
    std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

const SYSTEM_PROMPT: &str = "You are deciding whether a candidate should spend an application on \
a job posting. There are two separate questions, and conflating them produces useless advice:

1. QUALIFICATION — does the candidate clear the bar THIS POSTING SETS? Not whether they are the \
ideal candidate, not whether the domain is familiar, not whether it is the best job available. \
Read the posting's stated requirements and check them against the profile, one by one.

2. CALLBACK ODDS — if they apply, does a human or an automated screen plausibly surface them? A \
role they would excel at is worth nothing if the application is never read.

Score these independently. A perfect qualification match at a company drowning in applicants and a \
marginal match at a twelve-person startup are different decisions, and collapsing them into one \
number hides which is which.

RULES FOR QUALIFICATION

A gap is something the posting REQUIRES that the candidate lacks. It is not an unfamiliar domain, \
a product area they have not worked in, or a technology that appears in the posting's \
'nice to have' list. Most engineering roles do not require prior domain experience; do not invent \
a requirement the posting does not state. If the posting says 'payments experience preferred', \
that is a preference, not a gap.

Being MORE experienced than the posting asks for is never a gap and must never reduce \
qualification_match. A ten-year engineer applying to a role asking for four years meets the \
requirement. Title bands are not comparable across companies — a 'Software Engineer II' at a large \
company and a 'Senior Engineer' at a startup are often the same work. Over-qualification is a \
CALLBACK risk, not a qualification one; put it there.

Language and stack requirements are real when the posting makes them primary ('you will write \
Kotlin every day'), and not when they are one item in a long list. Distinguish the two.

RULES FOR CALLBACK ODDS

Judge from what the posting and its metadata actually tell you. Things that LOWER the odds: a \
large or well-known employer with a fully-remote req (those receive thousands of applications); a \
posting more than ~45 days old, which is often already filled or dead; hard knockout requirements \
the candidate misses that an ATS screens on mechanically (a required degree, a required \
certification, a security clearance, a specific years-of-experience floor stated as a minimum); a \
staffing-agency listing, which often has no real requisition behind it; being visibly \
over-leveled for the band, which recruiters screen out on the assumption the candidate will leave \
or decline the comp. Things that RAISE the odds: a small company where a founder or hiring manager \
reads every application; a niche stack that shrinks the applicant pool (Rust, Nix, \
Kubernetes-plus-Rust); a narrow geography; an unusual combination of skills the candidate \
genuinely has; a posting that describes a specific problem rather than a generic req.

Be calibrated, not polite. 50 is average. Use 15-30 for a mass-market remote req at a \
household-name employer, 40-60 for a typical mid-size company posting, and 70+ only when there is \
a specific reason to think the application gets read by a person. You are estimating, and a \
posting that gives you little to go on should land near the middle rather than at an extreme.

GROUNDING

Every requirement you cite must be one the posting actually states. When you name a years-of-\
experience floor, a degree, a certification or a clearance, quote the posting's own words for it \
rather than paraphrasing — paraphrasing is where invented requirements come from, and a fabricated \
'requires 1.5+ years' turns a good match into a rejection. If the posting does not state a \
requirement, say that it does not state one. Do not infer a level from the title's band alone.

GENERAL

Be honest and specific, not diplomatic — but honest means accurate in BOTH directions. Inventing a \
gap to seem balanced is as useless as glossing over a real one. Cite specific evidence from the \
profile when claiming a strength transfers; do not assert fit in the abstract.

This candidate is specifically targeting startups, so the company_stage assessment matters too. \
Judge stage from evidence in the posting — funding mentioned, team size, how the company describes \
itself, whether the role is titled \"founding\" anything, equity in the compensation — and set \
stage_confidence honestly low when the posting gives you little to go on. Do not guess a stage \
from the company's name alone.";

/// Field order is load-bearing, not documentation. The model tends to emit
/// tool JSON in declaration order, and with `max_tokens` capped, a run of
/// long free-text fields can exhaust the budget before later fields are
/// written — which caused a real missing-`recommendation` failure in the
/// Python version this ports. Putting the two short, decision-critical
/// fields first means they are very unlikely to be the ones lost to
/// truncation even if a long-text field still gets cut off.
fn evaluation_schema() -> Value {
    json!({
        "name": "submit_evaluation",
        "description": "Submit a structured fit evaluation for this job posting.",
        "input_schema": {
            "type": "object",
            "properties": {
                "match_score": {
                    "type": "integer", "minimum": 0, "maximum": 100,
                    "description": "Overall 'is this worth an application' score, 0-100. Weight qualification_match roughly 70% and callback_odds roughly 30%: a role they clearly qualify for is still worth applying to at poor odds, but a long-shot qualification at poor odds is not."
                },
                "recommendation": {
                    "type": "string", "enum": ["apply", "consider", "skip"],
                    "description": "apply = they meet the stated requirements and the application has a plausible path to being read. consider = they meet most requirements but something real is in the way — a missing hard requirement, or odds so poor the effort is better spent elsewhere. skip = they do not meet the posting's actual requirements, or the role is a different discipline. Do NOT use skip merely because the role is below their level, in an unfamiliar domain, or not the single best available option."
                },
                "qualification_match": {
                    "type": "integer", "minimum": 0, "maximum": 100,
                    "description": "Purely: do they clear the bar this posting sets? 0-100, judged ONLY against the posting's stated requirements. Being over-qualified never lowers this. An unfamiliar product domain never lowers this unless the posting requires domain experience. 70+ means they meet what is asked; below 40 means a stated requirement is genuinely missing."
                },
                "callback_odds": {
                    "type": "integer", "minimum": 0, "maximum": 100,
                    "description": "Estimated chance a human or an automated screen actually surfaces this application, 0-100. 50 is average. Judge from employer size and visibility, how long the posting has been up, agency vs direct, ATS knockout requirements they miss, over-leveling, and how narrow the applicant pool is. Be calibrated — see the callback-odds rules in the system prompt."
                },
                "callback_notes": {
                    "type": "string",
                    "description": "One or two sentences naming the SPECIFIC reasons for the callback_odds number — what would screen them out, or what makes this posting unusually reachable. Concrete factors only ('posted 60 days ago at a 10,000-person employer, fully remote'), never generic advice about tailoring a resume."
                },
                "company_stage": {
                    "type": "string",
                    "enum": ["pre-seed", "seed", "series-a", "series-b", "series-c", "growth", "public", "big-tech", "unknown"],
                    "description": "Company maturity, judged from evidence in the posting. Use 'unknown' when the posting genuinely gives you nothing to judge from — that is more useful than a guess."
                },
                "company_headcount": {
                    "type": "integer", "minimum": 0,
                    "description": "Approximate engineering+total headcount if the posting states or strongly implies one. Omit if it doesn't."
                },
                "stage_confidence": {
                    "type": "integer", "minimum": 0, "maximum": 100,
                    "description": "How confident you are in company_stage, 0-100. Low is fine and expected for a thin posting."
                },
                "genuine_gaps": {
                    "type": "string",
                    "description": "Requirements this posting STATES that the candidate does not meet. Not unfamiliar domains, not preferences, not 'nice to haves', and never that they are over-qualified. If they meet everything the posting asks for, say so plainly rather than manufacturing a gap. Keep to 2-3 sentences."
                },
                "transferable_strengths": {
                    "type": "string",
                    "description": "Which of the candidate's competencies genuinely transfer to this role, and why — cite specifics from their profile, not generic claims. Keep to 2-3 sentences."
                },
                "risk_factors": {
                    "type": "string",
                    "description": "Non-skill risks that would affect the JOB, not the application: likely comp mismatch, company-stage risk given the candidate's stated preferences, stack dealbreakers the deterministic filter might have missed, a role that is a different discipline than its title suggests. Screening risks belong in callback_notes, not here. Keep to 2-3 sentences."
                }
            },
            "required": ["match_score", "recommendation", "qualification_match", "callback_odds",
                         "callback_notes", "company_stage", "stage_confidence",
                         "genuine_gaps", "transferable_strengths", "risk_factors"]
        }
    })
}

pub struct Evaluator {
    api_key: String,
    model: String,
    client: reqwest::Client,
    profile_yaml: String,
}

impl Evaluator {
    pub fn new(profile: &serde_yaml::Value, model: String) -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .ok_or_else(|| anyhow!("ANTHROPIC_API_KEY is not set — put it in .env or export it"))?;
        Ok(Self {
            api_key,
            model,
            client: crate::sources::http::client().clone(),
            // Serialized once: the profile is identical for every job in a
            // run, and it's the largest part of the prompt.
            profile_yaml: serde_yaml::to_string(profile)
                .context("serializing profile.yaml for the prompt")?,
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn build_user_prompt(&self, job: &JobRow, looks_truncated: bool) -> String {
        let description = crate::filters::strip_html(&job.description);
        let description = description.trim();
        let description = if description.is_empty() {
            "(no JD text available)"
        } else {
            description
        };

        // Without this note, a thin snippet reads to the model as "this
        // role lists no responsibilities", and it invents gaps and drops
        // the score for something the posting never claimed.
        let partial_note = if looks_truncated && !job.description.is_empty() {
            "\nNOTE: this description looks like a short snippet, not the full posting — it may \
             have been cut off mid-sentence. Do NOT lower match_score or invent genuine_gaps just \
             because a responsibility isn't mentioned here; judge fit on title, company, location, \
             and whatever specifics ARE present. If the snippet is too thin to say anything \
             meaningful about stack or seniority, say so in genuine_gaps rather than guessing.\n"
        } else {
            ""
        };

        // Posting age and who is advertising it are both inputs the
        // callback-odds rules name explicitly, and neither is anywhere in
        // the JD text — without them the model is guessing at the two
        // strongest signals it has been asked to weigh.
        let posted = match job.posted_at.as_deref() {
            Some(p) if !p.is_empty() => {
                let date = &p[..p.len().min(10)];
                match crate::model::days_since(p) {
                    Some(days) => format!("Posted: {date} ({days} days ago)\n"),
                    None => format!("Posted: {date}\n"),
                }
            }
            _ => String::new(),
        };
        let channel = match job.channel {
            Some(crate::model::Channel::Agency) => {
                "Advertised by: a staffing agency, not the employer directly\n"
            }
            Some(crate::model::Channel::Direct) => "Advertised by: the employer directly\n",
            _ => "",
        };

        format!(
            "CANDIDATE PROFILE:\n{profile}\n\n---\n\nJOB POSTING TO EVALUATE:\n\
             Company: {company}\nTitle: {title}\nLocation: {location}\nURL: {url}\n\
             Source: {source}\n{posted}{channel}{partial_note}\nDescription:\n{description}\n\n---\n\n\
             Call submit_evaluation with your structured assessment.",
            profile = self.profile_yaml,
            company = job.company,
            title = job.title,
            location = job.location,
            url = job.url,
            source = job.source,
        )
    }

    /// Call the model and validate that the tool response carries every
    /// required field.
    ///
    /// A forced `tool_choice` can still produce an incomplete object if
    /// `max_tokens` is hit mid-generation, so a short response is retried
    /// once with more room rather than failing the whole run on one job.
    pub async fn evaluate(&self, job: &JobRow, looks_truncated: bool) -> Result<Evaluation> {
        let user_content = self.build_user_prompt(job, looks_truncated);
        let mut max_tokens = 1536u32;
        let max_retries = 1;

        let mut last_err = None;
        for attempt in 0..=max_retries {
            let body = json!({
                "model": self.model,
                "max_tokens": max_tokens,
                "system": SYSTEM_PROMPT,
                "tools": [evaluation_schema()],
                "tool_choice": {"type": "tool", "name": "submit_evaluation"},
                "messages": [{"role": "user", "content": user_content}],
            });

            let resp = self
                .client
                .post(API_URL)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
                .context("calling the Anthropic Messages API")?;

            let status = resp.status();
            let text = resp.text().await.context("reading the API response")?;
            if !status.is_success() {
                // A 4xx is a request problem that a retry won't fix (bad
                // key, bad model name, malformed schema) — fail fast and
                // say what the API said, rather than burning the retry.
                if status.as_u16() < 500 && status.as_u16() != 429 {
                    bail!("Anthropic API returned {status}: {}", truncate(&text, 500));
                }
                last_err = Some(anyhow!(
                    "Anthropic API returned {status}: {}",
                    truncate(&text, 300)
                ));
                continue;
            }

            match parse_response(&text) {
                Ok(eval) => return Ok(eval),
                Err(e) => {
                    last_err = Some(e);
                    // Most likely truncation mid-tool-call — give the
                    // retry more room.
                    max_tokens += 512;
                    if attempt == max_retries {
                        break;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("evaluation failed for {}", job.url)))
    }
}

#[derive(Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<Value>,
}

fn parse_response(text: &str) -> Result<Evaluation> {
    let resp: MessagesResponse =
        serde_json::from_str(text).context("parsing the Anthropic API response")?;

    let input = resp
        .content
        .iter()
        .find(|b| b.kind == "tool_use" && b.name.as_deref() == Some("submit_evaluation"))
        .and_then(|b| b.input.as_ref())
        .ok_or_else(|| {
            anyhow!(
                "model didn't call submit_evaluation (stop_reason={})",
                resp.stop_reason.as_deref().unwrap_or("unknown")
            )
        })?;

    let get_str = |key: &str| -> Option<String> {
        input
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    let get_i64 = |key: &str| -> Option<i64> { input.get(key).and_then(Value::as_i64) };

    // The two decision-critical fields are required; the prose fields are
    // not. A response that scored the job but got cut off before writing
    // three paragraphs of rationale is still worth keeping — re-running it
    // would cost money to recover text that isn't what the score is for.
    let match_score = get_i64("match_score").ok_or_else(|| {
        anyhow!(
            "response is missing match_score: {}",
            truncate(&input.to_string(), 300)
        )
    })?;
    let recommendation = get_str("recommendation").ok_or_else(|| {
        anyhow!(
            "response is missing recommendation: {}",
            truncate(&input.to_string(), 300)
        )
    })?;

    Ok(Evaluation {
        match_score: match_score.clamp(0, 100),
        recommendation,
        qualification_match: get_i64("qualification_match").map(|v| v.clamp(0, 100)),
        callback_odds: get_i64("callback_odds").map(|v| v.clamp(0, 100)),
        callback_notes: get_str("callback_notes").unwrap_or_default(),
        genuine_gaps: get_str("genuine_gaps").unwrap_or_default(),
        transferable_strengths: get_str("transferable_strengths").unwrap_or_default(),
        risk_factors: get_str("risk_factors").unwrap_or_default(),
        // "unknown" is a real answer from the schema, not a parse failure
        // — Stage::parse returns None for it, which is exactly right.
        company_stage: get_str("company_stage").as_deref().and_then(Stage::parse),
        company_headcount: get_i64("company_headcount").filter(|h| *h > 0),
        stage_confidence: get_i64("stage_confidence"),
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_complete_tool_response() {
        let body = r#"{
          "content": [{"type":"tool_use","name":"submit_evaluation","input":{
            "match_score": 87, "recommendation": "apply",
            "company_stage": "series-b", "company_headcount": 80, "stage_confidence": 70,
            "genuine_gaps": "g", "transferable_strengths": "s", "risk_factors": "r"
          }}],
          "stop_reason": "tool_use"
        }"#;
        let eval = parse_response(body).unwrap();
        assert_eq!(eval.match_score, 87);
        assert_eq!(eval.recommendation, "apply");
        assert_eq!(eval.company_stage, Some(Stage::SeriesB));
        assert_eq!(eval.company_headcount, Some(80));
        assert_eq!(eval.stage_confidence, Some(70));
    }

    #[test]
    fn parses_the_two_axis_scores() {
        let body = r#"{
          "content": [{"type":"tool_use","name":"submit_evaluation","input":{
            "match_score": 71, "recommendation": "apply",
            "qualification_match": 85, "callback_odds": 25,
            "callback_notes": "Posted 60 days ago at a 5,000-person employer.",
            "company_stage": "growth", "stage_confidence": 60,
            "genuine_gaps": "g", "transferable_strengths": "s", "risk_factors": "r"
          }}],
          "stop_reason": "tool_use"
        }"#;
        let eval = parse_response(body).unwrap();
        assert_eq!(eval.qualification_match, Some(85));
        assert_eq!(eval.callback_odds, Some(25));
        assert!(eval.callback_notes.starts_with("Posted 60 days"));
    }

    /// Evaluations stored before the two-axis rewrite have no such fields.
    /// They must read back as "not asked", never as zero — a 0 would sort
    /// as the worst possible odds and bury rows that were simply scored
    /// under the older prompt.
    #[test]
    fn an_evaluation_without_the_new_axes_is_unknown_not_zero() {
        let body = r#"{
          "content": [{"type":"tool_use","name":"submit_evaluation","input":{
            "match_score": 71, "recommendation": "apply",
            "company_stage": "growth", "stage_confidence": 60,
            "genuine_gaps": "g", "transferable_strengths": "s", "risk_factors": "r"
          }}],
          "stop_reason": "tool_use"
        }"#;
        let eval = parse_response(body).unwrap();
        assert_eq!(eval.qualification_match, None);
        assert_eq!(eval.callback_odds, None);
        assert_eq!(eval.callback_notes, "");
    }

    #[test]
    fn unknown_stage_is_no_stage_not_an_error() {
        let body = r#"{"content":[{"type":"tool_use","name":"submit_evaluation","input":{
            "match_score": 50, "recommendation": "consider", "company_stage": "unknown",
            "stage_confidence": 10, "genuine_gaps":"g","transferable_strengths":"s","risk_factors":"r"
        }}]}"#;
        let eval = parse_response(body).unwrap();
        assert_eq!(eval.company_stage, None);
        assert_eq!(eval.match_score, 50);
    }

    #[test]
    fn a_truncated_response_keeps_the_score_if_the_critical_fields_survived() {
        // Exactly the failure the field ordering is designed around: the
        // long prose fields were cut off, the decision fields weren't.
        let body = r#"{"content":[{"type":"tool_use","name":"submit_evaluation","input":{
            "match_score": 72, "recommendation": "consider"
        }}],"stop_reason":"max_tokens"}"#;
        let eval = parse_response(body).unwrap();
        assert_eq!(eval.match_score, 72);
        assert_eq!(eval.genuine_gaps, "");
    }

    #[test]
    fn a_response_missing_the_decision_fields_is_an_error() {
        let body = r#"{"content":[{"type":"tool_use","name":"submit_evaluation","input":{
            "genuine_gaps": "g"
        }}],"stop_reason":"max_tokens"}"#;
        assert!(parse_response(body).is_err());
    }

    #[test]
    fn a_text_only_response_is_an_error_naming_the_stop_reason() {
        let body = r#"{"content":[{"type":"text","text":"I cannot"}],"stop_reason":"end_turn"}"#;
        let err = parse_response(body).unwrap_err().to_string();
        assert!(
            err.contains("end_turn"),
            "error should name the stop reason: {err}"
        );
    }

    #[test]
    fn out_of_range_scores_are_clamped() {
        let body = r#"{"content":[{"type":"tool_use","name":"submit_evaluation","input":{
            "match_score": 150, "recommendation": "apply"
        }}]}"#;
        assert_eq!(parse_response(body).unwrap().match_score, 100);
    }
}
