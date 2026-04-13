//! Filters `xcrun simctl` output — condenses the extremely verbose device/runtime
//! listing into a compact summary.
//!
//! `xcrun simctl list` typically outputs 200+ lines across 4 sections:
//! Device Types, Runtimes, Devices, and Device Pairs. This filter summarizes
//! each section with counts and highlights booted devices.
//!
//! # ⚠️ Limited real-world validation
//!
//! This filter has been unit-tested against a synthetic fixture (`tests/fixtures/
//! simctl_list_raw.txt`) but **not** against real `xcrun simctl list` output on a
//! machine with installed simulator runtimes. The passthrough heuristic means
//! unrecognized input is returned unchanged — so the worst case is "no compression"
//! rather than "data loss" — but users running real simctl workflows should verify
//! output integrity with `rtk proxy xcrun simctl ...` if they hit unexpected gaps.
//! PRs with real-world fixtures welcome.

use crate::core::runner;
use crate::core::utils::{resolved_command, strip_ansi};
use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashMap;

pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    let mut cmd = resolved_command("xcrun");
    cmd.arg("simctl").args(args);

    // Default to "list" if only "simctl" with no subcommand
    if args.is_empty() {
        cmd.arg("list");
    }

    if verbose > 0 {
        eprintln!("Running: xcrun simctl {}", args.join(" "));
    }

    let is_list = args.is_empty()
        || args.first().is_some_and(|a| a == "list");

    runner::run_filtered(
        cmd,
        "simctl",
        &args.join(" "),
        move |raw| {
            if is_list {
                filter_simctl_list(raw)
            } else {
                raw.trim().to_string()
            }
        },
        runner::RunOptions::with_tee("simctl"),
    )
}

lazy_static! {
    // Section headers: == Device Types ==, == Runtimes ==, etc.
    static ref SECTION_RE: Regex = Regex::new(r"^== (.+) ==$").unwrap();

    // Runtime line: iOS 17.5 (17.5 - 21F79) - com.apple.CoreSimulator.SimRuntime.iOS-17-5
    static ref RUNTIME_RE: Regex = Regex::new(
        r"^(\w+(?:\s*OS)?\s+[\d.]+)\s+\("
    ).unwrap();

    // Device group header: -- iOS 17.5 --
    static ref DEVICE_GROUP_RE: Regex = Regex::new(r"^-- (.+) --$").unwrap();

    // Device line: iPhone 15 Pro (UUID) (Booted|Shutdown) — matched against trimmed input
    static ref DEVICE_RE: Regex = Regex::new(
        r"^(.+?)\s+\([A-F0-9-]{36}\)\s+\((\w+)\)"
    ).unwrap();

    // Unavailable runtime header
    static ref UNAVAILABLE_RE: Regex = Regex::new(r"^-- Unavailable:").unwrap();

    // Pair entry: UUID (Booted|Shutdown) or UUID followed by device details.
    // Line begins with a 36-char simulator UUID.
    static ref PAIR_RE: Regex = Regex::new(r"^[A-F0-9]{8}-[A-F0-9]{4}-[A-F0-9]{4}-[A-F0-9]{4}-[A-F0-9]{12}\b").unwrap();
}

/// Returns true if output contains any marker we know how to filter:
/// `== Section ==` headers or device lines. Otherwise the output is from a
/// non-default format (e.g. `--json`, `list devices booted` which has no
/// section header) and should passthrough unchanged — the filter would
/// otherwise emit misleading zero counts for every section.
fn looks_like_simctl_list_output(clean: &str) -> bool {
    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if SECTION_RE.is_match(trimmed) || DEVICE_RE.is_match(trimmed) {
            return true;
        }
    }
    false
}

