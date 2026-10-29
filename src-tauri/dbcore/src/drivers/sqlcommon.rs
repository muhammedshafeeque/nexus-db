use crate::model::{Column, ResultSet};
use anyhow::Result;
use futures::TryStreamExt;
use serde_json::Value;
use sqlx::{Column as _, Either, Row, TypeInfo};

/// Streams a raw (multi-statement) script through sqlx and splits the output
/// into one ResultSet per statement. Returns `true` when output was truncated
/// at `limit` rows, in which case the caller must not reuse the connection.
#[allow(deprecated)]
pub async fn run_raw<DB>(
    conn: &mut DB::Connection,
    query: &str,
    limit: usize,
    cell: fn(&DB::Row, usize) -> Value,
    affected: fn(&DB::QueryResult) -> u64,
) -> Result<(Vec<ResultSet>, bool)>
where
    DB: sqlx::Database,
    for<'c> &'c mut DB::Connection: sqlx::Executor<'c, Database = DB>,
{
    let mut sets = Vec::new();
    let mut cur: Option<ResultSet> = None;
    let mut truncated = false;
    let mut stream = sqlx::raw_sql(query).fetch_many(&mut *conn);

    while let Some(item) = stream.try_next().await? {
        match item {
            Either::Left(done) => {
                let mut set = cur.take().unwrap_or_default();
                if set.columns.is_empty() {
                    set.affected = Some(affected(&done));
                }
                sets.push(set);
            }
            Either::Right(row) => {
                let set = cur.get_or_insert_with(|| ResultSet::table(columns(&row), Vec::new()));
                if set.rows.len() >= limit {
                    set.truncated = true;
                    truncated = true;
                    break;
                }
                set.rows.push((0..row.len()).map(|i| cell(&row, i)).collect());
            }
        }
    }
    drop(stream);
    if let Some(set) = cur.take() {
        sets.push(set);
    }
    Ok((sets, truncated))
}

fn columns<R: Row>(row: &R) -> Vec<Column> {
    row.columns()
        .iter()
        .map(|c| Column::new(c.name(), c.type_info().name().to_lowercase()))
        .collect()
}

