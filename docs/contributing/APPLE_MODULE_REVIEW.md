# Code Review Guide: `src/cmds/apple/`

This guide is for reviewers who need to evaluate the Apple platform filter module. It assumes you know Rust but may not know the RTK codebase well. Read the sections in order; cross-references use `file:line` notation.

---

## 1. Architecture Overview

### How RTK's proxy pattern works

Every filter module in `src/cmds/*/` follows the same skeleton:

1. Parse args in `main.rs`, construct a `std::process::Command`.
2. Call `runner::run_filtered(cmd, tool_name, args_display, filter_fn, opts)`.
3. `run_filtered` executes the command, captures stdout+stderr, merges them into a single `raw` string, passes it to `filter_fn`, then prints the filtered output.
4. Token counts (raw vs. filtered) are recorded in SQLite via `tracking::TimedExecution`.

The key function is `runner::run_filtered` (`src/core/runner.rs:56`). Its signature:

```rust
pub fn run_filtered<F>(
    mut cmd: Command,
    tool_name: &str,
    args_display: &str,
    filter_fn: F,
    opts: RunOptions<'_>,
) -> Result<i32>
where
    F: Fn(&str) -> String,
```

Important behavior to understand before reviewing:

- **stdout and stderr are merged** (`runner.rs:74`: `let raw = format!("{}\n{}", stdout, stderr);`). Most Apple tools write build progress to stderr and results to stdout — the filters see both interleaved. This is intentional but means a filter cannot distinguish which stream a line came from.
- **The filter never sees exit code.** It receives only the combined text. Exit code is propagated after filtering via `Ok(exit_code)` at `runner.rs:123`.
- **`RunOptions::with_tee(label)`** enables the tee system: on failure, raw output is saved to a temp file and a hint is printed so the user can access unfiltered output. All Apple filters use `with_tee` except `swift package`, which uses `RunOptions::default()` (no tee).

### Module wiring

`src/cmds/apple/mod.rs` is a single line:

```rust
automod::dir!(pub "src/cmds/apple");
```

This re-exports all `*_cmd.rs` files in the directory. `main.rs:10` imports them individually:

```rust
use cmds::apple::{simctl_cmd, swift_cmd, swiftlint_cmd, xcodebuild_cmd, xctrace_cmd};
```

Routing in `main.rs` dispatches to each module's `run()` function (`main.rs:1867-1887`). The `swift` command is a nested subcommand enum (`SwiftCommands`); the others are flat.

---

## 2. Per-File Walkthrough

### `swift_cmd.rs`

**What it does.** Handles four `swift` subcommands: `build`, `test`, `package`, `run`. Each has a dedicated `filter_*` function. The public entry point `run(cmd, args, verbose)` dispatches via `SwiftCommand` enum.

**Design decisions worth noting:**

- `run_passthrough` (`swift_cmd.rs:33`) handles any `swift` subcommand RTK does not recognize (e.g., `swift package update`, `swift repl`). It calls `runner::run_passthrough`, which streams output directly without filtering but still records metrics. Routing in `main.rs:1878` handles the `SwiftCommands::Other` arm.
- `filter_swift_build` collects compile step counters from `[N/M]` progress lines, aggregates all errors and warnings, then emits a compact summary. It does not emit each intermediate step — only the high/low watermarks (`max_step`, `total_steps`). This is intentional: the reviewer's question is "did it build and were there errors?" not "which files compiled."
- Warnings are capped at 10 in the output (`swift_cmd.rs:126`) with a `+N more` suffix. Errors are uncapped (all printed). Intentional asymmetry.
- `swift package` is the only subcommand that uses `RunOptions::default()` (no tee, `swift_cmd.rs:355`). This is because `swift package` has many subcommands (`describe`, `resolve`, `update`, `clean`) and their output formats differ widely. The filter only really understands `swift package describe` output — other subcommands fall through the state machine without matching any sections and produce a bare `Package:` line.

**Critical regex patterns:**

