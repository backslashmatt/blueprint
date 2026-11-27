# Blueprint — requirements for continuous agent development

This document is the standing brief for a Claude 5.5 agent that develops the app
across many unattended sessions. It says what to build, how fast it must be, how
to decide things alone, and what "done" means. Read it in full at the start of
every session. If it conflicts with a GitHub issue, this document wins; if it
conflicts with the repository's `CLAUDE.md`, `CLAUDE.md` wins.

Sections 1–6 define the product. Sections 7–10 define how the agent works.
Section 11 is the prompt that starts a session.

---

## 1. Product

### 1.1 One-sentence definition

Blueprint is a native code review app that presents a change as a change to the
system's **architecture**, which the reviewer drills into until they reach code.

### 1.2 The core idea

In every existing review tool the unit of review is the file diff, and the
reviewer reconstructs the design in their head. Blueprint inverts this: **the
design is the primary artifact and code is the evidence for it.** A reviewer
should be able to approve or reject the *shape* of a change (what was added,
what now depends on what, which boundary was crossed) before reading a line,
and then spend their line-reading time only where the shape tells them to.

### 1.3 Where it is going

Review is the first product. The destination is an **IDE** built on the same
model, where you navigate and edit a system through its architecture. This
matters now for one reason: the architecture model (section 3) and the engine
(section 5) must be built as a general, incremental model of a codebase, not as
a diff viewer. Do not build IDE features until milestone M6, and do not make
choices that would have to be thrown away to get there.

### 1.4 Who it is for

A senior engineer reviewing a change of 200–5,000 lines in a codebase of up to
several million lines, who wants to answer three questions in order:

1. What changed in the design?
2. Is that the right design?
3. Is the code a correct implementation of it?

### 1.5 Non-goals

- Not a git client, CI dashboard or issue tracker.
- No AI-written review comments in v1. The model is computed deterministically
  from the code. (An LLM summary layer may come later, behind a flag, and must
  never be on the path of any performance budget in section 4.)
- No accounts, no server, no telemetry in v1. It runs locally against a git
  repository on disk.
- No plugin system before M6.

---

## 2. Product principles

Use these to settle any design question this document does not answer.

1. **Design first, code last.** Every screen opens at the highest level of
   abstraction that still shows the change. Code is always reachable and never
   the default.
2. **Speed is a feature, and it is measured.** A change that breaks a budget in
   section 4 is a failing change, the same as a failing test.
3. **Never lie about the architecture.** Anything shown is derived from the
   code. Where the analysis is uncertain (dynamic dispatch, reflection,
   unsupported language), show it as uncertain. A missing edge is better than an
   invented one.
4. **Keyboard complete.** Every action has a key binding; the mouse is optional.
5. **Local and private.** Source code never leaves the machine.
6. **One model, many views.** Views never compute architecture themselves. They
   render the engine's model.

---

## 3. The architecture model

### 3.1 Levels

The model is a hierarchy with four fixed levels. Drilling down moves one level.

| Level | Name | What a node is | Derived from |
|---|---|---|---|
| L1 | System | A deployable or publishable unit: app, service, library | Workspace manifests (`Cargo.toml`, `package.json`, `*.csproj`, `go.mod`, Nx/Bazel project files) |
| L2 | Module | A package, namespace or directory with a public surface | Directory and module structure, visibility rules |
| L3 | Component | A type, or a file-level group of functions | Syntax tree |
| L4 | Code | A function or method, and its lines | Syntax tree and text |

### 3.2 Edges

Edges are typed and directed: `depends-on` (import), `calls`, `implements` /
`extends`, `reads` / `writes` (data stores and shared state), and
`exposes` (public API). An edge between two L4 nodes is aggregated upward, so an
L1 edge is the roll-up of every lower edge that crosses that boundary, and
expanding it lists them.

Every node and edge carries a **confidence**: `exact` (resolved by the
analyser), `inferred` (heuristic, such as a name match), or `unknown`.

### 3.3 Architectural diff

A review compares two commits. The engine builds the model at each and diffs
them. The diff is expressed in design terms:

- nodes added, removed, moved, renamed, or changed in public surface
- edges added or removed, with **new cross-boundary dependencies** and **new
  dependency cycles** called out as first-class findings
- a per-node change weight (lines changed, and whether the change is to
  signature or to body), rolled up so a reviewer sees where the change is
  concentrated

A body-only change with no change to surface or edges is a **contained**
change. Contained changes are shown as a quiet state so that structural changes
stand out.

### 3.4 Language support