/// Flattens the first result set into strings – handy for catalog queries.
pub fn strings(sets: &[ResultSet]) -> Vec<Vec<String>> {
    sets.iter()
        .find(|s| !s.columns.is_empty())
        .map(|s| {
            s.rows
                .iter()
                .map(|r| {
                    r.iter()
                        .map(|v| match v {
                            Value::Null => String::new(),
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .collect()
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn first_value(sets: &[ResultSet]) -> String {
    strings(sets).into_iter().next().and_then(|r| r.into_iter().next()).unwrap_or_default()
}

// ---------------------------------------------------------------- editing & import

use crate::model::{ApplyReport, EditColumn, EditInfo, ImportCtx, ImportMode, ImportOptions, ImportReport, ImportRow, RowChange};
use crate::util::quote_ident;
use anyhow::bail;
use async_trait::async_trait;
use serde_json::Map;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SqlKind {
    Postgres,
    Mysql,
    Sqlite,
    Mssql,
}

impl SqlKind {
    fn quote(self) -> char {
        match self {
            SqlKind::Mysql => '`',
            SqlKind::Mssql => '[',
            _ => '"',
        }
    }

    pub fn ident(self, name: &str) -> String {
        quote_ident(name, self.quote())
    }

    fn begin(self) -> &'static str {
        match self {
            SqlKind::Mysql => "START TRANSACTION",
            SqlKind::Mssql => "BEGIN TRANSACTION",
            _ => "BEGIN",
        }
    }

    fn commit(self) -> &'static str {
        match self {
            SqlKind::Mssql => "COMMIT TRANSACTION",
            _ => "COMMIT",
        }
    }

    fn rollback(self) -> &'static str {
        match self {
            SqlKind::Mssql => "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION",
            _ => "ROLLBACK",
        }
    }

    fn savepoint(self) -> &'static str {
        match self {
            SqlKind::Mssql => "SAVE TRANSACTION nexus_batch",
            _ => "SAVEPOINT nexus_batch",
        }
    }

    fn rollback_savepoint(self) -> &'static str {
        match self {
            SqlKind::Mssql => "ROLLBACK TRANSACTION nexus_batch",
            _ => "ROLLBACK TO SAVEPOINT nexus_batch",
        }
    }

    fn release_savepoint(self) -> Option<&'static str> {
        match self {
            SqlKind::Mssql => None,
            _ => Some("RELEASE SAVEPOINT nexus_batch"),
        }
    }

    /// Largest multi-row VALUES list the engine accepts comfortably.
    fn max_batch(self) -> usize {
        match self {
            SqlKind::Mssql => 1000,
            SqlKind::Sqlite => 500,
            _ => 2000,
        }
    }

    /// JSON value → SQL literal. Strings stay untyped literals so the server
    /// converts them to the column type ('42' → int, '2024-01-01' → date …).
    pub fn lit(self, v: &Value) -> String {
        match v {
            Value::Null => "NULL".into(),
            Value::Bool(b) => match self {
                SqlKind::Postgres | SqlKind::Mysql => if *b { "TRUE" } else { "FALSE" }.into(),
                _ => if *b { "1" } else { "0" }.into(),
            },
            Value::Number(n) => n.to_string(),
            Value::String(s) => self.str_lit(s),
            other => self.str_lit(&other.to_string()),
        }
    }

    fn str_lit(self, s: &str) -> String {
        let quoted = s.replace('\'', "''");
        match self {
            SqlKind::Mysql => format!("'{}'", quoted.replace('\\', "\\\\")),
            SqlKind::Mssql => format!("N'{quoted}'"),
            _ => format!("'{quoted}'"),
        }
    }

    fn where_key(self, key: &Map<String, Value>) -> Result<String> {
        if key.is_empty() {
            bail!("The row has no key values – refresh the data and try again.");
        }
        Ok(key
            .iter()
            .map(|(k, v)| match v {
                Value::Null => format!("{} IS NULL", self.ident(k)),
                v => format!("{} = {}", self.ident(k), self.lit(v)),
            })
            .collect::<Vec<_>>()
            .join(" AND "))
    }
}

/// Runs one statement and returns the number of affected rows.
#[async_trait]
pub trait SqlExec: Send {
    async fn exec(&mut self, sql: &str) -> Result<u64>;
}

/// A pooled sqlx connection as a [`SqlExec`].
pub struct SqlxConn<'a, C>(pub &'a mut C);

macro_rules! sqlx_exec {
    ($conn:ty) => {
        #[async_trait]
        impl SqlExec for SqlxConn<'_, $conn> {
            async fn exec(&mut self, sql: &str) -> Result<u64> {
                use sqlx::Executor;
                let conn: &mut $conn = &mut *self.0;
                Ok(conn.execute(sqlx::raw_sql(sql)).await?.rows_affected())
            }
        }
    };
}
sqlx_exec!(sqlx::PgConnection);
sqlx_exec!(sqlx::MySqlConnection);
sqlx_exec!(sqlx::SqliteConnection);

/// Builds [`EditInfo`] from a driver's `describe` result: first column = name,
/// plus `type`, `nullable`, `default` and `primary_key` / `key` columns.
pub fn edit_info_from_describe(rs: &ResultSet) -> EditInfo {
    let idx = |names: &[&str]| rs.columns.iter().position(|c| names.contains(&c.name.as_str()));
    let (ty, nullable, default, pk, key, extra) = (
        idx(&["type"]),
        idx(&["nullable"]),
        idx(&["default"]),
        idx(&["primary_key"]),
        idx(&["key"]),
        idx(&["extra"]),
    );
    let truthy = |v: &Value| match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        Value::String(s) => matches!(s.to_ascii_uppercase().as_str(), "YES" | "TRUE" | "T" | "1"),
        _ => false,
    };
    let text = |row: &[Value], i: Option<usize>| match i.and_then(|i| row.get(i)) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(v) => v.to_string(),
    };
    let columns: Vec<EditColumn> = rs
        .rows
        .iter()
        .map(|row| {
            let is_key = pk.and_then(|i| row.get(i)).map(truthy).unwrap_or(false) || text(row, key) == "PRI";
            EditColumn {
                name: text(row, Some(0)),
                type_name: text(row, ty),
                nullable: nullable.and_then(|i| row.get(i)).map(truthy).unwrap_or(true),
                has_default: default.and_then(|i| row.get(i)).is_some_and(|v| !v.is_null())
                    || text(row, extra).to_ascii_lowercase().contains("auto_increment"),
                key: is_key,
            }
        })
        .collect();
    let key_columns: Vec<String> = columns.iter().filter(|c| c.key).map(|c| c.name.clone()).collect();
    let editable = !key_columns.is_empty();
    EditInfo {
        reason: (!editable).then(|| {
            "No primary key – rows cannot be identified safely, so grid editing is off. Importing still works.".to_string()
        }),
        editable,
        can_upsert: true,
        transactional: true,
        key_columns,
        columns,
        ..Default::default()
    }
}