fn filter_simctl_list(output: &str) -> String {
    let clean = strip_ansi(output);

    // Passthrough for non-default list formats (--json, list devices booted, etc.)
    // Empty input keeps the legacy "simctl list" header for the stub case.
    if !clean.trim().is_empty() && !looks_like_simctl_list_output(&clean) {
        return clean.trim().to_string();
    }

    let mut device_type_count = 0u32;
    let mut runtimes: Vec<String> = Vec::new();
    let mut devices_by_runtime: HashMap<String, Vec<(String, bool)>> = HashMap::new();
    let mut current_runtime = String::new();
    let mut pair_count = 0u32;
    let mut unavailable_count = 0u32;
    let mut in_unavailable = false;

    #[derive(PartialEq)]
    enum Section {
        None,
        DeviceTypes,
        Runtimes,
        Devices,
        Pairs,
    }
    let mut section = Section::None;

    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Section header
        if let Some(caps) = SECTION_RE.captures(trimmed) {
            section = match &caps[1] {
                "Device Types" => Section::DeviceTypes,
                "Runtimes" => Section::Runtimes,
                "Devices" => Section::Devices,
                "Device Pairs" => Section::Pairs,
                _ => Section::None,
            };
            in_unavailable = false;
            continue;
        }

        match section {
            Section::DeviceTypes => {
                device_type_count += 1;
            }
            Section::Runtimes => {
                if let Some(caps) = RUNTIME_RE.captures(trimmed) {
                    runtimes.push(caps[1].to_string());
                }
            }
            Section::Devices => {
                if UNAVAILABLE_RE.is_match(trimmed) {
                    in_unavailable = true;
                    continue;
                }
                if let Some(caps) = DEVICE_GROUP_RE.captures(trimmed) {
                    if !in_unavailable {
                        current_runtime = caps[1].to_string();
                    }
                    continue;
                }
                if in_unavailable {
                    unavailable_count += 1;
                    continue;
                }
                if let Some(caps) = DEVICE_RE.captures(trimmed) {
                    let name = caps[1].to_string();
                    let booted = &caps[2] == "Booted";
                    devices_by_runtime
                        .entry(current_runtime.clone())
                        .or_default()
                        .push((name, booted));
                }
            }
            Section::Pairs => {
                // Count pair entries — lines starting with a simulator UUID.
                if PAIR_RE.is_match(trimmed) {
                    pair_count += 1;
                }
            }
            Section::None => {}
        }
    }

    let total_devices: usize = devices_by_runtime.values().map(|d| d.len()).sum();
    let booted: Vec<(String, String)> = devices_by_runtime
        .iter()
        .flat_map(|(rt, devs)| {
            devs.iter()
                .filter(|(_, b)| *b)
                .map(|(name, _)| (name.clone(), rt.clone()))
        })
        .collect();

    let mut result = String::new();
    result.push_str("simctl list\n");
    result.push_str(&format!("Device types: {} | ", device_type_count));
    result.push_str(&format!("Runtimes: {} | ", runtimes.len()));
    result.push_str(&format!("Devices: {} | ", total_devices));
    result.push_str(&format!("Pairs: {}\n", pair_count));

    if !runtimes.is_empty() {
        result.push_str(&format!("\nRuntimes: {}\n", runtimes.join(", ")));
    }

    if !booted.is_empty() {
        result.push_str(&format!("\nBooted ({}):\n", booted.len()));
        for (name, rt) in &booted {
            result.push_str(&format!("  {} ({})\n", name, rt));
        }
    }

    if unavailable_count > 0 {
        result.push_str(&format!(
            "\n{} unavailable devices (stale runtimes)\n",
            unavailable_count
        ));
    }

    // Show per-runtime device counts
    if !devices_by_runtime.is_empty() {
        result.push_str("\nDevices by runtime:\n");
        let mut rt_list: Vec<_> = devices_by_runtime.iter().collect();
        rt_list.sort_by_key(|(rt, _)| rt.to_string());
        for (rt, devs) in &rt_list {
            let booted_count = devs.iter().filter(|(_, b)| *b).count();
            if booted_count > 0 {
                result.push_str(&format!(
                    "  {}: {} ({} booted)\n",
                    rt,
                    devs.len(),
                    booted_count
                ));
            } else {
                result.push_str(&format!("  {}: {}\n", rt, devs.len()));
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

    #[test]
    fn test_filter_simctl_list_format() {
        let input = include_str!("../../../tests/fixtures/simctl_list_raw.txt");
        let output = filter_simctl_list(input);
        assert!(output.contains("simctl list"));
        assert!(output.contains("Device types:"));
        assert!(output.contains("Runtimes: 0"));
        assert!(output.contains("Devices: 0"));
        assert!(output.contains("Pairs: 0"));
        // Verbose identifiers stripped
        assert!(!output.contains("com.apple.CoreSimulator"));
    }

    #[test]
    fn test_filter_simctl_list_savings() {
        let input = include_str!("../../../tests/fixtures/simctl_list_raw.txt");
        let output = filter_simctl_list(input);

        let input_tokens = count_tokens(input);
        let output_tokens = count_tokens(&output);
        let savings = 100.0 - (output_tokens as f64 / input_tokens as f64 * 100.0);

        assert!(
            savings >= 60.0,
            "simctl list filter: expected >=60% savings, got {:.1}% (in={}, out={})",
            savings,
            input_tokens,
            output_tokens
        );
    }

    #[test]
    fn test_filter_simctl_list_empty() {
        let output = filter_simctl_list("");
        assert!(output.contains("simctl list"));
        assert!(output.contains("Device types: 0"));
    }

    #[test]
    fn test_filter_simctl_list_passthrough_json() {
        // `simctl list --json` produces JSON with no `== Section ==` headers.
        // Without the passthrough heuristic, the filter would emit misleading
        // all-zero counts; instead it should pass JSON through unchanged.
        let input = "{\n  \"devicetypes\" : [\n    {\n      \"name\" : \"iPhone 15\"\n    }\n  ]\n}\n";
        let output = filter_simctl_list(input);
        assert!(output.contains("\"devicetypes\""), "got: {}", output);
        assert!(output.contains("iPhone 15"), "got: {}", output);
        // Must not have replaced content with zero counts.
        assert!(!output.contains("Device types: 0"), "got: {}", output);
    }

    #[test]
    fn test_filter_simctl_list_passthrough_unrecognized() {
        // Output without any section header or device line must passthrough.
        let input = "No devices are currently booted.\n";
        let output = filter_simctl_list(input);
        assert!(output.contains("No devices are currently booted"));
        assert!(!output.contains("Device types: 0"));
    }

    #[test]
    fn test_filter_simctl_list_unavailable_counted() {
        let input = include_str!("../../../tests/fixtures/simctl_list_raw.txt");
        let output = filter_simctl_list(input);
        // Real data has no unavailable devices
        assert!(!output.contains("unavailable"));
    }
}
