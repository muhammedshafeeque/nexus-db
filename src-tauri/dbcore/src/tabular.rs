//! Spreadsheet files: reading CSV / TSV / Excel / ODS for imports and writing
//! XLSX for exports, plus the value clean-up shared by every import target.

use crate::model::Column;
use anyhow::{anyhow, bail, Context, Result};
use calamine::{open_workbook_auto, Data, Reader};
use chrono::{NaiveDate, NaiveDateTime, Timelike};
use rust_xlsxwriter::{ExcelDateTime, Format, Workbook};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::path::Path;

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct FileOptions {
    pub path: String,
    /// Excel / ODS sheet; the first one when empty.
    pub sheet: Option<String>,
    /// First row holds column names (default true).
    pub has_header: Option<bool>,
    /// CSV delimiter; detected when empty.
    pub delimiter: Option<String>,
    /// CSV text encoding label (utf-8, windows-1252, utf-16le …); detected when empty.
    pub encoding: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Table {
    /// Sheets of a workbook (empty for CSV).
    pub sheets: Vec<String>,
    pub sheet: Option<String>,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    /// 1-based file row of `rows[0]`, for error messages.
    pub first_row: usize,
    pub delimiter: Option<String>,
    pub encoding: Option<String>,
    /// Cells with Excel errors (#N/A, #DIV/0! …), read as empty.
    pub error_cells: usize,
}

fn is_csv(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    [".csv", ".tsv", ".txt", ".tab"].iter().any(|e| p.ends_with(e))
}

pub fn read_table(opts: &FileOptions) -> Result<Table> {
    if opts.path.trim().is_empty() {
        bail!("Choose a file");
    }
    if !Path::new(&opts.path).exists() {
        bail!("File not found: {}", opts.path);
    }
    if is_csv(&opts.path) {
        read_csv(opts)
    } else {
        read_workbook(opts)
    }
}

// ---------------------------------------------------------------- CSV

fn decode(bytes: &[u8], label: Option<&str>) -> Result<(String, String)> {
    if let Some(label) = label.filter(|l| !l.trim().is_empty()) {
        let enc = encoding_rs::Encoding::for_label(label.trim().as_bytes()).ok_or_else(|| anyhow!("Unknown encoding \"{label}\""))?;
        let (text, _, _) = enc.decode(bytes);
        return Ok((text.into_owned(), enc.name().to_string()));
    }
    // a BOM decides; then strict UTF-8; legacy Windows exports fall back to Windows-1252
    if let Some((enc, _)) = encoding_rs::Encoding::for_bom(bytes) {
        let (text, _, _) = enc.decode(bytes);
        return Ok((text.into_owned(), enc.name().to_string()));
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok((s.to_string(), "UTF-8".into())),
        Err(_) => {
            let (text, _, _) = encoding_rs::WINDOWS_1252.decode(bytes);
            Ok((text.into_owned(), "windows-1252".into()))
        }
    }
}

/// Picks the candidate that splits the first lines into the same, largest number of fields.
fn detect_delimiter(text: &str, path: &str) -> u8 {
    if path.to_ascii_lowercase().ends_with(".tsv") || path.to_ascii_lowercase().ends_with(".tab") {
        return b'\t';
    }
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).take(20).collect();
    let count = |line: &str, d: char| {
        let mut n = 0;
        let mut quoted = false;
        for c in line.chars() {
            if c == '"' {
                quoted = !quoted;
            } else if c == d && !quoted {
                n += 1;
            }
        }
        n
    };
    let mut best = (b',', 0usize, false);
    for d in [',', ';', '\t', '|'] {
        let counts: Vec<usize> = lines.iter().map(|l| count(l, d)).collect();
        let first = counts.first().copied().unwrap_or(0);
        let consistent = first > 0 && counts.iter().all(|c| *c == first);
        if (consistent, first) > (best.2, best.1) {
            best = (d as u8, first, consistent);
        }
    }
    best.0
}

