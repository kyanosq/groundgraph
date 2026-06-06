//! Self-contained HTML renderer for `specslice graph --format html`.
//!
//! P6 shipped a lane-strip viewer. P6.1 rebuilds the UI as a code-graph
//! explorer:
//!
//! - Left: collapsible tree (modules → files → symbols) grouped per column,
//!   driven by `parent_id` chains in the JSON view model.
//! - Centre: deterministic SVG canvas. Visible nodes get layered on a grid
//!   keyed by column; edges are lifted to the nearest visible ancestor so a
//!   collapsed module shows aggregated relationships.
//! - Right: details panel with id / kind / path:line / source / incoming +
//!   outgoing edges / nearest requirement.
//!
//! Hard constraints (carried over from P6):
//!
//! - No remote URLs (no `https://`, no `http://`, no CDN).
//! - No npm/Vite/React/dev-server.
//! - One physical file; HTML + CSS + JSON + JS embedded.
//! - Renderer enforces a hard 80-visible-node ceiling and surfaces a
//!   "collapsed N nodes" badge whenever the ceiling kicks in.

use specslice_engine::graph::GraphViewModel;

pub fn render_html(view: &GraphViewModel) -> String {
    let payload =
        serde_json::to_string(view).unwrap_or_else(|_| "{\"schema_version\":2}".to_string());
    let safe_payload = sanitize_json_for_script(&payload);

    let mut out = String::with_capacity(payload.len() + STATIC_TEMPLATE.len() + 2048);
    out.push_str(DOCTYPE);
    out.push_str(STATIC_TEMPLATE);
    out.push_str("<script id=\"specslice-data\" type=\"application/json\">");
    out.push_str(&safe_payload);
    out.push_str("</script>\n");
    out.push_str("<script>\n");
    out.push_str(RENDERER_JS);
    out.push_str("\n</script>\n</body>\n</html>\n");
    out
}

