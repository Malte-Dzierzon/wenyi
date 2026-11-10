# Wenyi — Rust Engineering Rules

> **Purpose:** persistent engineering instructions for every Rust slice. Optimize for correctness, low memory use, low latency, small dependency surface, and low conceptual complexity. This is a project-specific adaptation of Google's Argon migration workflow, Rust's own LLM guidance, the Rust Performance Book, Tokio and SQLite documentation. It is not a generic mandate to maximize cleverness or minimize line count.

## 1. Engineering objective

Build one cohesive native application: a thin native GPUI frontend over a small, well-engineered Rust core. The core should own translation, document processing, glossary, review, jobs and persistence. Keep UI, OS integration, model-provider I/O and storage at explicit boundaries.

**Optimize for the smallest understandable implementation that satisfies the real requirements.** Fewer lines are not automatically better. Avoid duplicated logic, speculative abstractions, unnecessary crates, compatibility layers, wrappers that only forward calls, and frameworks used for problems already solved by the standard library or existing dependencies.

## 2. Mandatory workflow for every slice

1. **Inspect before changing.** Read the current implementation, callers, data types, tests, `PLAN.md`, and relevant architecture notes. Trace the complete data and state flow; do not design from an isolated file or from assumptions about the Python oracle.
2. **Establish a baseline.** Run the relevant existing tests, `cargo fmt --check`, and `cargo clippy`. For performance-sensitive work, record a repeatable baseline before optimizing.
3. **Choose one small vertical slice.** State its contract, invariants, dependencies, failure modes, and acceptance tests before implementation. Do not mix unrelated refactors into it.
4. **Implement the simplest correct design.** Reuse existing domain types and helpers where they fit. Keep one source of truth for each rule and each piece of state.
5. **Verify continuously.** Add a failing regression test for a bug before fixing it. Run tests and Clippy after the change; diagnose root causes instead of suppressing warnings or changing expected outputs blindly. Bound repair loops (for example, five focused iterations), then report unresolved failures honestly.
6. **Review the whole diff.** Remove dead code, debug output, accidental clones, redundant conversions, unused features and unrelated formatting. Check that the new code is easier to understand than the old code.
7. **Update `PLAN.md`.** Record only verified completion, test results, measured performance changes, known gaps and the next dependency-safe slice.

Do not start another slice while the current slice has unverified behavior or undocumented deviations from the reference implementation.

## 3. Safe, idiomatic Rust

- Put `#![forbid(unsafe_code)]` at the Rust core crate root. Do not introduce `unsafe`, raw pointers, unchecked indexing or custom memory management for speculative speed. If a future requirement genuinely cannot be met safely, isolate and justify it separately; never use `unsafe` merely to avoid a bounds check.
- Prefer ownership and borrowing that express the real lifetime. Use `&str`, `&[T]`, iterators and borrowed views for read-only data. Do not turn every value into `String`, `Vec<T>`, `Arc<T>` or a cloned owned DTO without a reason.
- Treat `.clone()` as a design decision, not a default escape hatch. Cloning a `String`, `Vec` or nested document can allocate and copy substantial data. Remove clones in repeated paths when borrowing or moving is clearer. Do not contort APIs to eliminate cheap clones outside measured hot paths.
- Avoid hidden global mutable state. Pass explicit, narrow context/dependencies. Use shared ownership only when multiple tasks genuinely need the same immutable data or coordinated state.
- Use domain types and enums for meaningful states. Represent pending, empty, completed, cancelled and failed distinctly; do not collapse them into booleans or string conventions.
- Avoid `unwrap()` / `expect()` in production paths. Return typed errors, preserve useful context, and handle expected external failures at the application boundary. Panics are acceptable in tests or for proven impossible construction invariants with a clear explanation.
- Public APIs need concise rustdoc describing purpose, invariants, inputs and errors. Private comments should explain *why* a non-obvious decision exists, not narrate the code.

## 4. Small architecture and dependency discipline

- Keep a **modular monolith**, not a microservice architecture. Use the existing crate boundaries; do not create a crate for every module or introduce generic plugin/DI systems without a concrete second use case.
- Preserve one-way dependency flow: UI/app → engine/domain → document, LLM and storage interfaces. Domain logic must not depend on GPUI widgets or desktop shell details. Keep provider protocols, SQLite operations and filesystem/document-format I/O behind narrow modules.
- A module should have one clear responsibility and a small public surface. Roughly 300–400 lines is a **review signal**, not a hard limit: split cohesive responsibilities, never split files mechanically just to hit a number. Large files should be decomposed only when the resulting boundaries improve comprehension and testing.
- **New dependency gate:** before adding a crate, explain (a) the concrete problem, (b) why `std` or an existing dependency is insufficient, (c) transitive dependency and compile-time cost, (d) license/MSRV/maintenance risk, (e) enabled features, and (f) expected measurable benefit. Prefer reusing an existing dependency. Disable default features when safe and enable only the required features. Do not add a crate for a one-line helper.
- Avoid duplicate representations and conversions across frontend/backend. Define stable request/response and domain contracts once, then adapt at the boundary.