- `BUILD_PROGRESS_RE` (`swift_cmd.rs:60`): `^\[(\d+)/(\d+)\]` — matches any `[N/M]` line. This includes non-compilation steps like `Write sources` and `Copying Shaders.metal`. The filter uses these for progress counting, not for content filtering, so false positives only affect the "Compiled N/M steps" counter, not correctness.
- `BUILD_COMPLETE_RE` (`swift_cmd.rs:68`): `^Build (?:of product '.+' )?complete!` — note the optional `of product '...'` clause. Both `Build complete!` and `Build of product 'Foo' complete!` match.
- `BUILD_FAILED_RE` (`swift_cmd.rs:71`): `(?i)^build .*(failed|error)` — case-insensitive, broad. Intended to catch `Build FAILED` and `build failed with error`. Review whether this can false-positive on error message text like `Build task: describe the error`.
- `XCTEST_RESULT_RE` (`swift_cmd.rs:165`): expects the exact XCTest format `Test Case '-[Module.Suite testName]' passed (N.NNN seconds).` — the dot between module and suite is load-bearing; it captures `caps[2]` as the suite name and `caps[3]` as the test name.

---

### `swiftlint_cmd.rs`

**What it does.** Runs `swiftlint` (or any args passed to it), captures violation lines, groups them by rule ID, counts errors vs. warnings per rule, tracks which files each rule fired in, and emits a compact summary sorted by error severity then total count.

**Design decisions:**

- The `RuleStats` struct (`swiftlint_cmd.rs:55`) uses `HashSet<String>` for file tracking. This gives unique-file-per-rule counts without O(N^2) deduplication. The file paths stored in the `HashSet` are already shortened by `shorten_path` (`swiftlint_cmd.rs:75`), which keeps only the last two path components. This means two files with the same name in different parent directories would collide as the same key. For typical project layouts (unique filenames) this is not a problem.
- Sorting at `swiftlint_cmd.rs:126-131`: primary sort is `b.errors.cmp(&a.errors)` (errors first), secondary is total count descending. Rules with only warnings sort below any rule with at least one error, regardless of warning count. This is the desired UX but worth confirming with the author.
- Output is capped at 20 rules (`swiftlint_cmd.rs:133`). Projects with many distinct lint rules will truncate.
- Autocorrect mode (`CORRECTED_RE`, `swiftlint_cmd.rs:49`): if `swiftlint --fix` is run, violation lines are consumed before correction, so `by_rule` may end up empty. The output path at `swiftlint_cmd.rs:107-118` handles this: if `by_rule` is empty, it prints the corrected/summary line or "No violations".

**Critical regex:**

- `VIOLATION_RE` (`swiftlint_cmd.rs:39`): `^(.+?):(\d+):\d+:\s+(warning|error):\s+(.+?)\s+\((\w+)\)\s*$` — the rule ID is captured as group 5 via `(\w+)` immediately before end-of-line. The `\w+` will not match rules with hyphens. SwiftLint rule IDs use underscores (`line_length`, `force_cast`) not hyphens, so this is safe — but worth confirming if third-party rules are in scope.

---

### `xcodebuild_cmd.rs`

**What it does.** Filters `xcodebuild` output, which is extremely verbose: each compile step emits a multi-line block including the full `swift-frontend` invocation with dozens of flags. The filter keeps: compile step summary grouped by target, link steps, resolved packages, errors, warnings, test results, and the final `** BUILD SUCCEEDED/FAILED **` line.

**Design decisions:**

- `BTreeMap` (`xcodebuild_cmd.rs:148`) is used to group compiled files by target. This gives deterministic (alphabetical) output order, which matters for test assertions.
- Errors are capped at 20, warnings at 10 (`xcodebuild_cmd.rs:170,182`). Same asymmetry as `swift build`.
- Test results from `xcodebuild test` are handled via the same `XCTEST_RESULT_RE` regex as `swift_cmd.rs`. The output format is identical because both use XCTest.

