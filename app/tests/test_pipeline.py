"""Sanity test for filters.py + dedup.py, using real sample data plus the
actual noise patterns found in a live run on 2026-08-11 (Remote
Poland/Spain/Australia leaking through, and non-engineering titles like
Analyst/Manager/Marketing passing because the old filter was exclusion-only).

Fixtures updated 2026-09-25 when filters.yaml was reconfigured for a
remote-US/Ohio platform-engineering search (was Alberta/Canada + Python/JS):
locations are now US-shaped, and the level/track cases cover
hard_exclusion_keywords, which exists because the priority-title list is
substring-matched and would otherwise rescue "Junior DevOps Engineer".

Run with: python -m app.tests.test_pipeline
"""
import os
from app import filters
from app import dedup

SAMPLE_JOBS = [
    # Should PASS: real fields, US-eligible remote, matches title allowlist
    {"company": "Cloudflare", "title": "Senior Software Engineer, Backend (Batch Infrastructure)",
     "location": "Remote US", "url": "https://job-boards.greenhouse.io/cloudflare/jobs/1111",
     "description": "We use Go, Kubernetes and Terraform on AWS."},
    # Should FAIL: excluded title keyword (compliance)
    {"company": "Cloudflare", "title": "Compliance Lead, US",
     "location": "Remote US", "url": "https://job-boards.greenhouse.io/cloudflare/jobs/7788916003",
     "description": ""},
    # Should FAIL: location not in allowlist — Canada-only remote is now out
    # of scope for this search (US-based candidate).
    {"company": "Cloudflare", "title": "Staff Software Engineer",
     "location": "Remote Canada", "url": "https://job-boards.greenhouse.io/cloudflare/jobs/2222",
     "description": ""},
    # Should FAIL: real noise from 2026-08-11 run — "Remote Poland" used to leak
    # through the old "\bremote\b(?!.*\bus\b)" pattern.
    {"company": "Cloudflare", "title": "Analytics Engineer II",
     "location": "Remote Poland", "url": "https://job-boards.greenhouse.io/cloudflare/jobs/7764109003",
     "description": ""},
    # Should FAIL: real noise — title has no engineering keyword at all,
    # old exclusion-only filter had nothing to catch this on.
    {"company": "Cloudflare", "title": "Marketing Operations Manager",
     "location": "Remote US", "url": "https://job-boards.greenhouse.io/cloudflare/jobs/9999",
     "description": ""},
    {"company": "Cloudflare", "title": "Senior Manager, Talent Brand",
     "location": "Remote US", "url": "https://job-boards.greenhouse.io/cloudflare/jobs/9998",
     "description": ""},
    # Should PASS: Ohio on-site posting (the local half of the location list)
    {"company": "Root Insurance", "title": "Site Reliability Engineer",
     "location": "Columbus, OH", "url": "https://job-boards.greenhouse.io/rootinsurance/jobs/4911972004",
     "description": "SRE role: Kubernetes, Prometheus, Terraform."},
    # Should FAIL: excluded keyword (nurse)
    {"company": "Some Co", "title": "Occupational Health Nurse",
     "location": "Remote US", "url": "https://example.com/jobs/9999",
     "description": ""},
    # Should FAIL: title matches allowlist ("engineer") but the JD is a stack
    # dealbreaker — Java shop, nothing from stack_core anywhere in it.
    {"company": "BigCorp", "title": "Senior Software Engineer",
     "location": "Remote US", "url": "https://example.com/jobs/8888",
     "description": "5+ years of Java and Spring Boot required. Experience with Kafka a plus."},
    # Should PASS: title matches, JD mentions Java AND the core stack — not a
    # hard dealbreaker, worth letting the AI step judge it.
    {"company": "DualStackCo", "title": "Backend Engineer",
     "location": "Remote (US)", "url": "https://example.com/jobs/7777",
     "description": "Our platform is a mix of Java services and a newer Rust/Kubernetes stack."},
    # Duplicate of the first PASS entry by URL -> should be filtered by dedup on 2nd pass
    {"company": "Cloudflare", "title": "Senior Software Engineer, Backend (Batch Infrastructure)",
     "location": "Remote US", "url": "https://job-boards.greenhouse.io/cloudflare/jobs/1111",
     "description": "We use Go, Kubernetes and Terraform on AWS."},
    # Repost under a new URL, same company+title -> should be caught by company+title dedup
    {"company": "Cloudflare", "title": "Senior Software Engineer, Backend (Batch Infrastructure)",
     "location": "Remote US", "url": "https://job-boards.greenhouse.io/cloudflare/jobs/3333-repost",
     "description": "We use Go, Kubernetes and Terraform on AWS."},
    # Should PASS: platform title + "United States" spelled out (Ashby-style)
    {"company": "Docker", "title": "Staff Platform Engineer",
     "location": "United States (Remote)", "url": "https://jobs.ashbyhq.com/docker/22d02404-b9c9-4b77-9448-add55d961444",
     "description": "EKS, Terraform and ArgoCD experience preferred."},
    # Should PASS: real 2026-08-11 false-positive fix — "Software Engineer"
    # is an unambiguous PRIORITY_TITLE_KEYWORDS match, so the "marketing"
    # substring no longer trips EXCLUSION_KEYWORDS.
    {"company": "Warner Music Group", "title": "Software Engineer, Automated Marketing",
     "location": "Ohio, United States", "url": "https://www.adzuna.com/details/5702928490",
     "description": "Build automated marketing tooling. Python and Docker."},
    # Should FAIL: wrong level. Contains the priority keyword "devops
    # engineer", so only hard_exclusion_keywords can stop it.
    {"company": "Cloudflare", "title": "Junior DevOps Engineer",
     "location": "Remote US", "url": "https://example.com/jobs/6666",
     "description": "Kubernetes and AWS."},
    # Should FAIL: wrong track — "Software Engineering Manager" contains the
    # priority keyword "software engineer" as a substring.
    {"company": "Datadog", "title": "Software Engineering Manager, Platform",
     "location": "Remote US", "url": "https://example.com/jobs/5555",
     "description": "Lead a platform team. Kubernetes, Terraform."},
    # Should FAIL: IT-ops title that matches the "systems"/"cloud" allowlist
    {"company": "Some MSP", "title": "Cloud Systems Administrator",
     "location": "Columbus, OH", "url": "https://example.com/jobs/4444",
     "description": "Windows, VMware and Azure administration."},
]

