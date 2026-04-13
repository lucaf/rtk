# Apple filter rigorous validation

Programmatic validation that the apple filters (`swift`, `xcodebuild`, `swiftlint`, `simctl`, `xctrace`) do not silently drop critical signals. These harnesses inject bugs into real Apple open-source projects, run raw and filtered versions, and verify every error/warning/failure is accounted for.

## Files

- `validate.py` — success-path validation (compression ratios, signal counts on passing projects)
- `validate_failures.py` — failure-path validation (injected compile errors, injected test failures, warning truncation math)
- `validate_deep.py` — edge cases (xcodebuild errors, XCTest assertion messages, swiftlint autocorrect, error ordering)
- `validate_all.py` — matrix: 11 Apple projects × 5 scenarios each

## Prerequisites

1. **RTK installed** (`cargo install --path .`)
2. **11 Apple projects cloned in /tmp/** — run these from anywhere:
   ```bash
   cd /tmp
   for repo in swift-algorithms swift-argument-parser swift-async-algorithms \
               swift-collections swift-crypto swift-docc swift-format \
               swift-log swift-nio swift-numerics swift-syntax; do
     [ -d "$repo" ] || git clone --depth 1 "https://github.com/apple/$repo.git"
   done
   ```
3. **swiftlint installed** (`brew install swiftlint`)

## Running

```bash
# Individual harnesses
python3 validate.py
python3 validate_failures.py
python3 validate_deep.py
python3 validate_all.py

# All together
for f in validate.py validate_failures.py validate_deep.py validate_all.py; do
  echo "=== $f ==="; python3 "$f" || echo "FAILED: $f"
done
```

## What gets verified

For every apple-filtered command:

1. **Error preservation** — every `error:` line in raw is either displayed in filtered output or counted in the declared total
2. **Error file:line location** — `Sources/Foo.swift:10:5: error:` format preserved
3. **Warning truncation math** — `shown_in_output + "+N more" == declared_total`
4. **Swiftlint rule counts** — per-rule violation counts match raw exactly
5. **Test failure enumeration** — failure names, file:line, and `XCTAssert` messages preserved
6. **Build status banners** — `** BUILD FAILED **` / `Build complete!` propagate
7. **Package structure** — package name and targets preserved in describe output

## Failure modes

The validators create backup files before mutating any project source. If a run is **interrupted**, some backups may remain in `/tmp/rtk_*.swift` and some injections may remain in project files. Restore manually:

```bash
cd /tmp/<project>
git checkout Sources/ Tests/
rm -f /tmp/rtk_*bk*.swift /tmp/rtk_*backup*.swift /tmp/rtk_deep_*.swift
```

## History

These validators were created after discovering that ad-hoc manual testing missed **4 data-loss bugs** in the initial apple module implementation:

1. xcodebuild informational subcommands (`-list`, `-version`) silently dropped
2. Project-level errors (signing, provisioning) not captured by strict `file:line:col:` regex
3. Paths with spaces not matched (e.g. `/tmp/metal code examples/...`)
4. `swift package dump-package` JSON silently dropped

All four were fixed and are now regression-tested by this harness. Current result: **71/71 scenarios pass**.
