//! Filters `swift` command output — build progress, test results, package info, run output.
//!
//! Subcommands:
//! - `swift build`:   strips compilation lines, keeps errors + final status
//! - `swift test`:    shows only pass/fail per test + summary
//! - `swift package`: condenses package description to name, deps, products, targets
//! - `swift run`:     strips build noise, keeps only program output

use crate::core::runner;
use crate::core::utils::{resolved_command, strip_ansi};
use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;
use std::ffi::OsString;

#[derive(Debug, Clone)]
pub enum SwiftCommand {
    Build,
    Test,
    Package,
    Run,
}

pub fn run(cmd: SwiftCommand, args: &[String], verbose: u8) -> Result<i32> {
    match cmd {
        SwiftCommand::Build => run_build(args, verbose),
        SwiftCommand::Test => run_test(args, verbose),
        SwiftCommand::Package => run_package(args, verbose),
        SwiftCommand::Run => run_run(args, verbose),
    }
}

pub fn run_passthrough(args: &[OsString], verbose: u8) -> Result<i32> {
    runner::run_passthrough("swift", args, verbose)
}

// ─── swift build ─────────────────────────────────────────────────────────────

fn run_build(args: &[String], verbose: u8) -> Result<i32> {
    let mut cmd = resolved_command("swift");
    cmd.arg("build");
    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: swift build {}", args.join(" "));
    }

    runner::run_filtered(
        cmd,
        "swift build",
        &args.join(" "),
        filter_swift_build,
        runner::RunOptions::with_tee("swift-build"),
    )
}

lazy_static! {
    static ref BUILD_PROGRESS_RE: Regex =
        Regex::new(r"^\[(\d+)/(\d+)\]").unwrap();
    // Swift compiler error/warning. Covers multiple formats. Path prefix may
    // contain spaces, so we use `.+?` (non-greedy) instead of `\S+`.
    //   /path/File.swift:10:5: error: message       (source file error)
    //   /path/Package.swift: error: message         (package-level error)
    //   error: message                              (top-level error)
    static ref BUILD_ERROR_RE: Regex =
        Regex::new(r"^(?:.+?:\s+)?error:\s").unwrap();
    static ref BUILD_WARNING_RE: Regex =
        Regex::new(r"^(?:.+?:\s+)?warning:\s").unwrap();
    // "Build complete! (3.42s)" or "Build of product 'Foo' complete! (0.28s)"
    static ref BUILD_COMPLETE_RE: Regex =
        Regex::new(r"^Build (?:of product '.+' )?complete!").unwrap();
    static ref BUILD_FAILED_RE: Regex =
        Regex::new(r"(?i)^build .*(failed|error)").unwrap();
}

fn filter_swift_build(output: &str) -> String {
    let clean = strip_ansi(output);
    let mut errors: Vec<&str> = Vec::new();
    let mut warnings: Vec<&str> = Vec::new();
    let mut max_step = 0u32;
    let mut total_steps = 0u32;
    let mut build_result = String::new();

    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(caps) = BUILD_PROGRESS_RE.captures(trimmed) {
            let step: u32 = caps[1].parse().unwrap_or(0);
            let total: u32 = caps[2].parse().unwrap_or(0);
            if step > max_step {
                max_step = step;
            }
            if total > total_steps {
                total_steps = total;
            }
        }

        if BUILD_ERROR_RE.is_match(trimmed) {
            errors.push(trimmed);
        } else if BUILD_WARNING_RE.is_match(trimmed) {
            warnings.push(trimmed);
        }

        if BUILD_COMPLETE_RE.is_match(trimmed) || BUILD_FAILED_RE.is_match(trimmed) {
            build_result = trimmed.to_string();
        }
    }

    let mut result = String::new();
    result.push_str("swift build\n");

    if total_steps > 0 {
        result.push_str(&format!("Compiled {}/{} steps\n", max_step, total_steps));
    }

    if !errors.is_empty() {
        result.push_str(&format!("\nErrors ({}):\n", errors.len()));
        for e in &errors {
            result.push_str(&format!("  {}\n", e));
        }
    }

    if !warnings.is_empty() {
        result.push_str(&format!("\nWarnings ({}):\n", warnings.len()));
        for w in warnings.iter().take(10) {
            result.push_str(&format!("  {}\n", w));
        }
        if warnings.len() > 10 {
            result.push_str(&format!("  ... +{} more\n", warnings.len() - 10));
        }
    }

    if !build_result.is_empty() {
        result.push_str(&format!("\n{}\n", build_result));
    } else if errors.is_empty() {
        result.push_str("ok\n");
    }

    result.trim().to_string()
}