fn read_csv(opts: &FileOptions) -> Result<Table> {
    let bytes = std::fs::read(&opts.path).with_context(|| format!("reading {}", opts.path))?;
    let (text, encoding) = decode(&bytes, opts.encoding.as_deref())?;
    let delimiter = match opts.delimiter.as_deref() {
        Some("\\t") | Some("tab") => b'\t',
        Some(d) if d.len() == 1 => d.as_bytes()[0],
        Some(d) if !d.is_empty() => bail!("The delimiter must be one character"),
        _ => detect_delimiter(&text, &opts.path),
    };
    let mut reader = csv::ReaderBuilder::new().delimiter(delimiter).has_headers(false).flexible(true).from_reader(text.as_bytes());
    let mut records: Vec<(usize, Vec<String>)> = Vec::new();
    for (i, rec) in reader.records().enumerate() {
        let rec = rec.with_context(|| format!("parsing line {}", i + 1))?;
        let line = rec.position().map(|p| p.line() as usize).unwrap_or(i + 1);
        records.push((line, rec.iter().map(str::to_string).collect()));
    }
    records.retain(|(_, r)| r.iter().any(|v| !v.trim().is_empty()));
    let raw: Vec<(usize, Vec<Value>)> = records.into_iter().map(|(l, r)| (l, r.into_iter().map(Value::String).collect())).collect();
    let mut table = build_table(raw, opts.has_header.unwrap_or(true));
    table.delimiter = Some(if delimiter == b'\t' { "\\t".into() } else { (delimiter as char).to_string() });
    table.encoding = Some(encoding);
    Ok(table)
}

// ---------------------------------------------------------------- workbooks

fn read_workbook(opts: &FileOptions) -> Result<Table> {
    let lower = opts.path.to_ascii_lowercase();
    if lower.ends_with(".numbers") {
        bail!("Apple Numbers files are not supported – export to .xlsx or .csv first.");
    }
    let mut book = open_workbook_auto(&opts.path).map_err(|e| {
        let msg = e.to_string();
        if msg.to_ascii_lowercase().contains("password") || msg.to_ascii_lowercase().contains("encrypt") {
            anyhow!("The workbook is password-protected – save an unprotected copy first.")
        } else {
            anyhow!("Could not open {}: {msg}", opts.path)
        }
    })?;
    let sheets = book.sheet_names();
    if sheets.is_empty() {
        bail!("The workbook has no sheets");
    }
    let sheet = match opts.sheet.as_deref().filter(|s| !s.is_empty()) {
        Some(s) if sheets.iter().any(|n| n == s) => s.to_string(),
        Some(s) => bail!("Sheet \"{s}\" not found"),
        None => sheets[0].clone(),
    };
    let range = book.worksheet_range(&sheet).map_err(|e| anyhow!("Reading sheet \"{sheet}\": {e}"))?;
    let start_row = range.start().map(|(r, _)| r as usize).unwrap_or(0);
    let mut error_cells = 0;
    let raw: Vec<(usize, Vec<Value>)> = range
        .rows()
        .enumerate()
        .map(|(i, row)| {
            let values = row
                .iter()
                .map(|c| {
                    if matches!(c, Data::Error(_)) {
                        error_cells += 1;
                    }
                    excel_value(c)
                })
                .collect::<Vec<_>>();
            (start_row + i + 1, values)
        })
        .filter(|(_, r)| r.iter().any(|v| !is_blank(v)))
        .collect();
    let mut table = build_table(raw, opts.has_header.unwrap_or(true));
    table.sheets = sheets;
    table.sheet = Some(sheet);
    table.error_cells = error_cells;
    Ok(table)
}

fn excel_value(c: &Data) -> Value {
    match c {
        Data::Empty | Data::Error(_) => Value::Null,
        Data::String(s) => Value::String(s.clone()),
        Data::Bool(b) => Value::Bool(*b),
        Data::Int(i) => Value::from(*i),
        Data::Float(f) => float_value(*f),
        Data::DateTime(d) => {
            if d.is_duration() {
                return Value::String(d.as_duration().map(|x| format!("{}s", x.num_seconds())).unwrap_or_default());
            }
            match d.as_datetime() {
                Some(dt) if dt.time().num_seconds_from_midnight() == 0 && dt.nanosecond() == 0 => {
                    Value::String(dt.date().format("%Y-%m-%d").to_string())
                }
                Some(dt) => Value::String(dt.format("%Y-%m-%d %H:%M:%S").to_string()),
                None => Value::from(d.as_f64()),
            }
        }
        Data::DateTimeIso(s) | Data::DurationIso(s) => Value::String(s.clone()),
    }
}

/// Excel stores every number as a float: 42.0 becomes 42.
fn float_value(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 {
        Value::from(f as i64)
    } else {
        serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null)
    }
}

fn is_blank(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        _ => false,
    }
}

