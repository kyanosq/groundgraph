//! P6.1–P6.4 — Self-contained HTML renderer for
//! `specslice search --format html`.
//!
//! The output is a **search-centric reader**, not a full-graph dump:
//!
//! - Left rail: ranked match list (the `focus_cards`). Clicking a card
//!   swaps the centre canvas.
//! - Centre canvas: only the *selected match's* 1-hop subgraph,
//!   constrained to ≤ 25 nodes by the engine-side budget. Anchor sits
//!   in the middle; neighbours layered by edge kind so the operator
//!   can read calls / persists_to / declares_verification at a glance.
//! - Right inspector: match reasons + upstream/downstream edges
//!   grouped by kind + tests + candidate description card.
//!
//! Hard constraints (mirror `graph_html.rs`):
//!
//! - No remote URLs, no CDN, no npm dev server.
//! - One physical HTML file: structure + CSS + embedded JSON + JS.
//! - JSON sanitised so `</script>` inside snippets cannot escape.

use anyhow::Result;
use specslice_engine::search::SearchHtmlPayload;

pub fn render_html(payload: &SearchHtmlPayload) -> Result<String> {
    let json = serde_json::to_string(payload)?;
    let safe = sanitize_for_script(&json);
    let mut out = String::with_capacity(STATIC_TEMPLATE.len() + json.len() + 4096);
    out.push_str(DOCTYPE);
    out.push_str(STATIC_TEMPLATE);
    out.push_str("<script id=\"specslice-search-data\" type=\"application/json\">");
    out.push_str(&safe);
    out.push_str("</script>\n");
    out.push_str("<script>\n");
    out.push_str(RENDERER_JS);
    out.push_str("\n</script>\n</body>\n</html>\n");
    Ok(out)
}

fn sanitize_for_script(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'<' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            out.push_str("<\\/");
            i += 2;
            continue;
        }
        out.push(b as char);
        i += 1;
    }
    out
}

const DOCTYPE: &str = "<!doctype html>\n";

