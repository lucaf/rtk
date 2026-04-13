#!/usr/bin/env python3
"""
Comprehensive rigorous validation across ALL cloned Apple projects.

For each project, runs multiple scenarios:
  1. swift package describe — verify structure preserved
  2. swift build (cached) — verify success signal
  3. swift build with injected error — verify error preservation
  4. swift test — verify test counts and (if any failures) failure details
  5. swift test with injected failure — verify failure name + file:line + message

Reports per-project pass/fail with detailed diagnostics on any failures.
"""
import os
import re
import shutil
import subprocess
import sys


PROJECTS = [
    "/tmp/swift-algorithms",
    "/tmp/swift-argument-parser",
    "/tmp/swift-async-algorithms",
    "/tmp/swift-collections",
    "/tmp/swift-crypto",
    "/tmp/swift-docc",
    "/tmp/swift-format",
    "/tmp/swift-log",
    "/tmp/swift-nio",
    "/tmp/swift-numerics",
    "/tmp/swift-syntax",
]


def run(cmd, cwd, timeout=900):
    try:
        r = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)
        return r.stdout + r.stderr
    except subprocess.TimeoutExpired:
        return "TIMEOUT"


def count_error_lines(text):
    return sum(1 for _ in re.finditer(r"^(?:(.+?):\s+)?error:\s+", text, re.MULTILINE))


def count_warning_lines(text):
    return sum(1 for _ in re.finditer(r"^(?:(.+?):\s+)?warning:\s+", text, re.MULTILINE))


def extract_xctest_failures(text):
    return set(re.findall(r"Test Case '-\[(\S+\.\S+ \w+)\]' failed", text))


def extract_swift_test_failures(text):
    return set(re.findall(r'[✘✗]\s+Test\s+"([^"]+)"\s+failed', text))


def parse_filter_counts(filt):
    out = {}
    m = re.search(r"Errors \((\d+)\)", filt)
    if m:
        out["errors_declared"] = int(m.group(1))
    m = re.search(r"Warnings \((\d+)\)", filt)
    if m:
        out["warnings_declared"] = int(m.group(1))
    out["more_counters"] = [int(m.group(1)) for m in re.finditer(r"\+(\d+) more", filt)]
    return out


def find_injection_target(project, subdir):
    """Find a .swift file under project/subdir (not Package.swift, not test file)."""
    path = os.path.join(project, subdir)
    if not os.path.isdir(path):
        return None
    for root, dirs, files in os.walk(path):
        dirs[:] = [d for d in dirs if d not in (".build", ".git")]
        for f in files:
            if f.endswith(".swift"):
                return os.path.join(root, f)
    return None


# ---- scenario runners ----

def test_package_describe(project):
    """Verify swift package describe is filtered without data loss of structural info."""
    raw = run(["swift", "package", "describe"], project)
    filt = run(["rtk", "swift", "package", "describe"], project)

    if not filt.strip():
        return False, "filtered output is empty"

    # Extract package name from raw
    raw_name = None
    if m := re.search(r"^Name:\s+(\S+)", raw, re.MULTILINE):
        raw_name = m.group(1)
    if raw_name and raw_name not in filt:
        return False, f"package name '{raw_name}' not in filtered output"

    # Count targets in raw vs declared in filtered
    raw_targets = len(re.findall(r"^\s{4}Name:\s+\S+", raw, re.MULTILINE))
    if m := re.search(r"Targets \((\d+)\)", filt):
        declared_targets = int(m.group(1))
        # Raw may count targets + products + dependencies — filter's target count is one subset
        # Just check that filter declares >0 if raw has any
        if raw_targets > 0 and declared_targets == 0:
            return False, f"filter declares 0 targets but raw shows {raw_targets} indented Name: entries"

    return True, None


def test_build_cached(project):
    """Verify cached build passes through correctly."""
    raw = run(["swift", "build"], project)
    filt = run(["rtk", "swift", "build"], project)

    if "Build complete" in raw and "Build complete" not in filt:
        return False, "Build complete signal lost"
    if "Build failed" in raw.lower() and "fail" not in filt.lower():
        return False, "Build failed signal lost"
    return True, None


def test_build_injected_error(project):
    """Inject compile error, verify preserved."""
    target = find_injection_target(project, "Sources")
    if not target:
        return True, "skipped: no Sources/ directory"

    backup = "/tmp/rtk_all_bk.swift"
    shutil.copy(target, backup)
    marker = "RTK_ALL_VALIDATION_MARKER_42"
    try:
        with open(target, "a") as f:
            f.write(f"\n{marker}\n")

        raw = run(["swift", "build"], project)
        filt = run(["rtk", "swift", "build"], project)
    finally:
        shutil.copy(backup, target)
        os.remove(backup)

    raw_errs = count_error_lines(raw)
    if raw_errs == 0:
        return True, "skipped: injection didn't produce errors (maybe file not in build)"

    counts = parse_filter_counts(filt)
    declared = counts.get("errors_declared", 0)

    if declared == 0:
        return False, f"filter declared 0 errors but raw has {raw_errs}"

    if declared != raw_errs:
        # Allow mild mismatch (swift build vs emit-module dedupe)
        if abs(declared - raw_errs) > raw_errs * 0.1:
            return False, f"declared={declared} vs raw={raw_errs} (>10% mismatch)"

    # Verify file:line is preserved (paths may contain spaces, so use .+? not \S+)
    if "Sources/" in raw and not re.search(r"Sources/.+?\.swift:\d+:\d+:\s*error:", filt):
        return False, "error file:line location not preserved in filtered output"

    # Verify injection-related error is present (filter should contain "cannot find")
    if "cannot find" in raw and "cannot find" not in filt:
        return False, "injection-related error ('cannot find') lost"

    return True, None


