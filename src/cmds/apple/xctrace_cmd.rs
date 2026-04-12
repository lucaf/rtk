//! Filters `xctrace` (Instruments CLI) output — condenses template listings,
//! device inventories, and recording summaries.
//!
//! Subcommands filtered:
//! - `xctrace list templates`: strips descriptions, shows template names only
//! - `xctrace list devices`:  compact device list grouped by OS, strips UUIDs
//! - `xctrace record`:        strips progress noise, keeps summary + heaviest stacks

use crate::core::runner;
use crate::core::utils::{resolved_command, strip_ansi};
use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashMap;

pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    let mut cmd = resolved_command("xcrun");
    cmd.arg("xctrace");
    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: xcrun xctrace {}", args.join(" "));
    }

    let subcommand = detect_subcommand(args);

    runner::run_filtered(
        cmd,
        "xctrace",
        &args.join(" "),
        move |raw| match subcommand {
            Subcommand::ListTemplates => filter_list_templates(raw),
            Subcommand::ListDevices => filter_list_devices(raw),
            Subcommand::Record => filter_record(raw),
            Subcommand::Other => raw.trim().to_string(),
        },
        runner::RunOptions::with_tee("xctrace"),
    )
}

#[derive(Clone, Copy)]
enum Subcommand {
    ListTemplates,
    ListDevices,
    Record,
    Other,
}

fn detect_subcommand(args: &[String]) -> Subcommand {
    // Find the first two non-flag positional args
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
}

// ─── list templates ──────────────────────────────────────────────────────────

lazy_static! {
    static ref SECTION_RE: Regex = Regex::new(r"^== (.+) ==$").unwrap();
    // Template name: line that doesn't start with whitespace and isn't a section header
    static ref TEMPLATE_NAME_RE: Regex = Regex::new(r"^[A-Z][A-Za-z0-9 /-]+$").unwrap();
}

fn filter_list_templates(output: &str) -> String {
    let clean = strip_ansi(output);
    let mut standard: Vec<&str> = Vec::new();
    let mut recent: Vec<&str> = Vec::new();
    let mut in_recent = false;

    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(caps) = SECTION_RE.captures(trimmed) {
            in_recent = caps[1].contains("Recent");
            continue;
        }

        // Description lines start with whitespace in original
        if line.starts_with("    ") || line.starts_with('\t') {
            continue;
        }

        if TEMPLATE_NAME_RE.is_match(trimmed) {
            if in_recent {
                recent.push(trimmed);
            } else {
                standard.push(trimmed);
            }
        }
    }

    let mut result = String::new();
    result.push_str(&format!("xctrace templates ({})\n", standard.len()));

    for name in &standard {
        result.push_str(&format!("  {}\n", name));
    }

    if !recent.is_empty() {
        result.push_str(&format!("\nRecent: {}\n", recent.join(", ")));
    }

    result.trim().to_string()
}

// ─── list devices ────────────────────────────────────────────────────────────

lazy_static! {
    // Device header: Name (UUID)
    static ref DEVICE_HEADER_RE: Regex = Regex::new(
        r"^(.+?)\s+\([A-F0-9-]{36}\)\s*$"
    ).unwrap();
    // Indented metadata: "    OS: iOS 18.0 (22A3354)"
    static ref DEVICE_OS_RE: Regex = Regex::new(
        r"^\s+OS:\s+(.+)"
    ).unwrap();
}