## 5. Performance and memory rules

**Measure first; optimize the measured bottleneck.** Rust is not a reason to add clever code pre-emptively.

- Establish representative workloads and record wall time, throughput, peak RSS and (where practical) allocation counts. Compare release builds on the same machine, corpus, configuration and warm/cold conditions.
- Benchmark Wenyi-specific hot paths separately: import/parse, segmentation/token counting, glossary matching and occurrence search, SQLite reads/writes, chapter translation preparation, review evidence lookup, and EPUB assembly/export. Keep external model/network latency separate from local engine measurements.
- Avoid repeated work: normalize/index source text once where possible; update indexes incrementally; cache only with clear invalidation rules and bounded size; do not rescan every chapter for every glossary term or rebuild large derived structures on every UI refresh.
- Process large books incrementally. Prefer chapter/page/segment streaming and bounded working sets over loading the entire book, all translations, all review evidence and every prompt into memory simultaneously. Keep prompt context bounded and task-specific.
- Avoid unnecessary heap work in hot loops: repeated `format!`, `String` concatenation, per-item temporary `Vec`s, repeated regex construction, repeated normalization and repeated hashing. Reuse buffers and pre-size collections when the expected size is known. Do not build complex buffer pools unless profiling proves they help.
- Prefer contiguous data and cache-friendly access when the workload benefits. Do **not** impose Struct-of-Arrays, SIMD, custom allocators or arenas on text-oriented code by default. Consider them only for a proven CPU-bound kernel, benchmark before/after, and retain the simplest version that wins.
- Avoid excessive task spawning. One task per segment, term or tiny operation is not acceptable. Use a small bounded worker pool, a semaphore or bounded queue, explicit concurrency limits and backpressure.
- Share one long-lived HTTP client per provider/runtime rather than constructing a new client for each request. Set sensible connect/request timeouts, cancellation behavior and response-size limits. Keep retry policy in one place; never stack provider retries with engine retries.
- Do not hold a mutex, database transaction or project write lock while awaiting a network/model request. Keep critical sections short and make lock ownership/scopes explicit.

## 6. Tokio, workers and SQLite

- Bound every queue and every class of concurrent work. Queue capacity and worker counts must be explicit and configurable only where users benefit. Backpressure is preferable to unbounded memory growth.
- Give tasks clear ownership and shutdown behavior. Track task handles; propagate cancellation at safe checkpoints; do not leave detached tasks mutating project state after a run ends. Preserve stable result ordering when parallel work is merged.
- Use `spawn_blocking` only for genuinely blocking or CPU-heavy synchronous work that would otherwise stall the async runtime (for example, expensive archive parsing). Do not wrap every small function in a task.
- SQLite WAL allows readers to proceed alongside a writer, but SQLite still serializes writes: design around **one writer at a time**. Batch related writes in short transactions, use prepared statements and indexes for demonstrated query patterns, and avoid N+1 lookups.
- Never keep a SQLite transaction open across `.await`. Do not use WAL over a network filesystem. Treat database busy/locked states as explicit operational errors, not as reasons for uncontrolled retries.
- Persist job intent, checkpoints and usage accounting so a crash or retry cannot duplicate completed work or billing. Resume must be idempotent. A job may stop only at a documented safe checkpoint.

## 7. Wenyi-specific correctness invariants

- The Python implementation remains the behavioral oracle until its corresponding Rust slice has passed parity and quality checks. Do not delete or silently reinterpret reference behavior.
- Use exact golden parity for deterministic behavior: parsing, segment identities, glossary matching, document structure, styles, annotations, export modes, state transitions and usage-ledger semantics. Compare structured output, not just “looks similar”.
- Model-generated prose is not deterministic. Test agent plumbing with a scripted fake provider; assess translation quality separately using a fixed public-domain corpus, repeated model/config records and human review. Do not claim exact LLM output parity.
- Preserve EPUB package structure and all untouched resources. Test TOC/NAV/NCX, spine, anchors, footnotes, annotation contexts, inline markup, bilingual output and CJK titles. Test malformed and unusual but valid input encodings explicitly; never silently replace source text with lossy decoding when a correct decoder or clear failure path is available.
- Glossary changes must be evidence-backed. Preserve established translations; record conflicting proposals rather than silently overwriting confirmed terms. Search/index results must retain chapter and segment identity and be invalidated when source text changes.
- Style guidance must be versioned and updated deliberately as more chapters provide evidence. Preserve user edits separately from generated observations; never overwrite a confirmed user choice with an automatic refresh. Translation runs should consume a stable style snapshot for each batch/run, with safe refresh checkpoints.
- Review must remain read-only until an explicit, recoverable publish operation applies approved fixes. Keep shadow results separate from formal chapter targets.
- Keep stable project, book, chapter, segment and annotation identities. Preserve the distinction between `None` (pending) and an intentionally empty translation where the format permits it.
- Large-book behavior is a first-class requirement: bounded memory, incremental indexing, paged UI reads, responsive cancellation and resumability must be tested on a large synthetic corpus as well as small fixtures.

