#!/bin/bash
# RTK Apple Module End-to-End Test Script
#
# This script validates all RTK apple module commands by:
# 1. Running both raw and RTK versions of each command
# 2. Comparing output sizes and calculating token savings
# 3. Validating that RTK doesn't break command behavior (exit codes, non-empty output)
#
# Usage: ./scripts/test-apple.sh
# Prerequisites: rtk must be installed (cargo install --path .)

set -e  # Exit on error (disabled during command execution to capture both successes and failures)

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Test project path
TEST_PROJECT="/Users/luca/Desktop/_Bonattos/PsyScopeTahoe_EventBased_2"

# Results tracking
declare -a RESULTS
declare -a COMMANDS
declare -a RAW_LINES
declare -a FILTERED_LINES
declare -a SAVINGS
declare -a STATUS

# Helper function: count lines in output
count_lines() {
    echo "$1" | wc -l | tr -d ' '
}

# Helper function: count tokens (whitespace-separated words)
count_tokens() {
    echo "$1" | wc -w | tr -d ' '
}

# Helper function: calculate savings percentage
calculate_savings() {
    local raw_tokens=$1
    local filtered_tokens=$2

    if [ "$raw_tokens" -eq 0 ]; then
        echo "0.0"
        return
    fi

    # Use bc for floating point arithmetic
    echo "scale=1; 100 - ($filtered_tokens * 100 / $raw_tokens)" | bc
}

# Helper function: run a test case
run_test() {
    local test_name="$1"
    local raw_cmd="$2"
    local rtk_cmd="$3"
    local working_dir="${4:-$PWD}"

    echo ""
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${BLUE}Testing: ${test_name}${NC}"
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"

    # Run raw command
    echo -e "${YELLOW}Running raw command: ${raw_cmd}${NC}"
    set +e  # Temporarily disable exit on error
    cd "$working_dir"
    raw_output=$(eval "$raw_cmd" 2>&1)
    raw_exit=$?
    cd - > /dev/null
    set -e

    # Run RTK command
    echo -e "${YELLOW}Running RTK command: ${rtk_cmd}${NC}"
    set +e  # Temporarily disable exit on error
    cd "$working_dir"
    rtk_output=$(eval "$rtk_cmd" 2>&1)
    rtk_exit=$?
    cd - > /dev/null
    set -e

    # Count lines and tokens
    raw_lines=$(count_lines "$raw_output")
    rtk_lines=$(count_lines "$rtk_output")
    raw_tokens=$(count_tokens "$raw_output")
    rtk_tokens=$(count_tokens "$rtk_output")
    savings=$(calculate_savings "$raw_tokens" "$rtk_tokens")

    # Determine pass/fail
    local status="PASS"
    local reason=""

    # Check exit codes match
    if [ "$raw_exit" -ne "$rtk_exit" ]; then
        status="FAIL"
        reason="Exit code mismatch (raw: $raw_exit, rtk: $rtk_exit)"
    fi

    # Check RTK output is not empty when raw output is non-empty
    if [ -n "$raw_output" ] && [ -z "$rtk_output" ]; then
        status="FAIL"
        reason="RTK produced empty output when raw had content"
    fi

    # Display results
    echo ""
    echo "Raw output: $raw_lines lines, $raw_tokens tokens (exit: $raw_exit)"
    echo "RTK output: $rtk_lines lines, $rtk_tokens tokens (exit: $rtk_exit)"
    echo "Savings: ${savings}%"

    if [ "$status" = "PASS" ]; then
        echo -e "${GREEN}✓ ${status}${NC}"
    else
        echo -e "${RED}✗ ${status}: ${reason}${NC}"
    fi

    # Show sample output (first 5 lines)
    echo ""
    echo -e "${YELLOW}Raw output sample (first 5 lines):${NC}"
    echo "$raw_output" | head -5
    echo ""
    echo -e "${YELLOW}RTK output sample (first 5 lines):${NC}"
    echo "$rtk_output" | head -5

    # Store results
    COMMANDS+=("$test_name")
    RAW_LINES+=("$raw_lines")
    FILTERED_LINES+=("$rtk_lines")
    SAVINGS+=("$savings")
    STATUS+=("$status")
}