/// Header row → unique, non-empty column names; pads ragged rows.
fn build_table(mut raw: Vec<(usize, Vec<Value>)>, has_header: bool) -> Table {
    let width = raw.iter().map(|(_, r)| r.len()).max().unwrap_or(0);
    let header: Vec<String> = if has_header && !raw.is_empty() {
        let (_, h) = raw.remove(0);
        (0..width)
            .map(|i| match h.get(i) {
                Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
                Some(Value::Null) | None => String::new(),
                Some(v) if !is_blank(v) => v.to_string(),
                _ => String::new(),
            })
            .collect()
    } else {
        vec![String::new(); width]
    };
    let mut seen = HashSet::new();
    let columns = header
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            let base = if name.is_empty() { format!("column_{}", i + 1) } else { name };
            let mut name = base.clone();
            let mut n = 2;
            while !seen.insert(name.to_lowercase()) {
                name = format!("{base}_{n}");
                n += 1;
            }
            name
        })
        .collect();
    let first_row = raw.first().map(|(l, _)| *l).unwrap_or(1);
    let rows = raw
        .into_iter()
        .map(|(_, mut r)| {
            r.resize(width, Value::Null);
            r
        })
        .collect();
    Table { columns, rows, first_row, ..Default::default() }
}

// ---------------------------------------------------------------- value clean-up

#[derive(Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct ValueOptions {
    pub trim: bool,
    /// Empty cells become NULL (documents leave the field out).
    pub empty_as_null: bool,
    /// Text that looks like a number / boolean becomes one (for document stores).
    pub detect_types: bool,
}

impl Default for ValueOptions {
    fn default() -> Self {
        Self { trim: true, empty_as_null: true, detect_types: false }
    }
}

pub fn prepare(v: &Value, o: ValueOptions) -> Value {
    match v {
        Value::String(s) => {
            let s = if o.trim { s.trim() } else { s.as_str() };
            if s.is_empty() && o.empty_as_null {
                Value::Null
            } else if o.detect_types {
                detect_type(s)
            } else {
                Value::String(s.to_string())
            }
        }
        other => other.clone(),
    }
}

/// "42" → 42, "3.5" → 3.5, "true" → true. Leading zeros ("007", zip codes) and
/// numbers JSON cannot hold exactly stay text.
pub fn detect_type(s: &str) -> Value {
    match s.to_ascii_lowercase().as_str() {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    let digits = s.strip_prefix('-').unwrap_or(s);
    let leading_zero = digits.len() > 1 && digits.starts_with('0') && !digits.starts_with("0.");
    if !digits.is_empty() && !leading_zero && digits.chars().all(|c| c.is_ascii_digit()) && digits.len() <= 15 {
        if let Ok(i) = s.parse::<i64>() {
            return Value::from(i);
        }
    }
    if !leading_zero && digits.contains('.') && digits.chars().all(|c| c.is_ascii_digit() || c == '.') && digits.len() <= 17 {
        if let Ok(f) = s.parse::<f64>() {
            if let Some(n) = serde_json::Number::from_f64(f) {
                return Value::Number(n);
            }
        }
    }
    Value::String(s.to_string())
}

/// Columns → document; `a.b` keys nest, NULLs are left out.
pub fn build_doc(columns: &[String], values: Vec<Value>) -> Result<Map<String, Value>> {
    let mut doc = Map::new();
    for (name, v) in columns.iter().zip(values) {
        if v.is_null() {
            continue;
        }
        let parts: Vec<&str> = name.split('.').filter(|p| !p.is_empty()).collect();
        let Some((last, parents)) = parts.split_last() else { continue };
        let mut cur = &mut doc;
        for p in parents {
            let entry = cur.entry(p.to_string()).or_insert_with(|| Value::Object(Map::new()));
            cur = entry.as_object_mut().ok_or_else(|| anyhow!("\"{name}\": \"{p}\" is already a value, not an object"))?;
        }
        cur.insert(last.to_string(), v);
    }
    Ok(doc)
}

// ---------------------------------------------------------------- XLSX export

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Sheet {
    pub name: String,
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
}

const MAX_ROWS: usize = 1_048_575; // plus the header row
const MAX_CELL: usize = 32_767;

fn sheet_name(raw: &str, used: &mut HashSet<String>) -> String {
    let cleaned: String = raw.chars().map(|c| if "[]:*?/\\".contains(c) { '_' } else { c }).collect();
    let cleaned = cleaned.trim().trim_matches('\'').to_string();
    let base: String = if cleaned.is_empty() { "Sheet".into() } else { cleaned.chars().take(31).collect() };
    let mut name = base.clone();
    let mut n = 2;
    while !used.insert(name.to_lowercase()) {
        let suffix = format!(" ({n})");
        name = format!("{}{suffix}", base.chars().take(31 - suffix.len()).collect::<String>());
        n += 1;
    }
    name
}

fn excel_datetime(s: &str) -> Option<(ExcelDateTime, bool)> {
    let s = s.trim();
    if s.len() == 10 {
        let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
        return ExcelDateTime::from_ymd(d.format("%Y").to_string().parse().ok()?, d.format("%m").to_string().parse().ok()?, d.format("%d").to_string().parse().ok()?)
            .ok()
            .map(|d| (d, false));
    }
    // only zone-less values: Excel has no time zones and must not shift them silently
    let dt = ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(s, f).ok())?;
    let date = ExcelDateTime::from_ymd(dt.format("%Y").to_string().parse().ok()?, dt.format("%m").to_string().parse().ok()?, dt.format("%d").to_string().parse().ok()?).ok()?;
    let ms = (dt.nanosecond() / 1_000_000) as u16;
    date.and_hms_milli(dt.hour() as u16, dt.minute() as u8, dt.second() as u8, ms).ok().map(|d| (d, true))
}

