<#
.SYNOPSIS
  Build + package GroundGraph for Windows x86_64.

.DESCRIPTION
  scripts/release_windows.ps1 [-Target x86_64-pc-windows-msvc]

  Same package layout as the macOS/Linux builds minus the bin/ shell wrappers
  (Windows users run .exe directly): both binaries land in bin/, alongside the
  Dart analyzer sidecar source, the AI skill, both package READMEs and a
  BUILD-INFO manifest. Output is dist/groundgraph-<ver>-windows-x86_64.zip plus
  a relative-path .sha256.

  The build runs through `cargo` (respecting rust-toolchain.toml). MSVC ships on
  windows-latest, so rusqlite bundled + the 13 tree-sitter grammar C/C++ deps
  link with no extra setup.

.PARAMETER Target
  The Rust target triple. Defaults to x86_64-pc-windows-msvc.
#>
[CmdletBinding()]
param(
  [string]$Target = 'x86_64-pc-windows-msvc'
)

$ErrorActionPreference = 'Stop'

$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

# Version: honour $env:VERSION, otherwise parse [workspace.package].version
# from the root Cargo.toml. [\s\S]*? matches across newlines without a
# multiline/dotall flag dependency.
if ($env:VERSION) {
  $Version = $env:VERSION
} else {
  $content = Get-Content (Join-Path $Root 'Cargo.toml') -Raw
  $m = [regex]::Match($content, '(?m)^\[workspace\.package\][\s\S]*?version\s*=\s*"([^"]+)"')
  if (-not $m.Success) {
    throw 'could not find [workspace.package].version in Cargo.toml'
  }
  $Version = $m.Groups[1].Value
}

switch -Wildcard ($Target) {
  'x86_64-*' { $Arch = 'x86_64' }
  'aarch64-*' { $Arch = 'aarch64' }
  default { throw "unsupported target '$Target' (expected x86_64-* or aarch64-*)" }
}

$PackageName = "groundgraph-$Version-windows-$Arch"
$DistDir = Join-Path $Root 'dist'
$Staging = Join-Path $DistDir $PackageName
$Archive = "$Staging.zip"

Write-Host "==> building $Target (groundgraph-cli + groundgraph-mcp)"
cargo build --release --locked --target $Target -p groundgraph-cli -p groundgraph-mcp

if (Test-Path $Staging) { Remove-Item -Recurse -Force $Staging }
if (Test-Path $Archive) { Remove-Item -Force $Archive }

$dirs = @(
  "$Staging\bin",
  "$Staging\tool\groundgraph_dart_analyzer\bin",
  "$Staging\tool\groundgraph_dart_analyzer\lib",
  "$Staging\tool\groundgraph_dart_analyzer\test",
  "$Staging\skills"
)
foreach ($d in $dirs) { New-Item -ItemType Directory -Force -Path $d | Out-Null }

Write-Host '==> staging binaries'
# No shell wrappers on Windows — both .exe go straight into bin/.
Copy-Item "$Root\target\$Target\release\groundgraph.exe" "$Staging\bin\groundgraph.exe"
Copy-Item "$Root\target\$Target\release\groundgraph-mcp.exe" "$Staging\bin\groundgraph-mcp.exe"

Write-Host '==> copying Dart analyzer sidecar'
$sidecar = "$Root\tool\groundgraph_dart_analyzer"
Copy-Item "$sidecar\README.md" "$Staging\tool\groundgraph_dart_analyzer\README.md"
Copy-Item "$sidecar\analysis_options.yaml" "$Staging\tool\groundgraph_dart_analyzer\analysis_options.yaml"
Copy-Item "$sidecar\pubspec.yaml" "$Staging\tool\groundgraph_dart_analyzer\pubspec.yaml"
if (Test-Path "$sidecar\pubspec.lock") {
  Copy-Item "$sidecar\pubspec.lock" "$Staging\tool\groundgraph_dart_analyzer\pubspec.lock"
}
Copy-Item "$sidecar\bin\groundgraph_dart_analyzer.dart" "$Staging\tool\groundgraph_dart_analyzer\bin\groundgraph_dart_analyzer.dart"
Copy-Item "$sidecar\lib\protocol.dart" "$Staging\tool\groundgraph_dart_analyzer\lib\protocol.dart"
Copy-Item "$sidecar\lib\walker.dart" "$Staging\tool\groundgraph_dart_analyzer\lib\walker.dart"
Copy-Item "$sidecar\test\walker_test.dart" "$Staging\tool\groundgraph_dart_analyzer\test\walker_test.dart"

Write-Host '==> copying AI skill'
# #99: ship the single source-of-truth skill (skills/groundgraph), not a
# drift-prone packaging copy.
Copy-Item -Recurse "$Root\skills\groundgraph" "$Staging\skills\groundgraph"

Write-Host '==> copying package docs'
Copy-Item "$Root\packaging\macos\README.md" "$Staging\README.md"
Copy-Item "$Root\packaging\macos\README-AI-SKILL.md" "$Staging\README-AI-SKILL.md"

$gitRev = 'unknown'
try {
  $rev = & git -C $Root rev-parse --short HEAD 2>$null
  if ($rev) { $gitRev = $rev.Trim() }
} catch {}
$buildTime = (Get-Date).ToUniversalTime().ToString('yyyy-MM-dd''T''HH:mm:ss''Z')
$buildInfo = @"
name: groundgraph
version: $Version
package: $PackageName
git_revision: $gitRev
build_time_utc: $buildTime
binaries: windows $Arch (groundgraph.exe + groundgraph-mcp.exe), target $Target
sidecar: Dart analyzer source included under tool/groundgraph_dart_analyzer
"@
Set-Content -Path "$Staging\BUILD-INFO.txt" -Value $buildInfo -Encoding UTF8

# Defensive: never ship a Dart cache dir.
$dartTool = "$Staging\tool\groundgraph_dart_analyzer\.dart_tool"
if (Test-Path $dartTool) { Remove-Item -Recurse -Force $dartTool }

Write-Host '==> creating archive'
# Compress from the dist dir using the package leaf name so the zip has a single
# top-level directory (matches the tar packages and `Expand-Archive` UX).
Push-Location $DistDir
try {
  Compress-Archive -Path $PackageName -DestinationPath "$PackageName.zip"
} finally {
  Pop-Location
}

# issues.md #80: relative filename in the checksum so `certutil`/`Get-FileHash`
# verification matches the file a downloader actually has.
$hash = (Get-FileHash $Archive -Algorithm SHA256).Hash.ToLower()
"$hash  $PackageName.zip" | Set-Content -Path "$Archive.sha256" -Encoding ascii

Write-Host "archive: $Archive"
Get-Content "$Archive.sha256"