/// One statement per change; updates and deletes must hit exactly one row.
pub fn change_statements(kind: SqlKind, table: &str, changes: &[RowChange]) -> Result<Vec<(String, bool)>> {
    changes
        .iter()
        .map(|c| {
            Ok(match c {
                RowChange::Update { key, values } => {
                    if values.is_empty() {
                        bail!("Nothing to update");
                    }
                    let set = values.iter().map(|(k, v)| format!("{} = {}", kind.ident(k), kind.lit(v))).collect::<Vec<_>>().join(", ");
                    (format!("UPDATE {table} SET {set} WHERE {}", kind.where_key(key)?), true)
                }
                RowChange::Insert { values } if values.is_empty() => match kind {
                    SqlKind::Mysql => (format!("INSERT INTO {table} () VALUES ()"), false),
                    _ => (format!("INSERT INTO {table} DEFAULT VALUES"), false),
                },
                RowChange::Insert { values } => {
                    let cols = values.keys().map(|k| kind.ident(k)).collect::<Vec<_>>().join(", ");
                    let vals = values.values().map(|v| kind.lit(v)).collect::<Vec<_>>().join(", ");
                    (format!("INSERT INTO {table} ({cols}) VALUES ({vals})"), false)
                }
                RowChange::Delete { key } => (format!("DELETE FROM {table} WHERE {}", kind.where_key(key)?), true),
            })
        })
        .collect()
}

/// Applies grid changes in one transaction: all of them or none.
pub async fn apply_changes(kind: SqlKind, ex: &mut dyn SqlExec, table: &str, changes: &[RowChange], dry_run: bool) -> Result<ApplyReport> {
    let stmts = change_statements(kind, table, changes)?;
    let mut report = ApplyReport { statements: stmts.iter().map(|(s, _)| s.clone()).collect(), atomic: true, ..Default::default() };
    if dry_run {
        return Ok(report);
    }
    ex.exec(kind.begin()).await?;
    for (i, (sql, expect_one)) in stmts.iter().enumerate() {
        let outcome = match ex.exec(sql).await {
            Ok(n) if *expect_one && n != 1 => Err(if n == 0 {
                "the row was not found – it may have been changed or deleted by someone else. Refresh and try again.".to_string()
            } else {
                format!("it would change {n} rows instead of one – the key columns are not unique.")
            }),
            Ok(_) => Ok(()),
            Err(e) => Err(format!("{e:#}")),
        };
        if let Err(msg) = outcome {
            let _ = ex.exec(kind.rollback()).await;
            report.error = Some(format!("Change {} of {}: {msg}\nNothing was saved.", i + 1, stmts.len()));
            return Ok(report);
        }
    }
    if let Err(e) = ex.exec(kind.commit()).await {
        let _ = ex.exec(kind.rollback()).await;
        report.error = Some(format!("Commit failed: {e:#}\nNothing was saved."));
        return Ok(report);
    }
    report.applied = changes.len();
    Ok(report)
}

