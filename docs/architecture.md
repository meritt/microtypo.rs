# Architecture

## Invariant

For any input and configuration, the engine returns the reference implementation's output, `changes`, `edits`, `trace` and errors (`name`, `message`, `code`, `details`) byte for byte. Text and coordinates are UTF-16 code units. `testdata/conformance/corpus.jsonl` holds the reference behaviour as cases and `corpus.sha256` pins its source commit and digest; the core and the npm package replay it.

Known divergences:

- A lone surrogate in the `message` or `details` of an error is rendered as U+FFFD.
- `maxProcessingMs` also holds inside a regex search, so a stage the reference runs past the budget and only then reports ends here as soon as the budget is spent, with the same error.
- Under `u` or `v`, a search from an index between the halves of a surrogate pair starts at the pair, as in V8 (`/./uy` from 2 in `a😀b` matches 1..3), but never tries the index itself: `/\B/uy` and `/(?<!a)/uy` from 2 and `/\B/gu` find an empty match at 2 in V8 and none here. regress reads the low half as a lone surrogate, so an attempt there would accept matches V8 rejects (`/(?<=[^a])/uy` from 2). No built-in expression in the corpus needs a match between the halves.

## Layout

```mermaid
flowchart LR
  core[microtypo-core] --> regress[vendor/regress]
  ffi[microtypo-ffi] --> core
  wasm[microtypo-wasm] --> core
  conformance[microtypo-conformance] --> core
  swift[packages/swift] --> ffi
  npm[packages/npm] --> wasm
  xtask -.->|cbindgen| ffi
```

| Part | Role |
|---|---|
| `crates/microtypo-core` | engine: rules, input formats, markup protection, budgets; `forbid(unsafe_code)` |
| `crates/microtypo-ffi` | C ABI, header in `include/microtypo.h`; the only workspace crate with `unsafe` |
| `crates/microtypo-wasm` | `wasm-bindgen` boundary |
| `crates/microtypo-conformance` | replays corpus cases against the core for the conformance test |
| `packages/npm` | `@meritt/microtypo-wasm`: JavaScript API over the module |
| `packages/swift`, `Package.swift` | Swift package `MicroTypo` over the XCFramework |
| `vendor/regress` | ECMAScript regex engine over UTF-16, prefilter |
| `xtask` | header, builds, release set, corpus copy |

Bindings depend on no workspace crate but the core. Core tests are `tests/behaviour` (library behaviour), `tests/growth` (linear scaling and timing) and `tests/conformance.rs`.

## Core