Language support is a plug-in boundary inside the engine: one trait, one
implementation per language, driven by tree-sitter grammars. Order of delivery:
Rust (so the app can review itself), TypeScript/JavaScript, C#, Python, Go.
A language with no analyser still appears, at L1–L2 from the file system and at
L4 as plain text diff, marked `unknown` confidence.

---

## 4. Performance budgets

Speed is the highest-value objective after correctness. These are hard budgets,
enforced by benchmarks in CI (section 9.3). Reference machine: a 4-core laptop,
16 GB RAM, SSD. Reference repositories: **small** (10k lines), **medium**
(500k lines), **large** (5M lines), pinned by commit in the benchmark suite.

| Operation | Budget |
|---|---|
| Cold start to interactive window | 300 ms |
| Open a previously indexed repository, to first model on screen | 200 ms |
| First index, medium repository | 10 s, with L1–L2 on screen inside 1 s |
| First index, large repository | 120 s, with L1–L2 on screen inside 3 s |
| Re-index after a one-file change | 50 ms |
| Architectural diff of two indexed commits, medium repository | 150 ms |
| Drill down or up one level (input to painted frame) | 50 ms |
| Any keystroke or pointer input to painted frame | 16 ms |
| Scrolling and pan/zoom | 60 fps minimum, 120 fps on capable displays |
| Open a 50,000-line file at L4 | 100 ms |
| Resident memory, medium repository open | 500 MB |
| Installed size | 50 MB |

Rules that follow from the budgets:

- The UI thread never does I/O, parsing or layout of off-screen content.
- Everything is incremental and cancellable. New input cancels stale work.
- Show partial results. Coarse levels render before fine levels are indexed.
- The index is persisted on disk and keyed by content hash, so no file is ever
  parsed twice.
- Measure before optimising, and record the measurement in the PR.

If a budget proves physically unreachable, do not quietly relax it. Record the
measurement and the reason in a decision record (section 8.3), set the new
number there, and label the issue `needs-owner`.

---

## 5. Technical requirements

### 5.1 Platforms

macOS (Apple silicon and Intel), Windows 10+ (x64 and ARM64) and Linux (x64 and
ARM64, Wayland and X11) are required and ship together from M1. iPadOS, Android
and web are later targets: do not build them, and do not block them (see 5.3).

