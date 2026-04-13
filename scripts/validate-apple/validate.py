#!/usr/bin/env python3
"""
Rigorous RTK Apple filter validation harness.

For each command, runs raw and filtered versions, extracts signals (errors,
warnings, test failures, lint violations) from each, and verifies that every
signal in raw is either:
  a) Present in filtered output (verbatim or by identifier), OR
  b) Accounted for in a truncation counter (e.g. "+N more")

Reports any signals lost silently — those are data-loss bugs.
"""
import re
import subprocess
import sys
import os
from dataclasses import dataclass, field
from typing import Callable


@dataclass
class Result:
    label: str
    raw_signals: dict = field(default_factory=dict)
    filtered_signals: dict = field(default_factory=dict)
    filtered_summary: dict = field(default_factory=dict)  # e.g. {"errors_declared": 5, "more_count": 3}
    issues: list = field(default_factory=list)
    raw_lines: int = 0
    filtered_lines: int = 0

    def ok(self) -> bool:
        return len(self.issues) == 0


def run(cmd: list[str], cwd: str) -> str:
    r = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=1800)
    return r.stdout + r.stderr


# ---- signal extractors ----

# errors like:
#   /path/File.swift:10:5: error: message
#   /path/Project.xcodeproj: error: message
#   error: top-level message
ERROR_RE = re.compile(r"^(?:(.+?):\s+)?error:\s+(.+?)$", re.MULTILINE)
WARNING_RE = re.compile(r"^(?:(.+?):\s+)?warning:\s+(.+?)$", re.MULTILINE)

# XCTest: Test Case '-[Module.Suite testName]' failed (0.003 seconds).
XCTEST_FAIL_RE = re.compile(r"Test Case '-\[\S+\.\S+ \w+\]' failed")
# Swift Testing: ✘ Test "name" failed after 0.001 seconds with N issues
SWIFT_TEST_FAIL_RE = re.compile(r"^[✘✗]\s+Test\s+\"([^\"]+)\"\s+failed", re.MULTILINE)

# swiftlint violation: path:line:col: warning/error: msg (rule_id)
VIOLATION_RE = re.compile(r"^(.+?):(\d+):\d+:\s+(warning|error):\s+.+?\s+\((\w+)\)\s*$", re.MULTILINE)


def extract_errors(text: str) -> set[str]:
    """Return set of unique error messages (after the 'error:' prefix)."""
    return {m.group(2).strip() for m in ERROR_RE.finditer(text)}


def extract_warnings(text: str) -> set[str]:
    """Return set of unique warning messages."""
    return {m.group(2).strip() for m in WARNING_RE.finditer(text)}


def extract_xctest_failures(text: str) -> set[str]:
    """Return set of XCTest failure identifiers."""
    return set(XCTEST_FAIL_RE.findall(text))


def extract_swift_test_failures(text: str) -> set[str]:
    """Return set of Swift Testing failure names."""
    return set(SWIFT_TEST_FAIL_RE.findall(text))


def extract_swiftlint_rules(text: str) -> dict[str, int]:
    """Return {rule_id: violation_count}."""
    counts = {}
    for m in VIOLATION_RE.finditer(text):
        rule = m.group(4)
        counts[rule] = counts.get(rule, 0) + 1
    return counts


# ---- filtered output summary extractors ----

FILT_ERRORS_COUNT_RE = re.compile(r"Errors \((\d+)\)")
FILT_WARNINGS_COUNT_RE = re.compile(r"Warnings \((\d+)\)")
FILT_MORE_RE = re.compile(r"\.\.\. \+(\d+) more")


def parse_filtered_counts(text: str) -> dict:
    """Extract declared counts from filtered output."""
    out = {}
    m = FILT_ERRORS_COUNT_RE.search(text)
    if m:
        out["errors_declared"] = int(m.group(1))
    m = FILT_WARNINGS_COUNT_RE.search(text)
    if m:
        out["warnings_declared"] = int(m.group(1))
    # List of enumerated errors/warnings shown after the count (one per line starting with "  ")
    out["more_counters"] = [int(m.group(1)) for m in FILT_MORE_RE.finditer(text)]
    return out


def extract_filtered_error_msgs(filt: str) -> set[str]:
    """Extract error messages that actually appear in filtered output."""
    return extract_errors(filt)


