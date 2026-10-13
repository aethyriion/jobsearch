# Changelog

## [0.3.2](https://github.com/aethyriion/jobsearch/compare/v0.3.1...v0.3.2) (2026-10-13)


### Fixes

* **docs:** correct the dependency badge and database paths ([65af033](https://github.com/aethyriion/jobsearch/commit/65af0333710d656e45380dd19044b32910d38aff))
* **release:** ship the licence through Homebrew too ([a1b997b](https://github.com/aethyriion/jobsearch/commit/a1b997b528c3221f9fb8a8971144c4999e88f67a))


### Internal

* **tui:** break up the detail pane builder ([1aa212d](https://github.com/aethyriion/jobsearch/commit/1aa212dde6310181e7c37df74392be9370176f54))


### Documentation

* add a recording sized for social feeds ([b237a3e](https://github.com/aethyriion/jobsearch/commit/b237a3ee1b6cf693fe5241e9cd1bbb4b60610951))
* split the reference out of the README ([9788471](https://github.com/aethyriion/jobsearch/commit/978847189654755d7585f8fef3163c3380aea53c))

## [0.3.1](https://github.com/aethyriion/jobsearch/compare/v0.3.0...v0.3.1) (2026-10-12)


### Fixes

* **ci:** let the release actually trigger the publish chain ([0b44440](https://github.com/aethyriion/jobsearch/commit/0b44440df8039935d496de5b884008e07c0cf2dc))

## [0.3.0](https://github.com/aethyriion/jobsearch/compare/v0.2.0...v0.3.0) (2026-10-12)


### ⚠ BREAKING CHANGES

* the database moves from ./data/ to the per-user data directory unless the working directory holds a filters.yaml, and the DB_PATH environment variable is now JOBSEARCH_DB_PATH. A git checkout is unaffected.
* rewrite in Rust as a TUI

### Features

* **adzuna:** enable source, fix query ([5d13730](https://github.com/aethyriion/jobsearch/commit/5d137300c6148dc912324340011a4489a23a0863))
* **cli:** add diagnose subcommand ([cfc8316](https://github.com/aethyriion/jobsearch/commit/cfc83169ad232bb3e184db13f9e827401271a62b))
* **eval:** split scoring into two axes ([b4887a8](https://github.com/aethyriion/jobsearch/commit/b4887a8ae71b9141642b6371ef08e8de2496fdd3))
* **filters:** add seniority axis ([24fb167](https://github.com/aethyriion/jobsearch/commit/24fb16787e6d20d6f7ac71824fb7b7956e10c1dc))
* find config outside the repo ([d36edc8](https://github.com/aethyriion/jobsearch/commit/d36edc89faee5d47fbcb71a6acecafb41d5a06e3))
* rewrite in Rust as a TUI ([333e506](https://github.com/aethyriion/jobsearch/commit/333e5061bc7f3eb1783c38069608d40a9f6974c5))
* **tech:** detect stack from JD text ([c4b5e2c](https://github.com/aethyriion/jobsearch/commit/c4b5e2c3cd7571cfd648112876ab8bfdb82e72db))
* **tui:** add channel and recruiter ([6b751cc](https://github.com/aethyriion/jobsearch/commit/6b751cc76ac110ea69db5df4779787bbac54ff75))
* **tui:** add manual job entries ([745bbb9](https://github.com/aethyriion/jobsearch/commit/745bbb97b3cc1a8d70da5f77ca21a2ed69f8dd81))
* **tui:** add reversible dismissal ([fc9b800](https://github.com/aethyriion/jobsearch/commit/fc9b80096d331fe52095d2e941b32492771befd0))
* **tui:** modal form, status filters ([a55b1af](https://github.com/aethyriion/jobsearch/commit/a55b1afecd240109898daa55f5202e8a523e3d43))
* **tui:** track status age ([6e7ded9](https://github.com/aethyriion/jobsearch/commit/6e7ded9a70eef38e67b4107e7b84866459d808b4))


### Fixes

* **ci:** declare the real MSRV and test it ([32bc0db](https://github.com/aethyriion/jobsearch/commit/32bc0db71b39d614d346434a0c9f49b3d4194aa3))
* **ci:** pick a checksum tool that exists on every runner ([a028b26](https://github.com/aethyriion/jobsearch/commit/a028b26742b3f3a2b3f8c446cff87e25558efb0c))


### Documentation

* add demo GIF and rewrite README ([95e9c59](https://github.com/aethyriion/jobsearch/commit/95e9c591e7c36369a0f3131982113342ce554a16))
* fix first-run setup gaps ([d55c3e5](https://github.com/aethyriion/jobsearch/commit/d55c3e5896f3d81a5c4f451fefae6967bedfd8d8))
