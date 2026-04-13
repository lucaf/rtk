//! Filters SwiftLint output — groups violations by rule, shortens paths, caps output.
//!
//! SwiftLint default output format:
//! ```text
//! /path/to/File.swift:10:5: warning: Line Length Violation: ... (line_length)
//! /path/to/File.swift:23:1: error: Force Cast Violation: ... (force_cast)
//! ```
//!
//! Filtered output groups by rule with file counts and shows the summary.

use crate::core::runner;
use crate::core::utils::{resolved_command, strip_ansi};
use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::{HashMap, HashSet};

pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    let mut cmd = resolved_command("swiftlint");
    cmd.args(args);

    if verbose > 0 {
        eprintln!("Running: swiftlint {}", args.join(" "));
    }

    runner::run_filtered(
        cmd,
        "swiftlint",
        &args.join(" "),
        filter_swiftlint,
        runner::RunOptions::with_tee("swiftlint"),
    )
}

lazy_static! {
    // /path/to/File.swift:10:5: warning: Line Length Violation: ... (line_length)
    static ref VIOLATION_RE: Regex = Regex::new(
        r"^(.+?):(\d+):\d+:\s+(warning|error):\s+(.+?)\s+\((\w+)\)\s*$"
    ).unwrap();

    // Done linting! Found 47 violations, 3 serious in 12 files.
    static ref SUMMARY_RE: Regex = Regex::new(
        r"(?i)^Done linting!.*Found (\d+) violations?,\s*(\d+) serious"
    ).unwrap();

    // Corrected N violations out of M (autocorrect mode)
    static ref CORRECTED_RE: Regex = Regex::new(
        r"(?i)^Done correcting!|corrected \d+ violations?"
    ).unwrap();
}

#[derive(Default)]
struct RuleStats {
    errors: u32,
    warnings: u32,
    files: HashSet<String>,
}

/// Returns true if output has any marker we know how to filter: a violation line,
/// a lint summary, or a corrected-violations line. Otherwise the output is from an
/// informational subcommand (e.g. `swiftlint version`, `swiftlint rules`) and should
/// passthrough unchanged.
fn looks_like_lint_output(clean: &str) -> bool {
    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if VIOLATION_RE.is_match(trimmed)
            || SUMMARY_RE.is_match(trimmed)
            || CORRECTED_RE.is_match(trimmed)
        {
            return true;
        }
    }
    false
}

fn filter_swiftlint(output: &str) -> String {
    let clean = strip_ansi(output);

    // Passthrough for informational subcommands (version, rules, reporters, docs)
    // that produce no violation/summary lines. Empty input keeps the legacy
    // "No violations" behavior (treated as a clean lint pass).
    if !clean.trim().is_empty() && !looks_like_lint_output(&clean) {
        return clean.trim().to_string();
    }

    let mut by_rule: HashMap<String, RuleStats> = HashMap::new();
    let mut summary_line = String::new();
    let mut corrected_line = String::new();

    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(caps) = VIOLATION_RE.captures(trimmed) {
            let file = shorten_path(&caps[1]);
            // caps[2] is the line number — unused by the aggregator but kept in
            // VIOLATION_RE so future work can surface per-file hot lines.
            let severity = &caps[3];
            let rule_id = caps[5].to_string();

            let stats = by_rule.entry(rule_id).or_default();
            if severity == "error" {
                stats.errors += 1;
            } else {
                stats.warnings += 1;
            }
            stats.files.insert(file);
            continue;
        }

        if SUMMARY_RE.is_match(trimmed) {
            summary_line = trimmed.to_string();
            continue;
        }

        if CORRECTED_RE.is_match(trimmed) {
            corrected_line = trimmed.to_string();
        }
    }

    let total_violations: u32 = by_rule.values().map(|s| s.errors + s.warnings).sum();
    let total_errors: u32 = by_rule.values().map(|s| s.errors).sum();
    let total_warnings: u32 = by_rule.values().map(|s| s.warnings).sum();

    let mut result = String::new();
    result.push_str("swiftlint\n");

    if by_rule.is_empty() {
        if !corrected_line.is_empty() {
            result.push_str(&corrected_line);
            result.push('\n');
        } else if !summary_line.is_empty() {
            result.push_str(&summary_line);
            result.push('\n');
        } else {
            result.push_str("No violations\n");
        }
        return result.trim().to_string();
    }

    result.push_str(&format!(
        "{} violations ({} errors, {} warnings)\n\n",
        total_violations, total_errors, total_warnings
    ));

    // Sort rules: errors first, then by count descending
    let mut rules: Vec<_> = by_rule.iter().collect();
    rules.sort_by(|a, b| {
        b.1.errors
            .cmp(&a.1.errors)
            .then_with(|| (b.1.errors + b.1.warnings).cmp(&(a.1.errors + a.1.warnings)))
    });

    for (rule, stats) in rules.iter().take(20) {
        let count = stats.errors + stats.warnings;
        let severity_tag = if stats.errors > 0 { "E" } else { "W" };
        let file_count = stats.files.len();

        result.push_str(&format!(
            "  [{}] {} (x{}, {} files)\n",
            severity_tag, rule, count, file_count
        ));
    }

    if rules.len() > 20 {
        result.push_str(&format!("  ... +{} more rules\n", rules.len() - 20));
    }

    // Append swiftlint's own summary line (if present) so the user can verify the
    // parsed counts against swiftlint's authoritative totals.
    if !summary_line.is_empty() {
        result.push('\n');
        result.push_str(&summary_line);
        result.push('\n');
    }

    result.trim().to_string()
}