TEST_DB = "data/test_seen_jobs.sqlite3"


def main():
    if os.path.exists(TEST_DB):
        os.remove(TEST_DB)

    print("=== Filter results ===")
    filtered = []
    for job in SAMPLE_JOBS:
        ok = filters.passes_filters(job)
        print(f"{'PASS' if ok else 'DROP':5s} | {job['company']:18s} | {job['title'][:50]:50s} | {job['location']}")
        if ok:
            filtered.append(job)

    print(f"\n{len(filtered)}/{len(SAMPLE_JOBS)} passed title+location+stack filters\n")

    print("=== Dedup results (processing filtered jobs in order) ===")
    kept = []
    with dedup.connect(TEST_DB) as conn:
        for job in filtered:
            if dedup.is_new(conn, job):
                dedup.mark_seen(conn, job)
                kept.append(job)
                print(f"NEW  | {job['company']:18s} | {job['title'][:50]:50s} | {job['url']}")
            else:
                print(f"DUPE | {job['company']:18s} | {job['title'][:50]:50s} | {job['url']}")

    print(f"\nFinal candidates after filters+dedup: {len(kept)}")

    # Assertions to make this a real check, not just eyeballing.
    # PASS: Cloudflare SWE (x3 raw, 1 unique after dedup), Root Insurance SRE,
    # DualStackCo (mixed stack, not a hard dealbreaker), Docker platform role,
    # Warner Music (priority-title rescue).
    # DROP: Compliance Lead, Remote Canada, Remote Poland, Marketing Ops
    # Manager, Senior Manager Talent Brand, Occupational Health Nurse,
    # BigCorp (Java-only dealbreaker), Junior DevOps Engineer (level),
    # Software Engineering Manager (track), Cloud Systems Administrator.
    assert len(filtered) == 7, f"expected 7 to pass all filters, got {len(filtered)}"
    assert len(kept) == 5, f"expected 5 unique candidates after dedup, got {len(kept)}"

    # Targeted unit checks for the specific bugs found in the 2026-08-11 run.
    assert not filters.location_is_allowed("Remote Poland")
    assert not filters.location_is_allowed("Remote Spain")
    assert not filters.location_is_allowed("Remote Australia")
    assert not filters.location_is_allowed("Remote Canada")
    assert not filters.location_is_allowed("Toronto, ON")
    assert filters.location_is_allowed("Remote US")
    assert filters.location_is_allowed("Remote - United States")
    assert filters.location_is_allowed("USA Only")
    assert filters.location_is_allowed("Northern America")   # Remotive spelling
    assert filters.location_is_allowed("Columbus, OH")
    assert filters.location_is_allowed("Cincinnati, Ohio")
    # Live-data cases from the 2026-09-25 smoke runs: Sysdig writes remote-US
    # as "Flexible - USA", and SmartRecruiters appends the country code to
    # on-site locations, which must NOT read as a remote-US signal.
    assert filters.location_is_allowed("Flexible - USA")
    assert filters.location_is_allowed("USA, Canada, Argentina, Mexico, Peru")
    assert filters.location_is_allowed("Remote (Dallas, TX, us)")
    assert not filters.location_is_allowed("Santa Clara, CA, us")
    assert not filters.location_is_allowed("Flexible - Italy")
    assert not filters.location_is_allowed("Germany (Remote)")
    assert not filters.title_is_relevant("Marketing Operations Manager")
    assert not filters.title_is_relevant("Senior Manager, Talent Brand")
    assert not filters.title_is_relevant("Compliance Lead, US")
    assert filters.title_is_relevant("Senior Software Engineer, Backend")
    assert filters.title_is_relevant("Software Engineer, Automated Marketing")
    assert filters.title_is_relevant("Staff Platform Engineer")
    assert filters.title_is_relevant("Site Reliability Engineer")
    assert filters.title_is_relevant("Senior Infrastructure Engineer")
    assert not filters.title_is_relevant("Sales Engineer")  # priority list shouldn't rescue this
    # Level/track vetoes that have to beat the substring-matched priority list.
    assert not filters.title_is_relevant("Junior DevOps Engineer")
    assert not filters.title_is_relevant("Software Engineer Intern")
    assert not filters.title_is_relevant("Software Engineering Manager, Platform")
    assert not filters.title_is_relevant("Cloud Systems Administrator")
    assert not filters.title_is_relevant("Director of Security Engineering")
    assert not filters.title_is_relevant("Head of Infrastructure")
    # ...without over-reaching: "intern" is word-boundary matched.
    assert filters.title_is_relevant("Software Engineer, Internal Tools")
    assert filters.jd_stack_mismatch("5+ years of Java and Spring Boot required.")
    assert not filters.jd_stack_mismatch("Java services and a Rust/Kubernetes stack.")
    assert not filters.jd_stack_mismatch("Go, Terraform and EKS.")
    assert not filters.jd_stack_mismatch("")  # no JD available -> don't reject on stack alone

    print("\nAll assertions passed.")


if __name__ == "__main__":
    main()
