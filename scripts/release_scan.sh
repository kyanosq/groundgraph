#!/usr/bin/env bash
# v0.2.0 收口阶段：对外部示例仓做"非侵入式"真实扫描。
#
# 策略：
# 1. 用 rsync 把目标仓的源码同步到 release-scans/_scratch/<name>/，
#    丢弃 node_modules / target / build / .dart_tool / .git / dist 等
#    本地工具产物。这样目标仓自始至终保持 0 副作用 —— 连 .specslice/
#    都不会创建在用户的代码库里。
# 2. 在 scratch 仓里写一份 .specslice.yaml（开启目标语言的 adapter），
#    跑 `specslice init` + `specslice index` + `specslice check`。
# 3. 把摘要写入 reports/release/<name>/{report.md, index.txt, check.txt}，
#    并把图导出（graph.json）也存进去（HTML 太大且 gitignored）。
# 4. graph.db / HTML / scratch 副本全部留在本仓库内部，不会污染目标仓。
#
# 用法：
#   release_scan.sh <name> <src_path> <language>
# 语言：dart | typescript | python | java

set -euo pipefail

NAME="${1:?need name}"
SRC="${2:?need src path}"
LANG="${3:?need language: dart|typescript|python|java}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SPECSLICE="$ROOT/target/release/specslice"
SCRATCH_BASE="$ROOT/release-scans/_scratch"
REPORTS_BASE="$ROOT/reports/release"

SCRATCH="$SCRATCH_BASE/$NAME"
REPORT="$REPORTS_BASE/$NAME"
mkdir -p "$SCRATCH" "$REPORT"

echo "==> [$NAME] rsync $SRC → $SCRATCH"
case "$LANG" in
  dart)
    rsync -a --delete \
      --exclude '.git/' --exclude '.dart_tool/' --exclude 'build/' \
      --exclude '.idea/' --exclude '.vscode/' --exclude '.specslice/' \
      --exclude '**/node_modules/' --exclude 'macos/Pods/' --exclude 'ios/Pods/' \
      --exclude '**/.cxx/' --exclude '**/Carthage/' --exclude '**/.gradle/' \
      --exclude '**/build/' --exclude '**/.symbols/' --exclude '**/*.lock' \
      --exclude '*.jks' --exclude '*.keystore' --exclude '*.aab' --exclude '*.apk' \
      --exclude '*.ipa' --exclude '*.zip' --exclude '*.tar.gz' \
      --exclude 'ohos/' --exclude 'huawei/' \
      --exclude '*.pbxproj' --exclude 'screenshots/' --exclude 'assets/' \
      "$SRC"/ "$SCRATCH"/
    cat > "$SCRATCH/.specslice.yaml" <<'YAML'
docs:
  paths: [docs, README.md]
code:
  paths: [lib, test, integration_test]
YAML
    ;;
  typescript)
    rsync -a --delete \
      --exclude '.git/' --exclude 'node_modules/' --exclude 'dist/' \
      --exclude '.next/' --exclude '.turbo/' --exclude 'coverage/' \
      --exclude '.specslice/' --exclude '*.lock' --exclude 'package-lock.json' \
      --exclude '.cache/' --exclude '*.log' \
      "$SRC"/ "$SCRATCH"/
    cat > "$SCRATCH/.specslice.yaml" <<'YAML'
docs:
  paths: [README.md, docs]
typescript:
  enabled: true
  paths: [".", components, lib, src, tests, test]
YAML
    ;;
  python)
    rsync -a --delete \
      --exclude '.git/' --exclude '.venv/' --exclude 'venv/' --exclude '__pycache__/' \
      --exclude '.pytest_cache/' --exclude '.ruff_cache/' --exclude '.mypy_cache/' \
      --exclude '.specslice/' --exclude 'node_modules/' --exclude 'dist/' \
      --exclude 'build/' --exclude '*.egg-info' --exclude '.tox/' \
      --exclude 'coverage/' --exclude '.coverage*' --exclude '*.log' \
      --exclude 'frontend/node_modules/' --exclude 'frontend/dist/' --exclude 'frontend/.next/' \
      --exclude 'frontend/coverage/' --exclude '**/__pycache__/' \
      "$SRC"/ "$SCRATCH"/
    cat > "$SCRATCH/.specslice.yaml" <<'YAML'