def extract_filtered_warning_msgs(filt: str) -> set[str]:
    """Extract warning messages that actually appear in filtered output."""
    return extract_warnings(filt)


# ---- validators ----

def validate_swift_build(raw: str, filt: str, r: Result):
    raw_errs = extract_errors(raw)
    raw_warns = extract_warnings(raw)
    r.raw_signals = {"errors": len(raw_errs), "warnings": len(raw_warns)}

    filt_errs = extract_filtered_error_msgs(filt)
    filt_warns = extract_filtered_warning_msgs(filt)

    # Verify every raw error appears in filtered (messages, not counts)
    missing_errs = raw_errs - filt_errs
    if missing_errs:
        # Check if filtered declares the total correctly
        filt_sum = parse_filtered_counts(filt)
        declared = filt_sum.get("errors_declared", 0)
        if declared != len(raw_errs):
            r.issues.append(
                f"ERRORS LOST: {len(missing_errs)} unique error messages in raw are not in filtered, "
                f"and declared count ({declared}) != raw count ({len(raw_errs)}). "
                f"Examples: {list(missing_errs)[:2]}"
            )
        else:
            # All errors counted but not all displayed — check truncation marker
            if not re.search(r"\+\d+ more", filt):
                r.issues.append(
                    f"ERRORS TRUNCATED WITHOUT MARKER: declared={declared}, "
                    f"shown={len(filt_errs)}, missing={len(missing_errs)} errors with no '+N more' indicator"
                )

    # Verify every raw warning appears in filtered OR is counted in truncation
    missing_warns = raw_warns - filt_warns
    if missing_warns:
        filt_sum = parse_filtered_counts(filt)
        declared = filt_sum.get("warnings_declared", 0)
        if declared != len(raw_warns):
            r.issues.append(
                f"WARNINGS COUNT MISMATCH: declared={declared}, raw_unique={len(raw_warns)}, "
                f"missing_from_filtered={len(missing_warns)}"
            )
        else:
            # Check truncation marker accuracy
            shown = len(filt_warns)
            expected_more = declared - shown
            actual_more_sum = sum(parse_filtered_counts(filt)["more_counters"])
            if expected_more > 0 and actual_more_sum == 0:
                r.issues.append(
                    f"WARNINGS TRUNCATED WITHOUT MARKER: declared={declared}, "
                    f"shown_unique={shown}, {expected_more} hidden with no '+N more' marker"
                )


def validate_swift_test(raw: str, filt: str, r: Result):
    xctest_raw = extract_xctest_failures(raw)
    swifttest_raw = extract_swift_test_failures(raw)
    r.raw_signals = {
        "xctest_failures": len(xctest_raw),
        "swift_test_failures": len(swifttest_raw),
    }

    # Look for FAIL lines in filtered (format: "  FAIL Suite.testName" or "  FAIL \"name\"")
    filt_fails = set()
    for line in filt.splitlines():
        m = re.match(r"\s*FAIL\s+(.+?)\s*\(", line)
        if m:
            filt_fails.add(m.group(1).strip())

    # Extract N failed from summary
    summary_match = re.search(r"(\d+)\s+failed", filt)
    declared_failed = int(summary_match.group(1)) if summary_match else 0

    total_raw_failures = len(xctest_raw) + len(swifttest_raw)
    if declared_failed != total_raw_failures:
        r.issues.append(
            f"TEST FAILURE COUNT MISMATCH: filtered declared {declared_failed} failures, "
            f"raw has {total_raw_failures} ({len(xctest_raw)} XCTest + {len(swifttest_raw)} Swift Testing)"
        )

    # If truncation message present, check its math; otherwise check we see all
    more_match = re.search(r"\.\.\. \+(\d+) more\b", filt)
    if more_match:
        hidden = int(more_match.group(1))
        if len(filt_fails) + hidden != declared_failed:
            r.issues.append(
                f"TEST FAILURE TRUNCATION WRONG: shown={len(filt_fails)}, "
                f"+N={hidden}, declared={declared_failed}"
            )
    elif declared_failed > 0 and len(filt_fails) < declared_failed:
        # Swift Testing failures may not be shown as "FAIL ..." — check swift test filter format
        # They're shown as: FAIL <name> (<s>)
        r.issues.append(
            f"TEST FAILURES NOT ENUMERATED: declared={declared_failed}, "
            f"enumerated_in_filtered={len(filt_fails)}, no '+N more' marker found"
        )