// ─── swift test ──────────────────────────────────────────────────────────────

fn run_test(args: &[String], verbose: u8) -> Result<i32> {
    let mut cmd = resolved_command("swift");
    cmd.arg("test");
    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: swift test {}", args.join(" "));
    }

    runner::run_filtered(
        cmd,
        "swift test",
        &args.join(" "),
        filter_swift_test,
        runner::RunOptions::with_tee("swift-test"),
    )
}

lazy_static! {
    // XCTest: Test Case '-[Module.Suite testName]' passed (0.003 seconds).
    static ref XCTEST_RESULT_RE: Regex =
        Regex::new(r"^Test Case '-\[(\S+)\.(\S+) (\w+)\]' (passed|failed) \(([0-9.]+) seconds\)").unwrap();
    // XCTest error detail: /path/File.swift:42: error: -[Module.Suite testFoo] : XCTAssert...
    static ref XCTEST_ERROR_RE: Regex =
        Regex::new(r"^\S+:\d+: error: -\[").unwrap();
    // Swift Testing: ✔ Test name() passed after 0.001 seconds.
    static ref SWIFT_TEST_PASS_RE: Regex =
        Regex::new(r"^[✔✓] Test (.+) passed after ([0-9.]+) seconds").unwrap();
    // Swift Testing: ✘ Test name() failed after 0.001 seconds.
    static ref SWIFT_TEST_FAIL_RE: Regex =
        Regex::new(r"^[✘✗] Test (.+) failed after ([0-9.]+) seconds").unwrap();
    // Swift Testing run summary
    static ref SWIFT_TEST_RUN_RE: Regex =
        Regex::new(r"^[✔✓✘✗] Test run with (\d+) tests? .* (passed|failed)").unwrap();
}

fn filter_swift_test(output: &str) -> String {
    let clean = strip_ansi(output);
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut failures: Vec<String> = Vec::new();
    let mut error_details: Vec<String> = Vec::new();

    for line in clean.lines() {
        let trimmed = line.trim();

        // XCTest results
        if let Some(caps) = XCTEST_RESULT_RE.captures(trimmed) {
            let suite = &caps[2];
            let test_name = &caps[3];
            let status = &caps[4];
            let duration = &caps[5];
            if status == "passed" {
                passed += 1;
            } else {
                failed += 1;
                failures.push(format!("  FAIL {}.{} ({}s)", suite, test_name, duration));
            }
        }

        // Collect error details for failed tests
        if XCTEST_ERROR_RE.is_match(trimmed) {
            error_details.push(format!("  {}", trimmed));
        }

        // Swift Testing results — skip the run summary line first
        if SWIFT_TEST_RUN_RE.is_match(trimmed) {
            continue;
        }
        if SWIFT_TEST_PASS_RE.is_match(trimmed) {
            passed += 1;
        }
        if let Some(caps) = SWIFT_TEST_FAIL_RE.captures(trimmed) {
            failed += 1;
            failures.push(format!("  FAIL {} ({}s)", &caps[1], &caps[2]));
        }
    }

    let total = passed + failed;
    let mut result = String::new();
    result.push_str("swift test\n");

    if total == 0 {
        result.push_str("No tests found\n");
        return result.trim().to_string();
    }

    if failed == 0 {
        result.push_str(&format!("{} passed, 0 failed\n", passed));
    } else {
        result.push_str(&format!(
            "{} passed, {} failed (of {})\n",
            passed, failed, total
        ));
        result.push('\n');
        for f in &failures {
            result.push_str(f);
            result.push('\n');
        }
        if !error_details.is_empty() {
            result.push('\n');
            for d in error_details.iter().take(20) {
                result.push_str(d);
                result.push('\n');
            }
            if error_details.len() > 20 {
                result.push_str(&format!("  ... +{} more errors\n", error_details.len() - 20));
            }
        }
    }

    result.trim().to_string()
}

