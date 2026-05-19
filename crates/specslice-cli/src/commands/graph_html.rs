//! Self-contained HTML renderer for `specslice graph --format html`.
//!
//! Hard constraints:
//!
//! - No remote URLs (no `https://`, no `http://`, no CDN).
//! - No `npm`, no React, no Vite, no dev server.
//! - One physical file: HTML + CSS + JSON + JS embedded.
//! - The rendered page must open cleanly with `open graph.html` and degrade
//!   gracefully when the view is empty.
//!
//! Visual encoding follows `docs/visualization-design.md`:
//!
//! - 5 lanes: Documents / Business / Code / Tests / Risks.
//! - Nodes are buttons (keyboard accessible).
//! - Layers (`fact`, `confirmed`, `candidate`, `risk`) drive CSS classes
//!   `layer-fact`, `layer-confirmed`, `layer-candidate`, `layer-risk`.
//! - SVG cubic bezier edges, recalculated on resize and filter changes by
//!   the embedded JS.

use specslice_engine::graph::GraphViewModel;

pub fn render_html(view: &GraphViewModel) -> String {
    let payload =
        serde_json::to_string(view).unwrap_or_else(|_| "{\"schema_version\":1}".to_string());
    let safe_payload = sanitize_json_for_script(&payload);

    let mut out = String::with_capacity(payload.len() + STATIC_TEMPLATE.len() + 1024);
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
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '<' && chars.peek() == Some(&'/') {
            out.push_str("<\\/");
            let _ = chars.next();
            continue;
        }
        out.push(ch);
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
      --text: #e7ecf3;
      --muted: #8a99b3;
      --line: #1f2a44;
      --fact: #5c6e8a;
      --confirmed: #2faa72;
      --candidate: #d39a2a;
      --risk: #d24a4a;
    }
    * { box-sizing: border-box; }
    html, body { margin: 0; padding: 0; background: var(--bg); color: var(--text); font: 13px/1.4 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; }
    header.toolbar {
      display: flex; flex-wrap: wrap; gap: 8px; align-items: center;
      padding: 10px 14px; border-bottom: 1px solid var(--line);
      background: var(--panel); position: sticky; top: 0; z-index: 5;
    }
    header h1 { font-size: 13px; margin: 0 12px 0 0; letter-spacing: 0.04em; text-transform: uppercase; color: var(--muted); }
    header input[type="text"] {
      background: #0e1626; border: 1px solid var(--line); color: var(--text);
      padding: 4px 8px; border-radius: 6px; min-width: 180px;
    }
    header label.toggle { display: inline-flex; align-items: center; gap: 4px; padding: 4px 8px; border: 1px solid var(--line); border-radius: 999px; cursor: pointer; user-select: none; }
    header label.toggle input { accent-color: var(--confirmed); }
    main {
      display: grid;
      grid-template-columns: 1fr 320px;
      min-height: calc(100vh - 50px);
    }
    section.board {
      position: relative;
      padding: 16px;
      overflow: auto;
    }
    .lanes {
      display: grid;
      grid-template-columns: repeat(5, minmax(180px, 1fr));
      gap: 16px;
      position: relative;
      z-index: 1;
    }
    .lane h2 {
      font-size: 11px;
      text-transform: uppercase;
      letter-spacing: 0.08em;
      color: var(--muted);
      margin: 0 0 8px;
      border-bottom: 1px solid var(--line);
      padding-bottom: 4px;
    }
    .nodes { display: flex; flex-direction: column; gap: 8px; }
    .node-card {
      text-align: left;
      background: var(--panel);
      border: 1px solid var(--line);
      border-left: 4px solid var(--fact);
      color: var(--text);
      padding: 8px 10px;
      border-radius: 8px;
      cursor: pointer;
      font: inherit;
    }
    .node-card:hover { border-color: var(--confirmed); }
    .node-card.active { outline: 2px solid var(--confirmed); }
    .node-card .label { display: block; font-weight: 600; }
    .node-card .path { display: block; color: var(--muted); font-size: 11px; word-break: break-all; }
    .node-card .badges { display: flex; gap: 4px; margin-top: 4px; flex-wrap: wrap; }
    .badge { background: #1c2742; color: var(--muted); border-radius: 4px; padding: 1px 6px; font-size: 10px; }
    .node-card.layer-fact { border-left-color: var(--fact); }
    .node-card.layer-confirmed { border-left-color: var(--confirmed); }
    .node-card.layer-candidate { border-left-color: var(--candidate); border-style: dashed; }
    .node-card.layer-risk { border-left-color: var(--risk); }
    .node-card.hidden { display: none; }
    .empty { color: var(--muted); font-size: 11px; padding: 8px 0; }
    svg.edges {
      position: absolute;
      inset: 16px;
      pointer-events: none;
      z-index: 0;
    }
    svg.edges path { fill: none; stroke-width: 1.4; opacity: 0.85; }
    svg.edges path.layer-fact { stroke: var(--fact); }
    svg.edges path.layer-confirmed { stroke: var(--confirmed); }
    svg.edges path.layer-candidate { stroke: var(--candidate); stroke-dasharray: 4 4; }
    svg.edges path.layer-risk { stroke: var(--risk); stroke-dasharray: 1 3; }
    aside.detail {
      border-left: 1px solid var(--line);
      padding: 16px;
      background: var(--panel);
      overflow: auto;
    }
    aside.detail h2 { margin: 0 0 6px; font-size: 14px; }
    aside.detail .row { display: flex; justify-content: space-between; gap: 12px; padding: 4px 0; border-bottom: 1px dashed var(--line); font-size: 12px; }
    aside.detail .row span:first-child { color: var(--muted); }
    aside.detail pre { background: #0e1626; padding: 8px; border-radius: 6px; overflow: auto; font-size: 11px; }
    .stats { display: flex; gap: 12px; color: var(--muted); margin-left: auto; font-size: 11px; }
    .stats span b { color: var(--text); }
    .empty-state {
      padding: 24px;
      color: var(--muted);
      text-align: center;
      border: 1px dashed var(--line);
      border-radius: 12px;
      margin: 24px auto;
      max-width: 480px;
    }
  </style>
</head>
<body>
  <header class="toolbar">
    <h1>SpecSlice Graph</h1>
    <input type="text" id="search" placeholder="Search label / path / id">
    <input type="text" id="focus" placeholder="Focus id (REQ-…)">
    <label class="toggle"><input type="checkbox" data-layer="fact" checked> Facts</label>
    <label class="toggle"><input type="checkbox" data-layer="confirmed" checked> Confirmed</label>
    <label class="toggle"><input type="checkbox" data-layer="candidate" checked> Candidates</label>
    <label class="toggle"><input type="checkbox" data-layer="risk" checked> Risks</label>
    <div class="stats" id="stats"></div>
  </header>
  <main>
    <section class="board">
      <svg class="edges" id="edges"></svg>
      <div class="lanes" id="lanes"></div>
      <div id="empty" class="empty-state" style="display:none"></div>
    </section>
    <aside class="detail" id="detail">
      <h2>Details</h2>
      <p class="empty">Click a node or edge to inspect.</p>
    </aside>
  </main>
"#;

const RENDERER_JS: &str = r#"(function () {
  const dataEl = document.getElementById('specslice-data');
  let view;
  try { view = JSON.parse(dataEl.textContent || '{}'); }
  catch (e) { view = { nodes: [], edges: [], findings: [], stats: {} }; }

  const COLUMNS = [
    { id: 'documents', title: 'Documents' },
    { id: 'business',  title: 'Business' },
    { id: 'code',      title: 'Code' },
    { id: 'tests',     title: 'Tests' },
    { id: 'risks',     title: 'Risks' },
  ];

  const LAYERS = ['fact', 'confirmed', 'candidate', 'risk'];

  const state = {
    search: '',
    focus: view.focus || '',
    layers: new Set(LAYERS),
  };

  const lanesEl = document.getElementById('lanes');
  const edgesEl = document.getElementById('edges');
  const detailEl = document.getElementById('detail');
  const statsEl = document.getElementById('stats');
  const emptyEl = document.getElementById('empty');

  function nodesByColumn() {
    const buckets = Object.fromEntries(COLUMNS.map(c => [c.id, []]));
    for (const n of view.nodes) {
      const col = n.column || 'code';
      if (!buckets[col]) buckets[col] = [];
      buckets[col].push(n);
    }
    // Findings show up as risk pseudo-nodes only inside the Risks lane.
    for (const f of (view.findings || [])) {
      if (!buckets.risks) buckets.risks = [];
      buckets.risks.push({
        id: 'finding::' + f.code + '::' + (f.target_id || ''),
        kind: 'finding',
        column: 'risks',
        layer: 'risk',
        label: f.code,
        path: f.target_id || null,
        line_range: null,
        status: 'unknown',
        badges: [f.severity],
        _finding: f,
      });
    }
    return buckets;
  }

  function resolveFocusId(raw) {
    const q = (raw || '').trim();
    if (!q) return null;
    const exact = (view.nodes || []).find(n => n.id === q);
    if (exact) return exact.id;
    const req = (view.nodes || []).find(n => n.id === 'req::' + q);
    if (req) return req.id;
    const badge = (view.nodes || []).find(n => (n.badges || []).includes(q));
    if (badge) return badge.id;
    const label = (view.nodes || []).find(n => n.label === q);
    return label ? label.id : null;
  }

  function focusedIds() {
    const target = resolveFocusId(state.focus);
    if (!state.focus) return null;
    if (!target) return new Set();
    const ids = new Set([target]);
    for (const e of (view.edges || [])) {
      if (e.from === target) ids.add(e.to);
      if (e.to === target) ids.add(e.from);
    }
    return ids;
  }

  function shouldShow(n, focusSet) {
    if (!state.layers.has(n.layer)) return false;
    if (focusSet) {
      const target = n._finding ? n._finding.target_id : n.id;
      if (!target || !focusSet.has(target)) return false;
    }
    if (state.search) {
      const q = state.search.toLowerCase();
      const blob = [n.label, n.path || '', n.id, ...(n.badges || [])].join(' ').toLowerCase();
      if (!blob.includes(q)) return false;
    }
    return true;
  }

  function render() {
    const buckets = nodesByColumn();
    const focusSet = focusedIds();
    const focusedId = resolveFocusId(state.focus);
    lanesEl.innerHTML = '';
    edgesEl.innerHTML = '';
    let visibleCount = 0;
    for (const col of COLUMNS) {
      const lane = document.createElement('div');
      lane.className = 'lane lane-' + col.id;
      lane.dataset.column = col.id;
      const title = document.createElement('h2');
      title.textContent = col.title;
      lane.appendChild(title);
      const list = document.createElement('div');
      list.className = 'nodes';
      const items = buckets[col.id] || [];
      let laneVisible = 0;
      for (const n of items) {
        const btn = document.createElement('button');
        btn.className = 'node-card layer-' + n.layer;
        btn.dataset.id = n.id;
        if (focusedId && n.id === focusedId) btn.classList.add('active');
        if (!shouldShow(n, focusSet)) btn.classList.add('hidden');
        else { laneVisible++; visibleCount++; }
        const label = document.createElement('span');
        label.className = 'label';
        label.textContent = n.label || n.id;
        btn.appendChild(label);
        if (n.path) {
          const p = document.createElement('span');
          p.className = 'path';
          p.textContent = n.line_range
            ? n.path + ':' + n.line_range[0] + '-' + n.line_range[1]
            : n.path;
          btn.appendChild(p);
        }
        if ((n.badges || []).length) {
          const bg = document.createElement('span');
          bg.className = 'badges';
          for (const b of n.badges) {
            const s = document.createElement('span');
            s.className = 'badge';
            s.textContent = b;
            bg.appendChild(s);
          }
          btn.appendChild(bg);
        }
        btn.addEventListener('click', () => showNode(n));
        list.appendChild(btn);
      }
      if (!laneVisible) {
        const e = document.createElement('p');
        e.className = 'empty';
        e.textContent = col.id === 'business'
          ? 'No confirmed business logic yet.'
          : 'No items.';
        list.appendChild(e);
      }
      lane.appendChild(list);
      lanesEl.appendChild(lane);
    }
    statsEl.innerHTML = '';
    const s = view.stats || {};
    for (const [label, value] of [
      ['docs', s.documents],
      ['biz',  s.business_logic],
      ['code', s.code_symbols],
      ['tests', s.tests],
      ['confirmed', s.confirmed_edges],
      ['risks', s.risks],
    ]) {
      const span = document.createElement('span');
      span.innerHTML = label + ' <b>' + (value || 0) + '</b>';
      statsEl.appendChild(span);
    }
    emptyEl.style.display = visibleCount === 0 ? 'block' : 'none';
    if (visibleCount === 0) {
      emptyEl.textContent = state.focus && !focusedId
        ? 'Focus id not found in current graph.'
        : (view.findings || []).some(f => f.code === 'focus_not_found')
        ? 'Focus id not found in current graph.'
        : 'No nodes match the current filters.';
    }
    requestAnimationFrame(drawEdges);
  }

  function drawEdges() {
    const board = edgesEl.parentElement.getBoundingClientRect();
    edgesEl.setAttribute('width', board.width);
    edgesEl.setAttribute('height', board.height);
    edgesEl.setAttribute('viewBox', '0 0 ' + board.width + ' ' + board.height);
    const positions = new Map();
    edgesEl.parentElement.querySelectorAll('.node-card').forEach(btn => {
      if (btn.classList.contains('hidden')) return;
      const r = btn.getBoundingClientRect();
      positions.set(btn.dataset.id, {
        x: r.left + r.width - board.left - 16,
        y: r.top + r.height / 2 - board.top - 16,
        xLeft: r.left - board.left - 16,
      });
    });
    let svgInner = '';
    const visibleEdges = [];
    for (const e of (view.edges || [])) {
      if (!state.layers.has(e.layer)) continue;
      const a = positions.get(e.from);
      const b = positions.get(e.to);
      if (!a || !b) continue;
      const dx = (b.xLeft - a.x) * 0.5;
      const d =
        'M' + a.x + ',' + a.y +
        ' C' + (a.x + dx) + ',' + a.y +
        ' ' + (b.xLeft - dx) + ',' + b.y +
        ' ' + b.xLeft + ',' + b.y;
      const idx = visibleEdges.push(e) - 1;
      svgInner += '<path class="layer-' + e.layer + '" d="' + d + '" data-idx="' + idx + '"></path>';
    }
    // Inline replace keeps elements in SVG namespace and avoids exposing the
    // SVG namespace URI literal that some test fixtures forbid.
    edgesEl.innerHTML = svgInner;
    edgesEl.querySelectorAll('path').forEach(p => {
      const idx = Number(p.getAttribute('data-idx'));
      const edge = visibleEdges[idx];
      if (edge) p.addEventListener('click', () => showEdge(edge));
    });
  }

  function showNode(n) {
    detailEl.innerHTML = '';
    const h = document.createElement('h2');
    h.textContent = n.label || n.id;
    detailEl.appendChild(h);
    addRow('id', n.id);
    addRow('kind', n.kind);
    addRow('layer', n.layer);
    addRow('status', n.status);
    if (n.path) addRow('path', n.line_range
      ? n.path + ':' + n.line_range[0] + '-' + n.line_range[1]
      : n.path);
    if (n.source) addRow('source', n.source);
    if (n._finding) {
      const pre = document.createElement('pre');
      pre.textContent = n._finding.message;
      detailEl.appendChild(pre);
    }
  }

  function showEdge(e) {
    detailEl.innerHTML = '';
    const h = document.createElement('h2');
    h.textContent = e.kind;
    detailEl.appendChild(h);
    addRow('from', e.from);
    addRow('to', e.to);
    addRow('layer', e.layer);
    addRow('status', e.status);
    if (e.source) addRow('source', e.source);
    if (e.rationale) {
      const pre = document.createElement('pre');
      pre.textContent = e.rationale;
      detailEl.appendChild(pre);
    }
  }

  function addRow(key, value) {
    const row = document.createElement('div');
    row.className = 'row';
    const a = document.createElement('span'); a.textContent = key;
    const b = document.createElement('span'); b.textContent = value == null ? '—' : String(value);
    row.appendChild(a); row.appendChild(b);
    detailEl.appendChild(row);
  }

  document.getElementById('search').addEventListener('input', (ev) => {
    state.search = ev.target.value || '';
    render();
  });
  document.getElementById('focus').addEventListener('change', (ev) => {
    state.focus = ev.target.value || '';
    render();
  });
  document.querySelectorAll('input[data-layer]').forEach(box => {
    box.addEventListener('change', () => {
      const layer = box.dataset.layer;
      if (box.checked) state.layers.add(layer);
      else state.layers.delete(layer);
      render();
    });
  });
  window.addEventListener('resize', () => requestAnimationFrame(drawEdges));

  render();
})();"#;

