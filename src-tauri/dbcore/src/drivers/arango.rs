use super::http::{Body, Http};
use crate::model::*;
use crate::tabular::build_doc;
use crate::util::{docs_to_resultset, document_edit_info, infer_schema, obj, split_statements};
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use reqwest::Method;
use serde_json::{json, Map, Value};
use urlencoding::encode;

pub struct ArangoDriver {
    http: Http,
    default_db: String,
}

impl ArangoDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let default_db = if cfg.database.is_empty() { "_system".to_string() } else { cfg.database.clone() };
        let driver = Self { http: Http::new(cfg, 8529)?, default_db };
        driver.server_info().await?;
        Ok(driver)
    }

    fn db_path(db: &str, path: &str) -> String {
        format!("/_db/{}{}", encode(db), path)
    }

    async fn cursor(&self, db: &str, body: Value, limit: usize) -> Result<(Vec<Value>, bool, u64)> {
        let mut res = self.http.json(Method::POST, &Self::db_path(db, "/_api/cursor"), Body::Json(body)).await?;
        let writes = res.pointer("/extra/stats/writesExecuted").and_then(Value::as_u64).unwrap_or(0);
        let mut docs: Vec<Value> = Vec::new();
        loop {
            if let Some(Value::Array(batch)) = res.get_mut("result").map(Value::take) {
                docs.extend(batch);
            }
            let has_more = res.get("hasMore").and_then(Value::as_bool).unwrap_or(false);
            let id = res.get("id").and_then(Value::as_str).map(str::to_string);
            match (has_more, id) {
                (true, Some(id)) if docs.len() < limit => {
                    res = self
                        .http
                        .json(Method::POST, &Self::db_path(db, &format!("/_api/cursor/{id}")), Body::None)
                        .await?;
                }
                (true, Some(id)) => {
                    let _ = self.http.send(Method::DELETE, &Self::db_path(db, &format!("/_api/cursor/{id}")), Body::None).await;
                    docs.truncate(limit);
                    return Ok((docs, true, writes));
                }
                _ => break,
            }
        }
        let truncated = docs.len() > limit;
        docs.truncate(limit);
        Ok((docs, truncated, writes))
    }
}

/// System attributes the server manages; PATCHing them fails.
fn strip_meta(values: &Map<String, Value>) -> Map<String, Value> {
    values.iter().filter(|(k, _)| !matches!(k.as_str(), "_key" | "_id" | "_rev")).map(|(k, v)| (k.clone(), v.clone())).collect()
}

#[async_trait]
impl Driver for ArangoDriver {
    async fn server_info(&self) -> Result<ServerInfo> {
        let v = self.http.json(Method::GET, "/_api/version", Body::None).await.context("connecting to ArangoDB")?;
        Ok(ServerInfo { version: format!("ArangoDB {}", v["version"].as_str().unwrap_or("?")) })
    }

    async fn list_children(&self, path: &[String]) -> Result<Vec<TreeNode>> {
        Ok(match path {
            [] => {
                let v = self.http.json(Method::GET, "/_api/database/user", Body::None).await?;
                v["result"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|n| TreeNode::new(path, n, "database", true))
                    .collect()
            }
            [db] => {
                let v = self
                    .http
                    .json(Method::GET, &Self::db_path(db, "/_api/collection?excludeSystem=true"), Body::None)
                    .await?;
                let mut nodes: Vec<TreeNode> = v["result"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|c| {
                        let name = c["name"].as_str()?;
                        let kind = if c["type"].as_i64() == Some(3) { "edge" } else { "collection" };
                        Some(TreeNode::new(path, name, kind, true).detail(if kind == "edge" { "edge" } else { "" }))
                    })
                    .collect();
                nodes.sort_by(|a, b| a.name.cmp(&b.name));
                nodes
            }
            [db, coll] => {
                let v = self
                    .http
                    .json(Method::GET, &Self::db_path(db, &format!("/_api/index?collection={}", encode(coll))), Body::None)
                    .await?;
                v["indexes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|ix| {
                        let fields = ix["fields"]
                            .as_array()
                            .map(|f| f.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
                            .unwrap_or_default();
                        let name = ix["name"].as_str().unwrap_or("index");
                        TreeNode::new(path, name, "index", false)
                            .detail(format!("{} ({fields})", ix["type"].as_str().unwrap_or("")))
                    })
                    .collect()
            }
            _ => Vec::new(),
        })
    }

    async fn execute(&self, query: &str, ctx: &ExecContext) -> Result<Vec<ResultSet>> {
        let db = ctx.database().unwrap_or(&self.default_db).to_string();
        let limit = ctx.limit();
        let mut sets = Vec::new();
        for stmt in split_statements(query) {
            let body = json!({ "query": stmt, "batchSize": limit.min(1000), "count": false });
            let (docs, truncated, writes) = self.cursor(&db, body, limit).await?;
            let mut rs = if docs.is_empty() && writes > 0 {
                ResultSet { affected: Some(writes), ..Default::default() }
            } else {
                docs_to_resultset(docs)
            };
            rs.truncated = truncated;
            sets.push(rs);
        }
        Ok(sets)
    }