docs:
  paths: [docs, README.md, AGENTS.md]
python:
  enabled: true
  paths: [backend, src, tests, test]
YAML
    ;;
  java)
    rsync -a --delete \
      --exclude '.git/' --exclude 'target/' --exclude 'build/' \
      --exclude '.idea/' --exclude '.vscode/' --exclude '.gradle/' \
      --exclude '.specslice/' --exclude '*.class' --exclude '*.jar' \
      --exclude '*.war' --exclude '*.log' --exclude 'logs/' \
      "$SRC"/ "$SCRATCH"/
    cat > "$SCRATCH/.specslice.yaml" <<'YAML'
docs:
  paths: [README.md, docs]
java:
  enabled: true
  paths: [src, "."]
YAML
    ;;
  *) echo "unknown language: $LANG" >&2; exit 1;;
esac

# Some scratched trees may still ship a `.specslice.yaml` from the
# source repo (none of the targets do today, but be defensive).
rm -rf "$SCRATCH/.specslice"

echo "==> [$NAME] init"
"$SPECSLICE" --repo-root "$SCRATCH" init >"$REPORT/init.txt" 2>&1 || \
  { tail -20 "$REPORT/init.txt"; exit 1; }

echo "==> [$NAME] index"
"$SPECSLICE" --repo-root "$SCRATCH" index >"$REPORT/index.txt" 2>&1

echo "==> [$NAME] check (best-effort, allowed to surface findings)"
"$SPECSLICE" --repo-root "$SCRATCH" check >"$REPORT/check.txt" 2>&1 || true

echo "==> [$NAME] graph code view (JSON)"
"$SPECSLICE" --repo-root "$SCRATCH" graph \
  --format json --view code >"$REPORT/graph-code.json" 2>>"$REPORT/check.txt" || true

echo "==> [$NAME] graph business view (JSON)"
"$SPECSLICE" --repo-root "$SCRATCH" graph \
  --format json --view business >"$REPORT/graph-business.json" 2>>"$REPORT/check.txt" || true

echo "==> [$NAME] dead-code (best-effort)"
"$SPECSLICE" --repo-root "$SCRATCH" dead-code \
  --json --min-confidence high >"$REPORT/dead-code-high.json" 2>>"$REPORT/check.txt" || true

# Snapshot quick summary of files / nodes so the report Markdown can
# render real numbers without re-parsing the JSON.
FILES_SCANNED=$(grep -E "files:" "$REPORT/index.txt" | head -5 | tr -d ' ' | paste -sd ' ' - || true)
SYMBOLS_SCANNED=$(grep -E "Symbols:" "$REPORT/index.txt" | head -5 | tr -d ' ' | paste -sd ' ' - || true)
NODE_COUNT=$(python3 -c "import json,sys; d=json.load(open('$REPORT/graph-code.json')); print(len(d.get('nodes',[])))" 2>/dev/null || echo "n/a")
EDGE_COUNT=$(python3 -c "import json,sys; d=json.load(open('$REPORT/graph-code.json')); print(len(d.get('edges',[])))" 2>/dev/null || echo "n/a")

echo "==> [$NAME] writing report"
{
  echo "## $NAME ($LANG) — specslice 0.2.0 真实扫描"
  echo
  echo "- 源仓: \`$SRC\`"
  echo "- scratch 副本: \`release-scans/_scratch/$NAME/\`（已 gitignore）"
  echo "- 目标仓副作用: 无 — 没有任何 \`.specslice/\` / \`graph.db\` / export 文件落到源仓内。"
  echo
  echo "### \`specslice index\` 输出"
  echo
  echo '```'
  cat "$REPORT/index.txt"
  echo '```'
  echo
  echo "### \`specslice check\` 摘要（前 60 行）"
  echo
  echo '```'
  head -60 "$REPORT/check.txt"
  echo '```'
  echo
  echo "### Graph code-view 规模"
  echo
  echo "- 节点: \`$NODE_COUNT\`"
  echo "- 边: \`$EDGE_COUNT\`"
  echo
  echo "### dead-code (high) 摘要（前 30 行）"
  echo
  echo '```'
  head -30 "$REPORT/dead-code-high.json"
  echo
  echo '```'
} > "$REPORT/report.md"

echo "==> [$NAME] done. report: $REPORT/report.md"