- Names and option spellings follow the reference, hence the relaxed naming and size lints in `Cargo.toml`. Inputs below the limits keep every `usize`/`u32`/`f64` cast exact, hence the allowed `cast_*` lints.
- Text is `JsString` (UTF-16 units); lone surrogates survive everywhere, including rule-group names, rule ids, trace names and cycle-hook ids, which have one canonical UTF-16 form that metadata JSON quotes without replacement.
- `js` reproduces the JavaScript semantics the engine relies on: ECMAScript regexes, `JSON.parse`/`JSON.stringify` with V8 error messages, insertion-ordered config objects.
- One `Error`; `ErrorKind` is `Config`, `Input`, `Budget` or `Type` (`MicroTypoConfigError`, `MicroTypoInputError`, `MicroTypoBudgetError`, `TypeError`). Bindings map it without changing fields.
- Limits: `maxInputLength` (default 30 000 units) before processing; `maxProcessingMs` at the reference's check points (between stages, after each rule, pre-parse hook and cycle iteration, after restoration) and inside regex searches; `MAX_ENTRIES` (100 000) per vault. The interpreter reads the clock once per 65 536 backtracking steps; past the budget the search fails and its later attempts fail, and the next stage check raises `MicroTypoBudgetError` with that stage's label. The sequence scanners in `protect/sequences.rs` check the same budget state every 65 536 units.
- The core performs no I/O. Its two effects, the clock and the random bytes, come from the `Host` each constructor takes: two plain functions, so the regex budget can keep the clock in a thread-local. The final binary picks the `getrandom` backend and the allocator. Each binding tests that a catastrophic input ends with `MicroTypoBudgetError`.
- Placeholders of vaulted URLs, addresses, tokens and tags carry a nonce from the host's CSPRNG, drawn from a 256-byte pool filled on the first `process` call that needs one, never at construction. A host without entropy panics rather than mint a guessable nonce. Placeholder-shaped text is stripped on entry, and a custom group's output holds no placeholder token more often than its input.
- A builtin rule declares the characters (`requires`, `requiresAny`) and expression (`requiresPattern`) its text must contain, as `lib/rule-features.js`. While every registered group is builtin and no cycle hook is set, the runner skips a rule whose text lacks them, and a structured value skips the pipeline as `canSkipValue` decides (after one budget check labelled `data-value`). The skip is observable: a rule that runs on a run with nothing to lift still settles the lifting and records a trace event, which moves later trace ranges.
- The character mask of a text is reused only while the runner knows the text is unchanged; after a handler it is recomputed or matched by content, since a handler may rewrite its buffer in place.
- A pass whose replacements all equal their matches returns its input and builds no text; replaced text goes back to a per-thread spare buffer (`js::regex::recycle`). Because buffers are recycled, a cache keyed by buffer address (the no-wrap context, the quote session) is valid only until the next pass of a rule.
- State the reference keeps per run in module scope (boundary activity, quote padding, space collapse, quote paragraphs, inline tag names) lives in `Ctx::bound`, set by the engine and the runner.
- Attribute values (`input.attributes`) and HTML `<title>` bodies are typeset by worker engines: format `plain`, or `text` for a title body holding a tag or comment. A worker is built on first use, kept between calls with per-call state reset, rebuilt when `maxProcessingMs` changes, and takes the outer engine's groups, order, settings and cycle hook on each use; vaults read so far are passed in so their tokens stay inherited.
- With `presets: false` nothing compiles the built-in groups; Miri depends on it.

## Public surface

- Modules are private; the crate root re-exports what a Rust caller and the bindings use, and `cargo doc` shows only that. The surface is checked by `cargo semver-checks` in CI.
- `process(&str)` returns `Cow<str>`, borrowing the input when no rule changes it; `process_utf16` takes and returns UTF-16 for callers that hold it. `process_with_options` takes the reference `process` options object and returns `Output::Text` or `Output::Diagnostics`.
- Diagnostics (`changes`, `edits`, `trace`) are selected independently. Source maps ride on the existing restore owners, inline positions and seal/unseal boundaries; nonce text is never diffed against authored text; rejected attempts and structured-value rollbacks discard their events. Processing without diagnostics allocates no maps.
- `from_json` and `call_json` read JSON through `RawJson`, which keeps original string units (escaped lone surrogates) beside the structure serde validates; names, pattern sources, replacements and selectors come from the raw side. The CLI merges configuration through the same reader. There is no second JSON or regex parser.
- Data rule groups (`pattern: { source, flags }`, string replacement) compile through the same `Re` and run under the same budgets as native rules.
- `Config` reading runs the engine's validation, so a bad config fails with the same `MicroTypoConfigError`.
- The conformance replay, the tests, the parity guard and the benchmark reach internals through one `#[doc(hidden)] internals` module; helpers that rules use (`Re::new`, `Caps::s`, `lit!`, `cat!`) are hidden the same way. A cargo feature would not hide them: features unify across the workspace, and `cargo doc --workspace` would still show them.

## Regular expressions

- `Re` constructors and the matcher generator enter the regress parser with UTF-16 source: legacy atoms are units, `u`/`v` atoms are code points, named groups always use the Unicode identifier grammar.
- `v` follows the pinned Node.js V8, including its quirks: under `i` a one-code-point `\q{}` string keeps only its folded code point, while longer strings match ignoring case.
- regress reads a prefilter off every compiled expression, including those built at run time: start, second-unit, preceding-unit and required unit sets, required strings, and runs a match must lie in. Every check is a necessary condition, so it skips positions without changing the match found; a construct the analysis does not understand widens its set to every unit, which turns that check off. Restricting a search to runs narrows the starts only: the matcher always gets the whole input, since a lookbehind can read before a run.
- In a debug build a search the prefilter rules out runs anyway and panics on a match, and a search kept to runs is repeated over the rest of the input and must agree. The conformance tests run that way.