def validate_swiftlint(raw: str, filt: str, r: Result):
    raw_rules = extract_swiftlint_rules(raw)
    r.raw_signals = {
        "unique_rules": len(raw_rules),
        "total_violations": sum(raw_rules.values()),
    }

    # Extract per-rule counts from filtered output: "  [E] rule_name (x47, N files)"
    filt_rule_re = re.compile(r"\[[EW]\]\s+(\w+)\s+\(x(\d+)")
    filt_rules = {m.group(1): int(m.group(2)) for m in filt_rule_re.finditer(filt)}

    # Verify every raw rule either appears in filtered or is in "+N more rules"
    missing_rules = set(raw_rules.keys()) - set(filt_rules.keys())
    more_match = re.search(r"\+(\d+) more rules?", filt)
    more_count = int(more_match.group(1)) if more_match else 0

    if len(missing_rules) != more_count:
        r.issues.append(
            f"SWIFTLINT RULE TRUNCATION WRONG: {len(missing_rules)} rules missing from filtered, "
            f"but '+N more rules' says +{more_count}. Missing: {sorted(missing_rules)[:5]}"
        )

    # Verify counts for rules that ARE in filtered match raw
    for rule, shown_count in filt_rules.items():
        raw_count = raw_rules.get(rule, 0)
        if raw_count != shown_count:
            r.issues.append(
                f"SWIFTLINT COUNT MISMATCH for {rule!r}: filtered says x{shown_count}, raw has {raw_count}"
            )


def validate_xcodebuild(raw: str, filt: str, r: Result):
    # Same as swift build but for xcodebuild output
    validate_swift_build(raw, filt, r)


# ---- runner ----

def run_validation(label: str, cmd: list[str], cwd: str, validator: Callable) -> Result:
    r = Result(label=label)
    try:
        raw = run(cmd, cwd)
        filt = run(["rtk"] + cmd, cwd)
        r.raw_lines = len(raw.splitlines())
        r.filtered_lines = len(filt.splitlines())
        validator(raw, filt, r)
    except Exception as e:
        r.issues.append(f"EXCEPTION: {e}")
    return r


def main():
    scenarios = []

    # 1. Clean build on swift-algorithms (should have no errors/warnings)
    scenarios.append(("swift-algorithms: clean build", ["swift", "build"], "/tmp/swift-algorithms", validate_swift_build))

    # 2. swift-argument-parser clean build (26 warnings)
    scenarios.append(("swift-argument-parser: build with warnings", ["swift", "build"], "/tmp/swift-argument-parser", validate_swift_build))

    # 3. swift tests — all pass
    for proj in ["swift-algorithms", "swift-argument-parser", "swift-collections"]:
        scenarios.append((f"{proj}: swift test (all pass)", ["swift", "test"], f"/tmp/{proj}", validate_swift_test))

    # 4. swiftlint on swift-syntax (massive violations)
    scenarios.append(("swift-syntax: swiftlint", ["swiftlint", "lint", "--quiet", "Sources"], "/tmp/swift-syntax", validate_swiftlint))

    # 5. swiftlint on PsyScopeTahoe
    scenarios.append(("PsyScopeTahoe: swiftlint", ["swiftlint", "lint", "--quiet", "PsyScopeFramework"],
                      "/Users/luca/Desktop/_Bonattos/PsyScopeTahoe_EventBased_2", validate_swiftlint))

    results = []
    for label, cmd, cwd, validator in scenarios:
        if not os.path.isdir(cwd):
            print(f"SKIP {label}: {cwd} missing")
            continue
        print(f"Running: {label} ...", flush=True)
        r = run_validation(label, cmd, cwd, validator)
        results.append(r)
        status = "PASS" if r.ok() else "FAIL"
        print(f"  {status} | raw_signals={r.raw_signals} | lines raw={r.raw_lines} filt={r.filtered_lines}")
        for issue in r.issues:
            print(f"    ISSUE: {issue}")

    print("\n" + "=" * 70)
    print(f"SUMMARY: {sum(1 for r in results if r.ok())}/{len(results)} validations passed")
    print("=" * 70)
    failed = [r for r in results if not r.ok()]
    if failed:
        print(f"\n{len(failed)} scenarios with data-loss issues:")
        for r in failed:
            print(f"  - {r.label}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
