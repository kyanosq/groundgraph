// Synthetic data only. Run: npm ci && npx playwright install chromium && npm run test:browser
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile, mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve, sep } from "node:path";
import { createHash } from "node:crypto";
import { chromium } from "playwright";
const root = fileURLToPath(new URL(".", import.meta.url));
const server = createServer(async (req, res) => {
  try {
    const path = resolve(
      root,
      "." +
        decodeURIComponent(
          new URL(req.url, "http://localhost").pathname,
        ).replace(/\/$/, "/index.html"),
    );
    if (!path.startsWith(root.endsWith(sep) ? root : root + sep))
      throw new Error("outside");
    const data = await readFile(path);
    res.setHeader(
      "Content-Type",
      path.endsWith(".js")
        ? "text/javascript"
        : path.endsWith(".css")
          ? "text/css"
          : "text/html",
    );
    res.end(data);
  } catch {
    res.writeHead(404);
    res.end("not found");
  }
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
let browser;
try {
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({
    viewport: { width: 1440, height: 1000 },
  });
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  await page.getByRole("button", { name: "浏览内置示例", exact: true }).click();
  assert.equal(await page.locator("#graph .node-card").count(), 6);
  await mkdir(new URL("./shots", import.meta.url), { recursive: true });
  await page.screenshot({
    path: fileURLToPath(
      new URL("./shots/workspace-desktop.png", import.meta.url),
    ),
    fullPage: true,
  });
  await page.getByRole("searchbox").fill("未解析");
  await page.locator(".tree-item").click();
  assert.equal(await page.locator("#graph .node-card").count(), 1);
  assert.match(await page.locator("#edge-list").innerText(), /没有关系/);
  const graph = {
    meta: { repo: "test snapshot" },
    nodes: [
      { id: "a", name: "Entry", kind: "java_method", path: "A.java", line: 3 },
      { id: "b", name: "Check", kind: "java_method" },
      { id: "solo", name: "Isolated", kind: "java_method" },
    ],
    links: [
      {
        source: "a",
        target: "b",
        kind: "calls",
        assertions: [
          {
            id: "one",
            source: "tree_sitter",
            certainty: "candidate",
            status: "proposed",
            confidence: 0.5,
          },
          {
            id: "two",
            source: "compiler",
            certainty: "fact",
            status: "confirmed",
            confidence: 1,
          },
        ],
      },
    ],
  };
  const file = (name, value) => ({
    name,
    mimeType: "application/json",
    buffer: Buffer.from(
      typeof value === "string" ? value : JSON.stringify(value),
    ),
  });
  await page.locator("#files").setInputFiles(file("graph.json", graph));
  await page
    .getByRole("button", { name: "2 份 · 含事实/声明", exact: true })
    .click();
  assert.equal(await page.locator(".assertion").count(), 2);
  assert.match(await page.locator("#inspector").innerText(), /tree_sitter/);
  assert.match(await page.locator("#inspector").innerText(), /compiler/);
  await page
    .locator("#files")
    .setInputFiles(file("invalid.json", '{"invalid":true}'));
  assert.equal(await page.locator("#error").isVisible(), true);
  assert.equal(await page.locator("#repo").innerText(), "test snapshot");
  const manifest = {
    schema_version: 1,
    review: { status: "pending" },
    items: [
      {
        id: "m1",
        sources: ["a"],
        disposition: "unresolved",
        reason: "await independent fixture",
        targets: [],
        checks: [],
      },
    ],
    checks: [],
  };
  const content = JSON.stringify(manifest);
  const report = {
    manifest_sha256: createHash("sha256").update(content).digest("hex"),
    ready_for_review: false,
    blockers: ["unresolved mapping: m1"],
    test_runs: [],
  };
  await page
    .locator("#files")
    .setInputFiles([
      file("migration.json", content),
      file("report.json", report),
    ]);
  await page.locator("#migration").waitFor({ state: "visible" });
  await page.getByText(/当前映射版本有 1 个阻断项/).waitFor();
  assert.match(await page.locator("#mapping-list").innerText(), /待解析/);
  await page
    .locator("#files")
    .setInputFiles(file("migration.json", content + " "));
  await page.getByText(/报告已过期或属于其他映射/).waitFor();
  assert.equal(
    await page.locator("#migration-status").getAttribute("class"),
    "notice",
  );
  await page.getByRole("button", { name: "依赖探索", exact: true }).click();
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({
    path: fileURLToPath(
      new URL("./shots/workspace-mobile.png", import.meta.url),
    ),
    fullPage: true,
  });
  assert.equal(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
    true,
    "page must not overflow horizontally",
  );
  assert.deepEqual(errors, []);
  console.log(
    "Browser checks passed: exploration, isolation, assertion provenance, invalid input, bound/stale migration reports, responsive layout.",
  );
} finally {
  if (browser) await browser.close();
  server.close();
}
