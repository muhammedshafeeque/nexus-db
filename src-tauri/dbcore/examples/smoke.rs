//! Read-only smoke test against a live server:
//!   cargo run --example smoke -- <engine> <url-or-file> "<query>"
use dbcore::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let engine: Engine = serde_json::from_value(serde_json::Value::String(args[1].clone()))?;
    let mut cfg = ConnectionConfig { engine, ..Default::default() };
    if engine == Engine::Sqlite {
        cfg.file_path = args[2].clone();
    } else {
        cfg.url = args[2].clone();
    }
    let d = connect(&cfg).await?;
    println!("server: {}", d.server_info().await?.version);

    let mut path: Vec<String> = Vec::new();
    for depth in 0..4 {
        let nodes = d.list_children(&path).await?;
        let names: Vec<_> = nodes.iter().take(6).map(|n| format!("{}[{}]", n.name, n.kind)).collect();
        println!("tree{depth} {path:?}: {} nodes {names:?}", nodes.len());
        match nodes.iter().find(|n| n.expandable && !n.name.starts_with("pg_") && n.name != "admin" && n.name != "config") {
            Some(n) => path = n.path.clone(),
            None => break,
        }
        if matches!(nodes[0].kind.as_str(), "table" | "view" | "collection" | "edge" | "key") {
            let oq = d.object_query(&path).await?;
            println!("object_query: {}", oq.query.replace('\n', " "));
            let ctx = ExecContext { database: oq.database, schema: oq.schema, limit: 3 };
            for rs in d.execute(&oq.query, &ctx).await? {
                println!("  -> {} cols, {} rows, truncated={}", rs.columns.len(), rs.rows.len(), rs.truncated);
            }
            let desc = d.describe(&path).await?;
            println!("describe: {} rows", desc.rows.len());
            break;
        }
    }
    if let Some(q) = args.get(3) {
        for rs in d.execute(q, &ExecContext { limit: 5, ..Default::default() }).await? {
            println!("query -> cols {:?}", rs.columns.iter().map(|c| format!("{}:{}", c.name, c.type_name)).collect::<Vec<_>>());
            for r in rs.rows {
                println!("   {}", serde_json::to_string(&r)?);
            }
            if let Some(m) = rs.message {
                println!("   msg: {m}");
            }
            if let Some(a) = rs.affected {
                println!("   affected: {a}");
            }
        }
    }
    d.close().await;
    Ok(())
}
