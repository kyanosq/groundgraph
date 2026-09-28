//! Static transaction projection over the authoritative trace graph.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;
use serde_json::Value;

use crate::trace::{TraceNode, TraceResult};

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