/// INSERT (or upsert) of a batch of rows as one statement.
pub fn insert_sql(kind: SqlKind, table: &str, columns: &[String], rows: &[ImportRow], opts: &ImportOptions) -> String {
    let cols = columns.iter().map(|c| kind.ident(c)).collect::<Vec<_>>().join(", ");
    let values = rows
        .iter()
        .map(|(_, r)| format!("({})", r.iter().map(|v| kind.lit(v)).collect::<Vec<_>>().join(", ")))
        .collect::<Vec<_>>()
        .join(",\n");
    let keys = &opts.key_columns;
    let rest: Vec<&String> = columns.iter().filter(|c| !keys.contains(c)).collect();
    if opts.mode == ImportMode::Insert {
        return format!("INSERT INTO {table} ({cols}) VALUES\n{values}");
    }
    match kind {
        SqlKind::Postgres | SqlKind::Sqlite => {
            let target = keys.iter().map(|k| kind.ident(k)).collect::<Vec<_>>().join(", ");
            let action = if rest.is_empty() {
                "DO NOTHING".to_string()
            } else {
                let excluded = if kind == SqlKind::Postgres { "EXCLUDED" } else { "excluded" };
                format!("DO UPDATE SET {}", rest.iter().map(|c| format!("{0} = {excluded}.{0}", kind.ident(c))).collect::<Vec<_>>().join(", "))
            };
            format!("INSERT INTO {table} ({cols}) VALUES\n{values}\nON CONFLICT ({target}) {action}")
        }
        SqlKind::Mysql => {
            let update = if rest.is_empty() {
                let k = kind.ident(&columns[0]);
                format!("{k} = {k}")
            } else {
                rest.iter().map(|c| format!("{0} = VALUES({0})", kind.ident(c))).collect::<Vec<_>>().join(", ")
            };
            format!("INSERT INTO {table} ({cols}) VALUES\n{values}\nON DUPLICATE KEY UPDATE {update}")
        }
        SqlKind::Mssql => {
            let on = keys.iter().map(|k| format!("t.{0} = s.{0}", kind.ident(k))).collect::<Vec<_>>().join(" AND ");
            let matched = if rest.is_empty() {
                String::new()
            } else {
                format!(
                    "\nWHEN MATCHED THEN UPDATE SET {}",
                    rest.iter().map(|c| format!("t.{0} = s.{0}", kind.ident(c))).collect::<Vec<_>>().join(", ")
                )
            };
            let src = columns.iter().map(|c| format!("s.{}", kind.ident(c))).collect::<Vec<_>>().join(", ");
            format!(
                "MERGE INTO {table} AS t USING (VALUES\n{values}\n) AS s ({cols}) ON {on}{matched}\nWHEN NOT MATCHED THEN INSERT ({cols}) VALUES ({src});"
            )
        }
    }
}