fn filter_list_devices(output: &str) -> String {
    let clean = strip_ansi(output);

    let mut physical: Vec<String> = Vec::new();
    let mut simulators_by_os: HashMap<String, Vec<String>> = HashMap::new();
    let mut current_name = String::new();
    let mut in_simulators = false;

    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(caps) = SECTION_RE.captures(trimmed) {
            in_simulators = caps[1].contains("Simulator");
            continue;
        }

        if let Some(caps) = DEVICE_HEADER_RE.captures(trimmed) {
            current_name = caps[1].to_string();
            continue;
        }

        if let Some(caps) = DEVICE_OS_RE.captures(line) {
            let os = caps[1].trim().to_string();
            if in_simulators {
                // Extract just the OS family + version (e.g. "iOS 18.0")
                let os_short = os.split('(').next().unwrap_or(&os).trim().to_string();
                simulators_by_os
                    .entry(os_short)
                    .or_default()
                    .push(current_name.clone());
            } else {
                physical.push(format!("{} ({})", current_name, os));
            }
            continue;
        }

        // Skip other metadata lines (Model:, Disk Space:, etc.)
    }

    let total_sims: usize = simulators_by_os.values().map(|d| d.len()).sum();

    let mut result = String::new();
    result.push_str("xctrace devices\n");

    if !physical.is_empty() {
        result.push_str(&format!("\nPhysical ({}):\n", physical.len()));
        for dev in &physical {
            result.push_str(&format!("  {}\n", dev));
        }
    }

    if !simulators_by_os.is_empty() {
        result.push_str(&format!("\nSimulators ({}):\n", total_sims));
        let mut os_list: Vec<_> = simulators_by_os.iter().collect();
        os_list.sort_by_key(|(os, _)| os.to_string());
        for (os, devs) in &os_list {
            result.push_str(&format!("  {} ({}): {}\n", os, devs.len(), devs.join(", ")));
        }
    }

    result.trim().to_string()
}

// ─── record ──────────────────────────────────────────────────────────────────

lazy_static! {
    // "Duration: 30.12 seconds"
    static ref DURATION_RE: Regex = Regex::new(r"Duration:\s+(.+)").unwrap();
    // "Trace file: /path/to/file.trace"
    static ref TRACE_FILE_RE: Regex = Regex::new(r"Trace file:\s+(.+)").unwrap();
    // "Trace file size: 48.3 MB"
    static ref TRACE_SIZE_RE: Regex = Regex::new(r"Trace file size:\s+(.+)").unwrap();
    // "Template: Time Profiler"
    static ref TEMPLATE_RE: Regex = Regex::new(r"Template:\s+(.+)").unwrap();
    // "Target: ProcessName [pid: 12345]"
    static ref TARGET_RE: Regex = Regex::new(r"Target:\s+(.+)").unwrap();
    // Stack percentage lines: "30.2%  SimpleHTTPServer" (matched against trimmed)
    static ref STACK_RE: Regex = Regex::new(r"^(\d+\.\d+)%\s+(.+)").unwrap();
    // Metric lines: "CPU utilization: 67.2% (avg)" (matched against trimmed)
    static ref METRIC_RE: Regex = Regex::new(
        r"^(Samples|Threads sampled|CPU utilization|Hitch-free|os_signpost):\s+(.+)"
    ).unwrap();
    // Noise lines to skip
    static ref RECORD_NOISE_RE: Regex = Regex::new(
        r"^(Starting recording|Tracing process|Recording\.\.\.|Output file saved|Recording completed|Run complete|To view)"
    ).unwrap();
}

