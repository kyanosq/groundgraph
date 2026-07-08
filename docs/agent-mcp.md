# GroundGraph for agents and MCP clients

GroundGraph is intended to be used by agents as a local, non-invasive code graph.
The MCP server exposes structured JSON tools over stdio so an agent can answer
repository-navigation questions without falling back to broad grep/read scans.

## Prepare a repository

Run these once per target repository:

```bash
groundgraph --repo-root /path/to/repo init
groundgraph --repo-root /path/to/repo index
groundgraph --repo-root /path/to/repo check
```

`init` writes `.groundgraph.yaml` and a rebuildable `.groundgraph/` workspace.
`index` populates `.groundgraph/graph.db`. GroundGraph never annotates or edits
source files.

## Auto-configure agents

GroundGraph can write MCP config for supported local agents:

```bash
# Project-local configs:
# - Cursor: .cursor/mcp.json
# - Claude Code: .mcp.json
groundgraph --repo-root /path/to/repo install --agent cursor,claude

# Codex CLI has no project-local MCP config, so it is global only:
# - Codex: ~/.codex/config.toml
groundgraph --repo-root /path/to/repo install --location global --agent codex
```

Use `--dry-run` to inspect the planned writes. Use `--auto-allow` with Claude
Code if you want the installer to add allow-list entries for GroundGraph MCP
tools.

## Keep the graph fresh

During active agent work, run the watcher in the repository:

```bash
groundgraph --repo-root /path/to/repo watch
```

The watcher runs an initial `index`, then polls repository files and re-indexes
after debounced changes. It ignores generated/cache directories including
`.groundgraph/`, `.git/`, `target/`, `node_modules/`, `build/`, `dist/`,
`.dart_tool/` and `.gradle/`.

Useful flags:

```bash
groundgraph watch --no-initial-index
groundgraph watch --interval-ms 1000 --debounce-ms 750
groundgraph watch --once --no-initial-index
```

## Start the MCP server

```bash
groundgraph-mcp --repo-root /path/to/repo
```

The server speaks newline-delimited JSON-RPC over stdio. Logs go to stderr;
stdout is reserved for MCP frames. Every tool also accepts `repo_root` to
override the server default for one call.

Use this config shape in stdio-capable MCP clients such as Cursor, Claude
Desktop, Claude Code, Codex, Continue, or similar agent runtimes:

```jsonc
{
  "mcpServers": {
    "groundgraph": {
      "type": "stdio",
      "command": "groundgraph-mcp",
      "args": ["--repo-root", "/path/to/repo"]
    }
  }
}
```

## Tool guide for agents

| Task | Tool | Notes |
| --- | --- | --- |
| Find symbols, concepts, files, or code snippets | `search_graph` | Start here. It returns ranked matches, reasons, snippets, and a bounded subgraph. |
| Drill into a known symbol id | `explain_symbol` | Use after `search_graph` or `impact` when the agent asks "what is this symbol?" |
| Expand graph neighbours | `get_subgraph` | Use when the agent already has a node id and needs controlled N-hop traversal. |
| Build an edit-ready bundle | `context_pack` | Accepts one of `requirement_id`, `candidate_id`, or `symbol_id`; can include snippets. |
| Review a PR or local edits | `impact` | Use `worktree: true` for uncommitted tracked changes. |
| Check doc/code drift | `check_drift` | Reports broken declared links, stale doc references, orphan requirements, and implementation hints. |
| Triage possible dead code | `dead_code` | Candidate report only; never treat it as automatic deletion proof. |

## Recommended agent policy

- Prefer `search_graph` before grep, glob, or broad file reads.
- Trust graph facts as evidence. Read source files only when a response is
  truncated, stale, missing needed source body, or the task requires editing.
- Run `impact` with `worktree: true` when reviewing the current uncommitted
  working tree.
- Run `check_drift` after changing docs, requirements, tests, or public API
  surfaces.
- Treat AI business logic as candidate until a human accepts it through the
  GroundGraph candidate workflow.

Example MCP `tools/call` payload for uncommitted-change impact:

```json
{
  "name": "impact",
  "arguments": {
    "base": "HEAD",
    "worktree": true,
    "reindex": true
  }
}
```

Example search:

```json
{
  "name": "search_graph",
  "arguments": {
    "query": "auth session refresh",
    "depth": 1,
    "limit": 10
  }
}
```
