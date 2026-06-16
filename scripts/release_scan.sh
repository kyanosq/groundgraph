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

# issues.md #83: NAME becomes a directory under release-scans/_scratch and is
# the target of `rsync --delete`. A traversal value like "../../crates" would
# make rsync wipe the specslice tree. Restrict NAME to a safe slug; reject any
# slash, leading dot, or `..` component before we touch the filesystem.
case "$NAME" in
  "" | . | .. | */* | *..* | .*)
    echo "error: invalid NAME '$NAME' — must be a plain slug ([A-Za-z0-9._-], no '/', no '..', no leading '.')" >&2
    exit 2
    ;;
esac
case "$NAME" in
  *[!A-Za-z0-9._-]*)
    echo "error: invalid NAME '$NAME' — only [A-Za-z0-9._-] allowed" >&2
    exit 2
    ;;
esac
if [ ! -d "$SRC" ]; then
  echo "error: SRC '$SRC' is not an existing directory" >&2
  exit 2
fi

# issues.md #81: never copy run-time secrets out of the target repo into our
# scratch tree (a later `tar -czf backup.tar.gz .` would otherwise leak third
# party production keys). Applied to every language's rsync below.
SECRET_EXCLUDES=(
  --exclude '.env' --exclude '.env.*' --exclude '*.env'
  --exclude '**/.env' --exclude '**/.env.*' --exclude '**/*.env'
  --exclude '*.pem' --exclude '*.key' --exclude '*.p12' --exclude '*.pfx'
  --exclude '*.keystore' --exclude 'secrets.*' --exclude '*secrets*.json'
  --exclude 'credentials' --exclude 'credentials.*' --exclude '*credentials*'
  --exclude '.aws/' --exclude '.ssh/' --exclude '.netrc'
  --exclude '.npmrc' --exclude '.pypirc'
)

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
    rsync -a --delete "${SECRET_EXCLUDES[@]}" \
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
    rsync -a --delete "${SECRET_EXCLUDES[@]}" \
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
    rsync -a --delete "${SECRET_EXCLUDES[@]}" \
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
    rsync -a --delete "${SECRET_EXCLUDES[@]}" \
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

# issues.md #81 belt-and-braces: delete any secret files that slipped past the
# rsync excludes (unusual names) before anything reads the scratch copy. Keep
# *.example templates which are safe by convention.
find "$SCRATCH" -type f \( \
  -name '.env' -o -name '.env.*' -o -name '*.env' \
  -o -name '*.pem' -o -name '*.key' -o -name '*.p12' -o -name '*.pfx' \
  -o -name '*.keystore' -o -name '.netrc' \) \
  ! -name '*.example' -delete 2>/dev/null || true

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
# issues.md #85/#185: pass the report path and array key as argv, never
# interpolate them into the Python source. A `$REPORT` containing a single
# quote or `__import__(...)` payload can no longer break out of the string.
count_json_array() {
  python3 -c 'import json, sys
try:
    with open(sys.argv[1]) as fh:
        doc = json.load(fh)
    print(len(doc.get(sys.argv[2], [])))
except Exception:
    print("n/a")' "$1" "$2"
}
NODE_COUNT=$(count_json_array "$REPORT/graph-code.json" nodes)
EDGE_COUNT=$(count_json_array "$REPORT/graph-code.json" edges)

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
