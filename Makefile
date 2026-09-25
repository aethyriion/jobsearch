SHELL := /bin/bash
.DEFAULT_GOAL := tui
.PHONY: tui build release install fetch evaluate refilter discover check test fmt clippy web

CARGO ?= cargo
BIN   := ./target/release/jobsearch

# The TUI is the main interface — fetching, scoring and triage are all one
# keypress from the review list, so the targets below are for cron and
# scripts more than for daily use.
tui: build
	$(CARGO) run --release -- tui

build:
	$(CARGO) build --release

release: build
	@echo "Built $(BIN)"

# Put `jobsearch` on your PATH. It reads its config from the current
# directory, so run it from this repo (or pass --root).
install:
	$(CARGO) install --path . --locked

fetch: build
	$(BIN) fetch

evaluate: build
	@read -rp "Dry run (no API calls, no cost)? [Y/n] " ans; \
	read -rp "Limit? (blank = all) " limit; \
	flags=""; \
	[[ ! "$$ans" =~ ^[Nn] ]] && flags="$$flags --dry-run"; \
	[[ -n "$$limit" ]] && flags="$$flags --limit $$limit"; \
	$(BIN) evaluate $$flags

refilter: build
	$(BIN) refilter --dry-run

# Pull the YC directory, record stage + headcount for every hiring company,
# and resolve as many ATS boards as possible. Cached, so re-running only
# probes companies it hasn't seen.
discover: build
	$(BIN) discover-yc

check: build
	$(BIN) check

test:
	$(CARGO) test

fmt:
	$(CARGO) fmt

clippy:
	$(CARGO) clippy --all-targets -- -D warnings

# The Next.js board is frozen — still works against the same database, but
# new features land in the TUI only. See web/README.md.
web:
	DB_PATH=$(CURDIR)/data/seen_jobs.sqlite3 npm --prefix web run dev
