# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/sunsided/resource-timeline/compare/v0.1.0...v0.1.1) - 2026-11-22

### Changed

- Link the crate to its page on the planning and search site

## [0.1.0](https://github.com/sunsided/resource-timeline/releases/tag/v0.1.0) - 2026-11-22

### Added

- Add the full lint table, clippy.toml and rustfmt.toml
- Add dormant release automation with release-plz
- Add CONTRIBUTING.md
- Add git hooks and Copilot review instructions
- Add README badges, usage sections and absolute links
- Add decoder and timeline fuzz targets
- Add REUSE licensing metadata
- Add cargo-deny, Dependabot and typos configuration
- Add the Codecov configuration
- Add the Taskfile and CI workflows
- Add a bridge example to temporal-control's dispatcher
- Add a checksummed byte encoding and optional serde support
- Add uncertain durations, precedences and state envelopes
- Add end-state level targets to timeline checks
- Add fixed-capacity storage shared with the heap variant
- Add level envelopes for activities with start windows
- Add a greedy priority scheduler and a rover example
- Add plans with atomic placement and earliest-start search
- Add state timelines for modes and visibility windows
- Add rate impacts and an incremental change-point profile
- Implement piecewise-constant resource timelines
- Add design document for resource timelines

### Changed

- Plan admissions to a capped therapy group in a new envelope example
- Check a strategy game build order against a scouted rush in a new example
- Describe the rover example without the planning-hub reference
- Show a colony game settler in a new greedy-scheduling example
- List the crate under game-development instead of algorithms
- Mention game AI and simulation in the README audience
- Name the fuzz crate in the deny.toml wildcard-path comment
- Depend on temporal-control from crates.io
- Describe the released scope and audience in the README
- Fuzz plans, envelopes and serde, and check exported profiles
- Split the bridge example's report into named steps
- Ignore the coverage report
- Render mermaid diagrams on docs.rs
- Write the bench main by hand
- Test that plan placement is all or nothing
- Let the plan search borrow its working memory
- Couple precedences between starts and ends exactly
- Make the docs build without the alloc feature
- Bound state envelope work by candidate ticks, cover targets in envelopes
- Document the implemented scope and benchmark the coupled envelope
- Solve the earliest start with rates in closed form per piece
- Extract level profiles for display and telemetry
- Describe the implemented scope in the README and design
- Benchmark the bound sweep and the earliest-start search
- Describe milestone 1 status in the README
- Test timelines against a tick-by-tick oracle
- Reserve the crate name with a 0.0.0 placeholder

### Dependencies

- Bump the version to 0.1.0 for the first release

### Fixed

- Correct envelope and status docs, test the placement rollback
