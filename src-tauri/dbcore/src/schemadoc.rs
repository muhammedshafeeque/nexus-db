//! Builds a [`SchemaDoc`] from four flat catalog queries, so each SQL driver
//! only has to supply engine-specific SQL with the column order below.

use crate::model::*;
use std::collections::HashMap;

/// Catalog rows as strings (NULL = empty), one query each:
pub struct Catalog {
    /// schema, table, kind, comment, rows
    pub tables: Vec<Vec<String>>,
    /// schema, table, position, column, type, nullable, default, comment
    pub columns: Vec<Vec<String>>,
    /// schema, table, constraint, type (p | u | f), column, position,
    /// ref schema, ref table, ref column, on update, on delete
    pub constraints: Vec<Vec<String>>,
    /// schema, table, index, unique, primary, column, position
    pub indexes: Vec<Vec<String>>,
}

fn get(r: &[String], i: usize) -> &str {
    r.get(i).map(String::as_str).unwrap_or("")
}

fn opt(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn truthy(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "t" | "1" | "yes")
}

/// "NO ACTION" is the default and only noise in the docs.
fn action(s: &str) -> Option<String> {
    let a = s.trim().replace('_', " ").to_uppercase();
    (!a.is_empty() && a != "NO ACTION").then_some(a)
}

pub fn build(database: &str, cat: Catalog) -> SchemaDoc {
    let mut tables: Vec<TableDoc> = Vec::new();
    let mut at: HashMap<(String, String), usize> = HashMap::new();
    let mut index_of = |tables: &mut Vec<TableDoc>, schema: &str, name: &str| -> usize {
        *at.entry((schema.to_string(), name.to_string())).or_insert_with(|| {
            tables.push(TableDoc { schema: schema.into(), name: name.into(), kind: "table".into(), ..Default::default() });
            tables.len() - 1
        })
    };

    for r in &cat.tables {
        let i = index_of(&mut tables, get(r, 0), get(r, 1));
        let t = &mut tables[i];
        t.kind = opt(get(r, 2)).unwrap_or_else(|| "table".into()).to_lowercase();
        t.comment = opt(get(r, 3));
        t.rows = get(r, 4).trim().parse::<f64>().ok().filter(|n| *n >= 0.0).map(|n| n as i64);
    }
    for r in &cat.columns {
        let i = index_of(&mut tables, get(r, 0), get(r, 1));
        tables[i].columns.push(ColumnDoc {
            name: get(r, 3).into(),
            type_name: get(r, 4).into(),
            nullable: truthy(get(r, 5)),
            default: opt(get(r, 6)),
            comment: opt(get(r, 7)),
        });
    }
    // rows arrive ordered by table, constraint, position
    for r in &cat.constraints {
        let i = index_of(&mut tables, get(r, 0), get(r, 1));
        let t = &mut tables[i];
        let (name, column) = (get(r, 2), get(r, 4).to_string());
        match get(r, 3) {
            "p" => t.primary_key.push(column),
            "u" => match t.unique_keys.iter_mut().find(|k| k.name == name) {
                Some(k) => k.columns.push(column),
                None => t.unique_keys.push(KeyDoc { name: name.into(), columns: vec![column] }),
            },
            "f" => {
                let fk = match t.foreign_keys.iter_mut().position(|k| k.name == name) {
                    Some(p) => &mut t.foreign_keys[p],
                    None => {
                        t.foreign_keys.push(ForeignKeyDoc {
                            name: name.into(),
                            ref_schema: get(r, 6).into(),
                            ref_table: get(r, 7).into(),
                            on_update: action(get(r, 9)),
                            on_delete: action(get(r, 10)),
                            ..Default::default()
                        });
                        t.foreign_keys.last_mut().unwrap()
                    }
                };
                fk.columns.push(column);
                fk.ref_columns.push(get(r, 8).into());
            }
            _ => {}
        }
    }
    for r in &cat.indexes {
        let i = index_of(&mut tables, get(r, 0), get(r, 1));
        let t = &mut tables[i];
        let name = get(r, 2);
        match t.indexes.iter_mut().find(|x| x.name == name) {
            Some(x) => x.columns.push(get(r, 5).into()),
            None => t.indexes.push(IndexDoc {
                name: name.into(),
                columns: vec![get(r, 5).into()],
                unique: truthy(get(r, 3)),
                primary: truthy(get(r, 4)),
            }),
        }
    }

    // SQLite leaves the referenced columns out when they are the target's primary key
    let pks: HashMap<(String, String), Vec<String>> =
        tables.iter().map(|t| ((t.schema.clone(), t.name.clone()), t.primary_key.clone())).collect();
    for t in &mut tables {
        for fk in &mut t.foreign_keys {
            if fk.ref_columns.iter().all(|c| c.is_empty()) {
                if let Some(pk) = pks.get(&(fk.ref_schema.clone(), fk.ref_table.clone())) {
                    fk.ref_columns = pk.clone();
                }
            }
        }
    }
    tables.retain(|t| !t.name.is_empty());
    SchemaDoc { database: database.to_string(), tables }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(v: &[&[&str]]) -> Vec<Vec<String>> {
        v.iter().map(|r| r.iter().map(|s| s.to_string()).collect()).collect()
    }

    #[test]
    fn groups_keys_and_relations() {
        let doc = build(
            "shop",
            Catalog {
                tables: rows(&[&["main", "customers", "table", "", "10"], &["main", "orders", "table", "Orders", ""]]),
                columns: rows(&[
                    &["main", "customers", "1", "id", "integer", "0", "", ""],
                    &["main", "orders", "1", "id", "integer", "false", "", ""],
                    &["main", "orders", "2", "customer_id", "integer", "true", "", ""],
                ]),
                constraints: rows(&[
                    &["main", "customers", "PRIMARY", "p", "id", "1", "", "", "", "", ""],
                    &["main", "orders", "PRIMARY", "p", "id", "1", "", "", "", "", ""],
                    &["main", "orders", "fk1", "f", "customer_id", "1", "main", "customers", "", "NO ACTION", "CASCADE"],
                ]),
                indexes: rows(&[&["main", "orders", "ix", "0", "0", "customer_id", "1"]]),
            },
        );
        assert_eq!(doc.tables.len(), 2);
        let orders = &doc.tables[1];
        assert_eq!(orders.comment.as_deref(), Some("Orders"));
        assert_eq!(doc.tables[0].rows, Some(10));
        assert!(orders.columns[1].nullable && !orders.columns[0].nullable);
        let fk = &orders.foreign_keys[0];
        assert_eq!(fk.ref_columns, vec!["id"]);
        assert_eq!(fk.on_delete.as_deref(), Some("CASCADE"));
        assert_eq!(fk.on_update, None);
        assert!(!orders.indexes[0].unique);
    }
}