**The `NOISE_RE` pattern** (`xcodebuild_cmd.rs:55-56`) is the most important line to scrutinize:

```rust
static ref NOISE_RE: Regex =
    Regex::new(r"^(?:CreateBuildDirectory|cd |/Applications/Xcode|Build description|note: |User defaults|Command line invocation|\s{4}/|Test Suite |Test Case .* started)").unwrap();
```

What it catches:

| Prefix | Catches |
|---|---|
| `CreateBuildDirectory` | Build directory creation steps |
| `cd ` | Directory change lines emitted before each tool invocation |
| `/Applications/Xcode` | Full paths to `swift-frontend`, `clang`, `ld`, etc. |
| `Build description` | Build system description block |
| `note: ` | Compiler notes (distinct from `warning:`) |
| `User defaults` | `xcodebuild` user defaults banner |
| `Command line invocation` | xcodebuild header line |
| `\s{4}/` | Four-space indented absolute paths (flags passed to compiler) |
| `Test Suite ` | XCTest suite start/end lines |
| `Test Case .* started` | Per-test start lines (passed/failed lines are kept separately) |

What it may miss or incorrectly suppress:

- Lines starting with `/Applications/Xcode` will catch the primary Xcode install path but miss custom Xcode installs at other locations (e.g., `/Volumes/Xcode15/Xcode.app`). Such lines would pass through as unrecognized content, falling through to the error/warning check — harmless but may add noise.
- `note: ` suppresses all compiler notes globally, including notes that accompany error messages (e.g., "note: did you mean..."). Reviewers who want notes preserved will see them missing.
- `\s{4}/` matches any line with four leading spaces followed by a slash. This is used to suppress compiler flag lines (which look like `    /usr/lib/swift/...`). It would also suppress any user-facing output line formatted that way.

---

### `simctl_cmd.rs`

**What it does.** Wraps `xcrun simctl`. When called with no args or with `list` as the first arg, it runs `xcrun simctl list` and applies `filter_simctl_list`. All other subcommands (`boot`, `shutdown`, `delete`, etc.) pass through with a trim-only filter (`simctl_cmd.rs:43`).

**Design decisions:**

- **Implicit `list` injection** (`simctl_cmd.rs:23-26`): if `args` is empty, `cmd.arg("list")` is appended before execution. This means `rtk simctl` (no args) behaves like `rtk simctl list` rather than printing the simctl help text. This is intentional — the most common use case in an AI context is listing devices. Reviewers should confirm this is acceptable UX.
- The filter uses a local `Section` enum (defined inside `filter_simctl_list`) with states: `None`, `DeviceTypes`, `Runtimes`, `Devices`, `Pairs`. Section transitions are driven by `== Header ==` lines.
- `in_unavailable` flag (`simctl_cmd.rs:79`): set to `true` when a `-- Unavailable:` line is seen inside the `Devices` section. All device lines after that flag are counted as unavailable rather than added to the device map. The flag resets on the next `== Section ==` header (`simctl_cmd.rs:106`).
- Device pair lines in the `Pairs` section are counted by checking if the line starts with a hex digit and is at least 36 characters (UUID length, `simctl_cmd.rs:145`). This is heuristic — any line meeting that criterion is counted as a pair entry, including potential device entries if the format changes.
- `DEVICE_RE` (`simctl_cmd.rs:62`): `^(.+?)\s+\([A-F0-9-]{36}\)\s+\((\w+)\)` — requires uppercase hex UUID and a single `(\w+)` status group. `Shutdown` and `Booted` both match `\w+`. Statuses like `Booted (Disconnected)` would not fully match — the outer `(\w+)` would capture only `Booted` and the rest would be trailing unmatched text. This is likely fine since the current simctl format uses only `Shutdown` or `Booted`.

---

### `xctrace_cmd.rs`

**What it does.** Wraps `xcrun xctrace`. Detects which subcommand is being run, then applies one of three specialized filters: `filter_list_templates`, `filter_list_devices`, or `filter_record`. Unrecognized subcommands (`Subcommand::Other`) trim and pass through.