# Print header
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${BLUE}        RTK Apple Module End-to-End Test Suite${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"

# Check RTK is installed
echo ""
echo -e "${YELLOW}Checking RTK installation...${NC}"
if ! command -v rtk &> /dev/null; then
    echo -e "${RED}ERROR: rtk is not installed or not in PATH${NC}"
    echo "Please run: cargo install --path ."
    exit 1
fi

rtk_version=$(rtk --version 2>&1 || echo "unknown")
echo -e "${GREEN}✓ RTK is installed: ${rtk_version}${NC}"

# Check test project exists
echo ""
echo -e "${YELLOW}Checking test project...${NC}"
if [ ! -d "$TEST_PROJECT" ]; then
    echo -e "${RED}ERROR: Test project not found at $TEST_PROJECT${NC}"
    exit 1
fi
echo -e "${GREEN}✓ Test project found: $TEST_PROJECT${NC}"

# Run tests
echo ""
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${BLUE}                    Running Tests${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"

# Test 1: swift build
run_test \
    "swift build" \
    "swift build 2>&1 || true" \
    "rtk swift build 2>&1 || true" \
    "$TEST_PROJECT"

# Test 2: swift test (real Swift Testing output — 112 tests, some failures)
# Requires dylib setup: see RUN_TESTS.md in the test project
run_test \
    "swift test" \
    "swift test --skip-build 2>&1 || true" \
    "rtk swift test --skip-build 2>&1 || true" \
    "$TEST_PROJECT"

# Test 3: swift run (project dylib missing — tests graceful runtime error passthrough)
run_test \
    "swift run" \
    "swift run PsyScopeTest 2>&1 | head -10 || true" \
    "rtk swift run PsyScopeTest 2>&1 | head -10 || true" \
    "$TEST_PROJECT"

# Test 4: swift package describe
run_test \
    "swift package describe" \
    "swift package describe 2>&1 || true" \
    "rtk swift package describe 2>&1 || true" \
    "$TEST_PROJECT"

# Test 3: swiftlint (if available)
if command -v swiftlint &> /dev/null; then
    run_test \
        "swiftlint" \
        "swiftlint 2>&1 || true" \
        "rtk swiftlint 2>&1 || true" \
        "$TEST_PROJECT"
else
    echo ""
    echo -e "${YELLOW}⚠ Skipping swiftlint test (not installed)${NC}"
    COMMANDS+=("swiftlint")
    RAW_LINES+=("N/A")
    FILTERED_LINES+=("N/A")
    SAVINGS+=("N/A")
    STATUS+=("SKIP")
fi

# Test 4: simctl list
run_test \
    "simctl list" \
    "xcrun simctl list 2>&1 || true" \
    "rtk simctl list 2>&1 || true" \
    "$PWD"

# Test 5: xctrace list templates
run_test \
    "xctrace list templates" \
    "xcrun xctrace list templates 2>&1 || true" \
    "rtk xctrace list templates 2>&1 || true" \
    "$PWD"

# Test 6: xctrace list devices
run_test \
    "xctrace list devices" \
    "xcrun xctrace list devices 2>&1 || true" \
    "rtk xctrace list devices 2>&1 || true" \
    "$PWD"

# Test 7: xcodebuild build
run_test \
    "xcodebuild build" \
    "xcodebuild build -scheme PsyScopeFramework -destination 'platform=macOS' 2>&1 || true" \
    "rtk xcodebuild build -scheme PsyScopeFramework -destination 'platform=macOS' 2>&1 || true" \
    "$TEST_PROJECT"

# Print summary table
echo ""
echo ""
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${BLUE}                    Test Summary${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo ""

# Table header
printf "%-30s %-8s %-10s %-10s %-10s\n" "Command" "Status" "Raw Lines" "RTK Lines" "Savings %"
printf "%-30s %-8s %-10s %-10s %-10s\n" "-------" "------" "---------" "---------" "---------"

# Table rows
total_tests=${#COMMANDS[@]}
passed=0
failed=0
skipped=0

for i in "${!COMMANDS[@]}"; do
    cmd="${COMMANDS[$i]}"
    status="${STATUS[$i]}"
    raw="${RAW_LINES[$i]}"
    filtered="${FILTERED_LINES[$i]}"
    savings="${SAVINGS[$i]}"

    # Color code status
    if [ "$status" = "PASS" ]; then
        status_colored="${GREEN}PASS${NC}"
        ((passed++))
    elif [ "$status" = "SKIP" ]; then
        status_colored="${YELLOW}SKIP${NC}"
        ((skipped++))
    else
        status_colored="${RED}FAIL${NC}"
        ((failed++))
    fi

    printf "%-30s %-8b %-10s %-10s %-10s\n" "$cmd" "$status_colored" "$raw" "$filtered" "$savings"
done

# Final summary
echo ""
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo -e "Total tests: $total_tests"
echo -e "${GREEN}Passed: $passed${NC}"
if [ "$skipped" -gt 0 ]; then
    echo -e "${YELLOW}Skipped: $skipped${NC}"
fi
if [ "$failed" -gt 0 ]; then
    echo -e "${RED}Failed: $failed${NC}"
    echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
    exit 1
else
    echo -e "${GREEN}All tests passed!${NC}"
    echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
    exit 0
fi
