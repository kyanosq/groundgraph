# GroundGraph Code Constellation (WebUI)

A WebGL force-directed viewer that renders a GroundGraph code graph as a real
network — a glowing 3D constellation instead of the layered/columnar explorer.
Built for high visual quality and interactive performance on large graphs.

- **Renderer**: Three.js (`three`) + `3d-force-graph` with `UnrealBloomPass` glow,
  in-scene gradient skydome and a starfield for depth.
- **Encoding**: node colour = kind (function / method / type / route / table /
  test / doc), node size = degree (connectivity), edge colour = source kind.
- **Interaction**: orbit / zoom, hover to highlight a node's neighbourhood,
  click to dolly-focus and open a details + connections panel, legend toggles
  per-kind visibility.
- **Scale**: small/medium graphs render in full; very large graphs (>6k nodes)
  hide low-signal kinds (tests/docs/files) and render the top-degree backbone
  (default 2,600 hubs) so the view stays at ~60fps. The HUD states what is
  hidden.

## Use it

### Single-file export (recommended)

Have the CLI inline the graph **and** the viewer bundle into one portable HTML
file that opens straight from `file://` with no server and no network:

```bash
groundgraph graph --repo-root /path/to/repo --format web --out graph-web.html
# then just double-click graph-web.html
```

### Dev page (live data, drag-drop)

1. Export any `.groundgraph/graph.db` to network JSON:

   ```bash
   python3 export_graph.py /path/to/repo/.groundgraph/graph.db data/mygraph.json
   # add --keep-isolated to include nodes with no edges
   ```

2. Open the viewer. Either:
   - serve the folder and load via query param:
     ```bash
     python3 -m http.server 8777
     # open http://localhost:8777/index.html?data=./data/mygraph.json
     ```
   - or just open `index.html` and **drag-drop** the JSON onto the page
     (or use **＋ load graph…** in the HUD).

> **Fully offline.** `three` / `3d-force-graph` / `UnrealBloomPass` are bundled
> into `vendor/groundgraph-viewer.bundle.js` (one classic IIFE) and loaded locally,
> so neither the dev page nor the export needs any network.

## Files

- `index.html` — the entire viewer (HTML + CSS + JS, single file).
- `vendor/groundgraph-viewer.bundle.js` — checked-in offline renderer bundle
  (`three` + `3d-force-graph` + `UnrealBloomPass`). The dev page `<script src>`s
  it; `graph --format web` inlines it.
- `vendor-src/entry.js` + `vendor-src/build.sh` — source and pinned, reproducible
  recipe to regenerate the bundle (rerun only when bumping a dependency).
- `export_graph.py` — `graph.db` → `{meta, nodes, links}` JSON exporter.
- `shoot.mjs` — Playwright screenshot helper used to iterate on visuals:
  ```bash
  node shoot.mjs "http://localhost:8777/index.html" shots/out.png 11000 [select-hub|hover-hub]
  ```