**Design decisions:**

- `detect_subcommand` (`xctrace_cmd.rs:51`) takes positional args only (filters out args starting with `-`). It matches on a slice of up to 2 positional args. The matching uses `starts_with` rather than equality for the sub-argument (`sub.starts_with("template")`, `sub.starts_with("device")`). This means `xctrace list templates` and `xctrace list template` (singular) both dispatch to `filter_list_templates`. This is deliberate tolerance for partial argument spelling.
- The `Subcommand` enum derives `Copy` (`xctrace_cmd.rs:43`) because it is moved into the closure passed to `run_filtered`. `Copy` avoids a `move` plus explicit lifetime annotation.
- `filter_list_templates` uses `TEMPLATE_NAME_RE` (`xctrace_cmd.rs:73`): `^[A-Z][A-Za-z0-9 /-]+$` to identify template names. The pattern requires an uppercase first letter. Lowercase-starting template names (custom templates from `.tracetemplate` files not in the standard set) would be silently dropped. The `/` in the character class is unusual — it is there to match template names like "CPU/GPU Counters".
- `filter_list_devices`: physical devices and simulators are distinguished by `in_simulators` flag set when the section header contains "Simulator" (`xctrace_cmd.rs:149`). The `DEVICE_OS_RE` pattern (`xctrace_cmd.rs:130`) matches against the original line (not trimmed) because the OS line is indented. `filter_list_templates` uses `line.starts_with("    ")` similarly for the same reason — whitespace is meaningful for distinguishing indent levels.
- `filter_record` uses `STACK_RE` (`xctrace_cmd.rs:214`): `^(\d+\.\d+)%\s+(.+)` — matches percentage lines like `30.2%  SimpleHTTPServer`. The top 5 stacks are emitted (`xctrace_cmd.rs:297`). This relies on the `xctrace` text export format, which is only produced when `--output` is a `.txt` file or when using `xctrace export`. Most users invoke `xctrace record` without text output; in that case no stack lines appear and the filter just prints template/target/duration metadata.

---

## 3. Areas to Scrutinize

### `filter_swift_package`: the `Section` state machine (`swift_cmd.rs:370-506`)

The state machine is defined with a local enum inside the function body. Section transitions happen at lines that have no leading whitespace and end with `:` (`swift_cmd.rs:395`). Flush logic runs before transitioning:

- When leaving `Targets`, the last pending target is pushed (`swift_cmd.rs:397-401`).
- When leaving `Dependencies`, the last pending dep is pushed (`swift_cmd.rs:402-405`).

There are two flush sites for targets: on blank lines (`swift_cmd.rs:446-452`) and on a new `INDENTED_NAME_RE` match while a target is already accumulated (`swift_cmd.rs:456-458`). The final flush at `swift_cmd.rs:471-477` handles the last entry when the loop ends.

**Risk areas:**

- If a `Targets:` section ends with content but no trailing blank line (possible in some `swift package describe` outputs), the last target is flushed at `swift_cmd.rs:474-476`. This is correct, but test coverage for this case only exists in the fixture, not as a targeted unit test.
- `INDENTED_VERSION_RE` (`swift_cmd.rs:365`) appends version in the format ` (version)` to the dep name. If a dep has no version line, only the name appears. If the version line appears before the name line (unexpected but theoretically possible), `current_dep_name` would be empty and the version would be silently discarded (`swift_cmd.rs:432-435`).
- The `Skip` variant swallows all lines in unrecognized sections. `swift package describe` can emit `Platforms:`, `Swift languages versions:`, `C language standard:`, etc. These sections are correctly ignored. But if Apple adds a section named something other than `Dependencies:`, `Products:`, or `Targets:`, it will also be silently skipped with no indication in the output.

### `filter_swift_run`: build-done detection (`swift_cmd.rs:285-334`)

The function distinguishes three states:

