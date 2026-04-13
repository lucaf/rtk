//! Filters `xcodebuild` output — strips verbose compiler invocations, DerivedData paths,
//! and build system noise. Keeps errors, warnings, resolved packages, and final status.

use crate::core::runner;
use crate::core::utils::{resolved_command, strip_ansi};
use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;

pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    let mut cmd = resolved_command("xcodebuild");
    cmd.args(args);

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
    // Error/warning lines — covers multiple xcodebuild formats. Path prefix may
    // contain spaces (e.g. "/tmp/metal code examples/Foo.xcodeproj"), so we use
    // `.+?` (non-greedy) instead of `\S+`.
    //   /path/File.swift:10:5: error: message       (source file error)
    //   /path/Project.xcodeproj: error: message     (project-level error, e.g. signing)
    //   error: message                              (top-level error)
    static ref ERROR_RE: Regex = Regex::new(r"^(?:.+?:\s+)?error:\s").unwrap();
    static ref WARNING_RE: Regex = Regex::new(r"^(?:.+?:\s+)?warning:\s").unwrap();
    // ** BUILD SUCCEEDED ** / ** BUILD FAILED ** / ** TEST SUCCEEDED ** etc.
    static ref BUILD_RESULT_RE: Regex =
        Regex::new(r"^\*\* (BUILD|TEST|CLEAN) (SUCCEEDED|FAILED) \*\*").unwrap();
    // XCTest result (legacy/swift-driver format):
    //   Test Case '-[Module.Suite testName]' passed/failed (0.003 seconds).
    // Test name uses [^\]]+ (anything but closing bracket) to support unicode
    // method names like testPasses_中文 or test絵文字_🚀.
    static ref XCTEST_RESULT_RE: Regex =
        Regex::new(r"^Test Case '-\[(\S+)\.(\S+) ([^\]]+)\]' (passed|failed) \(([0-9.]+) seconds\)").unwrap();
    // XCTest result (modern xcodebuild format on Xcode 26+):
    //   Test case 'Suite.testName()' passed on 'My Mac - xctest (PID)' (0.003 seconds)
    //   Test case 'Suite.testName()' failed on 'My Mac - xctest (PID)' (0.003 seconds)
    // captures: 1=suite, 2=testName, 3=passed/failed, 4=duration
    static ref XCB_TEST_RESULT_RE: Regex =
        Regex::new(r"^Test case '(\S+?)\.(\S+?)\(\)' (passed|failed) on '[^']+' \(([0-9.]+) seconds\)").unwrap();
    // Noise patterns to strip. The indented-path prefix `\s+/(?:Applications|usr|
    // Library/Developer|var/folders)/` targets compiler/toolchain invocation lines
    // but deliberately avoids the broader `\s+/` (which also matched Swift
    // diagnostic continuation lines like `    /path/File.swift:10: note: ...`).
    static ref NOISE_RE: Regex =
        Regex::new(r"^(?:CreateBuildDirectory|cd |/Applications/Xcode|Build description|note: |User defaults|Command line invocation|\s+/(?:Applications|usr|Library/Developer|var/folders)/|Test Suite |Test Case .* started)").unwrap();
}

/// Returns true if the output contains any marker we know how to filter:
/// compile/link/emit lines, build result banners, XCTest results, errors, warnings,
/// or resolved packages. If none are present, the output is from an informational
/// subcommand (e.g. `-list`, `-version`, `-showsdks`) and should passthrough unchanged.
fn looks_like_build_output(clean: &str) -> bool {
    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if COMPILE_RE.is_match(trimmed)
            || LINK_RE.is_match(trimmed)
            || EMIT_MODULE_RE.is_match(trimmed)
            || BUILD_RESULT_RE.is_match(trimmed)
            || XCTEST_RESULT_RE.is_match(trimmed)
            || XCB_TEST_RESULT_RE.is_match(trimmed)
            || ERROR_RE.is_match(trimmed)
            || WARNING_RE.is_match(trimmed)
            || RESOLVED_PKG_RE.is_match(trimmed)
        {
            return true;
        }
    }
    false
}

