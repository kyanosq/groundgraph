//! Java source inventory plus optional javac bindings. No build or source execution.
//! File metadata owns call records; graph edges and exports are projections of it.
use anyhow::{Context, Result};
use groundgraph_core::{
    ArtifactId, EdgeAssertion, EdgeCertainty, EdgeKind, EdgeSource, EdgeStatus, Node, NodeKind,
};
use groundgraph_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JavaSemanticsConfig {
    pub enabled: bool,
    pub java_command: String,
    pub classpath: Vec<String>,
    /// Annotation processors to run (e.g. the Lombok jar). Empty = none run.
    /// Only these jars are searched, never the classpath; generated output
    /// goes to a temp dir, never the repository.
    pub annotation_processor_path: Vec<String>,
    pub timeout_seconds: u64,
    /// Service identity is deployment knowledge, not inferred from package names.
    pub services: BTreeMap<String, Vec<String>>,
    /// Separate compilations with explicit dependency visibility (e.g. Maven modules).
    pub source_sets: BTreeMap<String, JavaSourceSet>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JavaSourceSet {
    pub roots: Vec<String>,
    pub dependencies: Vec<String>,
    pub classpath: Vec<String>,
}
impl Default for JavaSemanticsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            java_command: "java".into(),
            classpath: vec![],
            annotation_processor_path: vec![],
            timeout_seconds: 120,
            services: BTreeMap::new(),
            source_sets: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallSite {
    pub id: String,
    pub path: String,
    pub line: u32,
    pub column: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub caller: String,
    pub expression: String,
    pub kind: String,
    pub resolution: String,
    pub reason: String,
    pub target: Option<String>,
    pub external_target: Option<String>,
    /// Receiver's type arguments on the generic type declaring an inherited
    /// method (e.g. `BaseMapper<Order>.insert` → `[p.Order]`), with the source
    /// path when the argument is a project type. Lets framework conventions map
    /// inherited CRUD to its entity without guessing from names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_type_args: Vec<OwnerTypeArg>,
    /// Compiler evidence for an explicit Spring Data JPA receiver and its
    /// exact source entity mapping. This does not confirm runtime persistence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jpa_repository: Option<JpaRepositoryBinding>,
    #[serde(default)]
    pub caught_without_rethrow: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerTypeArg {
    pub name: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JpaRepositoryBinding {
    pub repository_type: String,
    pub entity: String,
    pub entity_path: String,
    pub entity_line: u32,
    pub table: String,
    pub entity_annotation: String,
    pub table_annotation: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JavaAnalysis {
    pub calls: Vec<CallSite>,
    pub diagnostics: Vec<String>,
    pub compiler: String,
}

/// Extract immutable input call sites before any compiler recovery/generation.
fn inventory(path: &str, source: &str, nodes: &mut [Node]) -> Result<JavaAnalysis> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_java::LANGUAGE.into())?;
    let tree = parser
        .parse(source, None)
        .context("Java inventory parse cancelled")?;
    let mut analysis = JavaAnalysis::default();
    if tree.root_node().has_error() {
        analysis.diagnostics.push(format!(
            "{path}: syntax_errors; inventory may be incomplete"
        ));
    }
    let mut declarations = vec![tree.root_node()];
    while let Some(n) = declarations.pop() {
        if n.kind() == "method_declaration" {
            if let Some(meta) = crate::java_treesitter::java_metadata(n, source.as_bytes()) {
                let v: Value = serde_json::from_str(&meta)?;
                let id = format!(
                    "java::{path}::{}",
                    v["java"]["local_name"].as_str().unwrap_or("")
                );
                if let Some(test) = nodes
                    .iter_mut()
                    .find(|node| node.kind == NodeKind::TestCase && node.id.as_str() == id)
                {
                    test.metadata_json = Some(meta);
                }
            }
        }
        let mut c = n.walk();
        declarations.extend(n.named_children(&mut c));
    }
    let ranges: Vec<_> = nodes
        .iter()
        .filter(|n| n.path.as_deref() == Some(path))
        .filter_map(|n| {
            let meta: Value = serde_json::from_str(n.metadata_json.as_deref()?).ok()?;
            Some((
                usize::try_from(meta["java"]["start_byte"].as_u64()?).ok()?,
                usize::try_from(meta["java"]["end_byte"].as_u64()?).ok()?,
                n.id.to_string(),
            ))
        })
        .collect();
    let mut pending = vec![tree.root_node()];
    while let Some(n) = pending.pop() {
        if matches!(
            n.kind(),
            "method_invocation"
                | "object_creation_expression"
                | "explicit_constructor_invocation"
                | "method_reference"
        ) {
            let caller = ranges
                .iter()
                .filter(|(start, end, _)| *start <= n.start_byte() && *end >= n.end_byte())
                .min_by_key(|(start, end, _)| end - start)
                .map(|(_, _, id)| id.clone())
                .unwrap_or_else(|| groundgraph_core::artifact_id::file_id(path).to_string());
            analysis.calls.push(CallSite {
                id: format!("java_call::{path}::{}:{}", n.start_byte(), n.end_byte()),
                path: path.into(),
                line: u32::try_from(n.start_position().row)?.saturating_add(1),
                column: n.start_position().column + 1,
                start_byte: n.start_byte(),
                end_byte: n.end_byte(),
                caller,
                expression: source[n.byte_range()].into(),
                kind: n.kind().into(),
                resolution: "unresolved".into(),
                reason: "compiler_binding_unavailable".into(),
                target: None,
                external_target: None,
                owner_type_args: Vec::new(),
                jpa_repository: None,
                caught_without_rethrow: false,
            });
        }
        let mut c = n.walk();
        pending.extend(n.named_children(&mut c));
    }
    analysis.calls.sort_by_key(|c| (c.start_byte, c.end_byte));
    Ok(analysis)
}

fn compiler(root: &Path, files: &[String], config: &JavaSemanticsConfig) -> Result<Vec<Value>> {
    anyhow::ensure!(
        config.timeout_seconds > 0,
        "java_semantics.timeout_seconds must be positive"
    );
    let temp = tempfile::tempdir()?;
    let helper = temp.path().join("GroundGraphJava.java");
    std::fs::write(&helper, include_str!("java/GroundGraphJava.java"))?;
    let file_list = temp.path().join("sources.txt");
    anyhow::ensure!(
        files.iter().all(|f| !f.contains(['\n', '\r'])),
        "Java source paths cannot contain newlines"
    );
    std::fs::write(&file_list, files.join("\n"))?;
    let output = temp.path().join("bindings.jsonl");
    let cp = std::env::join_paths(config.classpath.iter().map(|p| root.join(p)))?;
    let processors = std::env::join_paths(
        config
            .annotation_processor_path
            .iter()
            .map(|p| root.join(p)),
    )?;
    let generated = temp.path().join("generated");
    std::fs::create_dir(&generated)?;
    let mut cmd = std::process::Command::new(&config.java_command);
    cmd.current_dir(root)
        .arg("-Xmx1024m")
        .arg(&helper)
        .arg(root)
        .arg(&file_list)
        .arg(&output)
        .arg(cp)
        .arg(processors)
        .arg(&generated);
    // Reuse the existing bounded process-group runner rather than another launcher.
    let (status, stderr) = crate::scip_runner::run_with_capped_stderr_budget(
        &mut cmd,
        std::time::Duration::from_secs(config.timeout_seconds),
    )?;
    anyhow::ensure!(
        status.success(),
        "javac helper failed: {}",
        String::from_utf8_lossy(&stderr)
    );
    let text = std::fs::read_to_string(output)?;
    let records: Vec<Value> = text
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    anyhow::ensure!(
        records.last().is_some_and(|r| r["kind"] == "summary"),
        "incomplete javac output"
    );
    Ok(records)
}

fn within(path: &str, prefix: &str) -> bool {
    prefix == "."
        || path == prefix
        || path.starts_with(&format!("{}/", prefix.trim_end_matches('/')))
}

fn compile_source_sets(
    root: &Path,
    files: &[String],
    config: &JavaSemanticsConfig,
) -> Result<Vec<Value>> {
    if config.source_sets.is_empty() {
        // Do not give unrelated Maven/Gradle modules implicit visibility.
        let mut modules = BTreeSet::new();
        for file in files {
            let mut dir = root.join(file).parent().unwrap_or(root).to_path_buf();
            while dir.starts_with(root) {
                if ["pom.xml", "build.gradle", "build.gradle.kts"]
                    .iter()
                    .any(|f| dir.join(f).is_file())
                {
                    modules.insert(dir.clone());
                    break;
                }
                if !dir.pop() {
                    break;
                }
            }
        }
        anyhow::ensure!(modules.len() <= 1, "multiple build modules: configure java_semantics.source_sets and dependencies; source visibility must not be guessed");
        return compiler(root, files, config);
    }
    let mut ownership = BTreeMap::new();
    for file in files {
        let owners: Vec<_> = config
            .source_sets
            .iter()
            .filter(|(_, s)| s.roots.iter().any(|r| within(file, r)))
            .map(|(id, _)| id.clone())
            .collect();
        anyhow::ensure!(
            owners.len() == 1,
            "Java source must belong to exactly one source_set: {file}"
        );
        ownership.insert(file.clone(), owners[0].clone());
    }
    let mut records = Vec::new();
    for name in config.source_sets.keys() {
        let mut visible = BTreeSet::new();
        let mut pending = vec![name.clone()];
        while let Some(id) = pending.pop() {
            if !visible.insert(id.clone()) {
                continue;
            }
            let dep = config
                .source_sets
                .get(&id)
                .with_context(|| format!("unknown Java source_set dependency: {id}"))?;
            pending.extend(dep.dependencies.iter().cloned());
        }
        let inputs: Vec<_> = files
            .iter()
            .filter(|f| visible.contains(&ownership[*f]))
            .cloned()
            .collect();
        if inputs.is_empty() {
            continue;
        }
        let mut cfg = config.clone();
        cfg.classpath.extend(
            visible
                .iter()
                .flat_map(|id| config.source_sets[id].classpath.iter().cloned()),
        );
        cfg.classpath.sort();
        cfg.classpath.dedup();
        let group_records = match compiler(root, &inputs, &cfg) {
            Ok(records) => records,
            Err(e) => {
                for (path, owner) in &ownership {
                    if owner == name {
                        records.push(json!({"kind":"compiler_failure", "path":path, "message":format!("compiler_unavailable in {name}: {e:#}")}));
                    }
                }
                continue;
            }
        };
        for record in group_records {
            // Dependencies participate in attribution, but their records belong to their own compilation.
            let path = record["path"].as_str().or(record["target_path"].as_str());
            if path.is_none_or(|p| ownership.get(p) == Some(name)) {
                records.push(record);
            }
        }
    }
    Ok(records)
}

pub(crate) fn index_java_calls(store: &mut Store, root: &Path, files: &[String]) -> Result<usize> {
    let root = root.canonicalize()?;
    let config = match crate::config::load_config(&root) {
        Ok(c) => c.java_semantics,
        Err(crate::error::EngineError::NoWorkspace { .. }) => JavaSemanticsConfig::default(),
        Err(e) => return Err(e.into()),
    };
    let mut nodes_by_file = BTreeMap::<String, Vec<Node>>::new();
    for node in store.list_all_nodes()? {
        if let Some(path) = &node.path {
            nodes_by_file.entry(path.clone()).or_default().push(node);
        }
    }
    let mut analyses = BTreeMap::new();
    let mut eligible = Vec::new();
    for path in files.iter().filter(|p| p.ends_with(".java")) {
        let source_path = crate::source_text::resolve_source_path(&root, path)?;
        if crate::source_text::is_oversized_source(&source_path) {
            continue;
        }
        let source = std::fs::read_to_string(&source_path)
            .with_context(|| format!("reading Java inventory {path}"))?;
        analyses.insert(
            path.clone(),
            inventory(
                path,
                &source,
                nodes_by_file.entry(path.clone()).or_default(),
            )?,
        );
        eligible.push(path.clone());
    }
    eligible.sort();
    eligible.dedup();
    let nodes: Vec<Node> = nodes_by_file.into_values().flatten().collect();
    for node in nodes
        .iter()
        .filter(|n| n.kind == NodeKind::TestCase && n.id.as_str().starts_with("java::"))
    {
        store.upsert_node(node)?;
    }
    let mut records = Vec::new();
    if !eligible.is_empty() && config.enabled {
        match compile_source_sets(&root, &eligible, &config) {
            Ok(r) => {
                records = r;
                for a in analyses.values_mut() {
                    a.compiler = "javac".into();
                }
            }
            Err(e) => {
                let message = format!("compiler_unavailable: {e:#}");
                tracing::warn!("{message}");
                for a in analyses.values_mut() {
                    a.compiler = "unavailable".into();
                    a.diagnostics.push(message.clone());
                }
            }
        }
    } else {
        for a in analyses.values_mut() {
            a.compiler = "disabled".into();
            a.diagnostics
                .push("compiler_disabled; calls remain unresolved".into());
        }
    }
    let mut positions = BTreeMap::<(String, usize), Vec<String>>::new();
    for n in &nodes {
        // Synthetic javac constructors can reuse the class declaration offset.
        // Only actual source callables may be a bound invocation target.
        if !matches!(
            n.kind,
            NodeKind::JavaMethod | NodeKind::JavaConstructor | NodeKind::TestCase
        ) {
            continue;
        }
        if let (Some(path), Some(meta)) = (&n.path, &n.metadata_json) {
            let v: Value = serde_json::from_str(meta)?;
            if let Some(start) = v["java"]["start_byte"].as_u64() {
                positions
                    .entry((path.clone(), usize::try_from(start)?))
                    .or_default()
                    .push(n.id.to_string());
            }
        }
    }
    let target_at = |r: &Value, prefix: &str| -> Option<String> {
        let key = (
            r[format!("{prefix}path")].as_str()?.to_string(),
            usize::try_from(r[format!("{prefix}start")].as_u64()?).ok()?,
        );
        let ids = positions.get(&key)?;
        (ids.len() == 1).then(|| ids[0].clone())
    };
    let mut framework = BTreeMap::<String, Vec<Value>>::new();
    for r in &records {
        match r["kind"].as_str() {
            Some("framework") => {
                if let Some(id) = target_at(r, "") {
                    framework.entry(id).or_default().push(r.clone());
                } else if let Some(a) = r["path"].as_str().and_then(|p| analyses.get_mut(p)) {
                    a.diagnostics
                        .push(format!("framework_declaration_not_indexed: {r}"));
                }
                if r["resolution"] == "unresolved" {
                    if let Some(a) = r["path"].as_str().and_then(|p| analyses.get_mut(p)) {
                        a.diagnostics.push(format!("framework_unresolved: {r}"));
                    }
                }
            }
            Some("call") => {
                let Some(a) = r["path"].as_str().and_then(|p| analyses.get_mut(p)) else {
                    continue;
                };
                let (Some(start), Some(end)) = (r["start"].as_u64(), r["end"].as_u64()) else {
                    continue;
                };
                // Inventory is sorted by source interval. Some generated source
                // files contain thousands of calls; a linear lookup is quadratic.
                let index = a
                    .calls
                    .binary_search_by_key(&(start, end), |c| {
                        (c.start_byte as u64, c.end_byte as u64)
                    })
                    .ok()
                    .or_else(|| {
                        // javac excludes the semicolon from explicit this()/super().
                        a.calls
                            .binary_search_by_key(&start, |c| c.start_byte as u64)
                            .ok()
                            .filter(|i| a.calls[*i].kind == "explicit_constructor_invocation")
                    });
                let Some(call) = index.map(|i| &mut a.calls[i]) else {
                    continue;
                };
                call.reason = r["reason"].as_str().unwrap_or("binding_missing").into();
                call.caught_without_rethrow = r["caught_without_rethrow"] == true;
                call.jpa_repository = r
                    .get("jpa_repository")
                    .filter(|value| !value.is_null())
                    .map(|value| serde_json::from_value(value.clone()))
                    .transpose()?;
                if r["resolved"] == true {
                    call.target = target_at(r, "target_");
                    call.external_target = r["symbol"].as_str().map(str::to_string);
                    call.owner_type_args = r["owner_type_args"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|a| {
                            Some(OwnerTypeArg {
                                name: a["name"].as_str()?.to_string(),
                                path: a["path"].as_str().map(str::to_string),
                            })
                        })
                        .collect();
                    call.resolution = "resolved".into();
                    if call.target.is_none() && r["target_path"].is_string() {
                        call.resolution = "unresolved".into();
                        call.reason = "source_declaration_not_indexed".into();
                    }
                } else if r["candidate"] == true {
                    call.target = target_at(r, "target_");
                    call.external_target = r["symbol"].as_str().map(str::to_string);
                    call.owner_type_args = r["owner_type_args"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|a| {
                            Some(OwnerTypeArg {
                                name: a["name"].as_str()?.to_string(),
                                path: a["path"].as_str().map(str::to_string),
                            })
                        })
                        .collect();
                    if call.target.is_some() || call.external_target.is_some() {
                        call.resolution = "candidate".into();
                    }
                }
            }
            Some("diagnostic" | "compiler_failure") => {
                let message = r["message"]
                    .as_str()
                    .unwrap_or("javac diagnostic")
                    .to_string();
                if let Some(a) = r["path"].as_str().and_then(|p| analyses.get_mut(p)) {
                    if r["kind"] == "compiler_failure" {
                        a.compiler = "unavailable".into();
                    }
                    a.diagnostics.push(message);
                } else {
                    for a in analyses.values_mut() {
                        a.diagnostics.push(message.clone());
                    }
                }
            }
            _ => {}
        }
    }
    for mut node in nodes {
        let Some(raw) = node.metadata_json.as_deref() else {
            continue;
        };
        let mut metadata: Value = serde_json::from_str(raw)?;
        let Some(java) = metadata.get_mut("java").and_then(Value::as_object_mut) else {
            continue;
        };
        let old = java.remove("framework");
        let entries = framework.remove(node.id.as_str());
        if old.is_none() && entries.is_none() {
            continue;
        }
        if let Some(entries) = entries {
            java.insert("framework".into(), json!(entries));
        }
        node.metadata_json = Some(metadata.to_string());
        store.upsert_node(&node)?;
    }
    store.clear_indexer_outputs("java_semantics")?;
    store.clear_indexer_outputs("java_feign")?;
    let mut edges = Vec::new();
    let mut effect_nodes = Vec::new();
    for a in analyses.values() {
        for call in &a.calls {
            if let Some((effect, client)) = call
                .external_target
                .as_deref()
                .and_then(external_call_effect)
            {
                let id = ArtifactId::new(format!("external_effect::{}", call.id));
                let mut node = Node::new(id.clone(), NodeKind::ExternalEffect);
                node.name = Some(format!(
                    "{client} {}",
                    call.external_target.as_deref().unwrap_or("")
                ));
                node.path = Some(call.path.clone());
                node.source_file = Some(call.path.clone());
                node.start_line = Some(call.line);
                node.indexer = Some("java_semantics".into());
                node.metadata_json = Some(json!({"effect":effect,"client":client,"call_site":call.id,"symbol":call.external_target,"resolution":call.resolution}).to_string());
                effect_nodes.push(node);
                let mut edge = candidate(
                    &call.caller,
                    id.as_str(),
                    EdgeKind::Calls,
                    "java_semantics",
                    &json!({"path":call.path,"line":call.line,"call_site":call.id,"resolver":"known_java_client","resolution":"candidate","static_target":call.external_target}),
                );
                edge.id = ArtifactId::new(format!("external_effect_edge::{}", call.id));
                edges.push(edge);
            }
            if let Some(target) = &call.target {
                let mut edge = EdgeAssertion::fact(
                    ArtifactId::new(&call.caller),
                    ArtifactId::new(target),
                    if call.kind == "method_reference" {
                        EdgeKind::References
                    } else {
                        EdgeKind::Calls
                    },
                    EdgeSource::LanguageAdapter,
                );
                // Call-site identity keeps repeated calls and provenance separate.
                edge.id = ArtifactId::new(format!("{}::{target}", call.id));
                if call.resolution == "candidate" {
                    edge.certainty = EdgeCertainty::Candidate;
                    edge.status = EdgeStatus::Proposed;
                    edge.confidence = groundgraph_core::Confidence::new(0.75);
                }
                edge.indexer = Some("java_semantics".into());
                edge.source_file = Some(call.path.clone());
                edge.evidence_json = Some(json!({"call_site":call.id,"line":call.line,"column":call.column,"resolver":"javac","resolution":call.resolution,"reason":call.reason,"static_target_only":true,"snippet":call.expression}).to_string());
                edges.push(edge);
            }
        }
    }
    for route in records
        .iter()
        .filter(|r| r["kind"] == "route" && r["role"] == "client")
    {
        if let Some(method) = target_at(route, "") {
            let id = ArtifactId::new(format!("external_effect::feign::{method}"));
            let mut node = Node::new(id.clone(), NodeKind::ExternalEffect);
            node.name = Some(format!(
                "Feign {} {}",
                route["verb"].as_str().unwrap_or("ANY"),
                route["route"].as_str().unwrap_or("<unresolved>")
            ));
            node.path = route["path"].as_str().map(str::to_string);
            node.source_file = node.path.clone();
            node.start_line = route["line"]
                .as_u64()
                .and_then(|line| u32::try_from(line).ok());
            node.indexer = Some("java_semantics".into());
            node.metadata_json = Some(json!({"effect":"http","client":"Feign","route":route["route"],"verb":route["verb"],"resolution":"candidate"}).to_string());
            effect_nodes.push(node);
            edges.push(candidate(&method, id.as_str(), EdgeKind::Calls, "java_semantics", &json!({"path":route["path"],"line":route["line"],"resolver":"feign_annotation","resolution":"candidate"})));
        }
    }
    store.upsert_nodes_bulk(&effect_nodes)?;
    for r in records.iter().filter(|r| r["kind"] == "override") {
        if let (Some(from), Some(to)) = (target_at(r, "base_"), target_at(r, "target_")) {
            let mut edge = candidate(
                &from,
                &to,
                EdgeKind::DeclaresImplementation,
                "java_semantics",
                r,
            );
            edge.evidence_json = Some(json!({"resolver":"javac_overrides", "resolution":"candidate", "runtime_binding_not_resolved":true,"declaration":r}).to_string());
            edges.push(edge);
        }
    }
    link_feign(&records, &config, &target_at, &mut analyses, &mut edges);
    store.upsert_edges_bulk(&edges)?;
    for (path, analysis) in analyses {
        let id = groundgraph_core::artifact_id::file_id(&path);
        let Some(mut node) = store.find_node(&id)? else {
            continue;
        };
        let mut meta: Value = node
            .metadata_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_else(|| json!({}));
        meta["java_analysis"] = serde_json::to_value(analysis)?;
        node.metadata_json = Some(meta.to_string());
        store.upsert_node(&node)?;
    }
    Ok(edges.len())
}

fn external_call_effect(symbol: &str) -> Option<(&'static str, &'static str)> {
    let name = symbol
        .split('(')
        .next()?
        .rsplit('.')
        .next()?
        .rsplit('>')
        .next()?;
    if symbol.contains("springframework.web.client.RestTemplate.")
        && [
            "exchange",
            "execute",
            "getForObject",
            "getForEntity",
            "postForObject",
            "postForEntity",
            "postForLocation",
            "put",
            "delete",
            "patchForObject",
        ]
        .contains(&name)
    {
        Some(("http", "RestTemplate"))
    } else if symbol.contains("springframework.web.reactive.function.client.WebClient")
        && ["retrieve", "exchangeToMono", "exchangeToFlux", "exchange"].contains(&name)
    {
        Some(("http", "WebClient"))
    } else if symbol.contains("okhttp3.OkHttpClient.") && name == "newCall"
        || symbol.contains("okhttp3.Call.") && ["execute", "enqueue"].contains(&name)
    {
        Some(("http", "OkHttp"))
    } else if symbol.contains("cn.hutool.http.HttpUtil.")
        && ["get", "post", "put", "delete", "request"].contains(&name)
        || symbol.contains("cn.hutool.http.HttpRequest.") && name == "execute"
    {
        Some(("http", "Hutool"))
    } else if symbol.contains("java.net.http.HttpClient.") && ["send", "sendAsync"].contains(&name)
    {
        Some(("http", "JDK HttpClient"))
    } else if symbol.contains("org.springframework.kafka.core.KafkaTemplate.") && name == "send" {
        Some(("mq", "KafkaTemplate"))
    } else if (symbol.contains("org.springframework.amqp.rabbit.core.RabbitTemplate.")
        || symbol.contains("org.springframework.amqp.core.AmqpTemplate."))
        && ["send", "convertAndSend"].contains(&name)
    {
        Some(("mq", "RabbitTemplate"))
    } else if symbol.contains("org.springframework.jms.core.JmsTemplate.")
        && ["send", "convertAndSend"].contains(&name)
    {
        Some(("mq", "JmsTemplate"))
    } else if symbol.contains("org.springframework.cloud.stream.function.StreamBridge.")
        && name == "send"
    {
        Some(("mq", "StreamBridge"))
    } else if symbol.contains("org.springframework.context.ApplicationEventPublisher.")
        && name == "publishEvent"
    {
        Some(("event", "Spring Event"))
    } else {
        None
    }
}

fn candidate(
    from: &str,
    to: &str,
    kind: EdgeKind,
    indexer: &str,
    evidence: &Value,
) -> EdgeAssertion {
    let mut e = EdgeAssertion::with_confidence(
        ArtifactId::new(from),
        ArtifactId::new(to),
        kind,
        EdgeSource::LanguageAdapter,
        0.75,
    );
    e.certainty = EdgeCertainty::Candidate;
    e.status = EdgeStatus::Proposed;
    e.indexer = Some(indexer.into());
    e.evidence_json = Some(evidence.to_string());
    e.source_file = evidence["path"].as_str().map(str::to_string);
    e
}

fn link_feign(
    records: &[Value],
    config: &JavaSemanticsConfig,
    target_at: &impl Fn(&Value, &str) -> Option<String>,
    analyses: &mut BTreeMap<String, JavaAnalysis>,
    edges: &mut Vec<EdgeAssertion>,
) {
    let routes: Vec<_> = records.iter().filter(|r| r["kind"] == "route").collect();
    for client in routes.iter().filter(|r| r["role"] == "client") {
        let Some(from) = target_at(client, "") else {
            continue;
        };
        let service = client["service"].as_str().unwrap_or("");
        let roots = config.services.get(service);
        let mut matches = 0;
        for server in routes.iter().filter(|r| r["role"] == "server") {
            let path = server["path"].as_str().unwrap_or("");
            if roots.is_none_or(|rs| {
                !rs.iter().any(|r| {
                    path == *r || path.starts_with(&format!("{}/", r.trim_end_matches('/')))
                })
            }) {
                continue;
            }
            if client["route"] != server["route"] {
                continue;
            }
            let c = client["verb"].as_str().unwrap_or("");
            let s = server["verb"].as_str().unwrap_or("");
            if c != s && c != "ANY" && s != "ANY" {
                continue;
            }
            let Some(to) = target_at(server, "") else {
                continue;
            };
            let evidence = json!({"resolver":"feign_service_http_route", "resolution":"candidate","path":client["path"],"line":client["line"],"service":service,"route":client["route"],"verb":c,"server":server,"runtime_binding_not_resolved":true});
            let mut edge = candidate(&from, &to, EdgeKind::Calls, "java_feign", &evidence);
            edge.id = ArtifactId::new(format!(
                "java_feign::{from}::{to}::{c}::{}",
                client["route"]
            ));
            edges.push(edge);
            matches += 1;
        }
        if matches == 0 {
            if let Some(a) = client["path"].as_str().and_then(|p| analyses.get_mut(p)) {
                a.diagnostics.push(format!(
                    "unresolved_feign: {from}, service={service}, route={}, reason={}",
                    client["route"],
                    if roots.is_none() {
                        "service_mapping_missing"
                    } else {
                        "no_matching_endpoint"
                    }
                ));
            }
        }
    }
}

/// Read the same persisted inventory for trace, feature packs, UI and migration.
pub fn analysis_for_files(store: &Store, files: &BTreeSet<String>) -> Result<JavaAnalysis> {
    let mut out = JavaAnalysis::default();
    for n in store.list_nodes_by_kind(NodeKind::File)? {
        if !n.path.as_ref().is_some_and(|p| files.contains(p)) {
            continue;
        }
        let Some(meta) = n.metadata_json else {
            continue;
        };
        let meta: Value = serde_json::from_str(&meta)?;
        if let Some(a) = meta.get("java_analysis") {
            let a: JavaAnalysis = serde_json::from_value(a.clone())?;
            out.calls.extend(a.calls);
            out.diagnostics.extend(a.diagnostics);
        }
    }
    out.calls.sort_by(|a, b| a.id.cmp(&b.id));
    out.diagnostics.sort();
    out.diagnostics.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_java_external_clients_are_scoped_by_owner_and_method() {
        for (symbol, effect) in [
            ("org.springframework.web.client.RestTemplate.<T>postForEntity(java.lang.String)", "http"),
            ("org.springframework.web.reactive.function.client.WebClient.retrieve()", "http"),
            ("okhttp3.OkHttpClient.newCall(okhttp3.Request)", "http"),
            ("cn.hutool.http.HttpUtil.post(java.lang.String)", "http"),
            ("java.net.http.HttpClient.send(java.net.http.HttpRequest)", "http"),
            ("org.springframework.kafka.core.KafkaTemplate.send(java.lang.String)", "mq"),
            ("org.springframework.context.ApplicationEventPublisher.publishEvent(java.lang.Object)", "event"),
        ] {
            assert_eq!(external_call_effect(symbol).map(|(kind, _)| kind), Some(effect));
        }
        assert_eq!(
            external_call_effect("shop.RestTemplate.postForEntity()"),
            None
        );
        assert_eq!(
            external_call_effect("org.springframework.web.client.RestTemplate.hashCode()"),
            None
        );
    }
    #[test]
    fn inventory_does_not_require_a_compiler_or_resolvable_receiver() {
        let a = inventory(
            "A.java",
            "class A { Object f = factory(); void x() { a.b(); a.b(); } }",
            &mut [],
        )
        .unwrap();
        assert_eq!(a.calls.len(), 3);
        assert!(a
            .calls
            .iter()
            .all(|c| c.resolution == "unresolved" && c.line == 1));
        assert_ne!(a.calls[1].id, a.calls[2].id);
    }
}