// ─── swift run ───────────────────────────────────────────────────────────────

fn run_run(args: &[String], verbose: u8) -> Result<i32> {
    let mut cmd = resolved_command("swift");
    cmd.arg("run");
    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: swift run {}", args.join(" "));
    }

    runner::run_filtered(
        cmd,
        "swift run",
        &args.join(" "),
        filter_swift_run,
        runner::RunOptions::with_tee("swift-run"),
    )
}

/// Strip build noise from `swift run` output, keeping only program output.
/// Build lines are: "Building for ...", "[N/M] ...", "Build complete! (...)"
fn filter_swift_run(output: &str) -> String {
    let clean = strip_ansi(output);
    let mut program_output = Vec::new();
    let mut build_done = false;
    let mut saw_build_lines = false;

    for line in clean.lines() {
        if build_done {
            program_output.push(line);
            continue;
        }

        let trimmed = line.trim();

        // Skip build noise
        let is_build_complete = BUILD_COMPLETE_RE.is_match(trimmed);
        if trimmed.starts_with("Building for")
            || trimmed.starts_with("[0/1] Planning build")
            || BUILD_PROGRESS_RE.is_match(trimmed)
            || is_build_complete
        {
            saw_build_lines = true;
            if is_build_complete {
                build_done = true;
            }
            continue;
        }

        // If we saw build lines but never "Build complete!", it's a failed build.
        // Don't collect lines — we'll fall back to filter_swift_build at the end.
        if saw_build_lines && !build_done {
            continue;
        }

        // No build lines at all (cached build) — everything is program output
        program_output.push(line);
    }

    // Build started but never completed → failed build, use build filter
    if saw_build_lines && !build_done {
        return filter_swift_build(output);
    }

    if program_output.is_empty() {
        return filter_swift_build(output);
    }

    program_output.join("\n")
}

// ─── swift package ───────────────────────────────────────────────────────────

fn run_package(args: &[String], verbose: u8) -> Result<i32> {
    let mut cmd = resolved_command("swift");
    cmd.arg("package");
    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: swift package {}", args.join(" "));
    }

    // Only apply the describe filter for "describe" subcommand (or no subcommand).
    // Other subcommands (resolve, update, clean, dump-package, etc.) have different
    // output formats — dump-package in particular is JSON and would be silently
    // dropped by the describe-format filter.
    let is_describe = args.is_empty() || args.first().is_some_and(|a| a == "describe");

    runner::run_filtered(
        cmd,
        "swift package",
        &args.join(" "),
        move |raw| {
            if is_describe {
                filter_swift_package(raw)
            } else {
                raw.trim().to_string()
            }
        },
        runner::RunOptions::default(),
    )
}

lazy_static! {
    static ref PKG_NAME_RE: Regex = Regex::new(r"^Name:\s+(.+)").unwrap();
    // Shared: matches 4-space-indented "    Name: value" lines in deps, products, and targets
    static ref INDENTED_NAME_RE: Regex =
        Regex::new(r"^\s{4}Name:\s+(.+)").unwrap();
    static ref INDENTED_VERSION_RE: Regex =
        Regex::new(r"^\s{4}Version:\s+(.+)").unwrap();
    static ref INDENTED_TYPE_RE: Regex =
        Regex::new(r"^\s{4}Type:\s+(.+)").unwrap();
}

