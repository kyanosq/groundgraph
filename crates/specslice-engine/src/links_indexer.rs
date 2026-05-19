//! External links manifest indexer.
//!
//! SpecSlice is non-invasive: requirement-to-code/test relationships live in
//! `.specslice/links.yaml`, not in business code comments or product docs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use specslice_core::artifact_id::{
    dart_class_id, dart_constructor_id, dart_function_id, dart_group_id, dart_method_id,
    dart_test_id, doc_section_id, file_id, requirement_id, slugify,
};
use specslice_core::{ArtifactId, EdgeAssertion, EdgeKind, EdgeSource, Node, NodeKind};
use specslice_store::Store;

pub const LINKS_INDEXER_NAME: &str = "links_manifest";

#[derive(Debug, Clone)]
pub struct LinksIndexOptions {
    pub repo_root: PathBuf,
    pub manifest_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LinksIndexResult {
    pub requirements: usize,
    pub docs: usize,
    pub implementations: usize,
    pub tests: usize,
    pub edges: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
struct LinksManifest {
    #[serde(default)]
    requirements: BTreeMap<String, RequirementLinks>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
struct RequirementLinks {
    #[serde(default)]
    docs: Vec<String>,
    #[serde(default, alias = "implementation")]
    implementations: Vec<String>,
    #[serde(default)]
    tests: Vec<String>,
}

pub fn index_links(store: &mut Store, options: &LinksIndexOptions) -> Result<LinksIndexResult> {
    let path = resolve_manifest_path(&options.repo_root, &options.manifest_path);
    if !path.exists() {
        return Ok(LinksIndexResult::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading links manifest {}", path.display()))?;
    let manifest: LinksManifest = serde_yaml::from_str(&raw)
        .with_context(|| format!("parsing links manifest {}", path.display()))?;
    let rel_manifest = path
        .strip_prefix(&options.repo_root)
        .unwrap_or(&path)
        .to_string_lossy()
        .replace('\\', "/");

    let mut result = LinksIndexResult::default();
    for (req_key, links) in manifest.requirements {
        let req_id = requirement_id(&req_key);
        if store.find_node(&req_id)?.is_none() {
            let mut req = Node::new(req_id.clone(), NodeKind::Requirement);
            req.stable_key = Some(req_key.clone());
            req.source_file = Some(rel_manifest.clone());
            req.indexer = Some(LINKS_INDEXER_NAME.into());
            store.upsert_node(&req)?;
        }
        result.requirements += 1;

        for spec in links.docs {
            let from_id = resolve_doc_ref(store, &spec)?;
            upsert_link_edge(
                store,
                from_id,
                req_id.clone(),
                EdgeKind::Documents,
                &rel_manifest,
                &mut result,
            )?;
            result.docs += 1;
        }
        for spec in links.implementations {
            let from_id = resolve_implementation_ref(store, &spec)?;
            upsert_link_edge(
                store,
                from_id,
                req_id.clone(),
                EdgeKind::DeclaresImplementation,
                &rel_manifest,
                &mut result,
            )?;
            result.implementations += 1;
        }
        for spec in links.tests {
            let from_id = resolve_test_ref(store, &spec)?;
            upsert_link_edge(
                store,
                from_id,
                req_id.clone(),
                EdgeKind::DeclaresVerification,
                &rel_manifest,
                &mut result,
            )?;
            result.tests += 1;
        }
    }
    Ok(result)
}

fn upsert_link_edge(
    store: &mut Store,
    from_id: ArtifactId,
    to_id: ArtifactId,
    kind: EdgeKind,
    source_file: &str,
    result: &mut LinksIndexResult,
) -> Result<()> {
    let mut edge = EdgeAssertion::declared(from_id, to_id, kind, EdgeSource::ExternalManifest);
    edge.indexer = Some(LINKS_INDEXER_NAME.into());
    edge.source_file = Some(source_file.to_string());
    store.upsert_edge(&edge)?;
    result.edges += 1;
    Ok(())
}

fn resolve_manifest_path(repo_root: &Path, manifest_path: &Path) -> PathBuf {
    if manifest_path.is_absolute() {
        manifest_path.to_path_buf()
    } else {
        repo_root.join(manifest_path)
    }
}

fn resolve_doc_ref(store: &Store, spec: &str) -> Result<ArtifactId> {
    if let Some(id) = strict_resolve_doc(store, spec)? {
        return Ok(id);
    }
    let (path, fragment) = split_ref(spec);
    if let Some(fragment) = fragment {
        Ok(doc_section_id(path, &slugify_or_keep(fragment)))
    } else {
        Ok(file_id(path))
    }
}

fn resolve_implementation_ref(store: &Store, spec: &str) -> Result<ArtifactId> {
    if let Some(id) = strict_resolve_implementation(store, spec)? {
        return Ok(id);
    }
    let (path, fragment) = split_ref(spec);
    let Some(fragment) = fragment else {
        return Ok(file_id(path));
    };
    if let Some((class, member)) = fragment.split_once('.') {
        Ok(dart_method_id(path, class, member))
    } else {
        Ok(dart_class_id(path, fragment))
    }
}

fn resolve_test_ref(store: &Store, spec: &str) -> Result<ArtifactId> {
    if let Some(id) = strict_resolve_test(store, spec)? {
        return Ok(id);
    }
    let (path, fragment) = split_ref(spec);
    let Some(fragment) = fragment else {
        return Ok(file_id(path));
    };
    Ok(dart_test_id(path, &slugify_or_keep(fragment)))
}

/// Resolve a doc reference strictly: returns `Some(id)` only if a matching
/// node already exists in the store. Used by `connect::apply` to reject
/// candidates whose targets we cannot locate.
pub(crate) fn strict_resolve_doc(store: &Store, spec: &str) -> Result<Option<ArtifactId>> {
    let (path, fragment) = split_ref(spec);
    let Some(fragment) = fragment else {
        let id = file_id(path);
        return Ok(if store.find_node(&id)?.is_some() {
            Some(id)
        } else {
            None
        });
    };
    let slug = slugify_or_keep(fragment);
    let id = doc_section_id(path, &slug);
    if store.find_node(&id)?.is_some() {
        return Ok(Some(id));
    }
    find_node_by_path_and_name(store, &[NodeKind::DocSection], path, fragment)
}

/// Strict implementation resolver used by `connect::apply`.
pub(crate) fn strict_resolve_implementation(
    store: &Store,
    spec: &str,
) -> Result<Option<ArtifactId>> {
    let (path, fragment) = split_ref(spec);
    let Some(fragment) = fragment else {
        let id = file_id(path);
        return Ok(if store.find_node(&id)?.is_some() {
            Some(id)
        } else {
            None
        });
    };
    if let Some(id) = find_node_by_path_and_name(
        store,
        &[
            NodeKind::DartClass,
            NodeKind::DartMethod,
            NodeKind::DartFunction,
            NodeKind::DartConstructor,
        ],
        path,
        fragment,
    )? {
        return Ok(Some(id));
    }
    if let Some((class, member)) = fragment.split_once('.') {
        let method_id = dart_method_id(path, class, member);
        if store.find_node(&method_id)?.is_some() {
            return Ok(Some(method_id));
        }
        let ctor_id = dart_constructor_id(path, class, member);
        if store.find_node(&ctor_id)?.is_some() {
            return Ok(Some(ctor_id));
        }
    } else {
        let class_id = dart_class_id(path, fragment);
        if store.find_node(&class_id)?.is_some() {
            return Ok(Some(class_id));
        }
        let fn_id = dart_function_id(path, fragment);
        if store.find_node(&fn_id)?.is_some() {
            return Ok(Some(fn_id));
        }
    }
    Ok(None)
}

/// Strict test resolver used by `connect::apply`.
pub(crate) fn strict_resolve_test(store: &Store, spec: &str) -> Result<Option<ArtifactId>> {
    let (path, fragment) = split_ref(spec);
    let Some(fragment) = fragment else {
        let id = file_id(path);
        return Ok(if store.find_node(&id)?.is_some() {
            Some(id)
        } else {
            None
        });
    };
    if let Some(id) = find_node_by_path_and_name(
        store,
        &[NodeKind::TestCase, NodeKind::TestGroup],
        path,
        fragment,
    )? {
        return Ok(Some(id));
    }
    let slug = slugify_or_keep(fragment);
    let test_id = dart_test_id(path, &slug);
    if store.find_node(&test_id)?.is_some() {
        return Ok(Some(test_id));
    }
    let group_id = dart_group_id(path, &slug);
    if store.find_node(&group_id)?.is_some() {
        return Ok(Some(group_id));
    }
    Ok(None)
}

fn find_node_by_path_and_name(
    store: &Store,
    kinds: &[NodeKind],
    path: &str,
    name: &str,
) -> Result<Option<ArtifactId>> {
    for kind in kinds {
        for node in store.list_nodes_by_kind(*kind)? {
            if node.path.as_deref() != Some(path) {
                continue;
            }
            let fragment_slug = slugify(name);
            if node.name.as_deref() == Some(name)
                || node.stable_key.as_deref() == Some(name)
                || node.name.as_deref().map(slugify).as_deref() == Some(fragment_slug.as_str())
            {
                return Ok(Some(node.id));
            }
        }
    }
    Ok(None)
}

fn split_ref(spec: &str) -> (&str, Option<&str>) {
    match spec.split_once('#') {
        Some((path, fragment)) if !fragment.is_empty() => (path, Some(fragment)),
        Some((path, _)) => (path, None),
        None => (spec, None),
    }
}

fn slugify_or_keep(value: &str) -> String {
    let slug = slugify(value);
    if slug == "section" && value != "section" {
        value.to_string()
    } else {
        slug
    }
}
