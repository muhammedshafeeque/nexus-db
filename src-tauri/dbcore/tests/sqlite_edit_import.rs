//! Grid edits and imports end to end against a real SQLite file.

use dbcore::{ConnectionConfig, Engine, ExecContext, ImportCtx, ImportMode, ImportOptions, RowChange};
use serde_json::{json, Map, Value};
use std::sync::atomic::AtomicBool;

fn map(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

async fn setup() -> (std::sync::Arc<dyn dbcore::Driver>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("nexus-sqlite-{}-{}", std::process::id(), rand_suffix()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("t.db");
    let cfg = ConnectionConfig { engine: Engine::Sqlite, file_path: file.to_string_lossy().into(), ..Default::default() };
    let d = dbcore::connect(&cfg).await.unwrap();
    d.execute(
        "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER, email TEXT UNIQUE);
         INSERT INTO people VALUES (1, 'Ada', 36, 'ada@x'), (2, 'Bob', 40, 'bob@x');
         CREATE TABLE nokey (a TEXT);",
        &ExecContext::default(),
    )
    .await
    .unwrap();
    (d, dir)
}

fn rand_suffix() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
}

async fn rows(d: &dyn dbcore::Driver) -> Vec<Vec<Value>> {
    d.execute("SELECT id, name, age, email FROM people ORDER BY id", &ExecContext::default()).await.unwrap().remove(0).rows
}

#[tokio::test]
async fn grid_edits_are_atomic_and_detect_conflicts() {
    let (d, dir) = setup().await;
    let path = vec!["people".to_string()];

    let info = d.edit_info(&path).await.unwrap();
    assert!(info.editable && info.transactional);
    assert_eq!(info.key_columns, vec!["id"]);
    let nokey = d.edit_info(&["nokey".to_string()]).await.unwrap();
    assert!(!nokey.editable && nokey.reason.unwrap().contains("primary key"));

    let changes = vec![
        RowChange::Update { key: map(json!({"id": 1})), values: map(json!({"name": "Ada L.", "age": "37"})) },
        RowChange::Insert { values: map(json!({"name": "Cy", "email": "cy@x"})) },
        RowChange::Delete { key: map(json!({"id": 2})) },
    ];
    let preview = d.apply_changes(&path, &changes, true).await.unwrap();
    assert_eq!(preview.statements.len(), 3);
    assert_eq!(rows(&*d).await.len(), 2, "dry run changes nothing");

    let report = d.apply_changes(&path, &changes, false).await.unwrap();
    assert_eq!((report.applied, report.error.clone()), (3, None));
    let r = rows(&*d).await;
    assert_eq!(r[0], vec![json!(1), json!("Ada L."), json!(37), json!("ada@x")], "'37' was stored as an integer");
    assert_eq!(r[1][1], json!("Cy"));
    assert_eq!(r.len(), 2);

    // second change targets a deleted row → everything is rolled back
    let conflict = vec![
        RowChange::Update { key: map(json!({"id": 1})), values: map(json!({"name": "changed"})) },
        RowChange::Update { key: map(json!({"id": 2})), values: map(json!({"name": "ghost"})) },
    ];
    let report = d.apply_changes(&path, &conflict, false).await.unwrap();
    assert_eq!(report.applied, 0);
    assert!(report.error.unwrap().contains("not found"));
    assert_eq!(rows(&*d).await[0][1], json!("Ada L."), "first change was rolled back");

    // NOT NULL violation surfaces the database error
    let bad = vec![RowChange::Update { key: map(json!({"id": 1})), values: map(json!({"name": null})) }];
    let report = d.apply_changes(&path, &bad, false).await.unwrap();
    assert!(report.error.unwrap().contains("NOT NULL"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn imports_roll_back_skip_bad_rows_and_upsert() {
    let (d, dir) = setup().await;
    let path = vec!["people".to_string()];
    let cols: Vec<String> = ["id", "name", "email"].map(String::from).to_vec();
    let cancel = AtomicBool::new(false);
    let progress = |_n: usize| {};
    let ctx = ImportCtx { progress: &progress, cancel: &cancel };
    let batch = || {
        vec![
            (2, vec![json!(10), json!("Dee"), json!("dee@x")]),
            (3, vec![json!(11), Value::Null, json!("eve@x")]), // NOT NULL
            (4, vec![json!(12), json!("Fay"), json!("ada@x")]), // UNIQUE email
            (5, vec![json!(13), json!("Gus"), json!("gus@x")]),
        ]
    };

    // all or nothing: bad rows are reported, nothing is written
    let opts = ImportOptions { atomic: true, batch_size: 500, ..Default::default() };
    let r = d.import_rows(&path, &cols, batch(), &opts, &ctx).await.unwrap();
    assert!(r.rolled_back);
    assert_eq!(r.written, 0);
    assert_eq!(r.errors.iter().map(|e| e.row).collect::<Vec<_>>(), vec![3, 4]);
    assert_eq!(rows(&*d).await.len(), 2);

    // row by row: good rows land, bad rows are listed with their file row numbers
    let opts = ImportOptions { atomic: false, batch_size: 2, ..Default::default() };
    let r = d.import_rows(&path, &cols, batch(), &opts, &ctx).await.unwrap();
    assert_eq!((r.written, r.failed), (2, 2));
    assert!(r.errors[0].message.contains("NOT NULL"), "{:?}", r.errors);
    assert_eq!(rows(&*d).await.len(), 4);

    // upsert on id updates existing rows and inserts new ones
    let opts = ImportOptions { mode: ImportMode::Upsert, key_columns: vec!["id".into()], atomic: true, batch_size: 100, ..Default::default() };
    let up = vec![(2, vec![json!(1), json!("Ada (upd)"), json!("ada@x")]), (3, vec![json!(20), json!("Hal"), json!("hal@x")])];
    let r = d.import_rows(&path, &cols, up, &opts, &ctx).await.unwrap();
    assert_eq!((r.written, r.failed, r.rolled_back), (2, 0, false));
    let all = rows(&*d).await;
    assert_eq!(all[0][1], json!("Ada (upd)"));
    assert_eq!(all[0][2], json!(36), "columns that were not imported keep their value");
    assert!(all.iter().any(|r| r[1] == json!("Hal")));

    // upsert without key columns is refused up front
    let opts = ImportOptions { mode: ImportMode::Upsert, ..Default::default() };
    assert!(d.import_rows(&path, &cols, vec![], &opts, &ctx).await.is_err());

    // cancelling an atomic import rolls back
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    let opts = ImportOptions { atomic: true, batch_size: 1, ..Default::default() };
    let r = d.import_rows(&path, &cols, vec![(2, vec![json!(30), json!("Ivy"), json!("ivy@x")])], &opts, &ctx).await.unwrap();
    assert!(r.cancelled && r.rolled_back && r.written == 0);
    let _ = std::fs::remove_dir_all(dir);
}