## C ABI

- Text crosses as UTF-16 plus length; configuration and call arguments as UTF-8 JSON shaped like the JavaScript objects.
- A failing call returns `false` or `NULL` and, when `error` is not `NULL`, stores a `microtypo_error` there, freed with `microtypo_error_free`.
- Panics are caught and returned as `InternalError`, as is `NULL` for the engine or the method name. Other pointers follow each function's `Safety` contract.
- An engine is not thread-safe.
- Every `unsafe` block carries a `SAFETY` note; tests run under Miri without the `mimalloc` feature, which Miri cannot execute.
- `mimalloc` is on by default.

## WebAssembly package

- `cargo xtask wasm` builds with the `wasm` profile (`opt-level = "z"`), runs `wasm-bindgen --target web`, renames the glue's default module URL to `microtypo.wasm` so bundlers following `new URL(…, import.meta.url)` find it, and runs `wasm-opt -Os --strip-debug --strip-producers` (`-Oz` comes out larger). `dist` fails above 750 000 bytes of `gzip -9` (`WASM_GZIP_BUDGET`).
- `microtypo.js` exports only `load`. The glue holds one instance per realm: the first successful `load()` instantiates and later calls return the same API without reading their argument; a failed load is not kept. A module reporting another version stays in the glue, so every later `load()` in that realm rejects with `ERR_MICROTYPO_VERSION`. The export check before `initSync` keeps a module from another build out of the glue.
- `load()` calls `initSync` with a compiled module, never the streaming init; a `file:` URL is read with `node:fs`. `microtypo.js` targets Baseline Newly available and has no capability checks.
- `onCycleLimit` runs after the pass, since JavaScript called mid-pass could reenter the engine. The cycle hook is set only for a call made while `onCycleLimit` is a function, as the reference reads it at the start of `process`: a set hook turns off the requirements skip, which moves trace ranges.
- The JavaScript layer checks only what JavaScript values show (`undefined`, functions, symbols, `RegExp`) and passes the rest as JSON; rule groups with callbacks or `RegExp` patterns are rejected. Results keep their original UTF-16 units.
- `packages/npm/pnpm-workspace.yaml` sets `minimumReleaseAge` and `trustPolicy`; `dist` copies it into the consumer that installs the packed tarball.

## Swift package

`Package.swift` sits at the repository root. A checkout with `packages/swift/MicroTypoFFI.xcframework` uses it; otherwise SwiftPM downloads the release XCFramework by the URL and checksum `cargo xtask pin` writes from the release set CI built. Headers live in a module-named folder so `module.modulemap` cannot collide with another XCFramework. `MicroTypo` serialises calls with `NSLock`, so one instance is shareable across threads. It checks values before `JSONSerialization`, which raises an uncatchable Objective-C exception on NaN, infinity or a non-JSON type. Deployment target: macOS 26.

## Reproducibility

- Pins: Rust in `rust-toolchain.toml`; the Miri nightly in CI and `AGENTS.md`; Node.js and pnpm in `devEngines` of `packages/npm/package.json` (read by `pnpm/setup`), pnpm also in `pnpm-lock.yaml`; `wasm-bindgen` exactly in `Cargo.toml`, its CLI version from `Cargo.lock`; binaryen 133, checked by `cargo xtask wasm`.
- CI installs cargo-deny, gitleaks, cargo-semver-checks, binaryen and `wasm-bindgen` as release binaries checked against a SHA-256 in the workflow; a `wasm-bindgen` bump needs the digest of its CLI.
- The XCFramework is built with `SOURCE_DATE_EPOCH` = 1980-01-01 (mimalloc embeds `__DATE__`/`__TIME__`) and zipped with sorted entries, fixed times and modes, and no extra fields, so two builds of one commit yield identical `SHA256SUMS`.
- `cargo-deny` checks licences, sources and advisories against `deny.toml`; Dependabot proposes action and crate updates weekly.
