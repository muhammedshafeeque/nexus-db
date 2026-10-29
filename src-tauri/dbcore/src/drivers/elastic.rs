use super::http::{Body, Http};
use crate::model::*;
use crate::tabular::build_doc;
use crate::util::{docs_to_resultset, document_edit_info, obj};
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use reqwest::Method;
use serde_json::{json, Map, Value};
use urlencoding::encode;

/// Elasticsearch & OpenSearch, queried with Kibana-console syntax:
///
/// ```text
/// GET /my-index/_search
/// { "query": { "match_all": {} } }
/// ```
pub struct ElasticDriver {
    http: Http,
}

struct Request {
    method: Method,
    path: String,
    body: String,
}

fn parse_requests(src: &str) -> Result<Vec<Request>> {
    let mut out: Vec<Request> = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let head = parts.next().unwrap_or("").to_ascii_uppercase();
        if matches!(head.as_str(), "GET" | "POST" | "PUT" | "DELETE" | "HEAD" | "PATCH") {
            let mut path = parts.next().unwrap_or("/").trim().to_string();
            if !path.starts_with('/') {
                path.insert(0, '/');
            }
            out.push(Request { method: Method::from_bytes(head.as_bytes())?, path, body: String::new() });
        } else if let Some(req) = out.last_mut() {
            req.body.push_str(line);
            req.body.push('\n');
        } else if !trimmed.is_empty() {
            bail!("Requests must start with a method, e.g. `GET /_cat/indices`");
        }
    }
    Ok(out)
}

fn flatten_mapping(props: &Map<String, Value>, prefix: &str, out: &mut Vec<(String, String)>) {
    for (name, def) in props {
        let full = if prefix.is_empty() { name.clone() } else { format!("{prefix}.{name}") };
        let ty = def["type"].as_str().unwrap_or("object").to_string();
        out.push((full.clone(), ty));
        if let Some(Value::Object(sub)) = def.get("properties") {
            flatten_mapping(sub, &full, out);
        }
    }
}

impl ElasticDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let driver = Self { http: Http::new(cfg, 9200)? };
        driver.server_info().await?;
        Ok(driver)
    }

    async fn fields(&self, index: &str) -> Result<Vec<(String, String)>> {
        let v = self.http.json(Method::GET, &format!("/{}/_mapping", encode(index)), Body::None).await?;
        let mut out = Vec::new();
        if let Some(Value::Object(indices)) = Some(v) {
            for (_, def) in indices {
                if let Some(Value::Object(props)) = def.pointer("/mappings/properties") {
                    flatten_mapping(props, "", &mut out);
                }
            }
        }
        Ok(out)
    }

    fn response_to_sets(v: Value, limit: usize) -> Vec<ResultSet> {
        if let Some(Value::Array(hits)) = v.pointer("/hits/hits") {
            let docs: Vec<Value> = hits
                .iter()
                .take(limit)
                .map(|h| {
                    let mut m = Map::new();
                    m.insert("_id".into(), h["_id"].clone());
                    m.insert("_index".into(), h["_index"].clone());
                    if let Value::Object(src) = &h["_source"] {
                        for (k, val) in src {
                            m.insert(k.clone(), val.clone());
                        }
                    }
                    Value::Object(m)
                })
                .collect();
            let total = v.pointer("/hits/total/value").or_else(|| v.pointer("/hits/total")).cloned().unwrap_or(Value::Null);
            let mut rs = docs_to_resultset(docs);
            rs.message = Some(format!("{total} total hits · took {}ms", v["took"]));
            let mut sets = vec![rs];
            if let Some(Value::Object(aggs)) = v.get("aggregations") {
                for (name, agg) in aggs {
                    let mut set = match agg.get("buckets") {
                        Some(Value::Array(buckets)) => docs_to_resultset(buckets.clone()),
                        _ => docs_to_resultset(vec![agg.clone()]),
                    };
                    set.message = Some(format!("aggregation: {name}"));
                    sets.push(set);
                }
            }
            return sets;
        }
        match v {
            Value::Array(items) => vec![docs_to_resultset(items)],
            Value::Object(_) => vec![docs_to_resultset(vec![v])],
            other => vec![ResultSet::table(vec![Column::new("response", "")], vec![vec![other]])],
        }
    }
}

