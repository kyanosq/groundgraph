# Install the GroundGraph AI Skill

This package includes a Codex skill that teaches AI agents how to use
GroundGraph safely and non-invasively.

## Install for Codex

```bash
mkdir -p ~/.codex/skills
rm -rf ~/.codex/skills/groundgraph
cp -R /usr/local/groundgraph/skills/groundgraph ~/.codex/skills/groundgraph
```

If you installed this package somewhere else, replace `/usr/local/groundgraph`
with the extracted package path.

## Verify

Start a new Codex session and ask:

```text
Use $groundgraph to index this repository, generate a graph, and summarize candidate business logic for review.
```

The skill should guide the agent to:

- run `groundgraph init/index/check/logic`
- generate graph HTML or JSON
- prefer `groundgraph search "<query>" --format html` for human-readable local subgraphs
- run `groundgraph dead-code` only as a confidence-ranked candidate report
- keep AI-generated business logic as candidates
- avoid asking for code or document annotations
- report real command outputs and sidecar status

## Important Boundary

The skill does not make AI output authoritative. It instructs agents to treat:

- deterministic graph rows as facts
- AI-generated business descriptions as candidates
- human-reviewed items as confirmed

For dead-code analysis, the skill also tells agents:

- `high`, `medium`, and `low` are confidence buckets, not delete instructions.
- `--include-tests` means orphan test facts are reported; test helper functions
  are not production dead-code findings.
- Agents must inspect search/focus graph evidence before suggesting removal.
  They must not ask users to add `@used` or other annotations to code, tests,
  or docs.