/// Escape any `</...>` sequence that could prematurely close the script tag
/// when the JSON payload contains markup-like strings.
fn sanitize_json_for_script(raw: &str) -> String {
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

const STATIC_TEMPLATE: &str = r#"<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>SpecSlice Graph</title>
  <style>
    :root {
      --bg: #0b1220;
      --panel: #11192b;
      --panel-2: #0e1626;
      --text: #e7ecf3;
      --muted: #8a99b3;
      --line: #1f2a44;
      --accent: #2faa72;
      --fact: #5c6e8a;
      --confirmed: #2faa72;
      --candidate: #d39a2a;
      --risk: #d24a4a;
      --module: #6f5cff;
      --file: #2c87c7;
      --table: #2fb6c6;
    }
    * { box-sizing: border-box; }
    html, body { margin: 0; padding: 0; background: var(--bg); color: var(--text); font: 13px/1.4 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; }
    button { font: inherit; color: inherit; }
    header.toolbar {
      display: flex; flex-wrap: wrap; gap: 8px; align-items: center;
      padding: 10px 14px; border-bottom: 1px solid var(--line);
      background: var(--panel); position: sticky; top: 0; z-index: 5;
    }
    header h1 { font-size: 13px; margin: 0 12px 0 0; letter-spacing: 0.04em; text-transform: uppercase; color: var(--muted); }
    header select, header input[type="text"] {
      background: var(--panel-2); border: 1px solid var(--line); color: var(--text);
      padding: 4px 8px; border-radius: 6px;
    }
    header input[type="text"] { min-width: 200px; }
    header label.toggle { display: inline-flex; align-items: center; gap: 4px; padding: 4px 8px; border: 1px solid var(--line); border-radius: 999px; cursor: pointer; user-select: none; }
    header label.toggle input { accent-color: var(--confirmed); }
    .stats { display: flex; gap: 12px; color: var(--muted); margin-left: auto; font-size: 11px; }
    .stats b { color: var(--text); }
    main.explorer {
      display: grid;
      grid-template-columns: minmax(200px, 260px) 1fr minmax(220px, 280px);
      min-height: calc(100vh - 50px);
    }
    @media (max-width: 740px) {
      main.explorer { grid-template-columns: 1fr; }
      section.canvas { min-height: 320px; border-top: 1px solid var(--line); border-bottom: 1px solid var(--line); }
      aside.tree, aside.detail { max-height: 40vh; }
    }
    aside.tree, aside.detail {
      background: var(--panel);
      overflow: auto;
      border-right: 1px solid var(--line);
    }
    aside.detail { border-right: 0; border-left: 1px solid var(--line); padding: 14px; }
    aside.tree { padding: 8px 0; }
    section.canvas {
      position: relative;
      background:
        radial-gradient(circle at 50% 30%, #14203a 0%, var(--bg) 60%);
      overflow: hidden;
      min-height: 480px;
    }
    svg.canvas-svg { width: 100%; height: 100%; min-height: 480px; display: block; }
    .empty-state {
      position: absolute; inset: 0;
      display: flex; align-items: center; justify-content: center;
      color: var(--muted); font-size: 13px; text-align: center; padding: 24px;
    }
    .tree-group h2 {
      font-size: 11px;
      text-transform: uppercase; letter-spacing: 0.08em;
      color: var(--muted);
      margin: 12px 14px 4px;
    }
    .tree-item {
      width: 100%;
      display: grid;
      grid-template-columns: 14px 1fr auto;
      align-items: center;
      gap: 4px;
      padding: 3px 14px;
      background: transparent;
      border: 0;
      text-align: left;
      cursor: pointer;
      border-left: 3px solid transparent;
    }
    .tree-item:hover { background: #15203a; }
    .tree-item.selected { background: #18243f; border-left-color: var(--confirmed); }
    .tree-item .twirl { color: var(--muted); font-size: 10px; width: 14px; text-align: center; }
    .tree-item .label { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .tree-item .count { color: var(--muted); font-size: 10px; }
    .tree-item.layer-confirmed .label { color: var(--confirmed); }
    .tree-item.layer-risk .label { color: var(--risk); }
    .tree-item.kind-db_table .label { color: var(--table); }
    .tree-item .badge-cols { color: var(--table); font-size: 9px; margin-left: 4px; }
    .tree-item.depth-0 { padding-left: 14px; }
    .tree-item.depth-1 { padding-left: 28px; }
    .tree-item.depth-2 { padding-left: 42px; }
    .tree-item.depth-3 { padding-left: 56px; }
    .tree-item.depth-4 { padding-left: 70px; }
    .tree-item.hidden { display: none; }
    svg .node-card {
      cursor: pointer;
    }
    svg .node-card rect.node-bg {
      stroke: var(--line);
      stroke-width: 1;
      rx: 8;
      ry: 8;
      fill: var(--panel);
    }
    svg .node-card.layer-confirmed rect.node-bg { stroke: var(--confirmed); fill: #122b22; }
    svg .node-card.layer-fact rect.node-bg { stroke: var(--fact); fill: var(--panel); }
    svg .node-card.layer-candidate rect.node-bg { stroke: var(--candidate); stroke-dasharray: 4 4; fill: #2a2415; }
    svg .node-card.layer-risk rect.node-bg { stroke: var(--risk); fill: #2a1717; }
    svg .node-card.kind-module rect.node-bg { stroke: var(--module); }
    svg .node-card.kind-file rect.node-bg { stroke: var(--file); }
    svg .node-card.kind-db_table rect.node-bg { stroke: var(--table); fill: #112730; stroke-width: 1.6; }
    svg .node-card.kind-db_table text.node-label { fill: var(--table); }
    svg .node-card.selected rect.node-bg { stroke: var(--accent); stroke-width: 2.5; filter: drop-shadow(0 0 6px rgba(47,170,114,0.65)); }
    svg text.node-label { fill: var(--text); font-size: 12px; font-weight: 600; }
    svg text.node-sub { fill: var(--muted); font-size: 10px; }
    svg .edge path { fill: none; stroke-width: 1.4; opacity: 0.78; }
    svg .edge.layer-fact path { stroke: var(--fact); }
    svg .edge.layer-confirmed path { stroke: var(--confirmed); }
    svg .edge.layer-candidate path { stroke: var(--candidate); stroke-dasharray: 4 4; }
    svg .edge.layer-risk path { stroke: var(--risk); stroke-dasharray: 1 3; }
    svg .edge.aggregated path { stroke-width: 2; opacity: 0.95; }
    svg .edge text.edge-label { fill: var(--muted); font-size: 9px; }
    aside.detail h2 { margin: 0 0 6px; font-size: 14px; }
    aside.detail h3 { margin: 12px 0 4px; font-size: 11px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.08em; }
    aside.detail .row { display: grid; grid-template-columns: 80px 1fr; gap: 8px; padding: 3px 0; font-size: 12px; border-bottom: 1px dashed var(--line); }
    aside.detail .row span:first-child { color: var(--muted); }
    aside.detail .row span:last-child { word-break: break-word; }
    aside.detail ul { list-style: none; padding: 0; margin: 0; }
    aside.detail li.edge-row { font-size: 11px; padding: 4px 0; border-bottom: 1px dashed var(--line); }
    aside.detail li.edge-row .arrow { color: var(--muted); }
    aside.detail .placeholder { color: var(--muted); font-size: 12px; }
    aside.detail .cap-warning { background: #2a1717; border: 1px solid var(--risk); padding: 8px 10px; border-radius: 6px; font-size: 11px; margin-top: 8px; }
    .collapsed-pill {
      position: absolute; right: 14px; top: 14px;
      background: var(--panel); border: 1px solid var(--candidate); color: var(--candidate);
      padding: 4px 8px; border-radius: 999px; font-size: 11px;
    }
  </style>
</head>
<body>
  <header class="toolbar">
    <h1>SpecSlice Graph</h1>
    <select id="view">
      <option value="overview">View: overview</option>
      <option value="code">View: code</option>
      <option value="business">View: business</option>
      <option value="focus">View: focus</option>
    </select>
    <select id="level" title="Drill-down level (GitNexus style)">
      <option value="1">Level: L1 架构概览</option>
      <option value="2">Level: L2 文件/聚类</option>
      <option value="3">Level: L3 符号 + 表</option>
    </select>
    <input type="text" id="search" placeholder="Search label / path / id">
    <input type="text" id="focus" placeholder="Focus id (REQ-…)">
    <label class="toggle"><input type="checkbox" data-layer="fact" checked> Facts</label>
    <label class="toggle"><input type="checkbox" data-layer="confirmed" checked> Confirmed</label>
    <label class="toggle"><input type="checkbox" data-layer="candidate" checked> Candidates</label>
    <label class="toggle"><input type="checkbox" data-layer="risk" checked> Risks</label>
    <div class="stats" id="stats"></div>
  </header>
  <main class="explorer">
    <aside class="tree" id="tree"></aside>
    <section class="canvas" id="canvas">
      <svg class="canvas-svg" id="svg"></svg>
      <div class="empty-state" id="empty" style="display:none"></div>
      <div class="collapsed-pill" id="collapsed-pill" style="display:none"></div>
    </section>
    <aside class="detail" id="detail">
      <h2>Details</h2>
      <p class="placeholder">Click any node to inspect it.</p>
    </aside>
  </main>
"#;

const RENDERER_JS: &str = r#"(function () {
  const MAX_VISIBLE = 80;
  const COLUMN_TITLES = {
    documents: 'Documents',
    business: 'Business',
    code: 'Code',
    tests: 'Tests',
    risks: 'Risks',
  };
  const COLUMN_ORDER = ['documents', 'business', 'code', 'tests', 'risks'];

  const dataEl = document.getElementById('specslice-data');
  let view;
  try { view = JSON.parse(dataEl.textContent || '{}'); }
  catch (e) { view = { nodes: [], edges: [], findings: [], stats: {} }; }

  const nodes = (view.nodes || []).slice();
  const edges = (view.edges || []).slice();
  const findings = (view.findings || []).slice();
  const nodesById = new Map(nodes.map(n => [n.id, n]));
  const childrenOf = new Map();
  for (const n of nodes) {
    if (!n.parent_id) continue;
    if (!childrenOf.has(n.parent_id)) childrenOf.set(n.parent_id, []);
    childrenOf.get(n.parent_id).push(n);
  }

  // State.
  const layerToggle = new Set(['fact', 'confirmed', 'candidate', 'risk']);
  // `userExpanded` is the only switch that reveals descendants beyond the
  // engine's `default_visible` seed. `userCollapsed` lets the user hide a
  // default_visible node.
  const userExpanded = new Set();
  const userCollapsed = new Set();
  let searchText = '';
  let selected = null;
  const currentView = view.view || 'overview';

  function walkAncestors(id, fn) {
    let cur = nodesById.get(id);
    while (cur && cur.parent_id) {
      fn(cur.parent_id);
      cur = nodesById.get(cur.parent_id);
    }
  }

  function matchesSearch(node) {
    if (!searchText) return true;
    const blob = [node.label, node.path || '', node.id, ...(node.badges || [])]
      .join(' ').toLowerCase();
    return blob.includes(searchText);
  }

  function isVisible(node) {
    if (!layerToggle.has(node.layer)) return false;
    if (userCollapsed.has(node.id)) return false;
    // Search relaxes the hierarchy: any matching node + its ancestors render.
    if (searchText) {
      if (matchesSearch(node)) return true;
      // Show ancestors of matches so they can be navigated to.
      if (descendantMatches(node)) return true;
      return false;
    }
    if (node.default_visible) return true;
    if (!node.parent_id) return false;
    const parent = nodesById.get(node.parent_id);
    if (!parent) return false;
    if (!userExpanded.has(parent.id)) return false;
    return isVisible(parent);
  }

  const _descendantCache = new Map();
  function descendantMatches(node) {
    if (!searchText) return false;
    if (_descendantCache.has(node.id)) return _descendantCache.get(node.id);
    const kids = childrenOf.get(node.id) || [];
    let hit = false;
    for (const k of kids) {
      if (matchesSearch(k) || descendantMatches(k)) { hit = true; break; }
    }
    _descendantCache.set(node.id, hit);
    return hit;
  }
  function resetSearchCache() { _descendantCache.clear(); }

  function nearestVisibleAncestor(id) {
    let cur = nodesById.get(id);
    while (cur) {
      if (isVisible(cur)) return cur;
      if (!cur.parent_id) return null;
      cur = nodesById.get(cur.parent_id);
    }
    return null;
  }

  // ----- Tree rendering -----
  function renderTree() {
    const tree = document.getElementById('tree');
    tree.innerHTML = '';
    const byColumn = new Map();
    for (const n of nodes) {
      if (!n.parent_id) {
        if (!byColumn.has(n.column)) byColumn.set(n.column, []);
        byColumn.get(n.column).push(n);
      }
    }
    for (const col of COLUMN_ORDER) {
      const roots = byColumn.get(col);
      if (!roots || !roots.length) continue;
      const group = document.createElement('div');
      group.className = 'tree-group';
      const title = document.createElement('h2');
      title.textContent = COLUMN_TITLES[col] || col;
      group.appendChild(title);
      const sorted = roots.slice().sort(treeSort);
      for (const root of sorted) renderTreeNode(root, group, 0);
      tree.appendChild(group);
    }
  }

  function treeSort(a, b) {
    const ak = a.kind === 'module' ? 0 : 1;
    const bk = b.kind === 'module' ? 0 : 1;
    if (ak !== bk) return ak - bk;
    return (a.label || a.id).localeCompare(b.label || b.id);
  }

  function renderTreeNode(node, parent, depth) {
    if (!isVisible(node)) return;
    const kids = (childrenOf.get(node.id) || []).slice();
    const labelText = node.label || node.id;
    const item = document.createElement('button');
    item.className = 'tree-item depth-' + Math.min(depth, 4) + ' layer-' + node.layer + ' kind-' + node.kind;
    if (selected && selected.id === node.id) item.classList.add('selected');
    const colsBadge = (node.badges || []).find(b => /cols$/.test(b));
    const twirl = document.createElement('span');
    twirl.className = 'twirl';
    twirl.textContent = kids.length ? (userExpanded.has(node.id) ? '▾' : '▸') : '·';
    const label = document.createElement('span');
    label.className = 'label';
    label.textContent = labelText;
    const count = document.createElement('span');
    count.className = 'count';
    if (kids.length) count.textContent = node.child_count + '';
    else if (colsBadge) { count.className = 'badge-cols'; count.textContent = colsBadge; }
    item.appendChild(twirl);
    item.appendChild(label);
    item.appendChild(count);
    item.addEventListener('click', (ev) => {
      ev.stopPropagation();
      selectNode(node);
      if (kids.length) toggleExpand(node);
    });
    parent.appendChild(item);
    if (kids.length && (userExpanded.has(node.id) || searchText)) {
      kids.sort(treeSort);
      for (const k of kids) renderTreeNode(k, parent, depth + 1);
    }
  }

  function toggleExpand(node) {
    if (userExpanded.has(node.id)) {
      userExpanded.delete(node.id);
      // Also fold any expanded descendants so a re-open is clean.
      collapseSubtree(node);
    } else {
      userExpanded.add(node.id);
      // Make sure ancestors are open so this node remains addressable.
      walkAncestors(node.id, a => userExpanded.add(a));
    }
    resetSearchCache();
    render();
  }

  function collapseSubtree(node) {
    const kids = childrenOf.get(node.id) || [];
    for (const k of kids) {
      userExpanded.delete(k.id);
      collapseSubtree(k);
    }
  }

  // ----- Level drill-down (GitNexus style) -----
  // L1 架构概览: only the engine's default_visible seed (modules).
  // L2 文件/聚类: expand every module so its files surface.
  // L3 符号 + 表: expand modules + files so symbols and DB tables surface.
  function applyLevel(level) {
    userExpanded.clear();
    userCollapsed.clear();
    for (const n of nodes) {
      if (level >= 2 && n.kind === 'module') userExpanded.add(n.id);
      if (level >= 3 && (n.kind === 'module' || n.kind === 'file')) userExpanded.add(n.id);
    }
    resetSearchCache();
    render();
  }

  // ----- Canvas rendering -----
  function renderCanvas() {
    const svg = document.getElementById('svg');
    const canvas = document.getElementById('canvas');
    const emptyEl = document.getElementById('empty');
    const pill = document.getElementById('collapsed-pill');
    svg.innerHTML = '';
    let visible = nodes.filter(isVisible);
    let overflow = 0;
    if (visible.length > MAX_VISIBLE) {
      overflow = visible.length - MAX_VISIBLE;
      visible = visible.slice(0, MAX_VISIBLE);
    }
    if (!visible.length) {
      emptyEl.style.display = 'flex';
      emptyEl.textContent = computeEmptyMessage();
      pill.style.display = 'none';
      return;
    }
    emptyEl.style.display = 'none';
    if (overflow > 0) {
      pill.style.display = 'block';
      pill.textContent = 'collapsed ' + overflow + ' more nodes — expand or refine search';
    } else {
      pill.style.display = 'none';
    }

    // Lay out per column.
    const rect = canvas.getBoundingClientRect();
    const width = Math.max(rect.width || 600, 600);
    const height = Math.max(rect.height || 480, 480);
    const columns = {};
    for (const col of COLUMN_ORDER) columns[col] = [];
    for (const n of visible) {
      const col = COLUMN_TITLES[n.column] ? n.column : 'code';
      columns[col].push(n);
    }
    const colsWithData = COLUMN_ORDER.filter(c => columns[c].length);
    const colCount = Math.max(colsWithData.length, 1);
    const padX = 40;
    const padY = 32;
    const colWidth = (width - padX * 2) / colCount;
    const positions = new Map();
    colsWithData.forEach((col, ci) => {
      const list = columns[col].slice().sort((a, b) => {
        if (a.kind === 'module' && b.kind !== 'module') return -1;
        if (b.kind === 'module' && a.kind !== 'module') return 1;
        return (a.label || a.id).localeCompare(b.label || b.id);
      });
      const slotHeight = Math.min(58, (height - padY * 2) / Math.max(list.length, 1));
      list.forEach((n, i) => {
        const x = padX + ci * colWidth + 14;
        const y = padY + i * (slotHeight + 6);
        positions.set(n.id, { x, y, w: colWidth - 28, h: slotHeight });
      });
    });

    // Edges first (so nodes overlay).
    const drawnEdges = new Set();
    for (const e of edges) {
      if (!layerToggle.has(e.layer)) continue;
      const from = nearestVisibleAncestor(e.from);
      const to = nearestVisibleAncestor(e.to);
      if (!from || !to || from.id === to.id) continue;
      const key = from.id + '||' + to.id + '||' + e.kind;
      if (drawnEdges.has(key)) continue;
      drawnEdges.add(key);
      const a = positions.get(from.id);
      const b = positions.get(to.id);
      if (!a || !b) continue;
      const x1 = a.x + a.w;
      const y1 = a.y + a.h / 2;
      const x2 = b.x;
      const y2 = b.y + b.h / 2;
      const aggregated = (from.id !== e.from) || (to.id !== e.to);
      const dx = (x2 - x1) * 0.5;
      const d = 'M' + x1 + ',' + y1 +
        ' C' + (x1 + dx) + ',' + y1 +
        ' ' + (x2 - dx) + ',' + y2 +
        ' ' + x2 + ',' + y2;
      // Build the SVG fragment via innerHTML to keep the SVG namespace
      // without ever spelling out an XML namespace URI (the offline policy
      // forbids any URL literal inside the generated HTML).
      const tmp = document.createElement('div');
      tmp.innerHTML = '<svg><g class="edge layer-' + e.layer + (aggregated ? ' aggregated' : '') + '">' +
        '<path d="' + d + '" data-from="' + escapeAttr(from.id) + '" data-to="' + escapeAttr(to.id) + '"></path>' +
        '<title>' + escapeAttr(e.kind) + '</title>' +
        '</g></svg>';
      const wrapper = tmp.firstChild;
      while (wrapper && wrapper.firstChild) svg.appendChild(wrapper.firstChild);
    }

    // Nodes.
    for (const n of visible) {
      const p = positions.get(n.id);
      if (!p) continue;
      const tmp = document.createElement('div');
      const colsBadge = (n.badges || []).find(b => /cols$/.test(b));
      const subText = n.kind === 'module'
        ? (n.path || '') + ' • ' + (n.child_count || 0) + ' child' + ((n.child_count || 0) === 1 ? '' : 'ren')
        : n.kind === 'db_table'
          ? '🗄 ' + (colsBadge || 'table') + (n.path ? ' • ' + n.path : '')
          : (n.path
            ? (n.line_range
              ? n.path + ':' + n.line_range[0] + '-' + n.line_range[1]
              : n.path)
            : n.kind);
      const classes = ['node-card', 'kind-' + n.kind, 'layer-' + n.layer];
      if (selected && selected.id === n.id) classes.push('selected');
      tmp.innerHTML = '<svg><g class="' + classes.join(' ') + '" data-id="' + escapeAttr(n.id) + '">' +
        '<rect class="node-bg" x="' + p.x + '" y="' + p.y + '" width="' + p.w + '" height="' + p.h + '"></rect>' +
        '<text class="node-label" x="' + (p.x + 10) + '" y="' + (p.y + 18) + '">' + escapeText(n.label || n.id) + '</text>' +
        '<text class="node-sub" x="' + (p.x + 10) + '" y="' + (p.y + 34) + '">' + escapeText(subText) + '</text>' +
        '</g></svg>';
      const wrapper = tmp.firstChild;
      while (wrapper && wrapper.firstChild) svg.appendChild(wrapper.firstChild);
    }

    // Hook click handlers.
    svg.querySelectorAll('g.node-card').forEach(group => {
      group.addEventListener('click', () => {
        const id = group.getAttribute('data-id');
        const node = nodesById.get(id);
        if (!node) return;
        selectNode(node);
        if ((childrenOf.get(node.id) || []).length) toggleExpand(node);
        else render();
      });
    });
  }

  function computeEmptyMessage() {
    if (currentView === 'business') {
      const f = findings.find(x => x.code === 'no_business_logic');
      if (f) return f.message;
      return 'No confirmed business logic yet. Run `specslice connect propose`.';
    }
    if (findings.some(f => f.code === 'focus_not_found')) {
      return 'Focus id not found in the current graph.';
    }
    return 'No nodes match the current filters. Try expanding a module or clearing the search.';
  }

  // ----- Details panel -----
  function selectNode(node) {
    selected = node;
    renderDetail();
    renderTree();
    // Rerender canvas only to update highlight; cheap enough.
    renderCanvas();
  }

  function renderDetail() {
    const detail = document.getElementById('detail');
    detail.innerHTML = '';
    if (!selected) {
      detail.innerHTML = '<h2>Details</h2><p class="placeholder">Click any node to inspect it.</p>';
      return;
    }
    const n = selected;
    const h = document.createElement('h2');
    h.textContent = n.label || n.id;
    detail.appendChild(h);
    addRow(detail, 'id', n.id);
    addRow(detail, 'kind', n.kind);
    addRow(detail, 'layer', n.layer);
    addRow(detail, 'column', n.column);
    if (n.path) addRow(detail, 'path', n.line_range
      ? n.path + ':' + n.line_range[0] + '-' + n.line_range[1]
      : n.path);
    if (n.source) addRow(detail, 'source', n.source);
    if (n.child_count) addRow(detail, 'children', n.child_count);
    if (n.parent_id) addRow(detail, 'parent', n.parent_id);

    const incoming = edges.filter(e => e.to === n.id);
    const outgoing = edges.filter(e => e.from === n.id);
    if (incoming.length) {
      detail.appendChild(headerRow('Incoming'));
      const ul = document.createElement('ul');
      for (const e of incoming) ul.appendChild(edgeRow(e, 'in'));
      detail.appendChild(ul);
    }
    if (outgoing.length) {
      detail.appendChild(headerRow('Outgoing'));
      const ul = document.createElement('ul');
      for (const e of outgoing) ul.appendChild(edgeRow(e, 'out'));
      detail.appendChild(ul);
    }
    const relatedReqs = collectRelatedRequirements(n);
    if (relatedReqs.length) {
      detail.appendChild(headerRow('Related requirements'));
      const ul = document.createElement('ul');
      for (const r of relatedReqs) {
        const li = document.createElement('li');
        li.className = 'edge-row';
        li.textContent = r.label || r.id;
        li.addEventListener('click', () => selectNode(r));
        li.style.cursor = 'pointer';
        ul.appendChild(li);
      }
      detail.appendChild(ul);
    }
    if (n.kind === 'requirement' && !findings.length) {
      // nothing extra
    }
  }

  function collectRelatedRequirements(n) {
    const seen = new Set();
    const out = [];
    function pushReq(id) {
      if (seen.has(id)) return;
      seen.add(id);
      const node = nodesById.get(id);
      if (node && node.kind === 'requirement') out.push(node);
    }
    if (n.kind === 'requirement') return out;
    // Walk outgoing edges that point at requirements.
    for (const e of edges) {
      if (e.from === n.id && nodesById.get(e.to)?.kind === 'requirement') pushReq(e.to);
      if (e.to === n.id && nodesById.get(e.from)?.kind === 'requirement') pushReq(e.from);
    }
    // Walk up via parent and check for shared requirements (recursion).
    if (!out.length && n.parent_id) {
      const parent = nodesById.get(n.parent_id);
      if (parent) return collectRelatedRequirements(parent);
    }
    return out;
  }

  function headerRow(text) {
    const el = document.createElement('h3');
    el.textContent = text;
    return el;
  }

  function edgeRow(e, dir) {
    const li = document.createElement('li');
    li.className = 'edge-row';
    const other = dir === 'in' ? e.from : e.to;
    const otherNode = nodesById.get(other);
    const otherLabel = otherNode ? (otherNode.label || otherNode.id) : other;
    li.innerHTML = '<span class="arrow">' + (dir === 'in' ? '◀' : '▶') + '</span> ' +
      escapeText(e.kind) + ' — ' + escapeText(otherLabel);
    li.style.cursor = otherNode ? 'pointer' : 'default';
    if (otherNode) li.addEventListener('click', () => selectNode(otherNode));
    return li;
  }

  function addRow(parent, key, value) {
    const row = document.createElement('div');
    row.className = 'row';
    const a = document.createElement('span'); a.textContent = key;
    const b = document.createElement('span'); b.textContent = value == null ? '—' : String(value);
    row.appendChild(a); row.appendChild(b);
    parent.appendChild(row);
  }

  function escapeText(text) {
    return String(text == null ? '' : text)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;');
  }
  function escapeAttr(text) {
    return escapeText(text).replace(/"/g, '&quot;');
  }

  // ----- Stats / findings -----
  function renderStats() {
    const statsEl = document.getElementById('stats');
    statsEl.innerHTML = '';
    const s = view.stats || {};
    const pairs = [
      ['modules', s.modules],
      ['docs', s.documents],
      ['biz', s.business_logic],
      ['code', s.code_symbols],
      ['tests', s.tests],
      ['confirmed', s.confirmed_edges],
      ['risks', s.risks],
    ];
    for (const [label, value] of pairs) {
      const span = document.createElement('span');
      span.innerHTML = label + ' <b>' + (value || 0) + '</b>';
      statsEl.appendChild(span);
    }
  }

  function render() {
    renderTree();
    renderCanvas();
    renderDetail();
    renderStats();
  }

  // ----- Controls -----
  document.getElementById('search').addEventListener('input', (ev) => {
    searchText = (ev.target.value || '').toLowerCase().trim();
    resetSearchCache();
    render();
  });
  document.getElementById('focus').addEventListener('change', (ev) => {
    const raw = (ev.target.value || '').trim();
    if (!raw) return;
    const candidates = [raw, 'req::' + raw, 'module::' + raw];
    let found = null;
    for (const cand of candidates) {
      if (nodesById.has(cand)) { found = nodesById.get(cand); break; }
    }
    if (!found) return;
    walkAncestors(found.id, a => userExpanded.add(a));
    userExpanded.add(found.id);
    selectNode(found);
  });
  document.querySelectorAll('input[data-layer]').forEach(box => {
    box.addEventListener('change', () => {
      const layer = box.dataset.layer;
      if (box.checked) layerToggle.add(layer);
      else layerToggle.delete(layer);
      render();
    });
  });
  const levelSelect = document.getElementById('level');
  if (levelSelect) {
    levelSelect.addEventListener('change', (ev) => {
      const lvl = parseInt(ev.target.value, 10) || 1;
      applyLevel(lvl);
    });
  }
  document.getElementById('view').value = currentView;
  // The view-select acts as informational; switching actively requires
  // regenerating the HTML (CLI flag). Hint the user.
  document.getElementById('view').addEventListener('change', (ev) => {
    ev.target.value = currentView;
    const detail = document.getElementById('detail');
    detail.innerHTML = '<h2>View</h2><p class="placeholder">Rerun <code>specslice graph --view ' +
      escapeText(ev.target.value) + '</code> to switch the engine view.</p>';
  });

  window.addEventListener('resize', () => requestAnimationFrame(renderCanvas));

  render();
})();"#;

#[cfg(test)]
mod tests {
    use super::*;
    use specslice_engine::graph::{
        GraphColumn, GraphEdge, GraphLayer, GraphNode, GraphStatus, GraphViewModel,
    };

    fn empty_view() -> GraphViewModel {
        GraphViewModel {
            schema_version: 2,
            view: "overview".into(),
            repo_root: "/tmp".into(),
            generated_at: "now".into(),
            focus: None,
            stats: Default::default(),
            nodes: vec![],
            edges: vec![],
            findings: vec![],
        }
    }

    #[test]
    fn sanitize_breaks_script_close_tags_inside_payload() {
        let raw = r#"{"label":"bad </script>"}"#;
        let out = sanitize_json_for_script(raw);
        assert!(!out.contains("</script>"));
        assert!(out.contains("<\\/script>"));
    }

    #[test]
    fn sanitize_leaves_normal_text_untouched() {
        let raw = r#"{"a":"hello","b":42}"#;
        assert_eq!(sanitize_json_for_script(raw), raw);
    }

    #[test]
    fn render_html_embeds_explorer_layout() {
        let html = render_html(&empty_view());
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<script id=\"specslice-data\""));
        assert!(html.contains("class=\"tree\""));
        assert!(html.contains("class=\"canvas\""));
        assert!(html.contains("class=\"detail\""));
        assert!(html.contains("layer-confirmed"));
        assert!(html.contains("layer-fact"));
        // P25: GitNexus-style level drill-down control + DB-table evidence styling.
        assert!(html.contains("id=\"level\""));
        assert!(html.contains("applyLevel"));
        assert!(html.contains("kind-db_table"));
        assert!(!html.contains("https://"));
        assert!(!html.contains("http://"));
    }

    #[test]
    fn render_html_embeds_node_data_for_modules() {
        let mut view = empty_view();
        view.nodes.push(GraphNode {
            id: "module::lib".into(),
            kind: "module".into(),
            column: GraphColumn::Code,
            layer: GraphLayer::Fact,
            label: "lib".into(),
            path: Some("lib".into()),
            line_range: None,
            status: GraphStatus::Confirmed,
            parent_id: None,
            child_count: 3,
            default_visible: true,
            confidence: None,
            source: Some("module_aggregator".into()),
            badges: vec![],
        });
        view.nodes.push(GraphNode {
            id: "file::lib/main.dart".into(),
            kind: "file".into(),
            column: GraphColumn::Code,
            layer: GraphLayer::Fact,
            label: "main.dart".into(),
            path: Some("lib/main.dart".into()),
            line_range: None,
            status: GraphStatus::Confirmed,
            parent_id: Some("module::lib".into()),
            child_count: 0,
            default_visible: false,
            confidence: None,
            source: Some("dart_indexer".into()),
            badges: vec![],
        });
        view.edges.push(GraphEdge {
            id: "edge::1".into(),
            from: "file::lib/main.dart".into(),
            to: "module::lib".into(),
            kind: "contains".into(),
            layer: GraphLayer::Fact,
            status: GraphStatus::Confirmed,
            confidence: None,
            source: None,
            rationale: None,
            source_file: None,
            line_range: None,
            snippet: None,
            resolver: None,
            evidence_quality: None,
        });
        let html = render_html(&view);
        assert!(html.contains("module::lib"));
        assert!(html.contains("file::lib/main.dart"));
        assert!(html.contains("\"default_visible\":true"));
        assert!(html.contains("\"child_count\":3"));
    }
}
