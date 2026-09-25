//! Job sources. Two shapes:
//!
//! - **Job sources** (`ats`, `aggregators`, `hn`) return postings, and all
//!   normalize to `model::Job` so nothing downstream needs to know where a
//!   job came from.
//! - **Company sources** (`yc`) return employers with stage metadata,
//!   feeding `companies.yaml` and the `company_stage` table rather than
//!   the job pipeline directly.
//!
//! ### A note on Wellfound
//!
//! Wellfound (formerly AngelList Talent) is the other obvious
//! startup-native source, and there is deliberately no fetcher for it.
//! Its GraphQL endpoint returns 403 behind a Cloudflare interstitial, and
//! so does `robots.txt` itself, for any client that doesn't present as a
//! full browser. Getting past that would mean impersonating a browser to
//! defeat bot protection the site is plainly choosing to apply — so this
//! crate doesn't, and the YC directory plus the HN thread cover the same
//! ground through interfaces that are meant to be read by programs.
//!
//! If you want Wellfound coverage, the supported path is their RSS/email
//! job alerts into a mailbox you read yourself.

pub mod aggregators;
pub mod ats;
pub mod hn;
pub mod http;
pub mod yc;
