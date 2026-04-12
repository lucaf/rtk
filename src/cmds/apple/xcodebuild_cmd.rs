//! Filters `xcodebuild` output — strips verbose compiler invocations, DerivedData paths,
//! and build system noise. Keeps errors, warnings, resolved packages, and final status.

use crate::core::runner;
use crate::core::utils::{resolved_command, strip_ansi};
use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;

pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    let mut cmd = resolved_command("xcodebuild");
    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: xcodebuild {}", args.join(" "));
    }

    runner::run_filtered(
        cmd,
        "xcodebuild",
        &args.join(" "),
        filter_xcodebuild,
        runner::RunOptions::with_tee("xcodebuild"),
    )
}

lazy_static! {
    // SwiftCompile normal arm64 /path/to/File.swift (in target 'Foo' from project 'Bar')
    static ref COMPILE_RE: Regex =
        Regex::new(r"^SwiftCompile .+ /\S+/(\S+\.swift) \(in target '([^']+)'").unwrap();
    // Ld /path/to/binary normal arm64 (in target 'Foo' from project 'Bar')
    static ref LINK_RE: Regex =
        Regex::new(r"^Ld .+ \(in target '([^']+)'").unwrap();
    // SwiftEmitModule normal arm64 ...
    static ref EMIT_MODULE_RE: Regex =
        Regex::new(r"^SwiftEmitModule .+ \(in target '([^']+)'").unwrap();
    // Resolved source packages lines (matched against trimmed input)
    static ref RESOLVED_PKG_RE: Regex =
        Regex::new(r"^(\S+):\s+\S+\s+@\s+(\S+)").unwrap();
    // Compiler error/warning lines: /path/File.swift:10:5: error: message
    static ref ERROR_RE: Regex = Regex::new(r"^\S+:\d+:\d+: error:").unwrap();
    static ref WARNING_RE: Regex = Regex::new(r"^\S+:\d+:\d+: warning:").unwrap();
    // ** BUILD SUCCEEDED ** / ** BUILD FAILED ** / ** TEST SUCCEEDED ** etc.
    static ref BUILD_RESULT_RE: Regex =
        Regex::new(r"^\*\* (BUILD|TEST|CLEAN) (SUCCEEDED|FAILED) \*\*").unwrap();
    // XCTest result: Test Case '-[Module.Suite testName]' passed/failed (0.003 seconds).
    static ref XCTEST_RESULT_RE: Regex =
        Regex::new(r"^Test Case '-\[(\S+)\.(\S+) (\w+)\]' (passed|failed) \(([0-9.]+) seconds\)").unwrap();
    // CreateBuildDirectory, cd, command-line tool invocations — noise
    static ref NOISE_RE: Regex =
        Regex::new(r"^(?:CreateBuildDirectory|cd |/Applications/Xcode|Build description|note: |User defaults|Command line invocation|\s+/|Test Suite |Test Case .* started)").unwrap();
}

