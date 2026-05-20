# SpecSlice macOS Universal Package

This package contains the closed-source SpecSlice Rust CLI binary for macOS
Apple Silicon and Intel, plus the Dart analyzer sidecar source that enables
higher-precision Flutter/Dart indexing.

## Contents

```text
bin/specslice
libexec/specslice
tool/specslice_dart_analyzer/
skills/specslice/
README.md
README-AI-SKILL.md
BUILD-INFO.txt
```

`bin/specslice` is the user-facing wrapper. `libexec/specslice` is the
universal binary. The Dart analyzer sidecar is intentionally included as
source under `tool/specslice_dart_analyzer/`.

## Install

```bash
tar -xzf specslice-0.1.0-macos-universal.tar.gz
sudo cp -R specslice-0.1.0-macos-universal /usr/local/specslice
sudo ln -sf /usr/local/specslice/bin/specslice /usr/local/bin/specslice
specslice --help
```

If you do not want to use `/usr/local`, put the extracted directory anywhere
and add its `bin` directory to PATH.

## Dart analyzer sidecar

For Flutter/Dart repositories, install a Dart or Flutter SDK on the machine:

```bash
dart --version
# or
flutter --version
```

When Dart is available, SpecSlice automatically probes the bundled sidecar at:

```text
tool/specslice_dart_analyzer/bin/specslice_dart_analyzer.dart
```

If Dart is unavailable, SpecSlice falls back to the lightweight scanner and
prints a skip reason. The fallback is usable, but less precise.

## Basic Usage

```bash
specslice --repo-root /path/to/repo init
specslice --repo-root /path/to/repo index
specslice --repo-root /path/to/repo check
specslice --repo-root /path/to/repo logic --only-risks
specslice --repo-root /path/to/repo graph --format html --view code
specslice --repo-root /path/to/repo graph --format html --view business
```

HTML graph output defaults to:

```text
/path/to/repo/.specslice/export/graph.html
```

## AI Candidate Flow

```bash
specslice --repo-root /path/to/repo connect propose --pretty --out /tmp/specslice-evidence.json
specslice --repo-root /path/to/repo candidate list
specslice --repo-root /path/to/repo candidate review <candidate-id> --accept --note "用户确认"
```

AI candidates are not confirmed business logic until a human reviews them.
SpecSlice does not require annotations in production code, tests, or docs.

## Uninstall

```bash
sudo rm -f /usr/local/bin/specslice
sudo rm -rf /usr/local/specslice
```