## 8. Verification and performance gates

Run the checks that apply to the changed slice; do not claim unrelated suites passed.

- Formatting: `cargo fmt --all -- --check`
- Lints: `cargo clippy --workspace --all-targets --all-features -- -D warnings` (adapt feature selection to the actual workspace; do not enable all `pedantic`/`nursery`/`restriction` lints globally just to create noise)
- Tests: relevant crate tests, integration tests, doc tests and the full workspace suite before a slice is declared complete.
- Add unit tests for pure logic; integration tests for state and format boundaries; regression tests for every fixed bug. Use property/fuzz tests for parsers, Unicode normalization, segmentation and state-machine edge cases when they provide real coverage.
- Test failure, cancellation, crash/restart, repeated resume, concurrent chapter work, duplicate job admission, DB contention, glossary conflicts and interrupted export. A green happy path is not enough.
- Differential tests against the Python oracle should cover representative fixtures and boundary cases. Never “bless” a changed golden output without examining and explaining the semantic difference.
- Record before/after performance for a performance claim. Use Criterion or an existing benchmark harness where appropriate; for whole-app behavior use a repeatable end-to-end harness. Include corpus size, hardware, build mode, variance and peak memory. If no measurable improvement exists, do not call the change an optimization.
- Use release-profile experiments (`opt-level`, thin/fat LTO, codegen units, size-oriented profiles) only after functional parity. Cargo settings can trade runtime speed, binary size and compile time; benchmark the actual target. Do not force `target-cpu=native` in a portable release build.
- Apply LLVM PGO only after the architecture and hot paths stabilize, and only if representative profiles show meaningful CPU-bound work. Re-run correctness/parity tests and benchmarks on the PGO build. Do not add a PGO pipeline to every development build.
- No arbitrary universal 95% coverage threshold. Aim for strong coverage of real behavior, especially branches, invariants, failure states and persistence transitions. Coverage numbers do not replace meaningful tests.

## 9. Definition of done for a slice

A slice is complete only when:

- The intended contract and invariants are implemented and documented.
- The relevant tests, formatting and Clippy checks pass with no new unexplained warnings.
- Oracle parity is demonstrated wherever deterministic behavior is being ported; model-quality claims are evaluated separately.
- Memory/concurrency bounds and cancellation/recovery behavior are verified where relevant.
- Any claimed performance or size improvement has reproducible measurements.
- No unnecessary dependencies, dead code, debug scaffolding or unrelated changes remain.
- `PLAN.md` records what was actually done, the evidence, limitations and next dependency.

At the end, report changed modules, commands actually run, benchmark methodology/results, remaining risks and checks not run. Never invent successful tests, measurements or coverage.

---

## Reference material

These are source references, not a requirement to import Google's full migration tooling or copy its numerical-solver-specific constraints.

1. **Google Antigravity — Gemini 4 Argon Fortran→Rust case study** (2026-11-03): safe Rust, allocation control, architectural blueprint, parity loop and measured PGO. https://antigravity.google/blog/modernizing-40-year-old-scientific-code-with-gemini-4-argon
2. **Google Skills — `migrate-to-rust`**: reusable migration skill and guardrail structure. https://github.com/google/skills/tree/main/skills/developers/migrate-to-rust
3. **Rust Project — Writing LLM-created code**: small scoped changes, tests, dependency discipline, avoid unnecessary abstractions, review full diffs. https://rust.googlesource.com/rust/+/refs/heads/main/src/doc/rustc-dev-guide/src/llm-guidance/writing.md
4. **The Rust Performance Book**: profiling, allocations, memory, binary size and compile-time optimization. https://nnethercote.github.io/perf-book/
5. **Rust API Guidelines**: predictable, type-safe, maintainable Rust APIs. https://rust-lang.github.io/api-guidelines/
6. **Tokio — Channels and backpressure**: bounded queues and explicit concurrency. https://tokio.rs/tokio/tutorial/channels
7. **SQLite — WAL**: reader/writer behavior and WAL limitations. https://www.sqlite.org/wal.html
8. **SQLite — Isolation**: serialized writes and transaction semantics. https://www.sqlite.org/isolation.html
9. **Cargo — Profiles**: trade-offs among optimization level, LTO, code size and build time. https://doc.rust-lang.org/cargo/reference/profiles.html
10. **Clippy documentation**: lint categories and why pedantic/restriction groups should be selected deliberately. https://doc.rust-lang.org/clippy/
11. **Microsoft Rust Guidelines — hot paths**: profile first; watch repeated allocation, string cloning and hashing. https://github.com/microsoft/rust-guidelines/blob/main/src/guidelines/performance/M-HOTPATH.md
12. **rustc — Profile-guided optimization**: official LLVM PGO workflow and limitations. https://doc.rust-lang.org/rustc/profile-guided-optimization.html
