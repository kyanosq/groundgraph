//! Opt-in string-id dispatch projection. Registration scopes are references, not execution proof.
use crate::java_semantics::{CallSite, JavaAnalysis};
use anyhow::{ensure, Result};
use groundgraph_core::{
    ArtifactId, EdgeAssertion, EdgeCertainty, EdgeKind, EdgeSource, EdgeStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchContract {
    pub name: String,
    pub dispatcher_methods: Vec<String>,
    pub registration_methods: Vec<String>,
    pub handler_type: String,
    #[serde(default)]
    pub registration_annotation: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchHint {
    pub method: String,
    pub id: Option<String>,
    pub returned: bool,
    #[serde(default)]
    pub return_type: Option<String>,
    #[serde(default)]
    pub annotations: Vec<String>,
}

pub(crate) fn validate(contracts: &[DispatchContract]) -> Result<()> {
    let mut names = BTreeSet::new();
    for contract in contracts {
        ensure!(
            !contract.name.is_empty() && names.insert(&contract.name),
            "dispatch contract name empty or duplicated"
        );
        ensure!(
            !contract.dispatcher_methods.is_empty()
                && !contract.registration_methods.is_empty()
                && !contract.handler_type.is_empty(),
            "dispatch contract requires explicit methods and handler type"
        );
    }
    Ok(())
}

pub(crate) fn project(
    contracts: &[DispatchContract],
    analyses: &mut BTreeMap<String, JavaAnalysis>,
) -> Result<Vec<EdgeAssertion>> {
    let sites: Vec<CallSite> = analyses
        .values()
        .flat_map(|a| a.calls.iter().cloned())
        .collect();
    let mut edges = Vec::new();
    validate(contracts)?;
    for contract in contracts {
        let mut registrations = BTreeMap::<String, Vec<&CallSite>>::new();
        for site in &sites {
            let Some(hint) = site.dispatch_hint.as_ref() else {
                continue;
            };
            if site.resolution != "resolved"
                || !contract.registration_methods.contains(&hint.method)
            {
                continue;
            }
            if !hint.returned
                || hint.return_type.as_deref() != Some(&contract.handler_type)
                || contract
                    .registration_annotation
                    .as_ref()
                    .is_some_and(|a| !hint.annotations.contains(a))
            {
                continue;
            }
            if let Some(id) = hint.id.as_ref().filter(|s| !s.is_empty()) {
                registrations.entry(id.clone()).or_default().push(site);
            } else if let Some(a) = analyses.get_mut(&site.path) {
                a.diagnostics.push(format!(
                    "{}:{} dynamic_registration_id [{}]",
                    site.path, site.line, contract.name
                ));
            }
        }
        for site in &sites {
            let Some(hint) = site.dispatch_hint.as_ref() else {
                continue;
            };
            if site.resolution != "resolved" || !contract.dispatcher_methods.contains(&hint.method)
            {
                continue;
            }
            let Some(id) = hint.id.as_ref().filter(|s| !s.is_empty()) else {
                if let Some(a) = analyses.get_mut(&site.path) {
                    a.diagnostics.push(format!(
                        "{}:{} dynamic_dispatch_id [{}]",
                        site.path, site.line, contract.name
                    ));
                }
                continue;
            };
            let targets = registrations.get(id).cloned().unwrap_or_default();
            if targets.len() != 1 {
                let reason = if targets.is_empty() {
                    "dispatch_registration_missing"
                } else {
                    "ambiguous_dispatch_registration"
                };
                if let Some(a) = analyses.get_mut(&site.path) {
                    a.diagnostics.push(format!(
                        "{}:{} {reason} [{}] {id}",
                        site.path, site.line, contract.name
                    ));
                }
            }
            for target in &targets {
                let mut edge = EdgeAssertion::with_confidence(
                    ArtifactId::new(&site.caller),
                    ArtifactId::new(&target.caller),
                    EdgeKind::References,
                    EdgeSource::LanguageAdapter,
                    0.75,
                );
                edge.id = ArtifactId::new(format!(
                    "java_dispatch::{}::{}::{}",
                    contract.name, site.id, target.id
                ));
                edge.certainty = EdgeCertainty::Candidate;
                edge.status = EdgeStatus::Proposed;
                edge.indexer = Some("java_dispatch".into());
                edge.source_file = Some(site.path.clone());
                edge.evidence_json=Some(json!({"resolver":"configured_string_id_dispatch","contract":contract.name,"capability_id":id,"path":site.path,"line":site.line,"call_site":site.id,"snippet":site.expression,"registration":{"path":target.path,"line":target.line,"call_site":target.id,"snippet":target.expression},"registration_count":targets.len(),"target_is_registration_scope":true,"runtime_binding_not_resolved":true,"resolution":"candidate"}).to_string());
                edges.push(edge);
            }
        }
    }
    Ok(edges)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_configuration_adds_nothing() {
        assert!(project(&[], &mut BTreeMap::new()).unwrap().is_empty());
    }
    #[test]
    fn invalid_contract_fails_instead_of_guessing() {
        let c = DispatchContract {
            name: "".into(),
            dispatcher_methods: vec![],
            registration_methods: vec![],
            handler_type: "".into(),
            registration_annotation: None,
        };
        assert!(project(&[c], &mut BTreeMap::new()).is_err());
    }
}