1. **No build lines at all** (`saw_build_lines == false`): the build was cached and skipped entirely. All lines are program output.
2. **Build lines seen, build completed** (`saw_build_lines && build_done`): normal case. Lines after `Build complete!` are program output.
3. **Build lines seen, no `Build complete!`** (`saw_build_lines && !build_done`): build failed or was interrupted. Non-build-noise lines are skipped via `continue` at `swift_cmd.rs:319`.

State 3 is handled at `swift_cmd.rs:318-319`: when `saw_build_lines` is true but `build_done` is false, the `continue` skips all non-build-noise lines. At the end of the loop (line 327), the whole output is re-filtered through `filter_swift_build`. The `program_output` vector only accumulates lines in states 1 and 2 — there are no dead pushes.

The empty-output fallback at `swift_cmd.rs:331-333` also calls `filter_swift_build(output)`. This covers the case where build lines were seen and completed (so `build_done == true`) but no program output was produced — e.g., the program crashed before printing anything or exited silently.

### `filter_swiftlint`: `HashSet` deduplication (`swiftlint_cmd.rs:57-88`)

The `files: HashSet<String>` in `RuleStats` stores shortened paths. Shortened paths are the last two path components. Two scenarios worth checking:

1. **Same filename, different parent**: `Sources/Foo.swift` and `Tests/Foo.swift` in the same project would produce paths `Sources/Foo.swift` and `Tests/Foo.swift` respectively — no collision.
2. **Same filename, same parent**: if a project has `TargetA/Sources/Foo.swift` and `TargetB/Sources/Foo.swift`, both shorten to `Sources/Foo.swift`. The `HashSet` would count them as one file. The violation counts would still be correct; only the file count would be understated.

For the sorting to be deterministic the `by_rule` `HashMap` must be fully collected before sorting. It is (`swiftlint_cmd.rs:126`). The output order for rules with identical sort keys (same error count, same total count) is non-deterministic due to `HashMap` iteration order. This can cause test flakiness if tests assert on the relative order of equally-ranked rules.

### `NOISE_RE` in xcodebuild (`xcodebuild_cmd.rs:55-56`)

Already described above. The specific pattern worth re-reading:

```
\s{4}/
```