/// Batched import. Atomic: one transaction, rolled back on the first bad
/// batch (whose failing rows are reported). Otherwise each batch commits on
/// its own and a failing batch is retried row by row so good rows still land.
pub async fn import_rows(
    kind: SqlKind,
    ex: &mut dyn SqlExec,
    table: &str,
    columns: &[String],
    rows: Vec<ImportRow>,
    opts: &ImportOptions,
    ctx: &ImportCtx<'_>,
) -> Result<ImportReport> {
    if opts.mode == ImportMode::Upsert && opts.key_columns.is_empty() && kind != SqlKind::Mysql {
        bail!("Choose the key column(s) used to match existing rows.");
    }
    let mut report = ImportReport::default();
    let batch = opts.batch(kind.max_batch());
    if opts.atomic {
        ex.exec(kind.begin()).await?;
    }
    let mut done = 0;
    for chunk in rows.chunks(batch) {
        if ctx.cancelled() {
            report.cancelled = true;
            break;
        }
        if opts.atomic {
            ex.exec(kind.savepoint()).await?;
        }
        match ex.exec(&insert_sql(kind, table, columns, chunk, opts)).await {
            Ok(_) => {
                report.written += chunk.len() as u64;
                if let (true, Some(release)) = (opts.atomic, kind.release_savepoint()) {
                    ex.exec(release).await?;
                }
            }
            Err(batch_err) => {
                if opts.atomic {
                    ex.exec(kind.rollback_savepoint()).await?;
                }
                // find the offending rows
                let mut found = false;
                for row in chunk {
                    if opts.atomic {
                        ex.exec(kind.savepoint()).await?;
                    }
                    match ex.exec(&insert_sql(kind, table, columns, std::slice::from_ref(row), opts)).await {
                        Ok(_) => {
                            report.written += 1;
                            if let (true, Some(release)) = (opts.atomic, kind.release_savepoint()) {
                                ex.exec(release).await?;
                            }
                        }
                        Err(e) => {
                            found = true;
                            report.fail(row.0, format!("{e:#}"));
                            if opts.atomic {
                                ex.exec(kind.rollback_savepoint()).await?;
                            }
                        }
                    }
                }
                if !found {
                    // the batch failed but every row alone succeeded (e.g. duplicates within the batch)
                    report.fail(chunk[0].0, format!("rows {}–{}: {batch_err:#}", chunk[0].0, chunk[chunk.len() - 1].0));
                }
                if opts.atomic {
                    break;
                }
            }
        }
        done += chunk.len();
        (ctx.progress)(done);
    }
    if opts.atomic {
        if report.failed > 0 || report.cancelled {
            ex.exec(kind.rollback()).await?;
            report.written = 0;
            report.rolled_back = true;
        } else {
            ex.exec(kind.commit()).await?;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn literals_per_dialect() {
        assert_eq!(SqlKind::Postgres.lit(&json!("O'Neil")), "'O''Neil'");
        assert_eq!(SqlKind::Mysql.lit(&json!(r"a\b'")), r"'a\\b'''");
        assert_eq!(SqlKind::Mssql.lit(&json!("ü")), "N'ü'");
        assert_eq!(SqlKind::Sqlite.lit(&json!(true)), "1");
        assert_eq!(SqlKind::Postgres.lit(&json!({"a": [1]})), r#"'{"a":[1]}'"#);
        assert_eq!(SqlKind::Mssql.ident("we]ird"), "[we]]ird]");
    }

    #[test]
    fn change_statements_use_keys_and_null_checks() {
        let changes = vec![
            RowChange::Update { key: map(json!({"id": 7})), values: map(json!({"name": "x", "note": null})) },
            RowChange::Delete { key: map(json!({"a": 1, "b": null})) },
            RowChange::Insert { values: map(json!({})) },
        ];
        let s = change_statements(SqlKind::Postgres, r#""public"."t""#, &changes).unwrap();
        assert_eq!(s[0].0, r#"UPDATE "public"."t" SET "name" = 'x', "note" = NULL WHERE "id" = 7"#);
        assert!(s[0].1);
        assert_eq!(s[1].0, r#"DELETE FROM "public"."t" WHERE "a" = 1 AND "b" IS NULL"#);
        assert_eq!(s[2].0, r#"INSERT INTO "public"."t" DEFAULT VALUES"#);
        assert!(change_statements(SqlKind::Mysql, "t", &[RowChange::Delete { key: Map::new() }]).is_err());
    }

    #[test]
    fn upsert_sql_per_dialect() {
        let cols = vec!["id".to_string(), "name".to_string()];
        let rows = vec![(2, vec![json!(1), json!("a")]), (3, vec![json!(2), json!("b")])];
        let opts = ImportOptions { mode: ImportMode::Upsert, key_columns: vec!["id".into()], ..Default::default() };
        let pg = insert_sql(SqlKind::Postgres, "t", &cols, &rows, &opts);
        assert!(pg.ends_with(r#"ON CONFLICT ("id") DO UPDATE SET "name" = EXCLUDED."name""#), "{pg}");
        let my = insert_sql(SqlKind::Mysql, "t", &cols, &rows, &opts);
        assert!(my.ends_with("ON DUPLICATE KEY UPDATE `name` = VALUES(`name`)"), "{my}");
        let ms = insert_sql(SqlKind::Mssql, "[d].[s].[t]", &cols, &rows, &opts);
        assert!(ms.contains("ON t.[id] = s.[id]") && ms.contains("WHEN NOT MATCHED THEN INSERT ([id], [name]) VALUES (s.[id], s.[name]);"), "{ms}");
        let only_keys = insert_sql(SqlKind::Sqlite, "t", &cols[..1], &[(2, vec![json!(1)])], &opts);
        assert!(only_keys.ends_with(r#"ON CONFLICT ("id") DO NOTHING"#));
    }

    #[test]
    fn describe_to_edit_info() {
        let rs = ResultSet::table(
            vec![Column::new("column", ""), Column::new("type", ""), Column::new("nullable", ""), Column::new("default", ""), Column::new("key", ""), Column::new("extra", "")],
            vec![
                vec![json!("id"), json!("int"), json!("NO"), Value::Null, json!("PRI"), json!("auto_increment")],
                vec![json!("name"), json!("varchar(20)"), json!("YES"), Value::Null, json!(""), json!("")],
            ],
        );
        let info = edit_info_from_describe(&rs);
        assert!(info.editable);
        assert_eq!(info.key_columns, vec!["id"]);
        assert!(info.columns[0].has_default && !info.columns[0].nullable && info.columns[1].nullable);
    }
}