/// Shorten a file path by keeping only the last 2 components.
fn shorten_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 2 {
        path.to_string()
    } else {
        parts[parts.len() - 2..].join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_tokens(text: &str) -> usize {
        text.split_whitespace().count()
    }

    #[test]
    fn test_filter_swiftlint_format() {
        let input = include_str!("../../../tests/fixtures/swiftlint_raw.txt");
        let output = filter_swiftlint(input);
        assert!(output.contains("swiftlint"));
        assert!(output.contains("violations"));
        assert!(output.contains("line_length"));
        assert!(output.contains("force_cast"));
        // Verbose per-line violations should be grouped
        assert!(!output.contains("/Users/"));
    }

    #[test]
    fn test_filter_swiftlint_savings() {
        let input = include_str!("../../../tests/fixtures/swiftlint_raw.txt");
        let output = filter_swiftlint(input);

        let input_tokens = count_tokens(input);
        let output_tokens = count_tokens(&output);
        let savings = 100.0 - (output_tokens as f64 / input_tokens as f64 * 100.0);

        assert!(
            savings >= 60.0,
            "swiftlint filter: expected >=60% savings, got {:.1}% (in={}, out={})",
            savings,
            input_tokens,
            output_tokens
        );
    }

    #[test]
    fn test_filter_swiftlint_empty() {
        let output = filter_swiftlint("");
        assert!(output.contains("No violations"));
    }

    #[test]
    fn test_filter_swiftlint_passthrough_version() {
        let output = filter_swiftlint("0.63.2\n");
        assert_eq!(output, "0.63.2");
    }

    #[test]
    fn test_filter_swiftlint_passthrough_rules() {
        // `swiftlint rules` output — table format, no violation lines.
        let input = "+-------------------+---------+---------+\n\
            | identifier        | opt-in  | correct |\n\
            +-------------------+---------+---------+\n\
            | line_length       | no      | no      |\n\
            | force_cast        | no      | no      |\n\
            +-------------------+---------+---------+\n";
        let output = filter_swiftlint(input);
        assert!(output.contains("line_length"));
        assert!(output.contains("force_cast"));
        assert!(output.contains("identifier"));
    }

    #[test]
    fn test_filter_swiftlint_no_violations() {
        let input = "Done linting! Found 0 violations, 0 serious in 5 files.\n";
        let output = filter_swiftlint(input);
        assert!(output.contains("Done linting!"));
    }

    #[test]
    fn test_filter_swiftlint_groups_by_rule() {
        let input = "\
/p/A.swift:1:1: warning: Trailing Whitespace Violation: desc (trailing_whitespace)
/p/A.swift:5:1: warning: Trailing Whitespace Violation: desc (trailing_whitespace)
/p/B.swift:2:1: warning: Trailing Whitespace Violation: desc (trailing_whitespace)
/p/A.swift:10:5: error: Force Cast Violation: desc (force_cast)
Done linting! Found 4 violations, 1 serious in 2 files.
";
        let output = filter_swiftlint(input);
        assert!(output.contains("4 violations (1 errors, 3 warnings)"));
        assert!(output.contains("[E] force_cast (x1"));
        assert!(output.contains("[W] trailing_whitespace (x3"));
    }

    #[test]
    fn test_shorten_path() {
        assert_eq!(
            shorten_path("/Users/luca/Project/Sources/File.swift"),
            "Sources/File.swift"
        );
        assert_eq!(shorten_path("File.swift"), "File.swift");
        assert_eq!(shorten_path("Sources/File.swift"), "Sources/File.swift");
    }
}