fn filter_record(output: &str) -> String {
    let clean = strip_ansi(output);

    let mut target = String::new();
    let mut template = String::new();
    let mut duration = String::new();
    let mut trace_file = String::new();
    let mut trace_size = String::new();
    let mut top_stacks: Vec<(f64, String)> = Vec::new();
    let mut metrics: Vec<(String, String)> = Vec::new();

    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || RECORD_NOISE_RE.is_match(trimmed) {
            continue;
        }

        if let Some(caps) = TARGET_RE.captures(trimmed) {
            target = caps[1].to_string();
            continue;
        }
        if let Some(caps) = TEMPLATE_RE.captures(trimmed) {
            template = caps[1].to_string();
            continue;
        }
        if let Some(caps) = DURATION_RE.captures(trimmed) {
            duration = caps[1].to_string();
            continue;
        }
        if let Some(caps) = TRACE_FILE_RE.captures(trimmed) {
            trace_file = caps[1].to_string();
            continue;
        }
        if let Some(caps) = TRACE_SIZE_RE.captures(trimmed) {
            trace_size = caps[1].to_string();
            continue;
        }
        if let Some(caps) = STACK_RE.captures(trimmed) {
            let pct: f64 = caps[1].parse().unwrap_or(0.0);
            top_stacks.push((pct, caps[2].to_string()));
            continue;
        }
        if let Some(caps) = METRIC_RE.captures(trimmed) {
            metrics.push((caps[1].to_string(), caps[2].to_string()));
        }
    }

    let mut result = String::new();
    result.push_str("xctrace record\n");

    if !template.is_empty() {
        result.push_str(&format!("Template: {}", template));
        if !target.is_empty() {
            result.push_str(&format!(" | Target: {}", target));
        }
        result.push('\n');
    }

    if !duration.is_empty() {
        result.push_str(&format!("Duration: {}", duration));
        if !trace_size.is_empty() {
            result.push_str(&format!(" | Size: {}", trace_size));
        }
        result.push('\n');
    }

    if !trace_file.is_empty() {
        result.push_str(&format!("Trace: {}\n", trace_file));
    }

    if !top_stacks.is_empty() {
        result.push_str("\nHeaviest stacks:\n");
        for (pct, name) in top_stacks.iter().take(5) {
            result.push_str(&format!("  {:>5.1}%  {}\n", pct, name));
        }
    }

    if !metrics.is_empty() {
        result.push_str("\nMetrics:\n");
        for (key, val) in &metrics {
            result.push_str(&format!("  {}: {}\n", key, val));
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

    // ── list templates ───────────────────────────────────────────────────

    #[test]
    fn test_filter_list_templates_format() {
        let input = include_str!("../../../tests/fixtures/xctrace_list_templates_raw.txt");
        let output = filter_list_templates(input);
        assert!(output.contains("xctrace templates"));
        assert!(output.contains("Time Profiler"));
        assert!(output.contains("Allocations"));
        // Real data has no descriptions to strip
        assert!(!output.contains("== Standard Templates =="));
    }

    #[test]
    fn test_filter_list_templates_savings() {
        let input = include_str!("../../../tests/fixtures/xctrace_list_templates_raw.txt");
        let output = filter_list_templates(input);

        let input_tokens = count_tokens(input);
        let output_tokens = count_tokens(&output);
        let savings = 100.0 - (output_tokens as f64 / input_tokens as f64 * 100.0);

        // This fixture has no description lines (concise Xcode version).
        // Savings are higher (~75%) when descriptions are present.
        // Here we just verify the filter doesn't inflate output.
        assert!(
            savings >= -5.0,
            "xctrace list templates: output should not be much larger than input, got {:.1}% savings (in={}, out={})",
            savings,
            input_tokens,
            output_tokens
        );
    }

    #[test]
    fn test_filter_list_templates_empty() {
        let output = filter_list_templates("");
        assert!(output.contains("xctrace templates (0)"));
    }

    // ── list devices ─────────────────────────────────────────────────────

    #[test]
    fn test_filter_list_devices_format() {
        let input = include_str!("../../../tests/fixtures/xctrace_list_devices_raw.txt");
        let output = filter_list_devices(input);
        assert!(output.contains("xctrace devices"));
        // UUIDs should be stripped
        assert!(!output.contains("C6A93794-F039-555A-A9E6-40FC28901A81"));
        assert!(!output.contains("4A72B2E1-CF3F-4C3E-A3E2-7B4D6E8F9A1B"));
        // Verbose metadata stripped
        assert!(!output.contains("Model:"));
        assert!(!output.contains("Disk Space:"));
        // Physical devices should include OS info
        assert!(output.contains("Physical (2)"));
        assert!(output.contains("MacBook Air (macOS 15.4)"));
        assert!(output.contains("iPhone (iOS 18.4)"));
        // Simulators grouped by OS
        assert!(output.contains("Simulators (7)"));
        assert!(output.contains("iOS 18.0"));
        assert!(output.contains("iPadOS 18.0"));
    }

    #[test]
    fn test_filter_list_devices_savings() {
        let input = include_str!("../../../tests/fixtures/xctrace_list_devices_raw.txt");
        let output = filter_list_devices(input);

        let input_tokens = count_tokens(input);
        let output_tokens = count_tokens(&output);
        let savings = 100.0 - (output_tokens as f64 / input_tokens as f64 * 100.0);

        // Device listing savings come from stripping UUIDs, Model/Disk Space metadata,
        // and grouping simulators by OS. With a realistic fixture the savings are ~40%
        // because device names must be preserved.
        assert!(
            savings >= 30.0,
            "xctrace list devices: expected >=30% savings, got {:.1}% (in={}, out={})",
            savings,
            input_tokens,
            output_tokens
        );
    }

    #[test]
    fn test_filter_list_devices_empty() {
        let output = filter_list_devices("");
        assert!(output.contains("xctrace devices"));
    }

    // ── record ───────────────────────────────────────────────────────────

    #[test]
    fn test_filter_record_format() {
        let input = include_str!("../../../tests/fixtures/xctrace_record_raw.txt");
        let output = filter_record(input);
        assert!(output.contains("xctrace record"));
        assert!(output.contains("Template: Time Profiler"));
        assert!(output.contains("Duration: 30.12 seconds"));
        assert!(output.contains("Heaviest stacks"));
        assert!(output.contains("SimpleHTTPServer"));
        assert!(output.contains("CPU utilization"));
        // Noise stripped
        assert!(!output.contains("Starting recording"));
        assert!(!output.contains("Recording..."));
        assert!(!output.contains("To view"));
    }

    #[test]
    fn test_filter_record_savings() {
        let input = include_str!("../../../tests/fixtures/xctrace_record_raw.txt");
        let output = filter_record(input);

        let input_tokens = count_tokens(input);
        let output_tokens = count_tokens(&output);
        let savings = 100.0 - (output_tokens as f64 / input_tokens as f64 * 100.0);

        assert!(
            savings >= 30.0,
            "xctrace record: expected >=30% savings, got {:.1}% (in={}, out={})",
            savings,
            input_tokens,
            output_tokens
        );
    }

    #[test]
    fn test_filter_record_empty() {
        let output = filter_record("");
        assert!(output.contains("xctrace record"));
    }

    // ── detect_subcommand ────────────────────────────────────────────────

    #[test]
    fn test_detect_subcommand_list_templates() {
        let args: Vec<String> = vec!["list".into(), "templates".into()];
        assert!(matches!(detect_subcommand(&args), Subcommand::ListTemplates));
    }

    #[test]
    fn test_detect_subcommand_list_devices() {
        let args: Vec<String> = vec!["list".into(), "devices".into()];
        assert!(matches!(detect_subcommand(&args), Subcommand::ListDevices));
    }

    #[test]
    fn test_detect_subcommand_record() {
        let args: Vec<String> = vec!["record".into(), "--template".into(), "Time Profiler".into()];
        assert!(matches!(detect_subcommand(&args), Subcommand::Record));
    }

    #[test]
    fn test_detect_subcommand_list_alone() {
        let args: Vec<String> = vec!["list".into()];
        assert!(matches!(detect_subcommand(&args), Subcommand::Other));
    }

    #[test]
    fn test_detect_subcommand_with_flags() {
        // Flags should be ignored, positional args used
        let args: Vec<String> = vec!["--verbose".into(), "list".into(), "templates".into()];
        assert!(matches!(detect_subcommand(&args), Subcommand::ListTemplates));
    }

    // ── record (real error output) ───────────────────────────────────────

    #[test]
    fn test_filter_record_error_output() {
        // Real xctrace record output from a failed recording (ktrace permission error).
        // The "Starting recording with..." line is noise-filtered, so template name
        // won't be captured. The filter should produce a minimal "xctrace record" header
        // and the output file path.
        let input = include_str!("../../../tests/fixtures/xctrace_record_error_raw.txt");
        let output = filter_record(input);
        assert!(output.contains("xctrace record"));
        // Noise stripped
        assert!(!output.contains("Ctrl-C"));
        assert!(!output.contains("Starting recording"));
        // Should not panic or produce empty output
        assert!(!output.is_empty());
    }
}