#[async_trait]
impl Driver for ElasticDriver {
    async fn server_info(&self) -> Result<ServerInfo> {
        let v = self.http.json(Method::GET, "/", Body::None).await?;
        let dist = v.pointer("/version/distribution").and_then(Value::as_str).unwrap_or("Elasticsearch");
        let ver = v.pointer("/version/number").and_then(Value::as_str).unwrap_or("?");
        let dist = if dist == "opensearch" { "OpenSearch" } else { dist };
        Ok(ServerInfo { version: format!("{dist} {ver}") })
    }

    async fn list_children(&self, path: &[String]) -> Result<Vec<TreeNode>> {
        Ok(match path {
            [] => {
                let v = self
                    .http
                    .json(Method::GET, "/_cat/indices?format=json&h=index,docs.count,health&s=index", Body::None)
                    .await?;
                v.as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|ix| {
                        let name = ix["index"].as_str()?;
                        if name.starts_with('.') {
                            return None;
                        }
                        let docs = ix["docs.count"].as_str().unwrap_or("0");
                        Some(TreeNode::new(path, name, "collection", true).detail(format!("{docs} docs")))
                    })
                    .collect()
            }
            [index] => self
                .fields(index)
                .await?
                .into_iter()
                .map(|(f, t)| TreeNode::new(path, f, "column", false).detail(t))
                .collect(),
            _ => Vec::new(),
        })
    }

    async fn execute(&self, query: &str, ctx: &ExecContext) -> Result<Vec<ResultSet>> {
        let mut sets = Vec::new();
        for mut req in parse_requests(query)? {
            if req.path.starts_with("/_cat") && !req.path.contains("format=") {
                req.path.push_str(if req.path.contains('?') { "&format=json" } else { "?format=json" });
            }
            let body = req.body.trim();
            let body = if body.is_empty() {
                Body::None
            } else if req.path.contains("_bulk") || req.path.contains("_msearch") {
                Body::Text(format!("{body}\n"), "application/x-ndjson")
            } else {
                Body::Text(body.to_string(), "application/json")
            };
            let v = self.http.json(req.method, &req.path, body).await?;
            sets.extend(Self::response_to_sets(v, ctx.limit()));
        }
        Ok(sets)
    }

    async fn object_query(&self, path: &[String]) -> Result<ObjectQuery> {
        let [index, ..] = path else { bail!("Select an index") };
        Ok(ObjectQuery {
            query: format!("GET /{index}/_search\n{{\n  \"query\": {{ \"match_all\": {{}} }},\n  \"size\": 100\n}}"),
            ..Default::default()
        })
    }

    async fn edit_info(&self, path: &[String]) -> Result<EditInfo> {
        let [_, ..] = path else { bail!("Select an index") };
        // a new index (import target) has no mapping yet
        let described = self.describe(path).await.ok();
        let mut info = document_edit_info(described.as_ref(), &["_id", "_index"]);
        info.key_columns = vec!["_id".into(), "_index".into()];
        info.upsert_note = Some("Elasticsearch matches existing documents on _id – map a column to _id.".into());
        info.reason = Some("Changes are saved one by one and refreshed right away.".into());
        Ok(info)
    }

    async fn apply_changes(&self, path: &[String], changes: &[RowChange], dry_run: bool) -> Result<ApplyReport> {
        let [default_index, ..] = path else { bail!("Select an index") };
        // a search over an alias returns each hit's concrete index – write there
        let index_of = |key: &Map<String, Value>| key.get("_index").and_then(Value::as_str).unwrap_or(default_index).to_string();
        let id_of = |key: &Map<String, Value>| key.get("_id").and_then(Value::as_str).map(str::to_string).ok_or_else(|| anyhow!("The document has no _id – refresh and try again."));
        let clean = |m: &Map<String, Value>| -> Map<String, Value> { m.iter().filter(|(k, _)| !matches!(k.as_str(), "_id" | "_index")).map(|(k, v)| (k.clone(), v.clone())).collect() };
        let mut report = ApplyReport::default();
        let mut plan = Vec::new();
        for c in changes {
            let (method, url, body) = match c {
                RowChange::Update { key, values } => (
                    Method::POST,
                    format!("/{}/_update/{}?refresh=wait_for", encode(&index_of(key)), encode(&id_of(key)?)),
                    Some(json!({ "doc": clean(values) })),
                ),
                RowChange::Insert { values } => match values.get("_id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    Some(id) => (Method::PUT, format!("/{}/_create/{}?refresh=wait_for", encode(default_index), encode(id)), Some(Value::Object(clean(values)))),
                    None => (Method::POST, format!("/{}/_doc?refresh=wait_for", encode(default_index)), Some(Value::Object(clean(values)))),
                },
                RowChange::Delete { key } => (Method::DELETE, format!("/{}/_doc/{}?refresh=wait_for", encode(&index_of(key)), encode(&id_of(key)?)), None),
            };
            report.statements.push(format!("{method} {url}{}", body.as_ref().map(|b| format!("\n{b}")).unwrap_or_default()));
            plan.push((method, url, body));
        }
        if dry_run {
            return Ok(report);
        }
        for (i, (method, url, body)) in plan.into_iter().enumerate() {
            if let Err(e) = self.http.json(method, &url, body.map(Body::Json).unwrap_or(Body::None)).await {
                report.error = Some(format!("Change {} of {}: {e:#}\n{i} earlier change(s) were saved.", i + 1, changes.len()));
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
        let [index, ..] = path else { bail!("Select an index") };
        let id_col = columns.iter().position(|c| c == "_id");
        if opts.mode == ImportMode::Upsert && id_col.is_none() {
            bail!("Elasticsearch updates existing documents by _id – map a column to _id.");
        }
        let mut report = ImportReport::default();
        let mut done = 0;
        for chunk in rows.chunks(opts.batch(2000)) {
            if ctx.cancelled() {
                report.cancelled = true;
                break;
            }
            let mut body = String::new();
            let mut sent = Vec::new();
            for (row, values) in chunk {
                let id = id_col.and_then(|i| match &values[i] {
                    Value::Null => None,
                    Value::String(s) => Some(s.clone()),
                    v => Some(v.to_string()),
                });
                let mut doc = match build_doc(columns, values.clone()) {
                    Ok(d) => d,
                    Err(e) => {
                        report.fail(*row, format!("{e:#}"));
                        continue;
                    }
                };
                doc.remove("_id");
                let (action, source) = match (opts.mode, id) {
                    (ImportMode::Upsert, Some(id)) => (json!({ "update": { "_index": index, "_id": id } }), json!({ "doc": doc, "doc_as_upsert": true })),
                    (ImportMode::Upsert, None) => {
                        report.fail(*row, "_id is empty");
                        continue;
                    }
                    (_, Some(id)) => (json!({ "create": { "_index": index, "_id": id } }), Value::Object(doc)),
                    (_, None) => (json!({ "index": { "_index": index } }), Value::Object(doc)),
                };
                body.push_str(&format!("{action}\n{source}\n"));
                sent.push(*row);
            }
            if !sent.is_empty() {
                let res = self.http.json(Method::POST, "/_bulk", Body::Text(body, "application/x-ndjson")).await?;
                for (i, item) in res.get("items").and_then(Value::as_array).into_iter().flatten().enumerate() {
                    let op = item.as_object().and_then(|o| o.values().next()).cloned().unwrap_or(Value::Null);
                    match op.get("error") {
                        Some(err) => {
                            let reason = err.get("reason").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| err.to_string());
                            report.fail(sent.get(i).copied().unwrap_or(0), reason);
                        }
                        None => report.written += 1,
                    }
                }
            }
            done += chunk.len();
            (ctx.progress)(done);
        }
        // make the documents searchable right away
        let _ = self.http.json(Method::POST, &format!("/{}/_refresh", encode(index)), Body::None).await;
        Ok(report)
    }

    async fn describe(&self, path: &[String]) -> Result<ResultSet> {
        let [index, ..] = path else { bail!("Select an index") };
        let docs = self
            .fields(index)
            .await?
            .into_iter()
            .map(|(f, t)| obj(vec![("field", Value::String(f)), ("type", Value::String(t))]))
            .collect();
        Ok(docs_to_resultset(docs))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_console_syntax() {
        let reqs = super::parse_requests("GET /a/_search\n{\n \"size\": 1\n}\n\nGET _cat/indices").unwrap();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[1].path, "/_cat/indices");
        assert!(reqs[0].body.contains("size"));
    }
}