    async fn object_query(&self, path: &[String]) -> Result<ObjectQuery> {
        let [db, coll, ..] = path else { bail!("Select a collection") };
        Ok(ObjectQuery {
            query: format!("FOR doc IN `{coll}`\n  SORT doc._key DESC\n  LIMIT 100\n  RETURN doc"),
            database: Some(db.clone()),
            schema: None,
        })
    }

    async fn edit_info(&self, path: &[String]) -> Result<EditInfo> {
        let [_, _, ..] = path else { bail!("Select a collection") };
        let described = self.describe(path).await.ok();
        let mut info = document_edit_info(described.as_ref(), &["_key"]);
        info.upsert_note = Some("ArangoDB matches existing documents on _key – map a column to _key.".into());
        info.reason = Some("Changes are saved one by one.".into());
        Ok(info)
    }

    async fn apply_changes(&self, path: &[String], changes: &[RowChange], dry_run: bool) -> Result<ApplyReport> {
        let [db, coll, ..] = path else { bail!("Select a collection") };
        let doc_path = |key: &Map<String, Value>| -> Result<String> {
            let k = key.get("_key").and_then(Value::as_str).ok_or_else(|| anyhow!("The document has no _key – refresh and try again."))?;
            Ok(Self::db_path(db, &format!("/_api/document/{}/{}", encode(coll), encode(k))))
        };
        let mut report = ApplyReport::default();
        let mut plan = Vec::new();
        for c in changes {
            let (method, url, body) = match c {
                RowChange::Update { key, values } => (Method::PATCH, format!("{}?keepNull=true", doc_path(key)?), Some(Value::Object(strip_meta(values)))),
                RowChange::Insert { values } => (Method::POST, Self::db_path(db, &format!("/_api/document/{}", encode(coll))), Some(Value::Object(values.clone()))),
                RowChange::Delete { key } => (Method::DELETE, doc_path(key)?, None),
            };
            report.statements.push(format!("{method} {url}{}", body.as_ref().map(|b| format!("\n{b}")).unwrap_or_default()));
            plan.push((method, url, body));
        }
        if dry_run {
            return Ok(report);
        }
        for (i, (method, url, body)) in plan.into_iter().enumerate() {
            let res = self.http.json(method, &url, body.map(Body::Json).unwrap_or(Body::None)).await;
            if let Err(e) = res {
                let msg = format!("{e:#}");
                let msg = if msg.contains("HTTP 404") { "document not found – it may have been changed or deleted. Refresh and try again.".to_string() } else { msg };
                report.error = Some(format!("Change {} of {}: {msg}\n{i} earlier change(s) were saved.", i + 1, changes.len()));
                return Ok(report);
            }
            report.applied = i + 1;
        }
        Ok(report)
    }

    async fn import_rows(
        &self,
        path: &[String],
        columns: &[String],
        rows: Vec<ImportRow>,
        opts: &ImportOptions,
        ctx: &ImportCtx<'_>,
    ) -> Result<ImportReport> {
        let [db, coll, ..] = path else { bail!("Select a collection") };
        if opts.mode == ImportMode::Upsert && !columns.iter().any(|c| c == "_key") {
            bail!("ArangoDB updates existing documents by _key – map a column to _key.");
        }
        let on_duplicate = if opts.mode == ImportMode::Upsert { "update" } else { "error" };
        let url = Self::db_path(
            db,
            &format!("/_api/import?collection={}&type=list&details=true&createCollection=true&onDuplicate={on_duplicate}", encode(coll)),
        );
        let mut report = ImportReport::default();
        let mut done = 0;
        for chunk in rows.chunks(opts.batch(2000)) {
            if ctx.cancelled() {
                report.cancelled = true;
                break;
            }
            let mut sent: Vec<usize> = Vec::new();
            let mut docs = Vec::new();
            for (row, values) in chunk {
                match build_doc(columns, values.clone()) {
                    Ok(d) => {
                        sent.push(*row);
                        docs.push(Value::Object(d));
                    }
                    Err(e) => report.fail(*row, format!("{e:#}")),
                }
            }
            if !docs.is_empty() {
                let res = self.http.json(Method::POST, &url, Body::Json(Value::Array(docs))).await?;
                let n = |k: &str| res.get(k).and_then(Value::as_u64).unwrap_or(0);
                report.written += n("created") + n("updated");
                for detail in res.get("details").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                    // "at position 3: creating document failed with error 'unique constraint violated' …"
                    let pos: Option<usize> = detail.strip_prefix("at position ").and_then(|r| r.split(':').next()).and_then(|p| p.trim().parse().ok());
                    let row = pos.and_then(|p| sent.get(p).copied()).unwrap_or(0);
                    let msg = detail.split_once(": ").map(|(_, m)| m).unwrap_or(detail);
                    report.fail(row, msg.split(", offending document").next().unwrap_or(msg).to_string());
                }
            }
            done += chunk.len();
            (ctx.progress)(done);
        }
        report.errors.sort_by_key(|e| e.row);
        Ok(report)
    }

    async fn describe(&self, path: &[String]) -> Result<ResultSet> {
        let [db, coll, ..] = path else { bail!("Select a collection") };
        let body = json!({
            "query": "FOR d IN @@c LIMIT 200 RETURN d",
            "bindVars": obj(vec![("@c", Value::String(coll.clone()))]),
        });
        let (docs, _, _) = self.cursor(db, body, 200).await?;
        Ok(infer_schema(&docs))
    }
}
