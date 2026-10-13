#!/usr/bin/env bash
# Build a throwaway database of FICTIONAL companies for the README demo.
#
#   ./docs/demo-data.sh && vhs docs/demo.tape
#
# Why fictional: a screenshot that attaches a fabricated match score and an
# "apply / skip" verdict to a real employer's name reads as a real
# assessment of that business. These companies do not exist.
#
# Writes to $DEMO_DB (default /tmp/jobsearch-demo/data/seen_jobs.sqlite3),
# never to your real database.
set -euo pipefail
cd "$(dirname "$0")/.."

DEMO_DB="${DEMO_DB:-/tmp/jobsearch-demo/data/seen_jobs.sqlite3}"
mkdir -p "$(dirname "$DEMO_DB")"
rm -f "$DEMO_DB"

cargo build --release
# Let the binary create the authoritative schema rather than duplicating it.
JOBSEARCH_DB_PATH="$DEMO_DB" ./target/release/jobsearch check >/dev/null

DEMO_DB="$DEMO_DB" python3 - <<'PY'
import os, sqlite3

db = os.environ["DEMO_DB"]

def normalize(text):
    """Mirror of store.rs::normalize — alphanumerics kept, runs of anything
    else collapsed to one space. The company_stage join depends on it."""
    out, last_space = [], True
    for ch in text.lower():
        if ch.isalnum():
            out.append(ch); last_space = False
        elif not last_space:
            out.append(" "); last_space = True
    return "".join(out).strip()

# company, title, location, stage, headcount, score, rec, qualified, odds, tech, status, source
ROWS = [
 ("Hollowpoint","Senior Platform Engineer","Remote (US)","seed",14,91,"apply",93,72,"rust kubernetes aws terraform","","greenhouse"),
 ("Nimbus Relay","Infrastructure Engineer","Remote - US","series-a",38,88,"apply",90,64,"go kubernetes gcp prometheus","applied","ashby"),
 ("Tessellate","Staff SRE, Control Plane","Remote (United States)","series-a",52,86,"apply",88,58,"rust aws postgres terraform","interview","greenhouse"),
 ("Quiet Harbor","Founding Infrastructure Engineer","Remote (US)","pre-seed",6,84,"apply",81,88,"rust nix postgres","","lever"),
 ("Driftwood Labs","Platform Engineer, Developer Experience","USA | Remote","seed",22,81,"apply",86,69,"go kubernetes argocd ci","recruiter","ashby"),
 ("Saltmarsh","Senior Backend Engineer","Remote - USA","series-b",140,78,"apply",84,41,"go postgres redis kubernetes","applied","greenhouse"),
 ("Overcast Systems","Site Reliability Engineer II","Remote (US)","series-b",210,74,"consider",79,38,"python aws terraform prometheus","","workable"),
 ("Lanternfish","Senior Software Engineer, Storage","Remote (US)","series-a",44,73,"consider",77,55,"rust aws","","greenhouse"),
 ("Ironwood Compute","Infrastructure Engineer, Baremetal","Remote - US","series-c",380,71,"consider",80,33,"go kubernetes networking","","ashby"),
 ("Pale Blue Dot","DevOps Engineer","Remote (United States)","growth",900,66,"consider",74,27,"terraform aws jenkins","rejected","smartrecruiters"),
 ("Cinderpath","Cloud Platform Engineer","USA Only","seed",19,64,"consider",71,61,"gcp kubernetes python","applied","lever"),
 ("Mistral Keep","Senior DevOps Engineer","Remote - US","series-b",165,61,"consider",68,44,"azure terraform dotnet","","greenhouse"),
 ("Thorn & Co","Data Platform Engineer","Remote (US)","series-a",61,58,"consider",64,52,"python spark aws airflow","","ashby"),
 ("Glasswing Metrics","Observability Engineer","Remote - USA","seed",27,56,"consider",70,66,"go prometheus grafana","silence","greenhouse"),
 ("Ravelin Fields","Backend Engineer, Payments","Remote (US)","series-b",190,52,"consider",61,36,"kotlin postgres kafka","applied","lever"),
 ("Dockside Analytics","Software Engineer, Infrastructure","Remote (United States)","series-a",48,49,"skip",55,47,"java kubernetes aws","","workable"),
 ("Verdigris Tools","Frontend Platform Engineer","Remote - US","seed",16,41,"skip",46,58,"typescript react vite","","ashby"),
 ("Hallowmere","Machine Learning Engineer","Remote (US)","series-c",420,34,"skip",38,29,"python pytorch cuda","","greenhouse"),
 ("Steeplejack","Salesforce Developer","Remote - USA","growth",1200,18,"skip",21,24,"salesforce apex","skipped","smartrecruiters"),
 ("Bramblewick","Engineering Manager, Platform","Remote (US)","series-b",230,28,"skip",33,40,"kubernetes aws","","lever"),
]

