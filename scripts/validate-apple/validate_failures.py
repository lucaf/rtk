#!/usr/bin/env python3
"""
Extended validation — tests FAILURE PATHS by injecting bugs into real projects.

For each scenario:
1. Backup a file
2. Inject an error/failing test
3. Run raw and filtered
4. Extract signals from raw (errors, failures)
5. Verify every signal is accounted for in filtered output
6. Restore the file
"""
import os
import re
import shutil
import subprocess
import sys

# Reuse extractors from main validator (validate.py in this directory)
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from validate import (
    extract_errors,
    extract_warnings,
    extract_xctest_failures,
    extract_swift_test_failures,
    parse_filtered_counts,
    extract_filtered_error_msgs,
    extract_filtered_warning_msgs,
    extract_swiftlint_rules,
)


def run(cmd, cwd):
    r = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=900)
    return r.stdout + r.stderr


def inject_swift_build_error(project_dir: str, n_errors: int = 3):
    """Inject N independent compile errors into Swift source files, return list of (backup, original) tuples."""
    swift_files = []
    # Only look in Sources/ subdirectory to avoid Package.swift and build artifacts
    sources_dir = os.path.join(project_dir, "Sources")
    if not os.path.isdir(sources_dir):
        return []
    for root, dirs, files in os.walk(sources_dir):
        dirs[:] = [d for d in dirs if d not in (".build", ".git")]
        for f in files:
            if f.endswith(".swift"):
                swift_files.append(os.path.join(root, f))
            if len(swift_files) >= n_errors:
                break
        if len(swift_files) >= n_errors:
            break

    backups = []
    for i, fpath in enumerate(swift_files[:n_errors]):
        backup = f"/tmp/rtk_validation_backup_{i}.swift"
        shutil.copy(fpath, backup)
        with open(fpath, "a") as f:
            f.write(f"\nXXXinjectedError{i}XXX\n")
        backups.append((backup, fpath))
    return backups


def inject_test_failures(project_dir: str, n_fails: int = 3):
    """Inject N failing XCTest methods into a test file."""
    test_file = None
    for root, _, files in os.walk(os.path.join(project_dir, "Tests")):
        for f in files:
            if f.endswith("Tests.swift"):
                test_file = os.path.join(root, f)
                break
        if test_file:
            break
    if not test_file:
        return None

    backup = "/tmp/rtk_validation_testbackup.swift"
    shutil.copy(test_file, backup)
    with open(test_file, "r") as f:
        content = f.read()

    # Insert N failures inside the first XCTestCase
    failures = "\n".join(
        f'    func testInjectedFailure{i}() {{ XCTFail("injection {i}") }}'
        for i in range(n_fails)
    )
    content = re.sub(
        r"(final class \w+Tests\s*:\s*XCTestCase\s*\{)",
        r"\1\n" + failures,
        content,
        count=1,
    )
    with open(test_file, "w") as f:
        f.write(content)

    return [(backup, test_file)]


def restore(backups):
    for backup, original in backups:
        shutil.copy(backup, original)
        os.remove(backup)


# ---- scenarios ----

def scenario_swift_build_multi_errors(project: str):
    label = f"{os.path.basename(project)}: swift build with 3 injected errors"
    # Need to pre-clean for a fresh build
    run(["swift", "package", "clean"], project)
    backups = inject_swift_build_error(project, n_errors=3)
    try:
        raw = run(["swift", "build"], project)
        filt = run(["rtk", "swift", "build"], project)
    finally:
        restore(backups)

    raw_err_lines = count_error_lines(raw)
    filt_counts = parse_filtered_counts(filt)
    declared = filt_counts.get("errors_declared", 0)

    # Count shown error lines in filtered output
    shown_errs = [l for l in filt.splitlines() if re.match(r"\s+.*?error:", l)]
    more_sum = sum(filt_counts.get("more_counters", []))

    issues = []
    if declared != raw_err_lines:
        issues.append(
            f"ERROR COUNT MISMATCH: filter declared {declared}, raw has {raw_err_lines} error lines"
        )

    # Truncation math
    if declared > 20 and (len(shown_errs) + more_sum) != declared:
        issues.append(
            f"ERROR TRUNCATION MATH: declared={declared}, shown={len(shown_errs)}, "
            f"+N_more={more_sum}, sum={len(shown_errs) + more_sum}"
        )

    # Injection marker preserved?
    injection_in_filt = "XXXinjectedError" in filt or "cannot find" in filt
    if not injection_in_filt:
        issues.append("INJECTION NOT VISIBLE: no 'XXXinjectedError' or 'cannot find' text in filtered output")

    print(f"\n{label}:")
    print(f"  raw error lines: {raw_err_lines}")
    print(f"  filter declared: {declared}")
    print(f"  filter shown lines: {len(shown_errs)}")
    print(f"  +N more: {more_sum}")
    print(f"  injection visible: {injection_in_filt}")
    if issues:
        for i in issues:
            print(f"  ❌ {i}")
    else:
        print(f"  ✅ PASS")
    return len(issues) == 0