/// Writes result sets as worksheets: typed numbers / booleans / dates, bold
/// frozen header with filters, and overflow sheets beyond Excel's row limit.
pub fn write_xlsx(path: &str, sheets: &[Sheet]) -> Result<()> {
    let mut book = Workbook::new();
    let header = Format::new().set_bold();
    let date_fmt = Format::new().set_num_format("yyyy-mm-dd");
    let datetime_fmt = Format::new().set_num_format("yyyy-mm-dd hh:mm:ss");
    let mut used = HashSet::new();
    for sheet in sheets {
        let chunks: Vec<&[Vec<Value>]> = if sheet.rows.is_empty() { vec![&[]] } else { sheet.rows.chunks(MAX_ROWS).collect() };
        for (part, rows) in chunks.iter().enumerate() {
            let raw_name = if part == 0 { sheet.name.clone() } else { format!("{} {}", sheet.name, part + 1) };
            let ws = book.add_worksheet();
            ws.set_name(sheet_name(&raw_name, &mut used))?;
            for (c, col) in sheet.columns.iter().enumerate() {
                ws.write_string_with_format(0, c as u16, &col.name, &header)?;
            }
            for (r, row) in rows.iter().enumerate() {
                let r = r as u32 + 1;
                for (c, v) in row.iter().enumerate().take(16_384) {
                    let c = c as u16;
                    match v {
                        Value::Null => {}
                        Value::Bool(b) => {
                            ws.write_boolean(r, c, *b)?;
                        }
                        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
                            // beyond 2^53 Excel would round the number – keep the digits as text
                            (Some(i), _) if i.unsigned_abs() > 9_007_199_254_740_992 => {
                                ws.write_string(r, c, i.to_string())?;
                            }
                            (_, Some(f)) if n.as_u64().map_or(true, |u| u <= 9_007_199_254_740_992) => {
                                ws.write_number(r, c, f)?;
                            }
                            _ => {
                                ws.write_string(r, c, n.to_string())?;
                            }
                        },
                        Value::String(s) => match excel_datetime(s) {
                            Some((dt, true)) => {
                                ws.write_datetime_with_format(r, c, &dt, &datetime_fmt)?;
                            }
                            Some((dt, false)) => {
                                ws.write_datetime_with_format(r, c, &dt, &date_fmt)?;
                            }
                            None => {
                                ws.write_string(r, c, truncate(s))?;
                            }
                        },
                        other => {
                            ws.write_string(r, c, truncate(&other.to_string()))?;
                        }
                    }
                }
            }
            if !sheet.columns.is_empty() {
                ws.set_freeze_panes(1, 0)?;
                ws.autofilter(0, 0, rows.len() as u32, (sheet.columns.len().min(16_384) - 1) as u16)?;
            }
            if rows.len() <= 50_000 {
                ws.autofit();
            }
        }
    }
    book.save(path).with_context(|| format!("writing {path}"))?;
    Ok(())
}