const STATIC_TEMPLATE: &str = r#"<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>SpecSlice Search Reader</title>
  <style>
    :root {
      --bg: #f7f7f4;
      --panel: #ffffff;
      --panel-2: #f1f1ec;
      --line: #d9d6cf;
      --text: #1b1c1f;
      --muted: #6b6f76;
      --accent: #2f6f4d;
      --accent-soft: #e6f0ea;
      --candidate: #b07e1a;
      --candidate-soft: #fff6e0;
      --risk: #b3382b;
      --kind-method: #2c5da6;
      --kind-class: #5c3fbf;
      --kind-test: #2f6f4d;
      --kind-storage: #b3712b;
      --kind-route: #b04a8a;
      --kind-provider: #2e7e7e;
      --kind-doc: #4a4a4a;
      --kind-file: #777;
      --kind-candidate: #b07e1a;
      --selected: #1b1c1f;
    }
    * { box-sizing: border-box; }
    html, body {
      margin: 0; padding: 0;
      background: var(--bg); color: var(--text);
      font: 13px/1.45 -apple-system, BlinkMacSystemFont, "Segoe UI",
            "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", Roboto, sans-serif;
    }
    button { font: inherit; color: inherit; background: transparent; border: 0; cursor: pointer; }
    code, .mono { font-family: "JetBrains Mono", "SF Mono", Menlo, Consolas, monospace; font-size: 12px; }
    header.toolbar {
      display: flex; flex-wrap: wrap; gap: 10px; align-items: baseline;
      padding: 12px 18px; border-bottom: 1px solid var(--line);
      background: var(--panel); position: sticky; top: 0; z-index: 5;
    }
    header h1 {
      font-size: 13px; margin: 0; letter-spacing: 0.04em;
      text-transform: uppercase; color: var(--muted);
    }
    header .q { font-size: 15px; font-weight: 600; }
    header .tokens { color: var(--muted); font-size: 12px; }
    header .tokens .tok {
      display: inline-block; padding: 1px 8px; margin: 0 3px 0 0;
      border-radius: 999px; background: var(--panel-2); border: 1px solid var(--line);
    }
    header .stats { color: var(--muted); margin-left: auto; font-size: 12px; }
    header .stats b { color: var(--text); }
    /* P0b — edge-kind filter chips. Toggling re-renders canvas +
       inspector so the operator can isolate calls vs storage vs
       tests vs business-semantic edges. */
    header .edge-filters {
      display: flex; flex-wrap: wrap; gap: 6px; width: 100%;
      padding-top: 4px; margin-top: 2px;
    }
    header .edge-filters .label {
      font-size: 11px; color: var(--muted); padding: 2px 4px 0;
      letter-spacing: 0.04em; text-transform: uppercase;
    }
    .chip.filter {
      cursor: pointer; user-select: none;
      transition: background 0.08s ease, color 0.08s ease, opacity 0.08s ease;
    }
    .chip.filter:hover { background: var(--panel); }
    .chip.filter.off {
      opacity: 0.45;
      text-decoration: line-through;
    }
    .chip.filter .swatch {
      display: inline-block; width: 8px; height: 8px;
      border-radius: 50%; margin-right: 6px; vertical-align: middle;
    }
    .chip.filter .count {
      color: var(--muted); font-size: 10px; margin-left: 4px;
    }
    main.reader {
      display: grid;
      grid-template-columns: minmax(260px, 320px) 1fr minmax(280px, 360px);
      min-height: calc(100vh - 52px);
    }
    aside.matches, aside.inspector {
      background: var(--panel);
      overflow: auto;
    }
    aside.matches { border-right: 1px solid var(--line); padding: 8px 0; }
    aside.inspector { border-left: 1px solid var(--line); padding: 14px 16px; }
    .match-card {
      padding: 10px 14px; border-bottom: 1px solid var(--line);
      cursor: pointer; transition: background 0.08s ease;
    }
    .match-card:hover { background: var(--panel-2); }
    .match-card.active {
      background: var(--accent-soft);
      box-shadow: inset 3px 0 0 var(--accent);
    }
    .match-card .row1 { display: flex; gap: 6px; align-items: baseline; }
    .match-card .label { font-weight: 600; flex: 1; word-break: break-word; }
    .match-card .score { font-size: 11px; color: var(--muted); }
    .match-card .badge {
      display: inline-block; padding: 1px 7px; border-radius: 999px;
      font-size: 10px; background: var(--panel-2); border: 1px solid var(--line);
      color: var(--muted); margin-left: 4px; white-space: nowrap;
    }
    .match-card.is-candidate .badge { color: var(--candidate); border-color: var(--candidate); background: var(--candidate-soft); }
    .match-card .path { color: var(--muted); font-size: 11px; margin-top: 2px; word-break: break-all; }
    .match-card .reasons { color: var(--muted); font-size: 11px; margin-top: 4px; }
    section.canvas {
      position: relative;
      background: var(--panel);
      overflow: hidden;
      display: flex; flex-direction: column;
    }
    section.canvas .canvas-header {
      padding: 10px 16px; border-bottom: 1px solid var(--line);
      display: flex; align-items: baseline; gap: 10px; flex-wrap: wrap;
    }
    section.canvas .canvas-header .anchor-label { font-weight: 600; font-size: 14px; }
    section.canvas .canvas-header .truncated {
      color: var(--candidate); font-size: 11px;
      background: var(--candidate-soft); border: 1px solid var(--candidate);
      padding: 1px 8px; border-radius: 999px;
    }
    section.canvas svg { flex: 1; width: 100%; height: 100%; min-height: 360px; }
    .canvas-node { cursor: pointer; }
    .canvas-node text { font-size: 11px; pointer-events: none; }
    .canvas-node rect { stroke-width: 1.2; }
    .canvas-node .expand-badge {
      pointer-events: none;
    }
    .canvas-node.expandable rect {
      stroke-dasharray: 4 3;
    }
    .canvas-edge { cursor: pointer; }
    .canvas-edge.selected line { stroke-width: 2.5; }
    .canvas-edge text { font-size: 10px; fill: var(--muted); pointer-events: none; }
    /* P0b — "collapse" affordance for nodes the operator manually
       expanded into the canvas. Anchor + initial focus set are not
       collapsible. */
    .canvas-node.expanded rect { stroke-dasharray: 0; }
    .empty { padding: 24px; color: var(--muted); font-size: 13px; }

    aside.inspector h2 {
      font-size: 11px; text-transform: uppercase; letter-spacing: 0.05em;
      color: var(--muted); margin: 14px 0 6px; border-top: 1px solid var(--line); padding-top: 12px;
    }
    aside.inspector h2:first-of-type { border-top: 0; padding-top: 0; margin-top: 0; }
    aside.inspector .anchor-title { font-size: 15px; font-weight: 600; word-break: break-word; }
    aside.inspector .chips { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 6px; }
    .chip {
      display: inline-block; padding: 1px 8px; font-size: 11px; border-radius: 999px;
      background: var(--panel-2); border: 1px solid var(--line); color: var(--muted);
    }
    .chip.score { color: var(--accent); border-color: var(--accent); }
    .chip.kind { color: var(--kind-method); border-color: var(--kind-method); }
    .chip.candidate { color: var(--candidate); border-color: var(--candidate); background: var(--candidate-soft); }
    aside.inspector .reasons li { margin: 2px 0; color: var(--text); font-size: 12px; }
    aside.inspector ul { padding-left: 18px; margin: 4px 0; }
    .edge-group {
      margin-bottom: 6px;
    }
    .edge-group .group-header {
      display: flex; align-items: baseline; gap: 6px;
      cursor: pointer; user-select: none;
      padding: 4px 0; border-bottom: 1px dashed var(--line);
    }
    .edge-group .group-header b { font-size: 12px; }
    .edge-group .group-header .count { color: var(--muted); font-size: 11px; }
    .edge-group .items { margin: 4px 0 6px; padding-left: 0; list-style: none; }
    .edge-group .items.collapsed { display: none; }
    .edge-row {
      padding: 4px 4px; border-bottom: 1px dotted var(--line);
      cursor: pointer;
    }
    .edge-row:hover { background: var(--panel-2); }
    .edge-row.selected { background: var(--accent-soft); }
    .edge-row .neighbor { font-weight: 600; font-size: 12px; }
    .edge-row .meta { color: var(--muted); font-size: 11px; }
    .candidate-card {
      background: var(--candidate-soft); border: 1px solid var(--candidate);
      border-radius: 8px; padding: 10px 12px; margin-top: 6px;
    }
    .candidate-card .status {
      display: inline-block; padding: 1px 8px; border-radius: 999px;
      background: var(--candidate); color: #fff; font-size: 10px;
      text-transform: uppercase; letter-spacing: 0.05em;
    }
    .candidate-card.accepted .status { background: var(--accent); }
    .candidate-card.rejected .status { background: var(--risk); }
    .candidate-card p { margin: 6px 0 0; }
    .candidate-card .risk { color: var(--risk); }
    .test-row {
      padding: 4px 0; border-bottom: 1px dotted var(--line);
      font-size: 12px;
    }
    .test-row .name { font-weight: 600; }
    .test-row .path { color: var(--muted); font-size: 11px; word-break: break-all; }
    .edge-detail {
      margin-top: 8px; padding: 8px 10px; background: var(--panel-2);
      border: 1px solid var(--line); border-radius: 6px; font-size: 12px;
    }
    .edge-detail .meta { color: var(--muted); font-size: 11px; }
    .edge-detail .snippet {
      margin-top: 6px; padding: 6px 8px; background: #fff;
      border: 1px solid var(--line); border-radius: 4px;
      white-space: pre-wrap; word-break: break-word;
    }
    .copy-btn {
      font-size: 11px; padding: 2px 8px; border: 1px solid var(--line);
      border-radius: 4px; background: var(--panel-2); margin-left: 6px;
    }
    .copy-btn:hover { background: var(--panel); }
    @media (max-width: 1024px) {
      main.reader { grid-template-columns: 1fr; }
      aside.matches, aside.inspector { max-height: 40vh; }
      section.canvas { min-height: 320px; border-top: 1px solid var(--line); }
    }
  </style>