fn filter_xcodebuild(output: &str) -> String {
    let clean = strip_ansi(output);

    // Passthrough for informational subcommands (-list, -version, -showsdks, etc.)
    // that produce output with none of our expected markers. Empty input keeps the
    // legacy "xcodebuild" header (treated as a successful silent build).
    if !clean.trim().is_empty() && !looks_like_build_output(&clean) {
        return clean.trim().to_string();
    }

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

        // XCTest results (legacy format)
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

        // XCTest results (modern xcodebuild format on Xcode 26+).
        // The same test result may appear multiple times (parallel test execution
        // reports both worker and aggregate). Dedup is acceptable since the legacy
        // format also has this property — counts are best-effort approximations.
        if let Some(caps) = XCB_TEST_RESULT_RE.captures(trimmed) {
            if &caps[3] == "passed" {
                tests_passed += 1;
            } else {
                tests_failed += 1;
                test_failures.push(format!(
                    "{}.{} ({}s)",
                    &caps[1], &caps[2], &caps[4]
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
    fn test_filter_xcodebuild_passthrough_list() {
        // `xcodebuild -list` output: no build markers, must passthrough.
        let input = "Information about project \"CustomMetalView\":\n\
            Targets:\n\
                CustomMetalView-macOS\n\
                CustomMetalView-iOS\n\
            Schemes:\n\
                CustomMetalView-iOS\n";
        let output = filter_xcodebuild(input);
        assert!(output.contains("Information about project"));
        assert!(output.contains("CustomMetalView-iOS"));
        assert!(output.contains("Schemes:"));
    }

    #[test]
    fn test_filter_xcodebuild_passthrough_version() {
        let input = "Xcode 26.4\nBuild version 17E192\n";
        let output = filter_xcodebuild(input);
        assert!(output.contains("Xcode 26.4"));
        assert!(output.contains("Build version 17E192"));
    }

    #[test]
    fn test_filter_xcodebuild_passthrough_showsdks() {
        let input = "iOS SDKs:\n\
            iOS 18.0                 -sdk iphoneos18.0\n\n\
            macOS SDKs:\n\
            macOS 15.0               -sdk macosx15.0\n";
        let output = filter_xcodebuild(input);
        assert!(output.contains("iOS SDKs:"));
        assert!(output.contains("iphoneos18.0"));
        assert!(output.contains("macOS SDKs:"));
    }

    #[test]
    fn test_filter_xcodebuild_project_level_error() {
        // Real-world case: signing/provisioning errors are project-level, not source-level.
        // Format: /path/Project.xcodeproj: error: ...
        let input = "SwiftCompile normal arm64 /p/Foo.swift (in target 'App' from project 'App')\n\
            /tmp/App.xcodeproj: error: Signing for \"App\" requires a development team.\n\
            ** BUILD FAILED **\n";
        let output = filter_xcodebuild(input);
        assert!(output.contains("Errors (1)"));
        assert!(output.contains("Signing for"));
        assert!(output.contains("BUILD FAILED"));
    }

    #[test]
    fn test_filter_xcodebuild_error_with_spaces_in_path() {
        // Paths with spaces (like "/tmp/metal code examples/...") must still match.
        let input = "SwiftCompile normal arm64 /p/Foo.swift (in target 'App' from project 'App')\n\
            /tmp/metal code examples/App.xcodeproj: error: Signing requires a development team.\n\
            ** BUILD FAILED **\n";
        let output = filter_xcodebuild(input);
        assert!(output.contains("Errors (1)"));
        assert!(output.contains("Signing requires"));
    }

    #[test]
    fn test_filter_xcodebuild_toplevel_error() {
        // Format: error: ...  (no path prefix)
        let input = "SwiftCompile normal arm64 /p/Foo.swift (in target 'App' from project 'App')\n\
            error: no such module 'Foundation'\n\
            ** BUILD FAILED **\n";
        let output = filter_xcodebuild(input);
        assert!(output.contains("Errors (1)"));
        assert!(output.contains("no such module"));
    }

    #[test]
    fn test_filter_xcodebuild_strips_tool_invocations_but_preserves_diagnostics() {
        // Tool invocation paths under /Applications/Xcode and /usr/ are noise.
        // Diagnostic continuation lines (paths under /Users or /tmp with :line:col:
        // format) must be preserved — these carry error context.
        let input = "\
SwiftCompile normal arm64 /Users/dev/App/Foo.swift (in target 'App' from project 'App')
    cd /Users/dev/App
    /Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/swift-frontend -c
    /usr/bin/clang -fmodules
/Users/dev/App/Foo.swift:10:5: error: cannot find 'Bar' in scope
    /Users/dev/App/Foo.swift:15:1: note: did you mean 'Baz'?
** BUILD FAILED **
";
        let output = filter_xcodebuild(input);
        // Error captured
        assert!(output.contains("cannot find 'Bar' in scope"), "got: {}", output);
        // Tool invocations stripped
        assert!(!output.contains("swift-frontend"), "got: {}", output);
        assert!(!output.contains("/usr/bin/clang"), "got: {}", output);
        assert!(!output.contains("Applications/Xcode"), "got: {}", output);
        // Build failed banner propagated
        assert!(output.contains("BUILD FAILED"));
        // NOTE: note: continuation is dropped by `note: ` NOISE_RE pattern — that
        // existing behavior is preserved; only the `\s+/` over-strip was narrowed.
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
    fn test_filter_xcodebuild_test_results_modern_format() {
        // Modern xcodebuild (Xcode 26+) test format with parenthesized method name
        // and 'on My Mac - xctest (PID)' clause.
        let input = "\
Test case 'UniqueTests.testUnique()' passed on 'My Mac - xctest (1234)' (0.001 seconds)
Test case 'UniqueTests.testInjected()' failed on 'My Mac - xctest (1234)' (0.005 seconds)
Test case 'OtherTests.testFoo()' passed on 'My Mac - xctest (5678)' (0.002 seconds)
** TEST FAILED **
";
        let output = filter_xcodebuild(input);
        assert!(output.contains("Tests: 2 passed, 1 failed (of 3)"), "got: {}", output);
        assert!(output.contains("FAIL UniqueTests.testInjected"), "got: {}", output);
        assert!(output.contains("TEST FAILED"));
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
