use std::path::PathBuf;

use groundgraph_engine::prelude::*;

#[test]
fn prelude_exposes_embedded_library_surface() {
    let repo_root = PathBuf::from(".");

    let _init_options = InitOptions::new(&repo_root);
    let _index_options = IndexOptions::all(&repo_root);
    let _check_options = CheckOptions {
        repo_root: repo_root.clone(),
        impact: None,
    };
    let _search_options = SearchOptions::keywords(&repo_root, "auth");
    let _context_options = ContextOptions {
        repo_root: repo_root.clone(),
        requirement: "REQ-001".to_string(),
        include_snippets: true,
    };
    let _impact_options = ImpactOptions {
        repo_root,
        base_ref: "main".to_string(),
        head_ref: "HEAD".to_string(),
        reindex: false,
    };

    let _core_type = std::any::type_name::<NodeKind>();
    let _store_type = std::any::type_name::<Store>();
}