fn filter_xcodebuild(output: &str) -> String {
    let clean = strip_ansi(output);

    let mut compiled_files: Vec<(String, String)> = Vec::new(); // (file, target)
    let mut linked_targets: Vec<String> = Vec::new();
    let mut resolved_packages: Vec<(String, String)> = Vec::new(); // (name, version)
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut build_result = String::new();
    let mut tests_passed = 0u32;
    let mut tests_failed = 0u32;
    let mut test_failures: Vec<String> = Vec::new();

    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Skip noise lines (cd, full compiler invocations, DerivedData paths)
        if NOISE_RE.is_match(trimmed) {
            continue;
        }

        // Compile step
        if let Some(caps) = COMPILE_RE.captures(trimmed) {
            compiled_files.push((caps[1].to_string(), caps[2].to_string()));
            continue;
        }

        // Link step
        if let Some(caps) = LINK_RE.captures(trimmed) {
            linked_targets.push(caps[1].to_string());
            continue;
        }

        // Emit module (skip — just note it happened)
        if EMIT_MODULE_RE.is_match(trimmed) {
            continue;
        }

        // Resolved packages
        if let Some(caps) = RESOLVED_PKG_RE.captures(trimmed) {
            resolved_packages.push((caps[1].to_string(), caps[2].to_string()));
            continue;
        }

        // XCTest results
        if let Some(caps) = XCTEST_RESULT_RE.captures(trimmed) {
            if &caps[4] == "passed" {
                tests_passed += 1;
            } else {
                tests_failed += 1;
                test_failures.push(format!(
                    "{}.{} ({}s)",
                    &caps[2], &caps[3], &caps[5]
                ));
            }
            continue;
        }

        // Build/test result
        if BUILD_RESULT_RE.is_match(trimmed) {
            build_result = trimmed.to_string();
            continue;
        }

        // Errors and warnings (capture full line for context)
        if ERROR_RE.is_match(trimmed) {
            errors.push(trimmed.to_string());
        } else if WARNING_RE.is_match(trimmed) {
            warnings.push(trimmed.to_string());
        }
    }

    let mut result = String::new();
    result.push_str("xcodebuild\n");

    // Resolved packages
    if !resolved_packages.is_empty() {
        result.push_str(&format!("Packages ({}):", resolved_packages.len()));
        for (name, ver) in &resolved_packages {
            result.push_str(&format!(" {}@{}", name, ver));
        }
        result.push('\n');
    }

    // Compiled files grouped by target (sorted for deterministic output)
    if !compiled_files.is_empty() {
        let mut by_target: std::collections::BTreeMap<&str, Vec<&str>> =
            std::collections::BTreeMap::new();
        for (file, target) in &compiled_files {
            by_target.entry(target.as_str()).or_default().push(file.as_str());
        }
        for (target, files) in &by_target {
            result.push_str(&format!(
                "Compiled {} ({} files)\n",
                target,
                files.len()
            ));
        }
    }

    // Linked targets
    if !linked_targets.is_empty() {
        result.push_str(&format!("Linked: {}\n", linked_targets.join(", ")));
    }

    // Errors
    if !errors.is_empty() {
        result.push_str(&format!("\nErrors ({}):\n", errors.len()));
        for e in errors.iter().take(20) {
            result.push_str(&format!("  {}\n", e));
        }
        if errors.len() > 20 {
            result.push_str(&format!("  ... +{} more\n", errors.len() - 20));
        }
    }

    // Warnings
    if !warnings.is_empty() {
        result.push_str(&format!("\nWarnings ({}):\n", warnings.len()));
        for w in warnings.iter().take(10) {
            result.push_str(&format!("  {}\n", w));
        }
        if warnings.len() > 10 {
            result.push_str(&format!("  ... +{} more\n", warnings.len() - 10));
        }
    }

    // Test results (xcodebuild test)
    let total_tests = tests_passed + tests_failed;
    if total_tests > 0 {
        if tests_failed == 0 {
            result.push_str(&format!("\nTests: {} passed, 0 failed\n", tests_passed));
        } else {
            result.push_str(&format!(
                "\nTests: {} passed, {} failed (of {})\n",
                tests_passed, tests_failed, total_tests
            ));
            for f in test_failures.iter().take(20) {
                result.push_str(&format!("  FAIL {}\n", f));
            }
            if test_failures.len() > 20 {
                result.push_str(&format!(
                    "  ... +{} more\n",
                    test_failures.len() - 20
                ));
            }
        }
    }

    // Final result
    if !build_result.is_empty() {
        result.push_str(&format!("\n{}\n", build_result));
    }

    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_tokens(text: &str) -> usize {
        text.split_whitespace().count()
    }

    #[test]
    fn test_filter_xcodebuild_format() {
        let input = include_str!("../../../tests/fixtures/xcodebuild_raw.txt");
        let output = filter_xcodebuild(input);
        assert!(output.contains("xcodebuild"));
        // Verbose compiler invocations should be stripped
        assert!(!output.contains("swift-frontend"));
        assert!(!output.contains("CreateBuildDirectory"));
    }

    #[test]
    fn test_filter_xcodebuild_savings() {
        let input = include_str!("../../../tests/fixtures/xcodebuild_raw.txt");
        let output = filter_xcodebuild(input);

        let input_tokens = count_tokens(input);
        let output_tokens = count_tokens(&output);
        let savings = 100.0 - (output_tokens as f64 / input_tokens as f64 * 100.0);

        assert!(
            savings >= 60.0,
            "xcodebuild filter: expected >=60% savings, got {:.1}% (in={}, out={})",
            savings,
            input_tokens,
            output_tokens
        );
    }

    #[test]
    fn test_filter_xcodebuild_empty() {
        let output = filter_xcodebuild("");
        assert!(output.contains("xcodebuild"));
    }

    #[test]
    fn test_filter_xcodebuild_build_failed() {
        let input = "SwiftCompile normal arm64 /p/Foo.swift (in target 'App' from project 'App')\n\
            /p/Foo.swift:10:5: error: cannot find 'Bar' in scope\n\
            ** BUILD FAILED **\n";
        let output = filter_xcodebuild(input);
        assert!(output.contains("Errors (1)"));
        assert!(output.contains("BUILD FAILED"));
    }

    #[test]
    fn test_filter_xcodebuild_test_results() {
        let input = "\
Test Suite 'All tests' started at 2026-04-12 10:00:00.000.
Test Suite 'FooTests.xctest' started at 2026-04-12 10:00:00.001.
Test Case '-[FooTests.BarTests testAdd]' started.
Test Case '-[FooTests.BarTests testAdd]' passed (0.003 seconds).
Test Case '-[FooTests.BarTests testSub]' started.
Test Case '-[FooTests.BarTests testSub]' passed (0.001 seconds).
Test Case '-[FooTests.BarTests testDiv]' started.
Test Case '-[FooTests.BarTests testDiv]' failed (0.002 seconds).
Test Suite 'BarTests' passed at 2026-04-12 10:00:00.010.
\t Executed 3 tests, with 1 failures (1 unexpected) in 0.006 (0.010) seconds
** TEST FAILED **
";
        let output = filter_xcodebuild(input);
        assert!(output.contains("Tests: 2 passed, 1 failed (of 3)"));
        assert!(output.contains("FAIL BarTests.testDiv"));
        assert!(output.contains("TEST FAILED"));
        // Noise stripped
        assert!(!output.contains("Test Suite"));
        assert!(!output.contains("started"));
    }
}