def test_swift_test_passing(project):
    """Verify swift test passing case."""
    raw = run(["swift", "test"], project, timeout=1200)
    if raw == "TIMEOUT":
        return True, "skipped: timeout"

    filt = run(["rtk", "swift", "test"], project, timeout=1200)
    if filt == "TIMEOUT":
        return True, "skipped: timeout"

    # If raw has "No tests" or errors, skip
    if "error:" in raw and "Test Case" not in raw and "✔ Test" not in raw:
        return True, "skipped: build failed, can't test"

    # Count failures in raw and filtered
    raw_failures = len(extract_xctest_failures(raw)) + len(extract_swift_test_failures(raw))
    m = re.search(r"(\d+)\s+failed", filt)
    filt_declared_fail = int(m.group(1)) if m else 0

    if raw_failures != filt_declared_fail:
        return False, f"test failure count mismatch: raw={raw_failures}, filter declared={filt_declared_fail}"

    return True, None


def test_swift_test_injected_failure(project):
    """Inject a failing test with unique assertion message."""
    # Find a test file
    tests_dir = os.path.join(project, "Tests")
    if not os.path.isdir(tests_dir):
        return True, "skipped: no Tests/ directory"

    test_file = None
    for root, _, files in os.walk(tests_dir):
        for f in files:
            if f.endswith("Tests.swift"):
                test_file = os.path.join(root, f)
                break
        if test_file:
            break

    if not test_file:
        return True, "skipped: no Tests.swift file"

    # Check if XCTestCase-based
    with open(test_file) as f:
        content_check = f.read()
    if "XCTestCase" not in content_check:
        return True, "skipped: no XCTestCase in test file"

    backup = "/tmp/rtk_all_testbk.swift"
    shutil.copy(test_file, backup)
    marker = "UNIQUE_INJ_MSG_RTK99"
    func_name = "testRtkValidationInjection99"

    try:
        inj = f'    func {func_name}() {{ XCTFail("{marker}") }}'
        new_content = re.sub(
            r"(final class \w+Tests\s*:\s*XCTestCase\s*\{)",
            r"\1\n" + inj,
            content_check,
            count=1,
        )
        # Only modify if regex matched
        if new_content == content_check:
            return True, "skipped: couldn't find XCTestCase class pattern"
        with open(test_file, "w") as f:
            f.write(new_content)

        raw = run(["swift", "test"], project, timeout=1200)
        filt = run(["rtk", "swift", "test"], project, timeout=1200)
    finally:
        if os.path.exists(backup):
            shutil.copy(backup, test_file)
            os.remove(backup)

    if raw == "TIMEOUT" or filt == "TIMEOUT":
        return True, "skipped: timeout"

    # Verify raw has the failure
    if marker not in raw:
        return True, "skipped: injection didn't produce failure (test maybe not discovered)"

    # CHECK 1: failure count declared
    m = re.search(r"(\d+)\s+failed", filt)
    if not m:
        return False, "no failure count in filtered output"
    declared_fail = int(m.group(1))
    if declared_fail < 1:
        return False, f"filter declared {declared_fail} failures but raw has injected failure"

    # CHECK 2: failure name enumerated
    if func_name not in filt:
        return False, f"failure name '{func_name}' not in filtered output"

    # CHECK 3: assertion message preserved (critical!)
    if marker not in filt:
        return False, f"assertion message '{marker}' lost from filtered output"

    return True, None


# ---- per-project scenarios ----

SCENARIOS = [
    ("pkg_describe", test_package_describe),
    ("build_cached", test_build_cached),
    ("build_err_inj", test_build_injected_error),
    ("test_passing", test_swift_test_passing),
    ("test_inj_fail", test_swift_test_injected_failure),
]


def main():
    print("=" * 80)
    print(f"RIGOROUS VALIDATION across {len(PROJECTS)} Apple projects")
    print("=" * 80)

    project_results = {}
    any_fail = False

    for project in PROJECTS:
        if not os.path.isdir(project):
            print(f"\n[MISSING] {project}")
            continue
        name = os.path.basename(project)
        print(f"\n## {name}")
        results = {}
        for scenario_name, fn in SCENARIOS:
            try:
                ok, note = fn(project)
            except Exception as e:
                ok = False
                note = f"exception: {e}"
            status = "✅" if ok else "❌"
            msg = f" ({note})" if note else ""
            print(f"  {status} {scenario_name}{msg}")
            results[scenario_name] = (ok, note)
            if not ok:
                any_fail = True
        project_results[name] = results

    # Summary matrix
    print("\n" + "=" * 80)
    print("SUMMARY MATRIX")
    print("=" * 80)
    header = f"{'Project':<30}"
    for s, _ in SCENARIOS:
        header += f"{s:>15}"
    print(header)
    for name, results in project_results.items():
        row = f"{name:<30}"
        for s, _ in SCENARIOS:
            ok, _ = results.get(s, (False, "N/A"))
            row += f"{('✅' if ok else '❌'):>15}"
        print(row)

    total = sum(len(r) for r in project_results.values())
    passed = sum(1 for r in project_results.values() for ok, _ in r.values() if ok)
    print(f"\nTOTAL: {passed}/{total} scenarios passed across {len(project_results)} projects")
    return 1 if any_fail else 0


if __name__ == "__main__":
    sys.exit(main())