DESC = (
 "We are building the control plane that our customers deploy on. You will own "
 "the provisioning path end to end: cluster lifecycle, the delivery pipeline into "
 "it, and the observability that makes it debuggable at 3am.\n\n"
 "What you will do\n"
 "- Operate production Kubernetes across multiple regions\n"
 "- Build and maintain the Terraform that defines our infrastructure\n"
 "- Own the CI path from commit to deployed artifact\n"
 "- Share the on-call rotation with the rest of the platform team\n\n"
 "What we look for\n"
 "- Deep production experience with Kubernetes and a major cloud\n"
 "- Comfort writing services in Go or Rust\n"
 "- 4+ years building infrastructure\n"
)

c = sqlite3.connect(db)
for i, (co, ti, lo, stage, head, score, rec, qual, odds, tech, status, src) in enumerate(ROWS):
    slug = co.lower().replace(" ", "-").replace("&", "and")
    url = f"https://jobs.example.com/{slug}/{1000 + i}"
    c.execute("INSERT OR REPLACE INTO job_details "
              "(url,company,title,location,posted_at,description,passed_filters,source) "
              "VALUES (?,?,?,?,?,?,1,?)",
              (url, co, ti, lo, f"2026-10-{(i % 11) + 1:02d}T12:00:00Z", DESC + " Stack: " + tech, src))
    c.execute("INSERT OR REPLACE INTO seen_jobs (url,company_title_key,company,title) VALUES (?,?,?,?)",
              (url, f"{co}::{ti}::{lo}".lower(), co, ti))
    c.execute("INSERT OR REPLACE INTO ai_evaluations "
              "(url,match_score,recommendation,genuine_gaps,transferable_strengths,risk_factors,"
              "qualification_match,callback_odds,callback_notes,model) VALUES (?,?,?,?,?,?,?,?,?,?)",
              (url, score, rec,
               "The posting asks for deep experience with a managed service you have not run in production.",
               "Your Kubernetes and Terraform work maps directly onto the provisioning responsibilities described here.",
               "Compensation band is not stated, and the team is small enough that scope could shift quickly.",
               qual, odds,
               f"Posted recently at a {stage} company; a small team means a human is likely to read every application.",
               "claude-haiku-4-5"))
    c.execute("INSERT OR REPLACE INTO company_stage (company_key,company,stage,headcount,source) VALUES (?,?,?,?,?)",
              (normalize(co), co, stage, head, "curated"))
    if status:
        offset = f"-{(i % 4) * 6 + 2} days"
        c.execute("INSERT OR REPLACE INTO user_status (url,my_status,notes,updated_at) "
                  "VALUES (?,?,'',datetime('now',?))", (url, status, offset))
        c.execute("INSERT INTO status_history (url,status,at) VALUES (?,?,datetime('now',?))",
                  (url, status, offset))
c.commit()
print(f"demo database ready: {db}  ({len(ROWS)} fictional postings)")
PY
