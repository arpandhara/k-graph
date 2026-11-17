use kg::{
    CodeEdge, CodeEntityNode, EdgeKind, GraphStore, NodeKind, SourceSpan, SymbolUri, WriteBatch,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workdir = std::env::temp_dir().join(format!("kg-phase4-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir)?;
    let db_path = workdir.join("graph.redb");

    let source_uri = SymbolUri::new("demo", "src/main.rs", None, "main", "fn")?;
    let target_uri = SymbolUri::new("demo", "src/lib.rs", None, "run", "fn")?;

    let source = CodeEntityNode::try_new(
        source_uri.clone(),
        "main",
        NodeKind::Function,
        "src/main.rs",
        "rust",
        SourceSpan::new(1, 0, 3, 1, 0, 40)?,
    )?;
    let target = CodeEntityNode::try_new(
        target_uri.clone(),
        "run",
        NodeKind::Function,
        "src/lib.rs",
        "rust",
        SourceSpan::new(1, 0, 3, 1, 0, 40)?,
    )?;

    let edge = CodeEdge::new(source_uri.clone(), target_uri.clone(), EdgeKind::Calls)
        .with_confidence(0.95)?
        .with_call_site(SourceSpan::new(2, 4, 2, 12, 12, 20)?);

    let mut store = GraphStore::open(&db_path)?;
    let report = store.save_batch(
        &[source.clone(), target.clone()],
        std::slice::from_ref(&edge),
    )?;
    assert_eq!(report.nodes_written, 2);
    assert_eq!(report.edges_written, 1);
    assert_eq!(report.files_indexed, 2);

    let graph = store.load_graph()?;
    assert_eq!(graph.node_count(), 2);
    assert_eq!(graph.edge_count(), 1);

    let loaded = store
        .get_edge(&source_uri, &target_uri, EdgeKind::Calls)?
        .expect("edge should exist");
    assert_eq!(loaded.confidence().value(), 0.95);

    store.set_repo_name("demo")?;
    store.set_last_commit("deadbeef")?;
    assert_eq!(store.repo_name()?.as_deref(), Some("demo"));
    assert_eq!(store.last_commit()?.as_deref(), Some("deadbeef"));

    let mut batch = WriteBatch::new();
    batch.remove_file("src/main.rs");
    let removal = store.write_batch(&batch)?;
    assert_eq!(removal.nodes_removed, 1);
    assert_eq!(removal.edges_removed, 1);
    assert!(store.get_node(&source_uri)?.is_none());
    assert!(
        store
            .get_edge(&source_uri, &target_uri, EdgeKind::Calls)?
            .is_none()
    );

    let report = store.verify()?;
    assert_eq!(report.nodes, 1);
    assert_eq!(report.edges, 0);
    assert_eq!(report.files, 1);

    store.compact()?;
    drop(store);

    let reopened = GraphStore::open(&db_path)?;
    assert_eq!(reopened.schema_version()?, kg::STORAGE_SCHEMA_VERSION);
    assert_eq!(reopened.node_count()?, 1);
    assert_eq!(reopened.edge_count()?, 0);
    assert_eq!(reopened.last_commit()?.as_deref(), Some("deadbeef"));
    assert_eq!(reopened.verify()?.files, 1);

    println!("Phase 4 smoke test passed. Database: {}", db_path.display());
    std::fs::remove_dir_all(&workdir)?;
    Ok(())
}