</head>
<body>
<header class="toolbar">
  <h1>SpecSlice Search</h1>
  <span class="q" id="hdr-query"></span>
  <span class="tokens" id="hdr-tokens"></span>
  <span class="stats" id="hdr-stats"></span>
  <div class="edge-filters" id="edge-filter-host"></div>
</header>
<main class="reader">
  <aside class="matches" id="match-list"></aside>
  <section class="canvas">
    <div class="canvas-header">
      <span class="anchor-label" id="canvas-anchor"></span>
      <span class="chip" id="canvas-kind"></span>
      <span class="truncated" id="canvas-truncated" hidden></span>
      <span class="chip" id="canvas-budget"></span>
    </div>
    <svg id="canvas-svg" viewBox="0 0 800 480" preserveAspectRatio="xMidYMid meet"></svg>
  </section>
  <aside class="inspector" id="inspector"></aside>
</main>
"#;

const RENDERER_JS: &str = r#"
(function () {
  const dataEl = document.getElementById('specslice-search-data');
  if (!dataEl) return;
  let payload;
  try { payload = JSON.parse(dataEl.textContent); } catch (e) {
    document.body.innerHTML = '<pre>无法解析 search payload: ' + e.message + '</pre>';
    return;
  }
  const cards = payload.focus_cards || [];
  const fullSubgraph = payload.full_subgraph || { nodes: [], edges: [] };
  const fullNodeIndex = {};
  (fullSubgraph.nodes || []).forEach(function (n) { fullNodeIndex[n.id] = n; });
  const edgesByNode = {};
  (fullSubgraph.edges || []).forEach(function (e) {
    if (!edgesByNode[e.from]) edgesByNode[e.from] = [];
    if (!edgesByNode[e.to]) edgesByNode[e.to] = [];
    edgesByNode[e.from].push(e);
    edgesByNode[e.to].push(e);
  });

  // ---------- Header
  document.getElementById('hdr-query').textContent = payload.query || '';
  const tokensEl = document.getElementById('hdr-tokens');
  tokensEl.innerHTML = '';
  (payload.tokens || []).forEach(function (t) {
    const span = document.createElement('span');
    span.className = 'tok';
    span.textContent = t;
    tokensEl.appendChild(span);
  });
  document.getElementById('hdr-stats').innerHTML =
    '命中 <b>' + (payload.matches_total || 0) + '</b> · 焦点卡片 <b>' + cards.length + '</b>';

  // ---------- Edge-kind filter chips (toolbar)
  const hiddenEdgeKinds = new Set();
  const filterHost = document.getElementById('edge-filter-host');
  filterHost.innerHTML = '';
  if ((payload.edge_kinds || []).length) {
    const lbl = document.createElement('span');
    lbl.className = 'label';
    lbl.textContent = '按边过滤';
    filterHost.appendChild(lbl);
    payload.edge_kinds.forEach(function (meta) {
      const chip = document.createElement('span');
      chip.className = 'chip filter';
      chip.dataset.kind = meta.kind;
      const swatch = document.createElement('span');
      swatch.className = 'swatch';
      swatch.style.background = edgeStroke(meta.kind);
      chip.appendChild(swatch);
      chip.appendChild(document.createTextNode(meta.kind));
      const count = document.createElement('span');
      count.className = 'count';
      count.textContent = '·' + meta.count;
      chip.appendChild(count);
      chip.title = '点击隐藏 / 显示 `' + meta.kind + '` 类型的边';
      chip.addEventListener('click', function () {
        if (hiddenEdgeKinds.has(meta.kind)) {
          hiddenEdgeKinds.delete(meta.kind);
          chip.classList.remove('off');
        } else {
          hiddenEdgeKinds.add(meta.kind);
          chip.classList.add('off');
        }
        if (activeIdx >= 0) renderActive();
      });
      filterHost.appendChild(chip);
    });
  }

  // ---------- Match list (left rail)
  const listEl = document.getElementById('match-list');
  if (!cards.length) {
    listEl.innerHTML = '<div class="empty">未命中任何节点。试试其它关键词或更宽的 --depth。</div>';
  }
  cards.forEach(function (card, idx) {
    const div = document.createElement('div');
    div.className = 'match-card' + (card.candidate ? ' is-candidate' : '');
    div.dataset.idx = String(idx);
    const row1 = document.createElement('div');
    row1.className = 'row1';
    const label = document.createElement('span');
    label.className = 'label';
    label.textContent = card.label || card.match_id;
    row1.appendChild(label);
    const score = document.createElement('span');
    score.className = 'score';
    score.textContent = '分 ' + card.score;
    row1.appendChild(score);
    const badge = document.createElement('span');
    badge.className = 'badge';
    badge.textContent = card.badge || card.kind;
    row1.appendChild(badge);
    div.appendChild(row1);
    if (card.path) {
      const p = document.createElement('div');
      p.className = 'path mono';
      p.textContent = card.path + (card.line_range ? ':' + card.line_range[0] + '-' + card.line_range[1] : '');
      div.appendChild(p);
    }
    if (card.match_reasons && card.match_reasons.length) {
      const r = document.createElement('div');
      r.className = 'reasons';
      r.textContent = '原因: ' + card.match_reasons.slice(0, 2).join(' · ');
      div.appendChild(r);
    }
    div.addEventListener('click', function () { selectCard(idx); });
    listEl.appendChild(div);
  });

  let activeIdx = -1;
  let selectedEdgeId = null;
  // P0b — per-card "expanded" sets. Each card starts with its
  // `focused` node ids visible; the operator can click a node on the
  // canvas to reveal its hidden 1-hop neighbours (drawn from
  // `payload.full_subgraph`).
  const expandedByCard = new Map();

  function visibleIdsForCard(card, idx) {
    const baseline = (card.focused && card.focused.nodes) || [];
    const set = new Set(baseline.map(function (n) { return n.id; }));
    const added = expandedByCard.get(idx);
    if (added) added.forEach(function (id) { set.add(id); });
    return set;
  }

  function selectCard(idx) {
    activeIdx = idx;
    selectedEdgeId = null;
    Array.prototype.forEach.call(listEl.children, function (el, i) {
      if (el.classList && el.classList.toggle) {
        el.classList.toggle('active', i === idx);
      }
    });
    renderActive();
  }

  function renderActive() {
    if (activeIdx < 0 || activeIdx >= cards.length) return;
    renderCanvas(cards[activeIdx], activeIdx);
    renderInspector(cards[activeIdx], activeIdx);
  }

  // ---------- Canvas (centre)
  function renderCanvas(card, cardIdx) {
    const svg = document.getElementById('canvas-svg');
    while (svg.firstChild) svg.removeChild(svg.firstChild);
    document.getElementById('canvas-anchor').textContent = card.label || card.match_id;
    document.getElementById('canvas-kind').textContent = card.badge || card.kind;

    // Compute the *current* visible set (focused + manual expansions).
    const visibleIds = visibleIdsForCard(card, cardIdx);
    const anchorId = card.match_id;
    const baselineIds = new Set((card.focused && card.focused.nodes || []).map(function (n) { return n.id; }));

    // Filtered edges = edges from full_subgraph whose endpoints are
    // both visible AND whose kind is not hidden by the toolbar.
    const visibleEdges = (fullSubgraph.edges || []).filter(function (e) {
      return visibleIds.has(e.from) && visibleIds.has(e.to) && !hiddenEdgeKinds.has(e.kind);
    });

    // Drop nodes that become orphan after edge filtering — but always
    // keep the anchor and any explicitly expanded node so the operator
    // doesn't lose context.
    const expandedSet = expandedByCard.get(cardIdx) || new Set();
    const connected = new Set([anchorId]);
    visibleEdges.forEach(function (e) {
      connected.add(e.from);
      connected.add(e.to);
    });
    const renderedNodeIds = new Set();
    visibleIds.forEach(function (id) {
      if (id === anchorId || baselineIds.has(id) || expandedSet.has(id) || connected.has(id)) {
        renderedNodeIds.add(id);
      }
    });

    // Recompute hidden-neighbour count per visible node so the canvas
    // can show a "+N" badge that invites further drill-down.
    const hiddenNeighborsByNode = {};
    renderedNodeIds.forEach(function (id) {
      const peers = edgesByNode[id] || [];
      const hidden = new Set();
      peers.forEach(function (e) {
        if (hiddenEdgeKinds.has(e.kind)) return;
        const other = e.from === id ? e.to : e.from;
        if (other === id) return;
        if (!renderedNodeIds.has(other)) hidden.add(other);
      });
      hiddenNeighborsByNode[id] = hidden;
    });

    const hiddenCountTotal = (fullSubgraph.nodes || []).length - renderedNodeIds.size;
    const trunc = document.getElementById('canvas-truncated');
    if (hiddenCountTotal > 0) {
      trunc.hidden = false;
      trunc.textContent = '已折叠 ' + hiddenCountTotal + ' 个邻居 · 点节点展开';
    } else {
      trunc.hidden = true;
    }
    document.getElementById('canvas-budget').textContent =
      '画布: ' + renderedNodeIds.size + ' / ' + (fullSubgraph.nodes || []).length + ' 节点';

    // Layout: anchor in centre, others arranged in a ring sorted by
    // edge priority bucket (tests > business semantics > calls > misc).
    const renderedNodes = [];
    renderedNodeIds.forEach(function (id) {
      const fromFull = fullNodeIndex[id];
      const fromFocus = (card.focused && card.focused.nodes || []).find(function (n) { return n.id === id; });
      const n = fromFocus || fromFull;
      if (n) renderedNodes.push(n);
    });

    const anchor = renderedNodes.find(function (n) { return n.id === anchorId; });
    const others = renderedNodes.filter(function (n) { return n.id !== anchorId; });
    others.sort(function (a, b) {
      const pa = neighbourPriority(card, a.id);
      const pb = neighbourPriority(card, b.id);
      if (pa !== pb) return pb - pa;
      return (a.label || a.id).localeCompare(b.label || b.id);
    });

    const w = 880, h = 520;
    const cx = w / 2, cy = h / 2;
    svg.setAttribute('viewBox', '0 0 ' + w + ' ' + h);
    const positions = {};
    if (anchor) positions[anchor.id] = { x: cx, y: cy };
    const ring = Math.min(w, h) * 0.36;
    const n = Math.max(others.length, 1);
    others.forEach(function (node, i) {
      const angle = (i / n) * 2 * Math.PI - Math.PI / 2;
      positions[node.id] = {
        x: cx + Math.cos(angle) * ring,
        y: cy + Math.sin(angle) * ring,
      };
    });

    // Edges first (drawn under nodes).
    visibleEdges.forEach(function (e) {
      const from = positions[e.from];
      const to = positions[e.to];
      if (!from || !to) return;
      const g = svgNs('g');
      g.setAttribute('class', 'canvas-edge' + (selectedEdgeId === e.id ? ' selected' : ''));
      g.dataset.edgeId = e.id;
      const line = svgNs('line');
      line.setAttribute('x1', from.x);
      line.setAttribute('y1', from.y);
      line.setAttribute('x2', to.x);
      line.setAttribute('y2', to.y);
      line.setAttribute('stroke', edgeStroke(e.kind));
      line.setAttribute('stroke-width', '1.4');
      line.setAttribute('stroke-dasharray', e.kind === 'derives_from' ? '4 3' : 'none');
      line.setAttribute('opacity', '0.85');
      g.appendChild(line);
      const t = svgNs('text');
      t.setAttribute('x', (from.x + to.x) / 2);
      t.setAttribute('y', (from.y + to.y) / 2 - 4);
      t.setAttribute('text-anchor', 'middle');
      t.textContent = e.kind;
      g.appendChild(t);
      g.addEventListener('click', function (ev) {
        ev.stopPropagation();
        selectEdge(card, e.id);
      });
      svg.appendChild(g);
    });

    // Nodes (drawn on top).
    renderedNodes.forEach(function (node) {
      const pos = positions[node.id];
      if (!pos) return;
      const isAnchor = node.id === anchorId;
      const isExpanded = expandedSet.has(node.id);
      const hidden = hiddenNeighborsByNode[node.id] || new Set();
      const label = node.label || node.id;
      const charW = 6.6;
      const wBox = Math.min(220, Math.max(80, label.length * charW + 16));
      const hBox = 32;
      const g = svgNs('g');
      g.setAttribute('class', 'canvas-node' + (hidden.size ? ' expandable' : '') + (isExpanded ? ' expanded' : ''));
      g.dataset.nodeId = node.id;
      g.setAttribute('transform', 'translate(' + (pos.x - wBox / 2) + ',' + (pos.y - hBox / 2) + ')');
      const rect = svgNs('rect');
      rect.setAttribute('width', wBox);
      rect.setAttribute('height', hBox);
      rect.setAttribute('rx', '6');
      rect.setAttribute('ry', '6');
      rect.setAttribute('fill', '#fff');
      rect.setAttribute('stroke', nodeStroke(node.kind));
      if (isAnchor) {
        rect.setAttribute('stroke-width', '2.4');
        rect.setAttribute('fill', 'var(--accent-soft)');
      }
      g.appendChild(rect);
      const t1 = svgNs('text');
      t1.setAttribute('x', wBox / 2);
      t1.setAttribute('y', 13);
      t1.setAttribute('text-anchor', 'middle');
      t1.setAttribute('fill', nodeStroke(node.kind));
      t1.style.fontWeight = isAnchor ? '700' : '600';
      t1.textContent = label.length > 32 ? label.slice(0, 31) + '…' : label;
      g.appendChild(t1);
      const t2 = svgNs('text');
      t2.setAttribute('x', wBox / 2);
      t2.setAttribute('y', 26);
      t2.setAttribute('text-anchor', 'middle');
      t2.setAttribute('fill', 'var(--muted)');
      t2.textContent = node.kind;
      g.appendChild(t2);
      if (hidden.size > 0) {
        const badge = svgNs('g');
        badge.setAttribute('class', 'expand-badge');
        const bg = svgNs('circle');
        bg.setAttribute('cx', wBox - 8);
        bg.setAttribute('cy', 8);
        bg.setAttribute('r', 9);
        bg.setAttribute('fill', nodeStroke(node.kind));
        const tx = svgNs('text');
        tx.setAttribute('x', wBox - 8);
        tx.setAttribute('y', 11);
        tx.setAttribute('text-anchor', 'middle');
        tx.setAttribute('fill', '#fff');
        tx.style.fontSize = '10px';
        tx.style.fontWeight = '700';
        tx.textContent = '+' + hidden.size;
        badge.appendChild(bg);
        badge.appendChild(tx);
        g.appendChild(badge);
      }
      g.addEventListener('click', function (ev) {
        ev.stopPropagation();
        if (hidden.size > 0) {
          expandNode(cardIdx, node.id, hidden);
        } else if (!isAnchor && !baselineIds.has(node.id) && expandedSet.has(node.id)) {
          collapseNode(cardIdx, node.id);
        }
      });
      svg.appendChild(g);
    });
  }

  function expandNode(cardIdx, nodeId, hidden) {
    if (!expandedByCard.has(cardIdx)) expandedByCard.set(cardIdx, new Set());
    const set = expandedByCard.get(cardIdx);
    hidden.forEach(function (id) { set.add(id); });
    // Mark the originating node so the operator can also collapse it
    // (the children we just added carry the "expanded" affordance).
    set.add(nodeId);
    renderActive();
  }

  function collapseNode(cardIdx, nodeId) {
    const set = expandedByCard.get(cardIdx);
    if (!set) return;
    set.delete(nodeId);
    // Also drop any expansion that only existed because of this node.
    renderActive();
  }

  function neighbourPriority(card, neighborId) {
    // Mirror engine `edge_priority`: tests + business semantics first.
    const fromCard = (card.upstream || []).concat(card.downstream || []);
    let best = 0;
    for (let i = 0; i < fromCard.length; i++) {
      if (fromCard[i].neighbor_id === neighborId) {
        const p = priorityFor(fromCard[i].edge_kind);
        if (p > best) best = p;
      }
    }
    return best;
  }
  function priorityFor(kind) {
    if (kind === 'declares_verification') return 5;
    if (kind === 'reads_provider' || kind === 'persists_to' || kind === 'navigates_to' || kind === 'subscribes_stream') return 4;
    if (kind === 'derives_from') return 4;
    if (kind === 'calls' || kind === 'references') return 3;
    if (kind === 'contains') return 2;
    return 1;
  }

  function nodeStroke(kind) {
    switch (kind) {
      case 'dart_method':
      case 'dart_function':
      case 'dart_constructor': return 'var(--kind-method)';
      case 'dart_class': return 'var(--kind-class)';
      case 'test_case':
      case 'test_group': return 'var(--kind-test)';
      case 'storage': return 'var(--kind-storage)';
      case 'route': return 'var(--kind-route)';
      case 'dart_provider': return 'var(--kind-provider)';
      case 'doc_section': return 'var(--kind-doc)';
      case 'file': return 'var(--kind-file)';
      case 'business_candidate': return 'var(--kind-candidate)';
      default: return 'var(--muted)';
    }
  }
  function edgeStroke(kind) {
    if (kind === 'declares_verification') return 'var(--kind-test)';
    if (kind === 'persists_to') return 'var(--kind-storage)';
    if (kind === 'navigates_to') return 'var(--kind-route)';
    if (kind === 'reads_provider') return 'var(--kind-provider)';
    if (kind === 'derives_from') return 'var(--candidate)';
    if (kind === 'calls' || kind === 'references') return 'var(--kind-method)';
    return 'var(--muted)';
  }

  // ---------- Inspector (right rail)
  function renderInspector(card, cardIdx) {
    const panel = document.getElementById('inspector');
    panel.innerHTML = '';
    const title = document.createElement('div');
    title.className = 'anchor-title';
    title.textContent = card.label || card.match_id;
    panel.appendChild(title);

    const chips = document.createElement('div');
    chips.className = 'chips';
    const idChip = document.createElement('span');
    idChip.className = 'chip kind';
    idChip.textContent = card.badge || card.kind;
    chips.appendChild(idChip);
    const scoreChip = document.createElement('span');
    scoreChip.className = 'chip score';
    scoreChip.textContent = '分 ' + card.score;
    chips.appendChild(scoreChip);
    if (card.source) {
      const s = document.createElement('span');
      s.className = 'chip';
      s.textContent = card.source;
      chips.appendChild(s);
    }
    if (card.path) {
      const p = document.createElement('span');
      p.className = 'chip mono';
      p.textContent = card.path + (card.line_range ? ':' + card.line_range[0] + '-' + card.line_range[1] : '');
      chips.appendChild(p);
    }
    panel.appendChild(chips);

    // ID + copy
    const idRow = document.createElement('div');
    idRow.className = 'mono';
    idRow.style.marginTop = '6px';
    idRow.style.color = 'var(--muted)';
    idRow.style.fontSize = '11px';
    idRow.textContent = card.match_id;
    const copy = document.createElement('button');
    copy.className = 'copy-btn';
    copy.textContent = '复制 id';
    copy.addEventListener('click', function () {
      navigator.clipboard && navigator.clipboard.writeText(card.match_id);
    });
    idRow.appendChild(copy);
    panel.appendChild(idRow);

    // Match reasons
    panel.appendChild(h2('为什么命中'));
    const ul = document.createElement('ul');
    ul.className = 'reasons';
    (card.match_reasons || []).forEach(function (r) {
      const li = document.createElement('li');
      li.textContent = r;
      ul.appendChild(li);
    });
    panel.appendChild(ul);

    // Candidate card
    if (card.candidate) {
      panel.appendChild(h2('业务候选'));
      const c = card.candidate;
      const wrap = document.createElement('div');
      wrap.className = 'candidate-card ' + (c.status || '');
      const status = document.createElement('span');
      status.className = 'status';
      status.textContent = c.status || 'pending';
      wrap.appendChild(status);
      if (typeof c.confidence === 'number') {
        const conf = document.createElement('span');
        conf.className = 'chip';
        conf.style.marginLeft = '6px';
        conf.textContent = '可信度 ' + (Math.round(c.confidence * 100) / 100);
        wrap.appendChild(conf);
      }
      const desc = document.createElement('p');
      desc.textContent = c.description;
      wrap.appendChild(desc);
      (c.risks || []).forEach(function (r) {
        const p = document.createElement('p');
        p.className = 'risk';
        p.textContent = '风险: ' + r;
        wrap.appendChild(p);
      });
      if (c.recommendation) {
        const p = document.createElement('p');
        p.textContent = '建议: ' + c.recommendation;
        wrap.appendChild(p);
      }
      (c.open_questions || []).forEach(function (q) {
        const p = document.createElement('p');
        p.textContent = '待答: ' + q;
        wrap.appendChild(p);
      });
      panel.appendChild(wrap);
    }

    // Tests
    if (card.tests && card.tests.length) {
      panel.appendChild(h2('测试 (' + card.tests.length + ')'));
      card.tests.forEach(function (t) {
        const row = document.createElement('div');
        row.className = 'test-row';
        const name = document.createElement('div');
        name.className = 'name';
        name.textContent = t.label;
        row.appendChild(name);
        if (t.path) {
          const p = document.createElement('div');
          p.className = 'path mono';
          p.textContent = t.path + (t.line_range ? ':' + t.line_range[0] + '-' + t.line_range[1] : '');
          row.appendChild(p);
        }
        panel.appendChild(row);
      });
    }

    // Upstream / Downstream edges grouped by kind
    const filteredUp = (card.upstream || []).filter(function (r) { return !hiddenEdgeKinds.has(r.edge_kind); });
    const filteredDown = (card.downstream || []).filter(function (r) { return !hiddenEdgeKinds.has(r.edge_kind); });
    panel.appendChild(h2('上游 (谁影响我)'));
    panel.appendChild(renderEdgeGroups(card, filteredUp));
    panel.appendChild(h2('下游 (我影响谁)'));
    panel.appendChild(renderEdgeGroups(card, filteredDown));

    // P0b — expandable neighbours not currently on the canvas. Lets
    // the operator drill into a hidden symbol without learning the
    // graph CLI flags.
    const visibleIds = visibleIdsForCard(card, cardIdx);
    const expandableByNode = computeExpandable(visibleIds);
    if (expandableByNode.size > 0) {
      panel.appendChild(h2('可展开邻居 (' + expandableByNode.size + ')'));
      expandableByNode.forEach(function (hidden, fromId) {
        const row = document.createElement('div');
        row.className = 'edge-row';
        const fromNode = fullNodeIndex[fromId];
        const fromLabel = (fromNode && fromNode.label) || fromId;
        const head = document.createElement('div');
        head.className = 'neighbor';
        head.textContent = '从 ' + fromLabel;
        row.appendChild(head);
        const meta = document.createElement('div');
        meta.className = 'meta';
        meta.textContent = '+' + hidden.size + ' 个隐藏邻居 (' + Array.from(hidden).slice(0, 3).map(function (id) {
          const n = fullNodeIndex[id];
          return (n && n.kind) || 'node';
        }).join(', ') + (hidden.size > 3 ? ', …' : '') + ')';
        row.appendChild(meta);
        const btn = document.createElement('button');
        btn.className = 'copy-btn';
        btn.textContent = '展开';
        btn.addEventListener('click', function () { expandNode(cardIdx, fromId, hidden); });
        meta.appendChild(btn);
        panel.appendChild(row);
      });
    }

    // Edge detail placeholder
    const detail = document.createElement('div');
    detail.id = 'edge-detail-host';
    panel.appendChild(detail);
    if (selectedEdgeId) repaintEdgeDetail(card);
  }

  function h2(text) {
    const el = document.createElement('h2');
    el.textContent = text;
    return el;
  }

  function renderEdgeGroups(card, rows) {
    const wrap = document.createElement('div');
    if (!rows.length) {
      const empty = document.createElement('div');
      empty.className = 'empty';
      empty.style.padding = '4px 0';
      empty.textContent = '(无)';
      wrap.appendChild(empty);
      return wrap;
    }
    const grouped = {};
    rows.forEach(function (r) {
      if (!grouped[r.edge_kind]) grouped[r.edge_kind] = [];
      grouped[r.edge_kind].push(r);
    });
    const kinds = Object.keys(grouped).sort(function (a, b) {
      return priorityFor(b) - priorityFor(a) || a.localeCompare(b);
    });
    kinds.forEach(function (kind) {
      const group = document.createElement('div');
      group.className = 'edge-group';
      const header = document.createElement('div');
      header.className = 'group-header';
      const swatch = document.createElement('span');
      swatch.style.display = 'inline-block';
      swatch.style.width = '8px';
      swatch.style.height = '8px';
      swatch.style.borderRadius = '50%';
      swatch.style.background = edgeStroke(kind);
      header.appendChild(swatch);
      const b = document.createElement('b');
      b.textContent = kind;
      header.appendChild(b);
      const count = document.createElement('span');
      count.className = 'count';
      count.textContent = grouped[kind].length;
      header.appendChild(count);
      const items = document.createElement('ul');
      items.className = 'items';
      grouped[kind].forEach(function (r) {
        const li = document.createElement('li');
        li.className = 'edge-row' + (selectedEdgeId === r.edge_id ? ' selected' : '');
        li.dataset.edgeId = r.edge_id;
        const n = document.createElement('div');
        n.className = 'neighbor';
        n.textContent = r.neighbor_label;
        li.appendChild(n);
        const meta = document.createElement('div');
        meta.className = 'meta mono';
        meta.textContent = (r.neighbor_path || r.neighbor_id) + (r.line_range ? ':' + r.line_range[0] + '-' + r.line_range[1] : '');
        li.appendChild(meta);
        li.addEventListener('click', function () { selectEdge(card, r.edge_id); });
        items.appendChild(li);
      });
      header.addEventListener('click', function () {
        items.classList.toggle('collapsed');
      });
      group.appendChild(header);
      group.appendChild(items);
      wrap.appendChild(group);
    });
    return wrap;
  }

  function selectEdge(card, edgeId) {
    selectedEdgeId = edgeId;
    renderActive();
  }

  /// Compute a map<visible_node_id, Set<hidden_neighbor_id>> for the
  /// current visible set. Used to render the right-rail "可展开邻居"
  /// section. Hidden by edge-kind filter ⇒ not counted as expandable.
  function computeExpandable(visibleIds) {
    const out = new Map();
    visibleIds.forEach(function (id) {
      const peers = edgesByNode[id] || [];
      const hidden = new Set();
      peers.forEach(function (e) {
        if (hiddenEdgeKinds.has(e.kind)) return;
        const other = e.from === id ? e.to : e.from;
        if (other === id) return;
        if (!visibleIds.has(other)) hidden.add(other);
      });
      if (hidden.size > 0) out.set(id, hidden);
    });
    return out;
  }

  function repaintEdgeDetail(card) {
    const host = document.getElementById('edge-detail-host');
    if (!host) return;
    host.innerHTML = '';
    if (!selectedEdgeId) return;
    const all = (card.upstream || []).concat(card.downstream || []);
    const row = all.find(function (r) { return r.edge_id === selectedEdgeId; });
    if (!row) return;
    const wrap = document.createElement('div');
    wrap.className = 'edge-detail';
    const t = document.createElement('div');
    t.innerHTML = '<b>' + row.edge_kind + '</b> · ' + row.neighbor_kind;
    wrap.appendChild(t);
    const m = document.createElement('div');
    m.className = 'meta mono';
    let metaTxt = '邻居: ' + row.neighbor_id;
    if (row.source_file) metaTxt += '\n来源: ' + row.source_file;
    if (row.line_range) metaTxt += ':' + row.line_range[0] + '-' + row.line_range[1];
    m.textContent = metaTxt;
    m.style.whiteSpace = 'pre-wrap';
    wrap.appendChild(m);
    if (row.snippet) {
      const s = document.createElement('div');
      s.className = 'snippet mono';
      s.textContent = row.snippet;
      wrap.appendChild(s);
    }
    host.appendChild(wrap);
  }

  function svgNs(tag) {
    return document.createElementNS('http://www.w3.org/2000/svg', tag);
  }

  if (cards.length) selectCard(0);
})();
"#;
