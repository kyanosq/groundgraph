//! Static transaction projection over the authoritative trace graph.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;
use serde_json::Value;
use std::path::Path;

use groundgraph_core::{EdgeCertainty, EdgeKind, NodeKind};
use groundgraph_store::Store;

use crate::trace::{TraceNode, TraceResult};

#[derive(Debug, Clone, Serialize)]
pub struct TableEffect {
    pub table: String,
    pub operation: String,
    pub columns: Vec<String>,
    pub certainty: String,
    pub confidence: f32,
    pub source: Option<String>,
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalCall {
    pub id: String,
    pub name: String,
    pub effect: String,
    pub source: Option<String>,
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnresolvedCall {
    pub path: String,
    pub line: u32,
    pub expression: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EffectsCard {
    pub entry: String,
    pub seeds: Vec<String>,
    pub tables: Vec<TableEffect>,
    pub external_calls: Vec<ExternalCall>,
    pub events: Vec<ExternalCall>,
    pub transactions: TransactionAnalysis,
    pub unresolved_count: usize,
    pub breakpoints: Vec<UnresolvedCall>,
    pub breakpoints_truncated: bool,
    pub confirmed_effect_ratio: f32,
    pub truncated: bool,
}

pub fn run_effects(options: crate::trace::TraceOptions) -> crate::error::EngineResult<EffectsCard> {
    let trace = crate::trace::run_trace(options)?;
    Ok(effects_from_trace(&trace))
}

pub fn effects_from_trace(trace: &TraceResult) -> EffectsCard {
    let transactions = analyze_transactions(trace);
    let nodes: BTreeMap<_, _> = trace.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut tables = Vec::new();
    let mut score = BTreeMap::<&str, f32>::new();
    for seed in &trace.seeds {
        score.insert(seed, 1.0);
    }
    for _ in 0..trace.nodes.len() {
        let mut changed = false;
        for edge in &trace.edges {
            if let Some(before) = score.get(edge.from.as_str()).copied() {
                let after = before * edge.assertion.confidence.get();
                if after > score.get(edge.to.as_str()).copied().unwrap_or(0.0) {
                    score.insert(&edge.to, after);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut confirmed = 0usize;
    let mut measured = 0usize;
    for edge in &trace.edges {
        if edge.kind != "persists_to" {
            continue;
        }
        let Some(node) = nodes.get(edge.to.as_str()) else {
            continue;
        };
        if node.kind != "db_table" {
            continue;
        }
        let meta: Value = edge
            .assertion
            .metadata_json
            .as_deref()
            .and_then(|m| serde_json::from_str(m).ok())
            .unwrap_or(Value::Null);
        let op = meta["operation"].as_str().unwrap_or("unknown");
        let certainty = edge.assertion.certainty.as_str().to_string();
        measured += 1;
        if edge.assertion.certainty != EdgeCertainty::Candidate
            && score.get(edge.from.as_str()).copied().unwrap_or(0.0) >= 0.999
        {
            confirmed += 1;
        }
        tables.push(TableEffect {
            table: node.label.clone(),
            operation: op.into(),
            columns: meta["columns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            certainty,
            confidence: score.get(edge.from.as_str()).copied().unwrap_or(0.0)
                * edge.assertion.confidence.get(),
            source: edge.assertion.source_file.clone(),
            evidence: edge.assertion.evidence_json.clone(),
        });
    }
    tables.sort_by(|a, b| {
        (&a.table, &a.operation, &a.source).cmp(&(&b.table, &b.operation, &b.source))
    });
    let mut external_calls = Vec::new();
    let mut events = Vec::new();
    for node in &trace.nodes {
        if node.kind != "external_effect" {
            continue;
        }
        let meta: Value = node
            .metadata_json
            .as_deref()
            .and_then(|m| serde_json::from_str(m).ok())
            .unwrap_or(Value::Null);
        let item = ExternalCall {
            id: node.id.clone(),
            name: node.label.clone(),
            effect: meta["effect"].as_str().unwrap_or("unknown").into(),
            source: node.path.clone(),
            evidence: node.metadata_json.clone(),
        };
        measured += 1;
        if score.get(node.id.as_str()).copied().unwrap_or(0.0) >= 0.999 {
            confirmed += 1;
        }
        if item.effect == "event" {
            events.push(item);
        } else {
            external_calls.push(item);
        }
    }
    let unresolved: Vec<_> = trace
        .java_analysis
        .calls
        .iter()
        .filter(|c| c.resolution == "unresolved")
        .collect();
    let breakpoints = unresolved
        .iter()
        .take(30)
        .map(|c| UnresolvedCall {
            path: c.path.clone(),
            line: c.line,
            expression: c.expression.chars().take(160).collect(),
            reason: c.reason.clone(),
        })
        .collect();
    EffectsCard {
        entry: trace.query.clone(),
        seeds: trace.seeds.clone(),
        tables,
        external_calls,
        events,
        transactions,
        unresolved_count: unresolved.len(),
        breakpoints,
        breakpoints_truncated: unresolved.len() > 30,
        confirmed_effect_ratio: if measured == 0 {
            0.0
        } else {
            confirmed as f32 / measured as f32
        },
        truncated: trace.truncated,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WriterEntry {
    pub id: String,
    pub kind: String,
    pub path: Option<String>,
    pub certainty: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WritersResult {
    pub table: String,
    pub write_edges: usize,
    pub entries: Vec<WriterEntry>,
    pub truncated: bool,
}

pub fn run_writers(root: &Path, table: &str) -> crate::error::EngineResult<WritersResult> {
    let config = crate::config::load_config(root)?;
    let path = crate::config::resolve_storage_path(root, &config)?;
    let store = Store::open(&path)?;
    Ok(writers_with_store(&store, table)?)
}

pub fn writers_with_store(store: &Store, table: &str) -> anyhow::Result<WritersResult> {
    let table_ids: Vec<_> = store
        .list_nodes_by_kind(NodeKind::DbTable)?
        .into_iter()
        .filter(|n| {
            n.name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(table))
        })
        .map(|n| n.id)
        .collect();
    let mut starts = Vec::new();
    for id in table_ids {
        for edge in store.list_edges_to(&id)? {
            if edge.kind != EdgeKind::PersistsTo {
                continue;
            }
            let meta: Value = edge
                .metadata_json
                .as_deref()
                .and_then(|m| serde_json::from_str(m).ok())
                .unwrap_or(Value::Null);
            if matches!(
                meta["operation"].as_str(),
                Some("insert" | "update" | "upsert" | "delete")
            ) {
                starts.push((edge.from_id, edge.certainty));
            }
        }
    }
    let write_edges = starts.len();
    let mut entries = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut queue: VecDeque<_> = starts.into_iter().collect();
    let mut truncated = false;
    while let Some((id, certainty)) = queue.pop_front() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if seen.len() > 20_000 {
            truncated = true;
            break;
        }
        let Some(node) = store.find_node(&id)? else {
            continue;
        };
        let kind = writer_entry_kind(&node);
        if let Some(kind) = kind {
            entries.insert(
                id.to_string(),
                WriterEntry {
                    id: id.to_string(),
                    kind: kind.into(),
                    path: node.path.clone(),
                    certainty: certainty.as_str().into(),
                },
            );
            continue;
        }
        let incoming: Vec<_> = store
            .list_edges_to(&id)?
            .into_iter()
            .filter(|e| {
                matches!(
                    e.kind,
                    EdgeKind::Calls | EdgeKind::References | EdgeKind::DeclaresImplementation
                )
            })
            .collect();
        if incoming.is_empty() && node.kind.is_callable() {
            entries.insert(
                id.to_string(),
                WriterEntry {
                    id: id.to_string(),
                    kind: "method".into(),
                    path: node.path.clone(),
                    certainty: certainty.as_str().into(),
                },
            );
        } else {
            for edge in incoming {
                queue.push_back((
                    edge.from_id,
                    if edge.certainty == EdgeCertainty::Candidate {
                        EdgeCertainty::Candidate
                    } else {
                        certainty
                    },
                ));
            }
        }
    }
    Ok(WritersResult {
        table: table.into(),
        write_edges,
        entries: entries.into_values().collect(),
        truncated,
    })
}

fn writer_entry_kind(node: &groundgraph_core::Node) -> Option<&'static str> {
    if node.kind == NodeKind::HttpRoute {
        return Some("HTTP");
    }
    let meta: Value = serde_json::from_str(node.metadata_json.as_deref()?).ok()?;
    for entry in meta["java"]["framework"].as_array()? {
        if entry["role"] != "entrypoint" {
            continue;
        }
        let name = entry["annotation"].as_str().unwrap_or("");
        if name.ends_with("Scheduled") {
            return Some("scheduled");
        }
        if name.ends_with("EventListener") {
            return Some("listener");
        }
        if name.ends_with("KafkaListener")
            || name.ends_with("RabbitListener")
            || name.ends_with("JmsListener")
        {
            return Some("MQ");
        }
    }
    None
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TransactionAnalysis {
    pub groups: Vec<TransactionGroup>,
    pub node_groups: BTreeMap<String, Vec<String>>,
    pub risks: Vec<TransactionRisk>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TransactionGroup {
    pub id: String,
    pub owner: String,
    pub propagation: String,
    pub no_rollback_for: Option<String>,
    pub nodes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TransactionRisk {
    pub kind: String,
    pub at: String,
    pub detail: String,
}

fn framework(node: &TraceNode, role: &str) -> Option<Value> {
    let meta: Value = serde_json::from_str(node.metadata_json.as_deref()?).ok()?;
    let entries = meta["java"]["framework"].as_array()?;
    entries
        .iter()
        .filter(|entry| entry["role"] == role && entry["resolution"] != "unresolved")
        .min_by_key(|entry| entry["class_level"] == true)
        .cloned()
}

fn transaction(node: &TraceNode) -> Option<(String, Option<String>)> {
    let annotation = framework(node, "transaction")?;
    let args = annotation["args"].as_str().unwrap_or("");
    let propagation = [
        "REQUIRES_NEW",
        "MANDATORY",
        "NOT_SUPPORTED",
        "NEVER",
        "SUPPORTS",
        "NESTED",
    ]
    .into_iter()
    .find(|p| args.contains(p))
    .unwrap_or("REQUIRED")
    .to_string();
    let no_rollback_for = args
        .split("noRollbackFor")
        .nth(1)
        .and_then(|s| {
            s.split_once('=')
                .map(|(_, v)| v.trim().trim_end_matches(')').trim().to_string())
        })
        .filter(|s| !s.is_empty());
    Some((propagation, no_rollback_for))
}

fn same_class(from: &str, to: &str) -> bool {
    fn owner(id: &str) -> Option<&str> {
        let (prefix, _) = id.split('(').next()?.rsplit_once('.')?;
        Some(prefix)
    }
    owner(from).is_some_and(|class| Some(class) == owner(to))
}

/// Static groups are possible transaction contexts; runtime proxy wiring,
/// branch predicates and exception types still need execution evidence.
pub fn analyze_transactions(trace: &TraceResult) -> TransactionAnalysis {
    let nodes: BTreeMap<_, _> = trace.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let calls: BTreeMap<_, _> = trace
        .java_analysis
        .calls
        .iter()
        .map(|c| (c.id.as_str(), c))
        .collect();
    let mut outgoing: BTreeMap<&str, Vec<&crate::trace::TraceEdge>> = BTreeMap::new();
    for edge in &trace.edges {
        outgoing.entry(&edge.from).or_default().push(edge);
    }
    let mut groups = BTreeMap::<String, TransactionGroup>::new();
    let mut node_groups = BTreeMap::<String, BTreeSet<String>>::new();
    let mut risks = BTreeMap::<(String, String), TransactionRisk>::new();
    let mut queue = VecDeque::new();
    for seed in &trace.seeds {
        let group = nodes.get(seed.as_str()).and_then(|n| transaction(n)).map(
            |(propagation, no_rollback_for)| {
                let id = format!("tx::{seed}");
                groups
                    .entry(id.clone())
                    .or_insert_with(|| TransactionGroup {
                        id: id.clone(),
                        owner: seed.clone(),
                        propagation,
                        no_rollback_for,
                        nodes: Vec::new(),
                    });
                id
            },
        );
        queue.push_back((seed.as_str(), group));
    }
    let mut seen = BTreeSet::new();
    while let Some((id, active)) = queue.pop_front() {
        if !seen.insert((id.to_string(), active.clone())) {
            continue;
        }
        if let Some(group) = &active {
            node_groups
                .entry(id.to_string())
                .or_default()
                .insert(group.clone());
        }
        if let Some(node) = nodes.get(id) {
            if framework(node, "entrypoint").is_some_and(|f| {
                f["annotation"]
                    .as_str()
                    .unwrap_or("")
                    .ends_with("TransactionalEventListener")
            }) {
                risks
                    .entry(("transactional_event_listener".into(), id.into()))
                    .or_insert_with(|| TransactionRisk {
                        kind: "transactional_event_listener".into(),
                        at: id.into(),
                        detail: "event delivery follows transaction phase, not the caller stack"
                            .into(),
                    });
            }
        }
        for edge in outgoing.get(id).into_iter().flatten() {
            let to = edge.to.as_str();
            let Some(target) = nodes.get(to) else {
                continue;
            };
            let mut next = active.clone();
            if target.kind == "java_method" {
                let self_call = edge.kind == "calls" && same_class(id, to);
                if framework(target, "async").is_some() && !self_call {
                    risks
                        .entry(("async_boundary".into(), to.into()))
                        .or_insert_with(|| TransactionRisk {
                            kind: "async_boundary".into(),
                            at: to.into(),
                            detail: "@Async starts a separate execution context".into(),
                        });
                    next = None;
                }
                if let Some((propagation, no_rollback_for)) = transaction(target) {
                    if self_call {
                        risks
                            .entry(("self_invocation".into(), to.into()))
                            .or_insert_with(|| TransactionRisk {
                                kind: "self_invocation".into(),
                                at: to.into(),
                                detail: "same-class call bypasses Spring transaction proxy".into(),
                            });
                    } else if propagation == "REQUIRES_NEW"
                        || (next.is_none() && propagation == "REQUIRED")
                    {
                        let gid = format!("tx::{to}");
                        groups
                            .entry(gid.clone())
                            .or_insert_with(|| TransactionGroup {
                                id: gid.clone(),
                                owner: to.into(),
                                propagation,
                                no_rollback_for,
                                nodes: Vec::new(),
                            });
                        next = Some(gid);
                    } else if propagation == "MANDATORY" && next.is_none() {
                        risks
                            .entry(("mandatory_without_transaction".into(), to.into()))
                            .or_insert_with(|| TransactionRisk {
                                kind: "mandatory_without_transaction".into(),
                                at: to.into(),
                                detail: "MANDATORY requires an existing transaction".into(),
                            });
                    } else if propagation == "NOT_SUPPORTED" || propagation == "NEVER" {
                        next = None;
                    }
                }
            } else if target.kind == "external_effect" {
                let meta: Value = target
                    .metadata_json
                    .as_deref()
                    .and_then(|m| serde_json::from_str(m).ok())
                    .unwrap_or(Value::Null);
                if next.is_some() {
                    let kind = meta["effect"].as_str().unwrap_or("external");
                    risks
                        .entry(("external_in_transaction".into(), to.into()))
                        .or_insert_with(|| TransactionRisk {
                            kind: "external_in_transaction".into(),
                            at: to.into(),
                            detail: format!("{kind} effect may not roll back with the database"),
                        });
                }
                if meta["call_site"]
                    .as_str()
                    .and_then(|id| calls.get(id))
                    .is_some_and(|c| c.caught_without_rethrow)
                {
                    risks
                        .entry(("swallowed_external_failure".into(), to.into()))
                        .or_insert_with(|| TransactionRisk {
                            kind: "swallowed_external_failure".into(),
                            at: to.into(),
                            detail: "a catch block can absorb an error from this effect".into(),
                        });
                }
            }
            queue.push_back((to, next));
        }
    }
    for (id, members) in node_groups {
        for member in members {
            if let Some(group) = groups.get_mut(&member) {
                group.nodes.push(id.clone());
            }
        }
    }
    let node_groups = groups
        .values()
        .flat_map(|g| g.nodes.iter().map(|n| (n.clone(), g.id.clone())))
        .fold(BTreeMap::<String, Vec<String>>::new(), |mut map, (n, g)| {
            map.entry(n).or_default().push(g);
            map
        });
    TransactionAnalysis {
        groups: groups.into_values().collect(),
        node_groups,
        risks: risks.into_values().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::writers_with_store;
    use groundgraph_core::{
        ArtifactId, Confidence, EdgeAssertion, EdgeCertainty, EdgeKind, EdgeSource, EdgeStatus,
        Node, NodeKind,
    };
    use groundgraph_store::Store;
    use serde_json::json;

    #[test]
    fn writers_upsert_is_candidate_and_read_or_missing_operations_are_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        store.migrate().unwrap();
        let mut table = Node::new(ArtifactId::new("table"), NodeKind::DbTable);
        table.name = Some("rows".into());
        store.upsert_node(&table).unwrap();
        for (id, operation) in [
            ("write", Some("upsert")),
            ("read", Some("read")),
            ("unknown", None),
        ] {
            let node = Node::new(ArtifactId::new(id), NodeKind::JavaMethod);
            store.upsert_node(&node).unwrap();
            let mut edge = EdgeAssertion::fact(
                node.id,
                table.id.clone(),
                EdgeKind::PersistsTo,
                EdgeSource::LanguageAdapter,
            );
            edge.certainty = EdgeCertainty::Candidate;
            edge.status = EdgeStatus::Proposed;
            edge.confidence = Confidence::new(0.6);
            edge.metadata_json = operation.map(|op| json!({"operation":op}).to_string());
            store.upsert_edge(&edge).unwrap();
        }
        let result = writers_with_store(&store, "rows").unwrap();
        assert_eq!(result.write_edges, 1);
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].id, "write");
        assert_eq!(result.entries[0].certainty, "candidate");
        assert!(!result.truncated);
    }
}
