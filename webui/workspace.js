(() => {
  "use strict";
  const M = globalThis.GroundGraphModel,
    $ = (id) => document.getElementById(id);
  const state = {
    model: null,
    selected: null,
    edge: null,
    tab: "explore",
    kinds: new Set(),
    limit: 100,
    scope: null,
    manifest: null,
    manifestText: "",
    report: null,
    binding: "unbound",
    generation: 0,
    gapLimit: 100,
    mapLimit: 100,
  };
  const names = {
    mapped: "已建立映射",
    platform: "底座承担",
    excluded: "明确不迁",
    unresolved: "待解析",
  };
  const el = (tag, text, cls) => {
    const n = document.createElement(tag);
    if (text !== undefined) n.textContent = String(text);
    if (cls) n.className = cls;
    return n;
  };
  const button = (text, action, cls) => {
    const b = el("button", text, cls);
    b.type = "button";
    b.onclick = action;
    return b;
  };
  const path = (n) =>
    n.path ? `${n.path}${n.line ? ":" + n.line : ""}` : n.id;
  const set = (id, text) => ($(id).textContent = text);
  const error = (e) => {
    $("error").replaceChildren(
      el("span", e.message ?? e),
      button("关闭", () => ($("error").hidden = true)),
    );
    $("error").hidden = false;
  };
  const action =
    (fn) =>
    (...args) =>
      Promise.resolve()
        .then(() => fn(...args))
        .catch(error);
  const known = (e) =>
    e.assertions.some(
      (a) =>
        ["fact", "declared"].includes(a.certainty) &&
        a.status === "confirmed" &&
        a.confidence === 1 &&
        a.source,
    );
  function tab(name) {
    state.tab = name;
    document.querySelectorAll("[data-tab]").forEach((b) => {
      b.classList.toggle("active", b.dataset.tab === name);
      b.setAttribute("aria-pressed", String(b.dataset.tab === name));
    });
    for (const id of ["explore", "migration", "gaps"])
      $(id).hidden = id !== name || (id === "explore" && !state.model);
    $("empty").hidden = !!state.model || name !== "explore";
  }
  function setGraph(raw, label) {
    state.model = M.normalize(raw);
    state.gapLimit = 100;
    state.selected = null;
    state.edge = null;
    state.limit = 100;
    const kinds = [...new Set(state.model.nodes.map((n) => n.kind))].sort();
    $("kind").replaceChildren(
      new Option("全部类型", ""),
      ...kinds.map((k) => new Option(k, k)),
    );
    const relations = [...new Set(state.model.edges.map((e) => e.kind))].sort();
    state.kinds = new Set(
      relations.filter((k) => !["contains", "imports", "defines"].includes(k)),
    );
    $("relation-options").replaceChildren(
      ...relations.map((k) => {
        const l = el("label"),
          i = document.createElement("input");
        i.type = "checkbox";
        i.checked = state.kinds.has(k);
        i.onchange = () => {
          i.checked ? state.kinds.add(k) : state.kinds.delete(k);
          renderScope();
        };
        l.append(i, document.createTextNode(k));
        return l;
      }),
    );
    for (const e of state.model.edges)
      if (!e.assertions.some((a) => a.source || a.indexer || a.source_file))
        state.model.diagnostics.push({
          message: `Relationship has no provenance: ${e.kind}`,
          target: e.from,
          edge: e,
        });
    set("repo", state.model.repo);
    set("snapshot", label);
    set(
      "totals",
      `${state.model.nodes.length} 个符号 · ${state.model.edges.length} 种关系`,
    );
    set("gap-count", state.model.diagnostics.length);
    $("search").value = "";
    $("export").disabled = false;
    // Prefer a business entry when available; otherwise show a callable with dependencies.
    const entry =
      state.model.nodes.find((n) => n.kind === "http_route") ??
      state.model.nodes.find((n) =>
        state.model.adjacency.get(n.id).some((e) => e.kind === "calls"),
      ) ??
      state.model.nodes[0];
    state.selected = entry?.id ?? null;
    renderList();
    renderScope();
    renderDiagnostics();
    tab("explore");
  }
  function select(id) {
    if (!state.model?.byId.has(id)) {
      error(new Error("该源符号不在当前图快照中，请载入对应的 feature-pack。"));
      return;
    }
    state.selected = id;
    state.edge = null;
    renderList();
    renderScope();
    tab("explore");
  }
  function renderList() {
    if (!state.model) return;
    const nodes = M.filteredNodes(
      state.model,
      $("search").value,
      $("kind").value,
    );
    set("list-total", nodes.length);
    $("node-list").replaceChildren(
      ...nodes.slice(0, state.limit).map((n) => {
        const b = button("", () => select(n.id), "tree-item");
        b.append(el("strong", n.name), el("small", `${n.kind} · ${path(n)}`));
        b.title = n.id;
        b.setAttribute("aria-current", String(n.id === state.selected));
        return b;
      }),
    );
    if (!nodes.length)
      $("node-list").append(
        el("p", "没有匹配项。可按路径或完整 ID 搜索。", "muted"),
      );
    $("more").hidden = nodes.length <= state.limit;
    set(
      "more",
      `再显示 100 项（${Math.min(nodes.length, state.limit)} / ${nodes.length}）`,
    );
  }
  function renderScope() {
    if (!state.model) return;
    const n = state.model.byId.get(state.selected);
    set("selected-name", n?.name ?? "快照中没有符号");
    set("selected-kind", n?.kind ?? "EMPTY SNAPSHOT");
    set("selected-path", n ? path(n) : "");
    $("copy-location").disabled = !n;
    set(
      "relation-count",
      `${state.kinds.size}/${$("relation-options").children.length}`,
    );
    state.scope = M.neighborhood(
      state.model,
      state.selected,
      $("direction").value,
      Number($("depth").value),
      state.kinds,
      25,
    );
    const s = state.scope;
    set(
      "scope",
      `${s.nodes.length} 个节点 · ${s.edges.length} 种关系 · 最多显示 25 个节点`,
    );
    const hiddenKinds = [
      ...$("relation-options").querySelectorAll("input"),
    ].filter((i) => !i.checked).length;
    $("scope-warning").hidden = !(
      s.omittedNodes ||
      s.omittedEdges ||
      hiddenKinds
    );
    set(
      "scope-warning",
      [
        s.omittedNodes || s.omittedEdges
          ? `此范围还有 ${s.omittedNodes} 个节点、${s.omittedEdges} 种关系未绘制。缩小深度或选择更具体的符号。`
          : "",
        hiddenKinds
          ? `已关闭 ${hiddenKinds} 种关系类型，可在“关系类型”中启用。`
          : "",
      ]
        .filter(Boolean)
        .join(" "),
    );
    renderGraph();
    renderEdges();
    renderInspector();
  }
  function renderGraph() {
    const { nodes, edges, distances } = state.scope;
    $("graph").replaceChildren();
    if (!nodes.length) return;
    const incoming = M.neighborhood(
      state.model,
      state.selected,
      "in",
      Number($("depth").value),
      state.kinds,
      Number.MAX_SAFE_INTEGER,
    ).distances;
    const groups = new Map();
    for (const n of nodes) {
      const d = distances.get(n.id);
      const col = n.id === state.selected ? 0 : incoming.has(n.id) ? -d : d;
      if (!groups.has(col)) groups.set(col, []);
      groups.get(col).push(n);
    }
    const columns = [...groups.keys()].sort((a, b) => a - b),
      maxRows = Math.max(...[...groups.values()].map((g) => g.length)),
      height = Math.max(300, 80 + maxRows * 90),
      width = Math.max(600, columns.length * 250 + 40),
      positions = new Map();
    columns.forEach((col, i) =>
      groups
        .get(col)
        .forEach((n, j) =>
          positions.set(n.id, {
            x: 25 + (i * (width - 50)) / columns.length,
            y: (height - groups.get(col).length * 90) / 2 + j * 90 + 18,
          }),
        ),
    );
    const svgNS = "http://www.w3.org/2000/svg",
      svgEl = (tag, attrs = {}) => {
        const e = document.createElementNS(svgNS, tag);
        for (const [k, v] of Object.entries(attrs))
          e.setAttribute(k, String(v));
        return e;
      };
    const svg = svgEl("svg", {
      viewBox: `0 0 ${width} ${height}`,
      role: "img",
      "aria-label": `以 ${state.model.byId.get(state.selected).name} 为中心的依赖关系`,
    });
    const title = svgEl("title");
    title.textContent = "有向依赖图；点击节点聚焦，点击连线查看证据。";
    svg.append(title);
    svg.style.minWidth = width + "px";
    const defs = svgEl("defs"),
      marker = svgEl("marker", {
        id: "arrow",
        viewBox: "0 0 10 10",
        refX: 9,
        refY: 5,
        markerWidth: 6,
        markerHeight: 6,
        orient: "auto-start-reverse",
      });
    marker.append(
      svgEl("path", { d: "M 0 0 L 10 5 L 0 10 z", fill: "#7893a4" }),
    );
    defs.append(marker);
    svg.append(defs);
    for (const e of edges) {
      const a = positions.get(e.from),
        b = positions.get(e.to);
      let d;
      if (e.from === e.to) {
        d = `M ${a.x + 145} ${a.y} C ${a.x + 160} ${a.y - 35},${a.x + 65} ${a.y - 35},${a.x + 80} ${a.y}`;
      } else {
        const right = b.x >= a.x,
          ax = a.x + (right ? 205 : 0),
          bx = b.x + (right ? 0 : 205),
          ay = a.y + 28,
          by = b.y + 28;
        d = `M ${ax} ${ay} C ${(ax + bx) / 2} ${ay},${(ax + bx) / 2} ${by},${bx} ${by}`;
      }
      const line = svgEl("path", {
        d,
        fill: "none",
        stroke: state.edge === e.id ? "#087b78" : "#8ea6b6",
        "stroke-width": state.edge === e.id ? 2.5 : 1.4,
        "marker-end": "url(#arrow)",
        "stroke-dasharray": known(e) ? "none" : "5 4",
      });
      const t = svgEl("title");
      t.textContent = `${e.kind} · ${e.assertions.length} 份断言`;
      line.append(t);
      svg.append(line);
      const hit = svgEl("path", {
        d,
        fill: "none",
        stroke: "transparent",
        "stroke-width": 13,
        style: "cursor:pointer",
      });
      hit.onclick = () => inspectEdge(e);
      svg.append(hit);
    }
    for (const n of nodes) {
      const p = positions.get(n.id),
        selected = n.id === state.selected,
        g = svgEl("g", {
          transform: `translate(${p.x} ${p.y})`,
          class: "node-card",
          tabindex: 0,
          role: "button",
          "aria-label": `${n.name}，${n.kind}`,
        });
      g.append(
        svgEl("rect", {
          width: 205,
          height: 58,
          rx: 5,
          fill: selected ? "#e5f2f0" : "#ffffff",
          stroke: selected ? "#087b78" : "#c6d4df",
          "stroke-width": selected ? 1.7 : 1,
        }),
      );
      const small = svgEl("text", {
          x: 12,
          y: 18,
          fill: selected ? "#087b78" : "#718498",
          "font-size": 9,
        }),
        label = svgEl("text", {
          x: 12,
          y: 39,
          fill: "#203249",
          "font-size": 12,
          "font-weight": 550,
        });
      small.textContent = n.kind;
      label.textContent =
        n.name.length > 25 ? n.name.slice(0, 24) + "…" : n.name;
      const t = svgEl("title");
      t.textContent = `${n.name}\n${path(n)}`;
      g.append(small, label, t);
      g.onclick = () => select(n.id);
      g.onkeydown = (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          select(n.id);
        }
      };
      svg.append(g);
    }
    $("graph").append(svg);
  }
  function inspectEdge(e) {
    state.edge = e.id;
    renderGraph();
    renderEdges();
    renderInspector();
  }
  function renderEdges() {
    const rows = state.scope.edges.map((e) => {
      const tr = el("tr");
      tr.classList.toggle("selected", e.id === state.edge);
      const from = el("td"),
        rel = el("td"),
        to = el("td"),
        proof = el("td");
      from.append(
        button(state.model.byId.get(e.from).name, () => select(e.from)),
      );
      to.append(button(state.model.byId.get(e.to).name, () => select(e.to)));
      rel.append(button(e.kind, () => inspectEdge(e)));
      proof.append(
        button(
          `${e.assertions.length} 份 · ${known(e) ? "含事实/声明" : "待核查"}`,
          () => inspectEdge(e),
        ),
      );
      tr.append(from, rel, to, proof);
      return tr;
    });
    $("edge-list").replaceChildren(...rows);
    if (!rows.length) {
      const td = el(
        "td",
        "当前过滤范围没有关系。这可能是孤立节点、过滤结果，或尚未解析的依赖。",
      );
      td.colSpan = 4;
      const tr = el("tr");
      tr.append(td);
      $("edge-list").append(tr);
    }
  }
  function fields(items) {
    const dl = el("dl");
    for (const [k, v] of items) {
      if (v === undefined || v === null || v === "") continue;
      dl.append(el("dt", k), el("dd", v));
    }
    return dl;
  }
  function renderInspector() {
    const root = $("inspector");
    root.replaceChildren();
    const n = state.model.byId.get(state.selected);
    if (!n) return;
    const edge = state.scope.edges.find((e) => e.id === state.edge);
    if (edge) {
      root.append(
        el("h3", edge.kind),
        el(
          "p",
          `${state.model.byId.get(edge.from).name} → ${state.model.byId.get(edge.to).name}`,
        ),
        el("p", "不同来源的断言并列保留。置信度是索引器评分，不是业务正确率。"),
      );
      for (const a of edge.assertions) {
        const section = el("section", undefined, "assertion");
        section.append(
          fields([
            ["来源", a.source ?? "未提供"],
            ["索引器 / 解析器", a.indexer ?? a.resolver],
            ["性质 / 状态", `${a.certainty ?? "未知"} / ${a.status ?? "未知"}`],
            ["置信度", a.confidence],
            ["证据文件", a.source_file],
            ["断言 ID", a.id],
          ]),
        );
        if (a.evidence_json || a.rationale || a.snippet)
          section.append(
            el("pre", a.evidence_json ?? a.rationale ?? a.snippet),
          );
        const details = el("details");
        details.append(
          el("summary", "原始断言"),
          el("pre", JSON.stringify(a, null, 2)),
        );
        section.append(details);
        root.append(section);
      }
      if (!edge.assertions.length)
        root.append(el("p", "导出没有提供断言。不能把这条关系视为已证实。"));
    } else {
      root.append(
        el("h3", n.name),
        fields([
          ["完整 ID", n.id],
          ["源码位置", path(n)],
          ["类型", n.kind],
          ["纯度分析", n.purity],
          ["所有相邻关系", state.model.adjacency.get(n.id).length],
        ]),
      );
      if (n.metadata_json) root.append(el("pre", n.metadata_json));
      for (const line of n.summary ?? []) root.append(el("p", line));
      if (n.evidence?.length) {
        root.append(el("h3", "源码证据（导出时截取）"));
        for (const line of n.evidence)
          root.append(
            el("pre", typeof line === "string" ? line : JSON.stringify(line)),
          );
      } else
        root.append(
          el(
            "p",
            "此快照未附源码片段。复制位置到编辑器核对原始实现；关系表可查看索引器证据。",
          ),
        );
      const mappings =
        state.manifest?.items.filter((i) => i.sources.includes(n.id)) ?? [];
      if (mappings.length) {
        root.append(el("h3", "迁移去向"));
        for (const row of mappings)
          root.append(
            el(
              "p",
              `${names[row.disposition] ?? row.disposition} · ${row.reason}`,
            ),
            ...row.targets.map((t) => el("code", t.path)),
          );
      }
      root.append(
        el("h3", "核查建议"),
        el(
          "p",
          "先查上游调用者与下游副作用，再核对外部依赖、检查项和独立回归用例。图上的路径不代表所有运行时路径。",
        ),
      );
    }
  }
  function renderDiagnostics() {
    const box = $("diagnostics");
    box.replaceChildren();
    for (const d of (state.model?.diagnostics ?? []).slice(0, state.gapLimit)) {
      const row = el("div", undefined, "diagnostic");
      row.append(el("div", d.message));
      if (state.model.byId.has(d.target))
        row.append(button("定位源符号", () => select(d.target)));
      if (d.edge) {
        const details = el("details");
        details.append(
          el("summary", "查看原始关系"),
          el("pre", JSON.stringify(d.edge, null, 2)),
        );
        row.append(details);
      }
      box.append(row);
    }
    if ((state.model?.diagnostics.length ?? 0) > state.gapLimit)
      box.append(
        button(
          `再显示 100 项（${state.gapLimit} / ${state.model.diagnostics.length}）`,
          () => {
            state.gapLimit += 100;
            renderDiagnostics();
          },
        ),
      );
    if (!box.children.length)
      box.append(el("p", "当前数据未提供待核查记录。此状态不证明覆盖完整。"));
  }
  function renderMigration() {
    const m = state.manifest,
      r = state.report,
      b = state.binding;
    set("migration-count", m?.items.length ?? "—");
    $("migration-status").classList.remove("pass");
    if (!m) {
      set(
        "migration-status",
        "请打开 migration.json。映射与检查报告仅在浏览器读取，不执行其中的命令。",
      );
      $("mapping-list").replaceChildren();
      return;
    }
    let message = `${m.items.length} 条映射 · 审查声明：${m.review?.status ?? "未提供"}。`;
    if (!r) message += " 尚未载入检查报告；不能据此认定迁移通过。";
    else if (b !== "matched")
      message += ` 报告${b === "stale" ? "已过期或属于其他映射" : b === "unavailable" ? "版本校验不可用" : "未绑定映射版本"}，通过状态无效。请重新运行 migration.py check。`;
    else {
      message += r.ready_for_review
        ? " 本地检查报告满足审查条件；人工审查声明未认证，业务等价仍未证明。"
        : ` 当前映射版本有 ${r.blockers?.length ?? 0} 个阻断项。`;
      if (r.ready_for_review) $("migration-status").classList.add("pass");
    }
    set("migration-status", message);
    $("migration-summary").replaceChildren();
    const counts = el("div", undefined, "summary-row");
    for (const [key, label] of Object.entries(names)) {
      const s = el("span");
      s.append(
        el("strong", m.items.filter((i) => i.disposition === key).length),
        document.createTextNode(label),
      );
      counts.append(s);
    }
    $("migration-summary").append(counts);
    const visibleItems = m.items.filter(
      (i) =>
        !$("disposition").value || i.disposition === $("disposition").value,
    );
    const rows = visibleItems.slice(0, state.mapLimit).map((i) => {
      const tr = el("tr"),
        src = el("td"),
        disp = el("td"),
        targets = el("td"),
        checks = el("td");
      for (const id of i.sources) {
        const b = button(state.model?.byId.get(id)?.name ?? id, () =>
          select(id),
        );
        b.title = id;
        src.append(b, el("code", id));
      }
      disp.append(
        el(
          "span",
          names[i.disposition] ?? i.disposition,
          `tag${i.disposition === "unresolved" ? " amber" : ""}`,
        ),
        el("p", i.reason || "缺少理由"),
      );
      for (const t of i.targets) {
        targets.append(
          el("code", t.path),
          el("small", `SHA256 ${t.sha256 ?? "未固定"}`, "muted"),
        );
      }
      for (const id of i.checks) {
        const check = m.checks.find((c) => c.id === id);
        checks.append(
          el("strong", id),
          el(
            "p",
            check
              ? `${check.oracle ?? "缺少预期来源"} · ${check.oracle_evidence ?? ""}`
              : "缺少测试定义",
          ),
        );
      }
      if (!i.checks.length)
        checks.append(el("span", "未关联回归检查", "muted"));
      tr.append(src, disp, targets, checks);
      return tr;
    });
    $("mapping-list").replaceChildren(...rows);
    if (visibleItems.length > state.mapLimit) {
      const tr = el("tr"),
        td = el("td");
      td.colSpan = 4;
      td.append(
        button(
          `再显示 100 项（${state.mapLimit} / ${visibleItems.length}）`,
          () => {
            state.mapLimit += 100;
            renderMigration();
          },
        ),
      );
      tr.append(td);
      $("mapping-list").append(tr);
    }
    $("blockers").replaceChildren();
    if (r) {
      if (b !== "matched")
        $("blockers").append(
          el("p", "以下是未验证版本的原始报告内容，不计入当前映射结果。"),
        );
      for (const blocker of r.blockers ?? [])
        $("blockers").append(el("div", blocker, "diagnostic"));
      for (const run of r.test_runs ?? [])
        $("blockers").append(el("pre", JSON.stringify(run, null, 2)));
      if (!r.blockers?.length && !r.test_runs?.length)
        $("blockers").append(el("p", "报告未提供阻断项或执行记录。"));
    } else $("blockers").append(el("p", "尚未载入检查报告。"));
  }
  function validateManifest(m) {
    if (
      m.schema_version !== 1 ||
      !Array.isArray(m.items) ||
      !Array.isArray(m.checks)
    )
      throw new Error("Unsupported migration manifest");
    for (const i of m.items)
      if (
        !Array.isArray(i.sources) ||
        !i.sources.every((s) => typeof s === "string") ||
        !Array.isArray(i.targets) ||
        !i.targets.every((t) => t && typeof t.path === "string") ||
        !Array.isArray(i.checks)
      )
        throw new Error("Invalid migration mapping");
  }
  async function loadFiles(files) {
    const generation = ++state.generation,
      loaded = [];
    for (const file of files) {
      if (file.size > 100_000_000)
        throw new Error(`文件过大（上限 100 MB）：${file.name}`);
      const text = await file.text(),
        raw = JSON.parse(text);
      loaded.push({ name: file.name, text, raw });
    }
    let graph = null,
      manifest = state.manifest,
      manifestText = state.manifestText,
      report = state.report;
    for (const item of loaded) {
      if (Array.isArray(item.raw.items)) {
        validateManifest(item.raw);
        manifest = item.raw;
        manifestText = item.text;
      } else if (typeof item.raw.ready_for_review === "boolean") {
        if (
          !Array.isArray(item.raw.blockers) ||
          !Array.isArray(item.raw.test_runs)
        )
          throw new Error("Invalid review report");
        report = item.raw;
      } else {
        M.normalize(item.raw);
        graph = item;
      }
    }
    const binding =
      manifest && report ? await M.bindReport(manifestText, report) : "unbound";
    if (generation !== state.generation) return;
    state.manifest = manifest;
    state.manifestText = manifestText;
    state.report = report;
    state.binding = binding;
    if (graph) setGraph(graph.raw, graph.name);
    renderMigration();
    if (state.model) renderInspector();
    if (!graph && manifest) tab("migration");
    $("error").hidden = true;
    set(
      "load-status",
      `已载入 ${loaded.map((f) => f.name).join("、")} · 仅本地读取`,
    );
  }
  $("files").onchange = action(async (e) => {
    await loadFiles([...e.target.files]);
    e.target.value = "";
  });
  document
    .querySelectorAll("[data-tab]")
    .forEach((b) => (b.onclick = () => tab(b.dataset.tab)));
  $("search").oninput = () => {
    state.limit = 100;
    renderList();
  };
  $("kind").onchange = () => {
    state.limit = 100;
    renderList();
  };
  $("more").onclick = () => {
    state.limit += 100;
    renderList();
  };
  $("direction").onchange = $("depth").onchange = () => {
    state.edge = null;
    renderScope();
  };
  $("disposition").onchange = () => {
    state.mapLimit = 100;
    renderMigration();
  };
  $("fit").onclick = () => {
    const svg = $("graph").querySelector("svg");
    if (svg) {
      svg.style.minWidth = "0";
      $("graph").scrollLeft = 0;
    }
  };
  $("copy-location").onclick = action(async () => {
    await navigator.clipboard.writeText(
      path(state.model.byId.get(state.selected)),
    );
    set("load-status", "已复制源码位置");
  });
  $("export").onclick = () => {
    const s = state.scope;
    if (!s) return;
    const data = {
      meta: { repo: state.model.repo },
      nodes: s.nodes,
      edges: s.edges,
      limitations: [
        ...state.model.diagnostics.map((d) => d.message),
        "Local selection export; not a complete repository graph.",
      ],
      truncated: !!(s.omittedNodes || s.omittedEdges),
    };
    const url = URL.createObjectURL(
      new Blob([JSON.stringify(data, null, 2)], { type: "application/json" }),
    );
    const a = el("a");
    a.href = url;
    a.download = "groundgraph-selection.json";
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };
  $("demo").onclick = () => {
    setGraph(
      {
        meta: { repo: "订单提交 · 示例数据" },
        nodes: [
          {
            id: "submit",
            name: "提交订单",
            kind: "http_route",
            path: "OrderController.java",
            line: 42,
          },
          {
            id: "check",
            name: "提交前检查集",
            kind: "java_method",
            path: "OrderChecks.java",
            line: 18,
          },
          {
            id: "price",
            name: "计算价格",
            kind: "java_method",
            path: "Pricing.java",
            line: 63,
          },
          {
            id: "balance",
            name: "校验余额",
            kind: "java_method",
            path: "Account.java",
            line: 20,
          },
          {
            id: "save",
            name: "保存订单",
            kind: "java_method",
            path: "OrderService.java",
            line: 75,
          },
          {
            id: "orders",
            name: "orders",
            kind: "db_table",
            path: "schema.sql",
            line: 1,
          },
          {
            id: "legacy",
            name: "未解析的历史检查",
            kind: "java_method",
            path: "Legacy.java",
            line: 8,
          },
        ],
        links: [
          ["submit", "check", "calls"],
          ["check", "price", "calls"],
          ["check", "balance", "calls"],
          ["submit", "save", "calls"],
          ["save", "orders", "persists_to"],
        ].map(([source, target, kind]) => ({
          source,
          target,
          kind,
          assertions: [
            {
              source: "language_adapter",
              certainty: "inferred",
              status: "proposed",
              confidence: 0.7,
              indexer: "synthetic-demo",
              source_file: "DEMO ONLY",
              evidence_json: "示例数据；请载入自己的索引进行核查",
            },
          ],
        })),
        limitations: ["这是明确标注的示例，不是实际 OMS 运行或迁移证据。"],
      },
      "内置示例",
    );
    $("depth").value = "2";
    renderScope();
  };
  action(async () => {
    const embedded = document.getElementById("groundgraph-data");
    if (embedded) {
      setGraph(JSON.parse(embedded.textContent), "CLI 导出快照");
      set("load-status", "已载入导出时的图快照。重新索引并导出可更新。");
    } else {
      const data = new URL(location.href).searchParams.get("data");
      if (data) {
        const url = new URL(data, location.href);
        if (
          url.origin !== location.origin ||
          !["http:", "https:"].includes(url.protocol)
        )
          throw new Error("仅允许同源数据文件");
        const response = await fetch(url);
        if (!response.ok)
          throw new Error(`读取图数据失败：HTTP ${response.status}`);
        const blob = await response.blob();
        await loadFiles([new File([blob], url.pathname.split("/").pop())]);
      }
    }
    renderMigration();
    renderDiagnostics();
  })();
})();
