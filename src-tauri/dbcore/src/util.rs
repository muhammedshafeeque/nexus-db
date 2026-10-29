use crate::model::{Column, EditColumn, EditInfo, ResultSet};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Turns a list of JSON documents into a grid: columns are the union of
/// top-level keys in first-seen order, nested values stay as JSON.
pub fn docs_to_resultset(docs: Vec<Value>) -> ResultSet {
    let mut columns: Vec<String> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = Default::default();
    let mut has_scalar = false;

    for doc in &docs {
        match doc {
            Value::Object(map) => {
                for key in map.keys() {
                    if !index.contains_key(key) {
                        index.insert(key.clone(), columns.len());
                        columns.push(key.clone());
                    }
                }
            }
            _ => has_scalar = true,
        }
    }
    if has_scalar && !index.contains_key("value") {
        index.insert("value".into(), columns.len());
        columns.push("value".into());
    }

    let rows = docs
        .into_iter()
        .map(|doc| {
            let mut row = vec![Value::Null; columns.len()];
            match doc {
                Value::Object(map) => {
                    for (k, v) in map {
                        row[index[&k]] = v;
                    }
                }
                other => row[index["value"]] = other,
            }
            row
        })
        .collect();

    ResultSet {
        columns: columns.into_iter().map(|c| Column::new(c, "")).collect(),
        rows,
        documents: true,
        ..Default::default()
    }
}

pub fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "double",
        Value::Number(_) => "int",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Infers a "schema" from sampled documents: field, observed types, presence.
pub fn infer_schema(docs: &[Value]) -> ResultSet {
    let mut fields: Vec<String> = Vec::new();
    let mut stats: BTreeMap<String, (usize, BTreeMap<&'static str, usize>)> = BTreeMap::new();
    for doc in docs {
        if let Value::Object(map) = doc {
            for (k, v) in map {
                if !stats.contains_key(k) {
                    fields.push(k.clone());
                }
                let e = stats.entry(k.clone()).or_default();
                e.0 += 1;
                *e.1.entry(json_type_name(v)).or_default() += 1;
            }
        }
    }
    let total = docs.len().max(1) as f64;
    let rows = fields
        .into_iter()
        .map(|f| {
            let (count, types) = &stats[&f];
            let types = types.keys().cloned().collect::<Vec<_>>().join(" | ");
            vec![
                Value::String(f),
                Value::String(types),
                Value::String(format!("{:.0}%", *count as f64 * 100.0 / total)),
            ]
        })
        .collect();
    let mut rs = ResultSet::table(
        vec![Column::new("field", "string"), Column::new("types", "string"), Column::new("presence", "string")],
        rows,
    );
    rs.message = Some(format!("Inferred from a sample of {} documents", docs.len()));
    rs
}

/// Splits a script on top-level `;`, respecting quotes, comments and brackets.
pub fn split_statements(src: &str) -> Vec<String> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '\'' | '"' | '`' => {
                cur.push(c);
                i += 1;
                while i < chars.len() {
                    cur.push(chars[i]);
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        cur.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    if chars[i] == c {
                        break;
                    }
                    i += 1;
                }
            }
            '-' if next == Some('-') => {
                while i < chars.len() && chars[i] != '\n' {
                    cur.push(chars[i]);
                    i += 1;
                }
                continue;
            }
            '/' if next == Some('*') => {
                while i < chars.len() {
                    cur.push(chars[i]);
                    if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                        cur.push('/');
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            '(' | '[' | '{' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                cur.push(c);
            }
            ';' if depth <= 0 => {
                if !strip_comments(&cur).trim().is_empty() {
                    out.push(cur.trim().to_string());
                }
                cur.clear();
            }
            _ => cur.push(c),
        }
        i += 1;
    }
    if !strip_comments(&cur).trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn strip_comments(s: &str) -> String {
    s.lines()
        .map(|l| l.trim())
        .filter(|l| !l.starts_with("--") && !l.starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn first_keyword(stmt: &str) -> String {
    strip_comments(stmt)
        .trim_start_matches(|c: char| c.is_whitespace() || c == '(')
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase()
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn bytes_value(bytes: &[u8]) -> Value {
    match std::str::from_utf8(bytes) {
        Ok(s) => Value::String(s.to_string()),
        Err(_) => Value::String(hex(bytes)),
    }
}

pub fn parse_number(s: &str) -> Value {
    if let Ok(i) = s.parse::<i64>() {
        return Value::from(i);
    }
    if let Ok(u) = s.parse::<u64>() {
        return Value::from(u);
    }
    match s.parse::<f64>().ok().and_then(serde_json::Number::from_f64) {
        Some(n) => Value::Number(n),
        None => Value::String(s.to_string()),
    }
}

pub fn obj(pairs: Vec<(&str, Value)>) -> Value {
    let mut m = Map::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), v);
    }
    Value::Object(m)
}

/// Quotes an identifier with the given quote char, doubling embedded quotes.
pub fn quote_ident(name: &str, q: char) -> String {
    let close = if q == '[' { ']' } else { q };
    let escaped = name.replace(close, &format!("{close}{close}"));
    format!("{q}{escaped}{close}")
}

pub fn sql_lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Simple identifier: usable unquoted in JS / AQL.
pub fn is_simple_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_respecting_strings_and_comments() {
        let s = "select ';' as a; -- comment; here\nselect 2;\n/* x; y */ select 3";
        let parts = split_statements(s);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "select ';' as a");
    }

    #[test]
    fn docs_union_columns() {
        let rs = docs_to_resultset(vec![
            serde_json::json!({"a": 1, "b": 2}),
            serde_json::json!({"b": 3, "c": 4}),
        ]);
        let cols: Vec<_> = rs.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(cols, ["a", "b", "c"]);
        assert_eq!(rs.rows[1][0], Value::Null);
    }
}

/// Editing / import info for a document store, with known fields taken from
/// the first column of a `describe` result (sampled or mapped fields).
pub fn document_edit_info(described: Option<&ResultSet>, key_columns: &[&str]) -> EditInfo {
    let mut columns: Vec<EditColumn> = key_columns
        .iter()
        .map(|k| EditColumn { name: k.to_string(), key: true, nullable: false, ..Default::default() })
        .collect();
    for row in described.map(|rs| rs.rows.as_slice()).unwrap_or_default() {
        let name = row.first().and_then(Value::as_str).unwrap_or_default().to_string();
        if name.is_empty() || columns.iter().any(|c| c.name == name) {
            continue;
        }
        let type_name = row.get(1).and_then(Value::as_str).unwrap_or_default().to_string();
        columns.push(EditColumn { name, type_name, nullable: true, ..Default::default() });
    }
    EditInfo {
        columns,
        key_columns: key_columns.iter().map(|k| k.to_string()).collect(),
        editable: true,
        documents: true,
        can_upsert: true,
        ..Default::default()
    }
}