fn filter_swift_package(output: &str) -> String {
    let clean = strip_ansi(output);

    // Safety net: if the output has no recognizable describe-format sections,
    // passthrough. Prevents data loss on unexpected subcommands (e.g. resolve,
    // update) that slip past the dispatcher.
    if !clean.trim().is_empty()
        && !clean.lines().any(|l| {
            let t = l.trim();
            t == "Dependencies:" || t == "Products:" || t == "Targets:" || PKG_NAME_RE.is_match(l)
        })
    {
        return clean.trim().to_string();
    }

    let mut pkg_name = String::new();
    let mut deps: Vec<String> = Vec::new();
    let mut targets: Vec<(String, String)> = Vec::new();
    let mut products: Vec<String> = Vec::new();

    #[derive(PartialEq)]
    enum Section {
        Top,
        Dependencies,
        Products,
        Targets,
        Skip, // Platforms, Swift languages versions, Resources, etc.
    }
    let mut section = Section::Top;
    let mut current_dep_name = String::new();
    let mut current_target_name = String::new();
    let mut current_target_type = String::new();

    for line in clean.lines() {
        let trimmed = line.trim();

        // Detect section headers (top-level lines ending with colon, no leading whitespace)
        if !line.starts_with(' ') && !line.starts_with('\t') && line.ends_with(':') {
            // Flush state from previous section
            if section == Section::Targets && !current_target_name.is_empty() {
                targets.push((current_target_name.clone(), current_target_type.clone()));
                current_target_name.clear();
                current_target_type.clear();
            }
            if section == Section::Dependencies && !current_dep_name.is_empty() {
                deps.push(current_dep_name.clone());
                current_dep_name.clear();
            }

            section = match trimmed {
                "Dependencies:" => Section::Dependencies,
                "Products:" => Section::Products,
                "Targets:" => Section::Targets,
                _ => Section::Skip, // Platforms, Swift languages versions, Resources, etc.
            };
            continue;
        }

        match section {
            Section::Top => {
                if let Some(caps) = PKG_NAME_RE.captures(line) {
                    if pkg_name.is_empty() {
                        pkg_name = caps[1].trim().to_string();
                    }
                }
            }
            Section::Dependencies => {
                if let Some(caps) = INDENTED_NAME_RE.captures(line) {
                    // Flush previous dep
                    if !current_dep_name.is_empty() {
                        deps.push(current_dep_name.clone());
                    }
                    current_dep_name = caps[1].trim().to_string();
                }
                if let Some(caps) = INDENTED_VERSION_RE.captures(line) {
                    if !current_dep_name.is_empty() {
                        current_dep_name
                            .push_str(&format!(" ({})", caps[1].trim()));
                    }
                }
            }
            Section::Products => {
                if let Some(caps) = INDENTED_NAME_RE.captures(line) {
                    products.push(caps[1].trim().to_string());
                }
            }
            Section::Targets => {
                if trimmed.is_empty() {
                    // Flush target on blank line
                    if !current_target_name.is_empty() {
                        targets.push((current_target_name.clone(), current_target_type.clone()));
                        current_target_name.clear();
                        current_target_type.clear();
                    }
                    continue;
                }
                if let Some(caps) = INDENTED_NAME_RE.captures(line) {
                    // New target — flush previous
                    if !current_target_name.is_empty() {
                        targets.push((current_target_name.clone(), current_target_type.clone()));
                    }
                    current_target_name = caps[1].trim().to_string();
                    current_target_type.clear();
                } else if let Some(caps) = INDENTED_TYPE_RE.captures(line) {
                    if current_target_type.is_empty() {
                        current_target_type = caps[1].trim().to_string();
                    }
                }
            }
            Section::Skip => {} // Ignore lines in unrecognized sections
        }
    }

    // Flush remaining
    if !current_dep_name.is_empty() {
        deps.push(current_dep_name);
    }
    if !current_target_name.is_empty() {
        targets.push((current_target_name, current_target_type));
    }

    let mut result = String::new();
    result.push_str(&format!("Package: {}\n", pkg_name));

    if !deps.is_empty() {
        result.push_str(&format!("Dependencies ({}): ", deps.len()));
        result.push_str(&deps.join(", "));
        result.push('\n');
    }

    if !products.is_empty() {
        result.push_str(&format!("Products ({}): ", products.len()));
        result.push_str(&products.join(", "));
        result.push('\n');
    }

    if !targets.is_empty() {
        result.push_str(&format!("Targets ({}):\n", targets.len()));
        for (name, ttype) in &targets {
            if ttype.is_empty() {
                result.push_str(&format!("  {}\n", name));
            } else {
                result.push_str(&format!("  {} ({})\n", name, ttype));
            }
        }
    }

    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_tokens(text: &str) -> usize {
        text.split_whitespace().count()
    }

    // ── swift build ──────────────────────────────────────────────────────

    #[test]
    fn test_filter_swift_build_format() {
        let input = include_str!("../../../tests/fixtures/swift_build_raw.txt");
        let output = filter_swift_build(input);
        assert!(output.contains("swift build"));
        assert!(output.contains("Compiled"));
        assert!(output.contains("Build complete!"));
        // Verbose compilation lines should be stripped
        assert!(!output.contains("Compiling PsyScopeFramework"));
    }

    #[test]
    fn test_filter_swift_build_savings() {
        let input = include_str!("../../../tests/fixtures/swift_build_raw.txt");
        let output = filter_swift_build(input);

        let input_tokens = count_tokens(input);
        let output_tokens = count_tokens(&output);
        let savings = 100.0 - (output_tokens as f64 / input_tokens as f64 * 100.0);

        assert!(
            savings >= 60.0,
            "swift build filter: expected >=60% savings, got {:.1}% (in={}, out={})",
            savings,
            input_tokens,
            output_tokens
        );
    }

    #[test]
    fn test_filter_swift_build_empty() {
        let output = filter_swift_build("");
        assert!(output.contains("swift build"));
    }

    #[test]
    fn test_filter_swift_build_errors() {
        let input = "Building for debugging...\n\
            [1/3] Compiling Foo Bar.swift\n\
            /path/Bar.swift:10:5: error: use of undeclared type 'Baz'\n\
            /path/Bar.swift:15:1: warning: result unused\n\
            Build complete! (0.5s)\n";
        let output = filter_swift_build(input);
        assert!(output.contains("Errors (1)"));
        assert!(output.contains("Warnings (1)"));
    }

    // ── swift test ───────────────────────────────────────────────────────

    #[test]
    fn test_filter_swift_test_format() {
        let input = include_str!("../../../tests/fixtures/swift_test_raw.txt");
        let output = filter_swift_test(input);
        assert!(output.contains("swift test"));
        assert!(output.contains("105 passed"));
        assert!(output.contains("7 failed"));
        // Noise should be stripped
        assert!(!output.contains("◇ Test"));
        assert!(!output.contains("◇ Suite"));
        assert!(!output.contains("Testing Library Version"));
        // Run summary should NOT appear as a test failure
        assert!(!output.contains("FAIL run with"));
    }

    #[test]
    fn test_filter_swift_test_savings() {
        let input = include_str!("../../../tests/fixtures/swift_test_raw.txt");
        let output = filter_swift_test(input);

        let input_tokens = count_tokens(input);
        let output_tokens = count_tokens(&output);
        let savings = 100.0 - (output_tokens as f64 / input_tokens as f64 * 100.0);

        assert!(
            savings >= 60.0,
            "swift test filter: expected >=60% savings, got {:.1}% (in={}, out={})",
            savings,
            input_tokens,
            output_tokens
        );
    }

    #[test]
    fn test_filter_swift_test_empty() {
        let output = filter_swift_test("");
        assert!(output.contains("No tests found"));
    }

    #[test]
    fn test_filter_swift_test_all_pass() {
        let input = "Test Case '-[M.S testA]' passed (0.001 seconds).\n\
            Test Case '-[M.S testB]' passed (0.002 seconds).\n\
            \t Executed 2 tests, with 0 failures (0 unexpected) in 0.003 seconds\n";
        let output = filter_swift_test(input);
        assert!(output.contains("2 passed, 0 failed"));
    }

    // ── swift package ────────────────────────────────────────────────────

    #[test]
    fn test_filter_swift_package_format() {
        let input = include_str!("../../../tests/fixtures/swift_package_raw.txt");
        let output = filter_swift_package(input);
        assert!(output.contains("Package: PsyScopeTahoe"));
        assert!(output.contains("Targets"));
        // Verbose metadata should be stripped
        assert!(!output.contains("C99name"));
        assert!(!output.contains("Module type"));
        assert!(!output.contains("Path:"));
    }

    #[test]
    fn test_filter_swift_package_savings() {
        let input = include_str!("../../../tests/fixtures/swift_package_raw.txt");
        let output = filter_swift_package(input);

        let input_tokens = count_tokens(input);
        let output_tokens = count_tokens(&output);
        let savings = 100.0 - (output_tokens as f64 / input_tokens as f64 * 100.0);

        assert!(
            savings >= 60.0,
            "swift package filter: expected >=60% savings, got {:.1}% (in={}, out={})",
            savings,
            input_tokens,
            output_tokens
        );
    }

    #[test]
    fn test_filter_swift_package_empty() {
        let output = filter_swift_package("");
        assert!(output.contains("Package:"));
    }

    #[test]
    fn test_filter_swift_package_resolve_output() {
        // `swift package resolve` output looks nothing like `describe`.
        // Safety net: should passthrough unchanged rather than emit a misleading
        // empty "Package:" header that loses all info.
        let input = "Fetching https://github.com/apple/swift-nio.git\n\
            Computed swift-nio at 2.64.0 (0.01s)\n\
            Working copy of swift-nio resolved at 2.64.0\n";
        let output = filter_swift_package(input);
        assert!(output.contains("Fetching"));
        assert!(output.contains("swift-nio"));
        assert!(output.contains("2.64.0"));
    }

    #[test]
    fn test_filter_swift_package_dump_package_json() {
        // `swift package dump-package` outputs JSON. The describe filter would
        // silently drop all of this; the safety net must passthrough instead.
        let input = "{\n  \"name\" : \"MyPackage\",\n  \"dependencies\" : []\n}\n";
        let output = filter_swift_package(input);
        assert!(output.contains("\"name\""));
        assert!(output.contains("MyPackage"));
    }

    // ── swift run ────────────────────────────────────────────────────────

    #[test]
    fn test_filter_swift_run_format() {
        let input = include_str!("../../../tests/fixtures/swift_run_raw.txt");
        let output = filter_swift_run(input);
        // Build noise stripped
        assert!(!output.contains("Building for"));
        assert!(!output.contains("[0/19]"));
        assert!(!output.contains("Compiling"));
        assert!(!output.contains("Build of product"));
        // Program output kept
        assert!(output.contains("PsyScope Tahoe"));
        assert!(output.contains("3 checks passed"));
    }

    #[test]
    fn test_filter_swift_run_savings() {
        let input = include_str!("../../../tests/fixtures/swift_run_raw.txt");
        let output = filter_swift_run(input);

        let input_tokens = count_tokens(input);
        let output_tokens = count_tokens(&output);
        let savings = 100.0 - (output_tokens as f64 / input_tokens as f64 * 100.0);

        assert!(
            savings >= 60.0,
            "swift run filter: expected >=60% savings, got {:.1}% (in={}, out={})",
            savings,
            input_tokens,
            output_tokens
        );
    }

    #[test]
    fn test_filter_swift_run_no_build_output() {
        // Cached build — no build lines at all, just program output
        let input = "Hello, world!\nDone.\n";
        let output = filter_swift_run(input);
        assert_eq!(output, "Hello, world!\nDone.");
    }

    #[test]
    fn test_filter_swift_run_build_error_fallback() {
        // Build failed — no program output, falls back to build filter
        let input = "Building for debugging...\n\
            [1/3] Compiling Foo Bar.swift\n\
            /path/Bar.swift:10:5: error: use of undeclared type 'Baz'\n";
        let output = filter_swift_run(input);
        assert!(output.contains("swift build"));
        assert!(output.contains("Errors"));
    }
}