This matches exactly four spaces then a slash, targeting the indented compiler flag lines. However, `xcodebuild` sometimes emits lines with different indentation (two spaces, eight spaces) depending on the Xcode version and build system mode. Lines with non-four-space indentation would not be suppressed and would pass through to the error/warning regex check — likely emitted as unclassified noise (since they don't match `ERROR_RE` or `WARNING_RE`). They would just be ignored (the final `else` branch does nothing for unmatched lines), so no output corruption, but the output could contain unexpected lines in some Xcode versions.

### `detect_subcommand` in xctrace (`xctrace_cmd.rs:51-66`)

```rust
let positional: Vec<&str> = args
    .iter()
    .filter(|a| !a.starts_with('-'))
    .map(|a| a.as_str())
    .take(2)
    .collect();

match positional.as_slice() {
    ["list", sub] if sub.starts_with("template") => Subcommand::ListTemplates,
    ["list", sub] if sub.starts_with("device") => Subcommand::ListDevices,
    ["record", ..] => Subcommand::Record,
    _ => Subcommand::Other,
}
```

Filtering only by `starts_with('-')` may incorrectly classify value-bearing flags as positional args. For example, `xctrace record --device "iPhone 15"` — the word `iPhone` and `15` are not flag values (they are a quoted single arg), but if someone passes `--template "Time Profiler"` then `Time` could be classified as a positional arg if it appeared without quotes. In practice the `xctrace` CLI takes `--template <name>` as a separate token, not embedded. The current filter would see `["record", "Time"]` from `xctrace record --template Time` (treating `Time` as a positional arg) and match `Subcommand::Record` — which is correct. But `xctrace list --output file templates` would see `["list", "file"]` and not match any pattern (not `"template"`-prefixed, not `"device"`-prefixed), falling through to `Other`. This is an edge case that is unlikely in normal usage.

---

## 4. Known Limitations

### Fixtures

- **`swift_test_raw.txt`**: Real output from a Swift package using both XCTest and Swift Testing frameworks. Contains 112 tests with 7 failures across 22 suites. The fixture exercises both `XCTEST_RESULT_RE` and `SWIFT_TEST_PASS_RE`/`SWIFT_TEST_FAIL_RE` patterns.
- **`swift_run_raw.txt`**: Realistic fixture with 19 build/compilation steps followed by short program output (test runner summary). Exercises the build-stripping logic and achieves >=60% savings.
- **`xctrace_list_devices_raw.txt`**: Realistic fixture with 2 physical devices (with OS, Model, Disk Space metadata) and 7 simulators across iOS/iPadOS/watchOS/tvOS. Exercises `DEVICE_HEADER_RE`, `DEVICE_OS_RE`, simulator grouping by OS, and metadata stripping (Model/Disk Space lines).
- **`xctrace_record_raw.txt`**: This fixture is synthetic. `xctrace record` normally produces no useful text output to the terminal (output goes to a `.trace` file). The file was hand-constructed to match the format documented in the `xctrace` man page. Tests against this fixture verify the filter logic but not real-world format compatibility.

### No `insta` snapshots

RTK does not use the `insta` crate anywhere in the project. The `cli-testing.md` rules reference it, but the project's actual testing strategy uses direct `assert!` / `assert_eq!` checks with fixture files. The apple module follows the existing project pattern. There are no snapshot files to review.

### `rtk simctl` implicit `list`

When called with no arguments, `rtk simctl` injects `list` into the command (`simctl_cmd.rs:23-26`) and applies the list filter. This diverges from `xcrun simctl` with no args (which prints usage/help). This is a deliberate design choice: in an AI coding assistant context, the intent of `rtk simctl` without arguments is overwhelmingly "show me my simulators." If a user or LLM wants the help text, they must use `rtk simctl --help` or `rtk proxy xcrun simctl`.

### `swift package` filter scope

The `filter_swift_package` function only understands `swift package describe` output. When used with `swift package resolve`, `swift package update`, or `swift package clean`, the output contains none of the structured sections the state machine expects. The filter will emit `Package:` (with an empty name) and nothing else. This is not a crash or data loss — the underlying command executed correctly — but the filtered output is not useful. Users who need the full output of `swift package resolve` should use `rtk swift package resolve` through the passthrough path... but note that `swift package` is routed through `run_package`, not `run_passthrough`. Only unknown `swift` subcommands (not `package`) go to passthrough. This means `rtk swift package resolve` will produce a useless filtered output. A reviewer should raise this.

---

## 5. Test Coverage Summary

### What is tested

| File | Unit tests | Fixture-based | Savings threshold | Edge cases |
|---|---|---|---|---|
| `swift_cmd.rs` | Yes | Yes (4 fixtures) | `>=60%` for build/test/package/run | Empty input, error fallback, cached build, all-pass tests |
| `swiftlint_cmd.rs` | Yes | Yes (1 fixture) | `>=60%` | Empty input, zero violations, grouping correctness, path shortening |
| `xcodebuild_cmd.rs` | Yes | Yes (1 fixture) | `>=60%` | Empty input, build failure with errors, test results with pass/fail |
| `simctl_cmd.rs` | Yes | Yes (1 fixture) | `>=60%` | Empty input, unavailable device counting |
| `xctrace_cmd.rs` | Yes | Yes (4 fixtures) | `>=-5%` for templates, `>=30%` for devices/record | Empty input per sub-filter, subcommand detection with flags |

### Notable gaps

- **`filter_swift_package` last-target-no-trailing-blank**: no test ensures a `Targets:` section that ends without a blank line correctly flushes the final target. The fixture happens to end with a newline, but this is not asserted directly.
- **`swiftlint` non-deterministic sort order**: no test checks the relative order of rules with identical sort keys. If a future change alters `HashMap` iteration order, a test checking `output.contains("[E] force_cast")` and `output.contains("[W] trailing_whitespace")` would still pass but the output order might differ from expectations.
- **`NOISE_RE` coverage**: no test sends a line starting with `/Applications/Xcode` through the filter to verify it is suppressed. The fixture implicitly covers this, but an explicit test would make the intent clearer.
- **`rtk swift package resolve` producing useless output**: not tested. This is a behavioral gap, not just a test gap.
- **`xctrace list templates` savings threshold is `>=-5%`** (`xctrace_cmd.rs:346`), which means the test would pass even if the filter inflated output by 5%. This is because the fixture (`xctrace_list_templates_raw.txt`) contains template names without multi-line descriptions, so there is almost nothing to compress. The low threshold is a known limitation documented in the test comment.
- **`xctrace list devices` savings threshold is `>=30%`** (`xctrace_cmd.rs:396`). Device listing savings come from stripping UUIDs and metadata (Model, Disk Space). Device names must be preserved, so savings are structurally limited to ~40% with a realistic fixture. Higher savings would require a fixture with many more metadata-heavy devices.

---

## 6. Real-World Savings Data

Measured against real Swift projects:

### PsyScopeTahoe (19 targets, 112 tests)

| Command | Raw tokens | Filtered | Savings |
|---------|-----------|----------|---------|
| `swift test` | 2,426 | 64 | **97.4%** |
| `swift package describe` | 471 | 45 | **90.4%** |
| `swiftlint` | 709 lines | 23 lines | **97%** |
| `swift build` (cached) | 12 | 8 | 33.3% |
| `swift run PsyScopeTest` | 1,296 | 1,281 | 1.2% |

### Apple open-source projects (stress tests)

| Project | Command | Raw tokens | Filtered | Savings |
|---------|---------|-----------|----------|---------|
| swift-algorithms | `swift test` | 3,672 | 6 | **99.8%** (224 tests) |
| swift-argument-parser | `swift test` | 9,358 | 6 | **99.9%** (558 tests) |
| swift-argument-parser | `swift build` | 1,759 | 112 | 93.6% (26 warnings) |
| swift-collections | `swift test` | 13,024 | 6 | **100.0%** (772 tests) |
| swift-syntax | `swift test` | 49,493 | 6 | **99.988%** (3,528 tests) |
| swift-syntax | `swiftlint Sources` | 114,946 | 115 | **99.9%** (39 rules, 6566 violations) |

Key observations:

- **`swift test`** consistently delivers 99%+ savings regardless of suite size (224 → 3,528 tests). Test counts and failure names/messages are preserved exactly.
- **`swift package describe`** delivers 90%+ savings across all projects tested.
- **`swiftlint`** aggregates thousands of violations into a compact rule summary with per-rule counts and file counts preserved.
- **`swift build`** savings scale with compilation volume. Cached/no-op builds are trivially small so there is nothing to strip; full rebuilds with many compilation steps would see 70%+ savings.
- **`swift run`** preserves program output by design. Savings depend on the ratio of build lines to runtime output. A program with verbose output and a cached build is the worst case.
- **`xctrace list devices`** savings scale with the number of installed simulators. A machine with no Xcode simulators has trivially small output.

---

## 6.1. Rigorous Validation (Regression Protection)

A validation harness at `scripts/validate-apple/` programmatically verifies that no critical signal is silently dropped by the filters. It runs **71 scenarios** across **11 Apple open-source projects** plus synthetic failure-injection cases.

### What the harness verifies

For every project:

1. **Package describe** — package name, targets, and product count preserved
2. **Build (cached)** — success signal propagates
3. **Build with injected compile error** — error file:line location preserved, error count matches raw, injection-related error (`cannot find`) visible
4. **Test passing** — test pass/fail counts match raw (XCTest + Swift Testing)
5. **Test with injected failure** — failure name enumerated, file:line preserved, `XCTAssert` message preserved verbatim
6. **Swiftlint rule aggregation** — per-rule counts exactly match raw; truncated rules accounted for by `+N more rules` counter
7. **Warning truncation math** — `shown_in_output + "+N more" == declared_total`
8. **Error ordering preservation** — filter does not reshuffle errors relative to raw order

### Projects covered

- swift-algorithms (224 tests)
- swift-argument-parser (558 tests, 26 warnings)
- swift-async-algorithms (async patterns)
- swift-collections (772 tests across 11 libraries)
- swift-crypto (C/Swift mixed)
- swift-docc (CLI tool)
- swift-format (executable + plugins)
- swift-log (lightweight library)
- swift-nio (production-scale networking)
- swift-numerics (math)
- swift-syntax (3,528 tests, 6566 lint violations)

### Current result

**71/71 scenarios pass** across the matrix of 11 projects × 5 scenarios plus 16 dedicated failure-path and edge-case tests.

### Data-loss bugs fixed during validation development

The rigorous harness uncovered **4 data-loss bugs** that escaped unit-test coverage. All fixed in the merge commit that added validation:

1. **Informational subcommands silently dropped** — `xcodebuild -list` / `-version` / `-showsdks` and `swiftlint version` / `rules` had output replaced with the filter's fixed header. Fixed by adding passthrough heuristic (if no build/lint markers detected, return raw unchanged).
2. **`ERROR_RE` too narrow** — only matched source-file format (`path:line:col: error:`). Project-level errors (signing, provisioning) and top-level errors (`error: emit-module failed`) were dropped. Broadened to `^(?:.+?:\s+)?error:\s`.
3. **Paths with spaces not matched** — `\S+` couldn't capture paths like `/tmp/metal code examples/Foo.xcodeproj`. Changed to non-greedy `.+?`.
4. **`swift package dump-package` JSON dropped** — the describe-format filter was incorrectly applied to JSON output. Removed from dispatch + added safety-net passthrough inside `filter_swift_package`.

### How to re-run validation

```bash
# Requires the 11 Apple projects cloned in /tmp/ and swiftlint installed
python3 scripts/validate-apple/validate_all.py
python3 scripts/validate-apple/validate_failures.py
python3 scripts/validate-apple/validate_deep.py
```

The validators auto-inject bugs into source/test files and restore them with try/finally. If a run is interrupted, `git checkout Sources/ Tests/` in the affected project will restore clean state.

---

## 7. How to Validate

A smoke test script is available at `scripts/test-apple.sh`. The primary validation path is the standard Rust test suite.

**Run all apple module tests:**

```bash
cargo test --all 2>&1 | grep -E "apple|swift|xcode|simctl|xctrace|swiftlint"
```

**Run a specific module:**

```bash
cargo test -p rtk swift_cmd
cargo test -p rtk swiftlint_cmd
cargo test -p rtk xcodebuild_cmd
cargo test -p rtk simctl_cmd
cargo test -p rtk xctrace_cmd
```

**Run with stdout for debugging filter output:**

```bash
cargo test test_filter_swift_package_format -- --nocapture
```

**Manual smoke test (requires Xcode installed):**

```bash
# Install RTK locally first
cargo install --path .

# swift build
cd /path/to/any-swift-package
rtk swift build

# swift package describe
rtk swift package describe

# simctl list
rtk simctl list

# xctrace list devices (requires Xcode)
rtk xctrace list devices

# xcodebuild (in a project with an Xcode scheme)
rtk xcodebuild -scheme MyScheme -configuration Debug build
```

**Check token savings are enforced:**

```bash
cargo test test_filter_swift_build_savings
cargo test test_filter_swiftlint_savings
cargo test test_filter_xcodebuild_savings
cargo test test_filter_simctl_list_savings
cargo test test_filter_list_devices_savings
```

All savings tests follow the same pattern: `count_tokens` is `text.split_whitespace().count()` — whitespace-based, not BPE. This consistently undercounts savings compared to actual LLM tokenizers but is fast and reproducible.

**Pre-commit gate (required before merging):**

```bash
cargo fmt --all && cargo clippy --all-targets && cargo test --all
```