def scenario_swift_test_multi_failures(project: str, n: int = 3):
    label = f"{os.path.basename(project)}: swift test with {n} injected failures"
    backups = inject_test_failures(project, n_fails=n)
    if not backups:
        print(f"{label}: SKIP (no test file found)")
        return True
    try:
        raw = run(["swift", "test"], project)
        filt = run(["rtk", "swift", "test"], project)
    finally:
        restore(backups)

    xctest_raw = extract_xctest_failures(raw)
    # Extract summary count from filtered
    m = re.search(r"(\d+)\s+failed", filt)
    declared_failed = int(m.group(1)) if m else 0

    # Extract FAIL lines from filtered
    filt_fails = re.findall(r"^\s*FAIL\s+(.+?)\s*\(", filt, re.MULTILINE)

    issues = []
    if declared_failed != len(xctest_raw):
        issues.append(
            f"FAILURE COUNT MISMATCH: declared={declared_failed}, raw_xctest_failures={len(xctest_raw)}"
        )

    # Look for our injection markers in filtered FAIL lines
    injected_found = sum(1 for f in filt_fails if "testInjectedFailure" in f)
    if injected_found != n:
        issues.append(
            f"INJECTED FAILURES MISSING: {n} injected, {injected_found} found in filtered FAIL lines"
        )

    # The swift test filter doesn't cap FAIL lines (shows all failures).
    # It caps error_details at 20 — "+N more errors" applies to those, not failures.
    # So we only check that all declared failures are enumerated.
    if len(filt_fails) != declared_failed:
        issues.append(
            f"FAIL ENUMERATION INCOMPLETE: declared={declared_failed}, enumerated={len(filt_fails)}"
        )

    print(f"\n{label}:")
    print(f"  raw xctest failures: {len(xctest_raw)}")
    print(f"  filtered declared failed: {declared_failed}")
    print(f"  filtered FAIL lines: {len(filt_fails)}")
    print(f"  injected failures found: {injected_found}/{n}")
    if issues:
        for i in issues:
            print(f"  ❌ {i}")
    else:
        print(f"  ✅ PASS")
    return len(issues) == 0


def scenario_swift_test_many_failures(project: str):
    """Test with more failures than the 20-cap to exercise truncation."""
    return scenario_swift_test_multi_failures(project, n=25)


def count_warning_lines(text: str) -> int:
    """Count total warning LINES (not unique messages)."""
    return sum(1 for _ in re.finditer(r"^(?:(.+?):\s+)?warning:\s+", text, re.MULTILINE))


def count_error_lines(text: str) -> int:
    """Count total error LINES (not unique messages)."""
    return sum(1 for _ in re.finditer(r"^(?:(.+?):\s+)?error:\s+", text, re.MULTILINE))


def scenario_clean_rebuild_with_warnings(project: str):
    """Force a clean rebuild to actually get warnings in output.

    Methodology: count warning LINES in raw (not unique messages — Swift emits
    the same warning at the same location for each build phase that references it),
    then verify the filter's declared count matches and truncation math is sound.
    """
    label = f"{os.path.basename(project)}: clean rebuild (expect warnings)"
    run(["swift", "package", "clean"], project)
    raw = run(["swift", "build"], project)
    run(["swift", "package", "clean"], project)
    filt = run(["rtk", "swift", "build"], project)

    raw_warn_lines = count_warning_lines(raw)
    filt_counts = parse_filtered_counts(filt)
    declared = filt_counts.get("warnings_declared", 0)

    # Filter shows individual warning lines in filtered output, capped at 10
    # Use line counting in filtered too (each line starting with "  " and containing "warning:")
    shown_warns = [l for l in filt.splitlines() if re.match(r"\s+.*?warning:", l)]
    more_sum = sum(filt_counts.get("more_counters", []))

    issues = []
    if declared != raw_warn_lines:
        issues.append(
            f"WARNING COUNT MISMATCH: filter declared {declared}, raw has {raw_warn_lines} warning lines"
        )

    # Shown + more_counter should equal declared (truncation math)
    if declared > 0 and (len(shown_warns) + more_sum) != declared:
        issues.append(
            f"TRUNCATION MATH: declared={declared}, shown_lines={len(shown_warns)}, "
            f"+N_more={more_sum}, sum={len(shown_warns) + more_sum}"
        )

    print(f"\n{label}:")
    print(f"  raw warning lines: {raw_warn_lines}")
    print(f"  filter declared: {declared}")
    print(f"  filter shown lines: {len(shown_warns)}")
    print(f"  +N more: {more_sum}")
    print(f"  shown + more = {len(shown_warns) + more_sum} (should equal declared {declared})")
    if issues:
        for i in issues:
            print(f"  ❌ {i}")
    else:
        print(f"  ✅ PASS")
    return len(issues) == 0


def main():
    results = []

    # Failure paths for swift build
    results.append(scenario_swift_build_multi_errors("/tmp/swift-algorithms"))

    # Failure paths for swift test (under 20, so no truncation)
    results.append(scenario_swift_test_multi_failures("/tmp/swift-algorithms", n=3))

    # Over-cap test — does truncation work?
    results.append(scenario_swift_test_many_failures("/tmp/swift-algorithms"))

    # Clean rebuild with known warnings
    results.append(scenario_clean_rebuild_with_warnings("/tmp/swift-argument-parser"))

    print("\n" + "=" * 70)
    print(f"SUMMARY: {sum(1 for r in results if r)}/{len(results)} scenarios passed")
    print("=" * 70)
    return 0 if all(results) else 1


if __name__ == "__main__":
    sys.exit(main())