fn truncate(s: &str) -> String {
    if s.chars().count() <= MAX_CELL {
        s.to_string()
    } else {
        s.chars().take(MAX_CELL).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("nexus-tabular-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name).to_string_lossy().into_owned()
    }

    #[test]
    fn csv_with_semicolons_bom_quotes_and_duplicate_headers() {
        let path = tmp("a.csv");
        std::fs::write(&path, "\u{feff}id;name;name;\n1;\"Smith; John\";x;\n\n2;\"multi\nline\";y;z\n").unwrap();
        let t = read_table(&FileOptions { path, ..Default::default() }).unwrap();
        assert_eq!(t.delimiter.as_deref(), Some(";"));
        assert_eq!(t.columns, vec!["id", "name", "name_2", "column_4"]);
        assert_eq!(t.rows.len(), 2, "blank line skipped");
        assert_eq!(t.rows[0][1], json!("Smith; John"));
        assert_eq!(t.rows[1][1], json!("multi\nline"));
        assert_eq!(t.rows[0][3], json!(""), "ragged rows are padded");
        assert_eq!(t.first_row, 2);
    }

    #[test]
    fn legacy_windows_1252_csv() {
        let path = tmp("b.csv");
        std::fs::write(&path, b"city,price\nM\xfcnchen,5\x80\n").unwrap();
        let t = read_table(&FileOptions { path, ..Default::default() }).unwrap();
        assert_eq!(t.encoding.as_deref(), Some("windows-1252"));
        assert_eq!(t.rows[0][0], json!("München"));
        assert_eq!(t.rows[0][1], json!("5€"));
    }

    #[test]
    fn xlsx_round_trip_keeps_types() {
        let path = tmp("c.xlsx");
        let sheet = Sheet {
            name: "Bad/Name:that is far too long for excel".into(),
            columns: vec![Column::new("id", ""), Column::new("name", ""), Column::new("ok", ""), Column::new("at", ""), Column::new("doc", ""), Column::new("big", "")],
            rows: vec![
                vec![json!(1), json!("=1+1"), json!(true), json!("2024-03-05 10:20:30"), json!({"a": 1}), json!(12345678901234567890u64)],
                vec![json!(2.5), Value::Null, json!(false), json!("2024-03-05"), json!([1, 2]), json!(1)],
            ],
        };
        write_xlsx(&path, &[sheet]).unwrap();
        let t = read_table(&FileOptions { path, ..Default::default() }).unwrap();
        assert_eq!(t.sheets.len(), 1);
        assert!(t.sheets[0].len() <= 31 && !t.sheets[0].contains('/'));
        assert_eq!(t.columns, vec!["id", "name", "ok", "at", "doc", "big"]);
        assert_eq!(t.rows[0][0], json!(1), "whole floats come back as integers");
        assert_eq!(t.rows[0][1], json!("=1+1"), "text that looks like a formula stays text");
        assert_eq!(t.rows[0][2], json!(true));
        assert_eq!(t.rows[0][3], json!("2024-03-05 10:20:30"));
        assert_eq!(t.rows[1][3], json!("2024-03-05"));
        assert_eq!(t.rows[0][4], json!(r#"{"a":1}"#));
        assert_eq!(t.rows[0][5], json!("12345678901234567890"), "precision kept as text");
        assert_eq!(t.rows[1][0], json!(2.5));
        assert_eq!(t.rows[1][1], Value::Null);
    }

    #[test]
    fn value_preparation() {
        let o = ValueOptions { trim: true, empty_as_null: true, detect_types: true };
        assert_eq!(prepare(&json!("  "), o), Value::Null);
        assert_eq!(prepare(&json!(" 42 "), o), json!(42));
        assert_eq!(prepare(&json!("007"), o), json!("007"));
        assert_eq!(prepare(&json!("-3.25"), o), json!(-3.25));
        assert_eq!(prepare(&json!("TRUE"), o), json!(true));
        assert_eq!(prepare(&json!("12345678901234567890"), o), json!("12345678901234567890"));
        assert_eq!(prepare(&json!("1.2.3"), o), json!("1.2.3"));
        let keep = ValueOptions { trim: false, empty_as_null: false, detect_types: false };
        assert_eq!(prepare(&json!(" 42 "), keep), json!(" 42 "));
    }

    #[test]
    fn nested_documents() {
        let cols = vec!["name".to_string(), "address.city".to_string(), "address.zip".to_string(), "skip".to_string()];
        let doc = build_doc(&cols, vec![json!("Ada"), json!("Paris"), json!("75001"), Value::Null]).unwrap();
        assert_eq!(Value::Object(doc), json!({"name": "Ada", "address": {"city": "Paris", "zip": "75001"}}));
        let clash = vec!["a".to_string(), "a.b".to_string()];
        assert!(build_doc(&clash, vec![json!(1), json!(2)]).is_err());
    }

    #[test]
    fn unique_sheet_names() {
        let mut used = HashSet::new();
        assert_eq!(sheet_name("Data", &mut used), "Data");
        assert_eq!(sheet_name("data", &mut used), "data (2)");
        assert_eq!(sheet_name("[]", &mut used), "__");
        assert_eq!(sheet_name("", &mut used), "Sheet");
    }
}