#[cfg(test)]
mod tests {
    use super::*;

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
    fn sanitize_preserves_utf8_text() {
        let raw = r#"{"label":"像素画编辑器行为规范"}"#;
        assert_eq!(sanitize_json_for_script(raw), raw);
    }

    #[test]
    fn render_html_embeds_data_and_class_names() {
        let view = specslice_engine::graph::GraphViewModel {
            schema_version: 1,
            repo_root: "/tmp/repo".into(),
            generated_at: "2026-01-01T00:00:00Z".into(),
            focus: None,
            stats: Default::default(),
            nodes: vec![],
            edges: vec![],
            findings: vec![],
        };
        let html = render_html(&view);
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<script id=\"specslice-data\""));
        assert!(html.contains("\"schema_version\":1"));
        assert!(html.contains("layer-confirmed"));
        assert!(html.contains("layer-fact"));
        assert!(!html.contains("https://"));
        assert!(!html.contains("http://"));
    }

    #[test]
    fn render_html_contains_client_side_focus_filtering() {
        let view = specslice_engine::graph::GraphViewModel {
            schema_version: 1,
            repo_root: "/tmp/repo".into(),
            generated_at: "2026-01-01T00:00:00Z".into(),
            focus: None,
            stats: Default::default(),
            nodes: vec![],
            edges: vec![],
            findings: vec![],
        };
        let html = render_html(&view);
        assert!(html.contains("function resolveFocusId"));
        assert!(html.contains("function focusedIds"));
        assert!(html.contains("focusSet.has"));
    }
}