On Linux the named distributions are Ubuntu (current LTS) and Fedora (current
release). Each must build, pass the tests and run the app on Wayland and X11;
Wayland is the primary session type, since both default to it. CI covers both
distributions, with build dependencies documented for each, and the M5
installers include a `.deb` and an `.rpm` (#37, 2026-11-18).

"Native" means compiled to machine code with a GPU-rendered UI and platform
conventions for menus, shortcuts, file dialogs, windowing, fonts and
accessibility. It rules out Electron and other browser-shell approaches.

### 5.2 Stack

- **Language:** Rust, stable toolchain, one Cargo workspace.
- **Parsing:** tree-sitter, with incremental re-parse.
- **Git:** `gix` (pure Rust). Never shell out to `git` on a hot path.
- **Incremental computation:** a query-based engine (`salsa` or equivalent), so
  every derived fact is memoised and invalidated precisely.
- **Storage:** an embedded, memory-mapped, content-addressed index.
- **UI toolkit:** GPUI is the default, because it is GPU-accelerated, runs on
  all three desktop platforms, and already carries a production IDE. This is
  the one choice that must be **validated before it is built on**: M0 includes
  a time-boxed spike that renders a 5,000-node graph and a 50,000-line text
  view on all three platforms against the section 4 budgets. If GPUI fails,
  evaluate Slint and a custom `wgpu` renderer the same way and record the
  result as a decision record.

Check current documentation for every dependency before adopting it. Versions,
APIs and platform support change faster than this document.

### 5.3 Architecture

```
crates/
  model/      Types for nodes, edges, levels, diffs. No I/O. No dependencies on other crates.
  lang-*/     One crate per language analyser, behind the trait in `model`.
  engine/     Indexing, incremental queries, git, storage, diffing. Headless.
  protocol/   The serialisable request/response API between engine and any front end.
  cli/        Headless front end: index, diff, dump the model as JSON. Used by tests and benchmarks.
  ui/         View state and rendering. Depends on `protocol` only.
  app/        Platform shell: windows, menus, packaging.
bench/        Benchmarks and the pinned reference repositories.
```

Hard rules:

- `engine` has no UI dependency and is fully usable from `cli`. Everything the
  app can show, the CLI can print. This is what makes the product testable by
  an agent in a container with no display, and what keeps mobile, web and the
  IDE possible later.
- `ui` talks to `engine` only through `protocol`, asynchronously.
- No `unsafe` outside a crate whose README justifies it. No `unwrap` or
  `expect` outside tests. Errors are typed.
- No dependency is added without stating in the PR what it costs in build time
  and binary size.

### 5.4 Views (the v1 surface)

1. **Review list** — open a repository; pick a base and head (branch, commit or
   pull request ref).
2. **Map** — the architecture graph at the current level, with the diff
   overlaid (added, removed, changed, contained). Pan, zoom, select, drill.
   Layout must be **stable**: the same model always lays out the same way, and
   a small change moves few nodes.
3. **Findings** — the ordered list from section 3.3: new cross-boundary
   dependencies, new cycles, public surface changes, then change concentration.
   This is the reviewer's default starting point.
4. **Drill-down** — selecting a node or edge moves one level down, keeping a
   breadcrumb back to L1. At L4 it becomes a side-by-side code diff with syntax
   highlighting.
5. **Review state** — mark any node at any level as reviewed, with an optional
   note. State rolls up (a module is reviewed when its changed children are),
   persists locally, and survives a rebase where the node's content is unchanged.
6. **Command palette** — every action, searchable.

### 5.5 Quality requirements

- **Accessibility:** full keyboard operation, screen reader labels through the
  platform accessibility API, WCAG AA contrast, respect for reduced-motion and
  system font-size settings. The map must have a non-visual equivalent (the
  findings list and an outline tree satisfy this).
- **Theming:** light and dark, following the system.
- **Robustness:** a parse error, an unsupported language or a corrupt index
  degrades one node, never the session. The index can always be rebuilt.
- **Security:** treat every repository as untrusted input. Never execute
  anything from it (no build scripts, hooks or config-driven commands). Fuzz the
  analysers and the index reader.

---

## 6. Milestones

Work in this order. A milestone is complete when its exit test passes in CI on
all three platforms. Then start the next one without waiting.

| | Milestone | Exit test |
|---|---|---|
| M0 | **Foundations.** Workspace, CI on three platforms, benchmark harness with reference repositories, UI toolkit spike and its decision record. | CI green; benchmark job reports numbers; toolkit decision recorded. |
| M1 | **Engine, L1–L2.** Manifest and directory analysis, persisted index, `cli index` and `cli model`. | CLI emits the L1–L2 model of this repository and the reference repositories inside budget; snapshot tests pass. |
| M2 | **Engine, L3–L4 and diff.** Rust analyser, incremental re-index, architectural diff, findings. | `cli diff <base> <head>` produces correct findings for a fixture suite of at least 30 hand-written changes (added dependency, new cycle, moved type, surface change, contained change, rename, and so on). |
| M3 | **App shell and map.** Window, review list, map with diff overlay, drill-down to L4, command palette. | A reviewer can open this repository, pick two commits and drill from L1 to a code diff, with every interaction inside budget. |
| M4 | **Review workflow.** Findings view, review state, notes, persistence, keyboard completeness, accessibility pass. | The agent reviews one of its own pull requests end to end in the app using only the keyboard, and attaches the exported review to that PR. |
| M5 | **Breadth and release.** TypeScript, C#, Python and Go analysers; signed installers and auto-update for all three platforms; large-repository budgets met. | Installers produced by CI; all section 4 budgets green on the large repository. |
| M6 | **Towards the IDE.** Live model of the working tree (not only commits), editing at L4 with the model updating as you type, navigation by architecture. | Scope to be written as its own requirements document when M5 closes. Draft that document and label it `needs-owner`. |

---

## 7. Operating rules for the agent

### 7.1 Keep going

**Do not ask whether to continue. Continue.** The owner is not watching the
session. Finishing a task is the signal to start the next one, not to report
and wait. A session ends only when one of these is true:

- every open issue for the current and next milestone is closed or labelled
  `needs-owner`, or
- the session's budget (time, tokens or context) is nearly spent, in which case
  finish or cleanly park the task in hand and write the handoff (section 7.5).

Never end a turn with a question such as "shall I proceed?" or "would you like
me to…?". Permission to continue is always granted.

### 7.1a Ask when less than 70% sure

Asking for permission and asking for information are different things. The
first is banned; the second is required whenever confidence is low.

Before acting on a decision, estimate how likely it is that the owner would
agree with the choice if asked.

- **70% or more:** decide, record why, and move on.
- **Less than 70%:** ask. Do not guess, and do not build on the guess.

This applies to what the owner wants (product behaviour, scope, priorities,
taste), not to facts the agent can establish itself. A technical unknown is
resolved by reading documentation, measuring or running a spike, not by asking.

How to ask:

- If the owner is present in the session, ask directly, one decision per
  question, with the options and a recommendation.
- If the session is unattended, open a `needs-owner` issue with the question,
  the options, a recommendation and the stated confidence.
- Either way, **asking never stops the session.** Carry on with work that does
  not depend on the answer. Do not start work that does.
- Batch related questions. Do not ask what this document, an issue or a
  decision record already answers.

### 7.2 The work loop

Repeat until a stop condition in 7.1:

1. **Orient.** Read this document, the newest handoff comment on the tracking
   issue, and the open issues for the current milestone.
2. **Pick** the highest-priority `ready` issue in the current milestone. If
   there is none, write the issues for the next slice of the milestone first.
3. **Slice** it so it fits one pull request that can be reviewed in under
   400 changed lines, excluding fixtures and generated files.
4. **Test first.** Write the failing test or benchmark that defines done.
5. **Build** the smallest change that passes.
6. **Verify** against the definition of done (section 9). Run everything
   yourself and read the output.
7. **Review.** Run an adversarial review of the diff, briefed to break it, and
   fix what holds up.
8. **Ship.** Commit, push, open the PR, respond to every comment, merge when CI
   is green and the merge rule in 8.2 allows it.
9. **Close** the issue with a link to the PR and the measured numbers. Go to 2.

### 7.3 When blocked

A blocker on one task is never a reason to stop the session.

- **Ordinary obstacle** (failing build, flaky test, missing tool, unclear API):
  diagnose and solve it. Read the documentation. Try a second approach.
- **Same failure after three distinct attempts:** stop that task. Write what
  was tried and what was learned on the issue, label it `blocked`, and pick the
  next issue.
- **Needs the owner** (section 8.1, or confidence below 70% under 7.1a): label
  it `needs-owner` with a one-paragraph question and a recommended answer, then
  pick the next issue that does not depend on it.

### 7.4 Tracking

GitHub Issues are the system of record. Create these labels in M0:
`type:{bug,feature,task,epic,chore,decision}`, `priority:{1,2,3}`, `ready`,
`blocked`, `in-progress` and `needs-owner`. One epic per
milestone, with tasks as native sub-issues. Do not keep task lists in markdown
files.

### 7.5 Handoff

Sessions do not share memory, so the repository and the tracker must carry all
state. Keep one pinned issue, **"Blueprint: agent log"**. At the end of every
session add one comment to it with: what merged (PR links), what is in flight
and on which branch, what is blocked or awaiting the owner, the current
benchmark numbers against budget, and the first thing the next session should
do. Write it for a reader who has seen nothing else.

---

## 8. Decisions and authority

### 8.1 What the agent decides and what the owner decides

**Decide alone, and record it** (when at least 70% sure the owner would agree;
otherwise ask, per 7.1a): anything inside the engine and UI
implementation; library choices that meet 5.2 and 5.3; data structures;
algorithms; layout; visual design within section 5.5; test strategy; issue
breakdown and ordering within a milestone.

**Stop that task and label `needs-owner`:**

- changing anything in sections 1–4 of this document (product definition,
  principles, the model's levels, or a budget)
- anything that costs money or needs a credential, account, certificate or
  signing identity
- publishing outside the repository: releases, package registries, app stores,
  websites, announcements
- a new dependency whose licence is not MIT, Apache-2.0, BSD, ISC, Zlib or
  MPL-2.0. The owner has also allowed Unicode-3.0 and CC0-1.0, and NCSA
  for developer tooling such as fuzzing (#33, 2026-11-17 and 2026-11-18)
- sending source code or repository contents to any external service
- anything destructive or hard to reverse: deleting branches or history, force
  pushing a shared branch, rewriting the index format after a release without a
  migration

### 8.2 Repository rules

- Blueprint has its own repository, with the Cargo workspace at the root and
  this document at `docs/REQUIREMENTS.md`. Work only in that repository.
- All work happens on a branch and reaches the default branch through a pull
  request. Never push to the default branch directly.
- **Merging:** the agent may merge its own PR when CI is green on all three
  platforms, the adversarial review is done and every comment is answered. It
  may not merge a PR that changes a budget or this document; those wait for
  the owner.
- Never skip, ignore or delete a test, or lower a budget, to get green.

### 8.3 Decision records

Any choice a future session might reasonably question gets a short record in
`docs/decisions/NNNN-title.md`: context, options considered, the
decision, what was measured, and what would cause it to be revisited. Keep each
under one page. A decision is changed by adding a new record that supersedes
the old one, never by editing history.

---

## 9. Definition of done

### 9.1 For every change

- `cargo fmt --check`, `cargo clippy -- -D warnings` and the full test suite
  pass on macOS, Windows and Linux in CI.
- New behaviour has tests that fail without the change.
- No benchmark regresses by more than 5% and no budget in section 4 is exceeded.
  The PR description states the before and after numbers for anything it could
  affect.
- Public items are documented. User-visible behaviour is reflected in
  `docs/`.
- The adversarial review is done and its findings are fixed or answered.

### 9.2 Test strategy

- **Model and engine:** unit tests, plus snapshot tests of `cli` JSON output
  against fixture repositories. Fixtures are small, hand-written and committed.
- **Diff correctness:** each fixture change states its expected findings. This
  suite is the product's specification; grow it with every bug.
- **Analysers:** property tests and fuzzing. An analyser must never panic on
  any input.
- **UI:** view-state logic is tested headlessly through `protocol`. Rendering
  is covered by screenshot tests run under a virtual display in CI.
- **Self-hosting:** from M3, the app must open and review this repository, and
  this is checked in CI through the CLI.

### 9.3 Benchmarks

`bench/` holds a benchmark for every row in section 4 that can be measured
headlessly, run in CI on a fixed runner class and compared against both the
budget and the last result on the default branch. Frame-time budgets are measured with
an instrumented build that records input-to-present latency. Results are
published to the agent log every session.

### 9.4 What cannot be verified in a container

The agent usually runs on headless Linux. It cannot see a macOS or Windows
window, feel latency, or judge visual quality. Therefore:

- rely on CI for the other platforms and on instrumentation for latency
- attach screenshots from the virtual display to any PR that changes rendering
- say plainly in the PR what was verified by measurement and what was not. Do
  not claim something "feels fast" or "looks right"; report the number or the
  screenshot.
- collect anything that needs human eyes into one `needs-owner` issue per
  milestone, titled "M*n*: owner checks", instead of interrupting per item

---

## 10. Standards for agent conduct

- Report faithfully. A failing test is reported as failing, with its output. A
  skipped step is reported as skipped.
- Prefer the boring solution. Novelty is spent on the architecture model and on
  speed, nowhere else.
- Leave the tree working. The default branch builds and passes at every commit.
- Use sub-agents for independent, parallel work and for the adversarial review;
  keep design decisions in the main session.
- Do not write planning or status documents into the repository. Plans go in
  issues, decisions in decision records, state in the agent log.

---

## 11. Session prompt

Start every session, or every scheduled run, with this prompt. It is the same
for the first session and the last.

```
You are the sole developer of Blueprint, a native code review app that presents
a change as a change to the system's architecture. Nobody is watching this
session, and no later session will remember it, so the repository and the issue
tracker have to carry everything.

Your standing brief is docs/REQUIREMENTS.md. Read it in full before anything
else. It defines the product, the performance budgets, how you work, what you
may decide alone, and what done means. Follow it over your own preferences.

Goal: bring Blueprint to the point where milestones M0 to M5 in section 6 pass
their exit tests in CI on macOS, Windows and Linux, and the M6 requirements
draft is written. The project is finished only then.

This session:

1. Orient. Read the newest comment on the "Blueprint: agent log" issue and the
   open issues for the current milestone. Check that the default branch builds
   and passes; if it does not, fixing that comes first. If the log issue does
   not exist, this is the first session: create the labels, the log issue, one
   epic per milestone and the M0 task issues, then start M0.
2. Work the loop in section 7.2 continuously. When a task is done, start the
   next. When a milestone's exit test passes, start the next milestone. Do not
   stop to report progress and do not ask whether to continue.
3. When a task is blocked, park it as section 7.3 describes and take another.
4. When you are less than 70% sure what the owner would want, ask through a
   needs-owner issue as section 7.1a describes, and keep working on whatever
   does not depend on the answer. Before starting work, check whether earlier
   needs-owner issues have been answered, and act on the answers.
5. Verify everything yourself against section 9 before calling it done, and
   report what you measured. A claim without a test run, a benchmark number or
   a screenshot behind it does not count.
6. Stop only on the conditions in section 7.1. Before stopping, write the
   handoff comment described in section 7.5, so the next session can start
   from it alone.

If every milestone is complete, say so in the agent log with the evidence for
each exit test, and make no further changes.
```

For unattended development, run this as a scheduled task that starts a fresh
session in the Blueprint repository each time, so that work resumes from the
agent log without anyone present.
