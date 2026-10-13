//! Job sources. Two shapes:
//!
//! - **Job sources** (`ats`, `aggregators`, `hn`) return postings, and all
//!   normalize to `model::Job` so nothing downstream needs to know where a
//!   job came from.
//! - **Company sources** (`yc`) return employers with stage metadata,
//!   feeding `companies.yaml` and the `company_stage` table rather than
//!   the job pipeline directly.
//!
//! There is no Wellfound fetcher, on purpose; the README explains why.

pub mod aggregators;
pub mod ats;
pub mod hn;
pub mod http;
pub mod yc;
