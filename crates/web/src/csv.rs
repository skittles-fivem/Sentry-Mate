//! CSV answers (docs/ui-api.md, "CSV"): a UTF-8 byte order mark, a header row, nested fields as dotted columns
//! (`start.place.name`), lists of plain values joined with `;` (an origin in `sources` as `smo`, `tesla_api` or
//! `import:teslamate`), null as an empty cell, `Content-Disposition: attachment`. Text starting with `=`, `+`, `-`, `@`, a tab or a carriage return gets a leading `'` so a
//! spreadsheet shows it instead of running it; numbers are written as they are.
//!
//! A CSV answer carries every matching row, not one page: a handler that answers CSV gathers them all first.

use crate::error::{ApiError, ApiResult};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value};

fn flatten(prefix: &str, v: &Value, out: &mut Vec<(String, String)>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                flatten(&key, x, out);
            }
        }
        Value::Array(a) => {
            let joined: Vec<String> = a.iter().map(plain).collect();
            out.push((prefix.to_string(), joined.join(";")));
        }
        other => out.push((prefix.to_string(), plain(other))),
    }
}

fn plain(v: &Value) -> String {
    match v {
        // an origin in `sources`: `smo`, `tesla_api`, `import:teslamate`, `import:teslafi`
        Value::Object(m) if m.get("kind").is_some_and(Value::is_string) && m.contains_key("label") => {
            let kind = m.get("kind").and_then(Value::as_str).unwrap_or("");
            match m.get("site").and_then(Value::as_str) {
                Some(site) => format!("{kind}:{site}"),
                None => kind.to_string(),
            }
        }
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

fn cell(v: &str, numeric: bool) -> String {
    let safe =
        if !numeric && v.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{v}") } else { v.to_string() };
    if safe.contains([',', '"', '\n', '\r']) { format!("\"{}\"", safe.replace('"', "\"\"")) } else { safe }
}

/// Rows of objects as CSV text (with the byte order mark). Columns in the order they first appear.
pub fn rows(items: &[Value]) -> String {
    let mut columns: Vec<String> = Vec::new();
    let mut index = std::collections::HashMap::new();
    let mut flat_rows = Vec::with_capacity(items.len());
    for item in items {
        let mut flat = Vec::new();
        flatten("", item, &mut flat);
        let mut row = Map::new();
        for (k, v) in flat {
            if !index.contains_key(&k) {
                index.insert(k.clone(), columns.len());
                columns.push(k.clone());
            }
            row.insert(k, Value::String(v));
        }
        flat_rows.push((row, item));
    }
    let mut out = String::from("\u{feff}");
    out.push_str(&columns.iter().map(|c| cell(c, false)).collect::<Vec<_>>().join(","));
    out.push_str("\r\n");
    for (row, item) in flat_rows {
        let line: Vec<String> = columns
            .iter()
            .map(|c| {
                let text = row.get(c).and_then(Value::as_str).unwrap_or("");
                cell(text, is_number_at(item, c))
            })
            .collect();
        out.push_str(&line.join(","));
        out.push_str("\r\n");
    }
    out
}

fn is_number_at(item: &Value, dotted: &str) -> bool {
    let mut v = item;
    for part in dotted.split('.') {
        match v.get(part) {
            Some(x) => v = x,
            None => return false,
        }
    }
    v.is_number()
}

/// Columns (`{ t_ms: [], values: { field: [] } }`) as CSV: one row per `t_ms`.
pub fn columns(t_ms: &[Value], values: &Map<String, Value>) -> String {
    let names: Vec<&String> = values.keys().collect();
    let mut items = Vec::with_capacity(t_ms.len());
    for (i, t) in t_ms.iter().enumerate() {
        let mut row = Map::new();
        row.insert("t_ms".into(), t.clone());
        for n in &names {
            let v = values.get(*n).and_then(|c| c.get(i)).cloned().unwrap_or(Value::Null);
            row.insert((*n).clone(), v);
        }
        items.push(Value::Object(row));
    }
    rows(&items)
}

/// A CSV answer named `<name>.csv`.
pub fn response(name: &str, body: String) -> Response {
    let safe: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' }).collect();
    let mut res = body.into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/csv; charset=utf-8"));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{safe}.csv\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    res
}

/// A list answer as CSV when asked (`items` of `answer`, or its columns), else as JSON. Something that is neither
/// asked for as CSV is `400` on `format`.
pub fn answer(q: &crate::Params, name: &str, answer: Value) -> ApiResult<Response> {
    if !q.wants_csv()? {
        return Ok(axum::Json(answer).into_response());
    }
    if let Some(items) = answer.get("items").and_then(Value::as_array) {
        return Ok(response(name, rows(items)));
    }
    if let (Some(t), Some(v)) =
        (answer.get("t_ms").and_then(Value::as_array), answer.get("values").and_then(Value::as_object))
    {
        return Ok(response(name, columns(t, v)));
    }
    Err(ApiError::bad_input("format", "CSV is offered for lists and columns only."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dotted_columns_lists_and_formula_guard() {
        let items = vec![
            json!({ "id": "1", "start": { "place": { "name": "=HYPERLINK(1)" } }, "tags": ["a", "b"], "kwh": -1.5 }),
            json!({ "id": "2", "start": { "place": null }, "tags": [], "kwh": null, "note": "a, \"b\"" }),
        ];
        let out = rows(&items);
        assert!(out.starts_with('\u{feff}'));
        let lines: Vec<&str> = out.trim_start_matches('\u{feff}').split("\r\n").collect();
        // keys in each object's order (serde_json keeps them sorted), new ones after
        assert_eq!(lines[0], "id,kwh,start.place.name,tags,note,start.place");
        assert_eq!(lines[1], "1,-1.5,'=HYPERLINK(1),a;b,,");
        assert_eq!(lines[2], "2,,,,\"a, \"\"b\"\"\",");
    }

    #[test]
    fn sources_are_words() {
        let items = vec![json!({
            "id": "1",
            "sources": [
                { "kind": "smo", "label": "Data from SMO" },
                { "kind": "import", "site": "teslamate", "label": "Imported from TeslaMate" },
            ],
            "source_label": "Data from SMO; imported from TeslaMate",
        })];
        let out = rows(&items);
        let lines: Vec<&str> = out.trim_start_matches('\u{feff}').split("\r\n").collect();
        assert_eq!(lines[0], "id,source_label,sources");
        assert_eq!(lines[1], "1,Data from SMO; imported from TeslaMate,smo;import:teslamate");
    }
}
