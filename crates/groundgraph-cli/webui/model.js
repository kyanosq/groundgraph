/* One projection for raw networks, curated views, traces and migration packs.
 * No ratio or graph shape is a proof of behavioral equivalence. */
(() => {
  "use strict";
  const text = (v, fallback = "") => (typeof v === "string" ? v : fallback);
  const location = (n) =>
    Array.isArray(n.line_range) ? n.line_range[0] : n.line_range?.start;
  function normalize(raw) {
    if (!raw || (!Array.isArray(raw.nodes) && !Array.isArray(raw.symbols)))
      throw new Error("Unsupported graph: expected nodes or symbols");
    if (!Array.isArray(raw.links ?? raw.edges))
      throw new Error("Graph relationships must be an array");
    for (const field of ["findings", "limitations", "dangling_assertions"])
      if (raw[field] !== undefined && !Array.isArray(raw[field]))
        throw new Error(`Invalid graph ${field}`);
    const diagnostics = (raw.findings ?? []).map((d) => ({
      message: text(d.message),
      target: d.target_id,
    }));
    for (const message of raw.limitations ?? [])
      diagnostics.push({ message: String(message) });
    for (const call of raw.java_analysis?.calls ?? []) {
      if (call.resolution !== "resolved")
        diagnostics.push({
          message: `${call.resolution}: ${call.path}:${call.line}:${call.column} · ${call.expression} · ${call.reason}`,
          target: call.caller,
          call,
        });
    }
    for (const message of raw.java_analysis?.diagnostics ?? [])
      diagnostics.push({ message: String(message) });
    if (raw.omitted_symbols?.length || Number(raw.omitted_symbols) > 0)
      diagnostics.push({
        message: `Source evidence omitted: ${raw.omitted_symbols.length ?? raw.omitted_symbols}`,
      });
    if (raw.truncated || raw.stats?.truncated)
      diagnostics.push({
        message:
          "Export is truncated; re-export with a larger scope to inspect missing relationships.",
      });
    if (raw.meta?.omitted_isolated)
      diagnostics.push({
        message: `Export omitted ${raw.meta.omitted_isolated} isolated nodes`,
      });
    const byId = new Map();
    const nodes = (raw.nodes ?? raw.symbols)
      .map((n) => {
        if (!n || typeof n.id !== "string" || !n.id)
          throw new Error("Graph node requires a nonempty id");
        if (byId.has(n.id)) throw new Error(`Duplicate node identity: ${n.id}`);
        for (const field of ["summary", "evidence"])
          if (n[field] !== undefined && !Array.isArray(n[field]))
            throw new Error(`Invalid node ${field}`);
        const node = {
          ...n,
          name: text(n.name ?? n.label, n.id),
          kind: text(n.kind, "unknown"),
          path: text(n.path),
          line: n.line ?? n.start_line ?? location(n),
        };
        byId.set(node.id, node);
        return node;
      })
      .sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
    const edges = [],
      groups = new Map();
    for (const e of [
      ...(raw.links ?? raw.edges),
      ...(raw.dangling_assertions ?? []).map((a) => ({
        from: a.from_id,
        to: a.to_id,
        kind: a.kind,
        assertion: a,
      })),
    ]) {
      const from =
        e.source && typeof e.source === "string" && "target" in e
          ? e.source
          : (e.from ?? e.from_id);
      const to = e.target ?? e.to ?? e.to_id;
      if (
        typeof from !== "string" ||
        typeof to !== "string" ||
        typeof e.kind !== "string"
      )
        throw new Error("Invalid graph relationship");
      if (!byId.has(from) || !byId.has(to)) {
        diagnostics.push({
          message: `Endpoint outside snapshot: ${from} → ${to} (${e.kind})`,
          target: byId.has(from) ? from : to,
          edge: e,
        });
        continue;
      }
      const key = JSON.stringify([from, to, e.kind]);
      let edge = groups.get(key);
      if (!edge) {
        edge = { id: key, from, to, kind: e.kind, assertions: [] };
        groups.set(key, edge);
        edges.push(edge);
      }
      const assertions = e.assertions ?? [e.assertion ?? e];
      if (
        !Array.isArray(assertions) ||
        assertions.some((a) => !a || typeof a !== "object" || Array.isArray(a))
      )
        throw new Error("Relationship assertions must be objects");
      edge.assertions.push(...assertions);
    }
    const adjacency = new Map(nodes.map((n) => [n.id, []]));
    for (const e of edges) {
      adjacency.get(e.from).push(e);
      if (e.to !== e.from) adjacency.get(e.to).push(e);
    }
    return {
      repo: text(
        raw.meta?.repo ?? raw.focus ?? raw.repo_root,
        "Local snapshot",
      ),
      nodes,
      edges,
      byId,
      adjacency,
      diagnostics,
      raw,
    };
  }
  function filteredNodes(model, query = "", kind = "") {
    const q = query.trim().toLocaleLowerCase();
    return model.nodes.filter(
      (n) =>
        (!kind || n.kind === kind) &&
        (!q || `${n.name} ${n.path} ${n.id}`.toLocaleLowerCase().includes(q)),
    );
  }
  function neighborhood(
    model,
    id,
    direction = "both",
    depth = 1,
    kinds = null,
    cap = 25,
  ) {
    if (!model.byId.has(id))
      return {
        nodes: [],
        edges: [],
        omittedNodes: 0,
        omittedEdges: 0,
        distances: new Map(),
      };
    const distances = new Map([[id, 0]]),
      queue = [id];
    for (let i = 0; i < queue.length; i++) {
      const current = queue[i],
        level = distances.get(current);
      if (level >= depth) continue;
      for (const e of model.adjacency.get(current)) {
        if (kinds && !kinds.has(e.kind)) continue;
        const next =
          e.from === current
            ? direction === "in"
              ? null
              : e.to
            : direction === "out"
              ? null
              : e.from;
        if (next !== null && !distances.has(next)) {
          distances.set(next, level + 1);
          queue.push(next);
        }
      }
    }
    const visible = queue.slice(0, Math.max(1, cap)),
      ids = new Set(visible);
    const relevant = model.edges.filter(
      (e) =>
        distances.has(e.from) &&
        distances.has(e.to) &&
        (!kinds || kinds.has(e.kind)),
    );
    const edges = relevant.filter((e) => ids.has(e.from) && ids.has(e.to));
    return {
      nodes: visible.map((id) => model.byId.get(id)),
      edges,
      distances,
      omittedNodes: queue.length - visible.length,
      omittedEdges: relevant.length - edges.length,
    };
  }
  async function bindReport(manifestText, report) {
    if (!report?.manifest_sha256) return "unbound";
    if (!globalThis.crypto?.subtle) return "unavailable";
    const digest = await crypto.subtle.digest(
      "SHA-256",
      new TextEncoder().encode(manifestText),
    );
    const hash = Array.from(new Uint8Array(digest), (b) =>
      b.toString(16).padStart(2, "0"),
    ).join("");
    return hash === report.manifest_sha256 ? "matched" : "stale";
  }
  globalThis.GroundGraphModel = {
    normalize,
    filteredNodes,
    neighborhood,
    bindReport,
  };
})();
