import test from "node:test";
import assert from "node:assert/strict";
import "./model.js";
const { normalize, neighborhood, filteredNodes, bindReport } =
  globalThis.GroundGraphModel;
const fixture = {
  meta: { repo: "test" },
  nodes: [
    { id: "entry", name: "Submit", kind: "method" },
    { id: "check", name: "Check", kind: "method" },
    { id: "solo", name: "孤立", kind: "table" },
  ],
  links: [
    {
      source: "entry",
      target: "check",
      kind: "calls",
      assertions: [
        { id: "a", source: "parser" },
        { id: "b", source: "semantic" },
      ],
    },
    { source: "check", target: "check", kind: "calls", assertions: [] },
  ],
};
test("projection retains evidence, recursion and isolated searchable nodes", () => {
  const m = normalize(fixture);
  assert.equal(m.edges[0].assertions.length, 2);
  assert.equal(filteredNodes(m, "孤立", "").length, 1);
  assert.equal(
    neighborhood(m, "check", "out", 2, new Set(["calls"]), 20).edges.length,
    1,
  );
  assert.equal(
    neighborhood(m, "check", "in", 1, new Set(["calls"]), 20).nodes.length,
    2,
  );
});
test("caps are explicit and selection is never dropped", () => {
  const m = normalize(fixture);
  const n = neighborhood(m, "entry", "both", 3, new Set(["calls"]), 1);
  assert.deepEqual(
    n.nodes.map((n) => n.id),
    ["entry"],
  );
  assert.equal(n.omittedNodes, 1);
  assert.equal(n.omittedEdges, 2);
});
test("invalid identity and broken endpoints are never silently accepted", () => {
  assert.throws(
    () => normalize({ nodes: [{ id: "a" }, { id: "a" }], links: [] }),
    /duplicate/i,
  );
  assert.throws(() => normalize({ hello: "world" }), /graph/i);
  assert.throws(
    () =>
      normalize({
        ...fixture,
        links: [
          {
            source: "entry",
            target: "check",
            kind: "calls",
            assertions: [null],
          },
        ],
      }),
    /assertion/i,
  );
  const m = normalize({
    nodes: [{ id: "a" }],
    links: [{ source: "a", target: "missing", kind: "calls" }],
  });
  assert.equal(m.edges.length, 0);
  assert.equal(m.diagnostics.length, 1);
});
test("feature packs and curated views share one model without manufacturing proof", () => {
  const m = normalize({
    schema_version: 2,
    symbols: [
      {
        id: "x",
        name: "X",
        kind: "java_method",
        path: "X.java",
        start_line: 3,
      },
    ],
    edges: [],
    limitations: ["overloads unresolved"],
    omitted_symbols: 2,
  });
  assert.equal(m.nodes[0].line, 3);
  assert.ok(m.diagnostics.some((d) => d.message.includes("overloads")));
  assert.ok(m.diagnostics.some((d) => d.message.includes("2")));
  const c = normalize({
    nodes: [{ id: "x", label: "X", line_range: { start: 5, end: 6 } }],
    edges: [
      {
        id: "e",
        from: "x",
        to: "x",
        kind: "calls",
        confidence: 0.5,
        source: "heuristic",
      },
    ],
    findings: [],
  });
  assert.equal(c.edges[0].assertions[0].confidence, 0.5);
});
test("review report must match the exact manifest before showing a pass", async () => {
  const manifest = '{"schema_version":1,"items":[]}';
  const hash = Array.from(
    new Uint8Array(
      await crypto.subtle.digest("SHA-256", new TextEncoder().encode(manifest)),
    ),
    (b) => b.toString(16).padStart(2, "0"),
  ).join("");
  assert.equal(
    await bindReport(manifest, {
      manifest_sha256: hash,
      ready_for_review: true,
    }),
    "matched",
  );
  assert.equal(
    await bindReport(manifest + " ", {
      manifest_sha256: hash,
      ready_for_review: true,
    }),
    "stale",
  );
  assert.equal(
    await bindReport(manifest, { ready_for_review: true }),
    "unbound",
  );
});
