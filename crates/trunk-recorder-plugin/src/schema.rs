//! Settings schemas: a config struct's JSON Schema (from `schemars`), reduced
//! to the subset the recorder's settings form draws.
//!
//! The form understands an object whose properties are:
//! - `"type": "string"` — a text box; `"x-secret": true` hides what's typed,
//!   `"format": "uri"` checks for a URL, `"x-multiline": true` for a text area
//! - `"type": "integer"` or `"number"` — `minimum` / `maximum` apply
//! - `"type": "boolean"` — a switch
//! - `"enum": [...]` — a menu (`"x-enum-labels"`, when given, are what it shows)
//! - `"type": "array"` of strings or numbers — a list
//! - `"type": "array"` of objects — a list of groups, added and removed one by one
//! - `"type": "object"` — a group of the above
//!
//! Two more, for a field of either:
//! - `"x-system": true` on a string — a menu of the recorder's systems (by
//!   short name); the recorder updates it when a system is renamed
//! - `"x-required": true` — the field has to be filled in: until it is, the
//!   recorder shows the plugin, or that system, as not set up. ([`normalize`]
//!   moves these into the object's `required` list; `#[schemars(required)]`
//!   doesn't work on a `#[serde(default)]` struct, so mark them this way:
//!   `#[schemars(extend("x-required" = true))]`.)
//!
//! Fields are shown in `x-order` (the struct's order, added by [`normalize`]).
//! Each may have a `title` (else the key, spelled out), a `description`
//! (help under the field) and a `default` — which `schemars` only writes
//! when the config type also derives `Serialize`.
//!
//! [`normalize`] turns what `schemars` makes of ordinary Rust into that:
//! `Option<T>` becomes `T` (left empty = not set), unit-variant enums become
//! `enum`, nested structs are inlined, and a doc comment's first paragraph
//! becomes the `title` with the rest the `description`.

use schemars::generate::SchemaSettings;
use schemars::JsonSchema;
use serde_json::{Map, Value};

/// The normalized schema of `T`.
pub fn schema_for<T: JsonSchema>() -> Value {
    let generator = SchemaSettings::draft2020_12()
        .with(|s| {
            s.inline_subschemas = true;
            s.meta_schema = None;
        })
        .into_generator();
    let mut v = generator.into_root_schema_for::<T>().to_value();
    normalize(&mut v);
    if let Some(o) = v.as_object_mut() {
        // The root's title is the Rust type's name, and its doc comment is
        // for programmers: neither is for users.
        o.remove("title");
        o.remove("description");
    }
    v
}

/// See the module docs.
pub fn normalize(v: &mut Value) {
    let Some(o) = v.as_object_mut() else { return };
    // Option<T>: "type": ["T", "null"] → "T"
    if let Some(Value::Array(types)) = o.get("type") {
        let rest: Vec<Value> = types.iter().filter(|t| t.as_str() != Some("null")).cloned().collect();
        if rest.len() == 1 {
            o.insert("type".into(), rest[0].clone());
        }
    }
    // Option<Struct> / Option<Enum>: "anyOf": [{…}, {"type": "null"}] → {…}
    if let Some(Value::Array(any)) = o.get("anyOf") {
        let rest: Vec<Value> = any.iter().filter(|s| s.get("type").and_then(Value::as_str) != Some("null")).cloned().collect();
        if rest.len() == 1 {
            o.remove("anyOf");
            if let Value::Object(inner) = &rest[0] {
                for (k, x) in inner {
                    o.entry(k.clone()).or_insert_with(|| x.clone());
                }
            }
        }
    }
    // Unit-variant enums: "oneOf": [{"enum": ["A"]}, {"const": "B", "description": …}] → "enum"
    if let Some(Value::Array(one)) = o.get("oneOf") {
        let mut values = Vec::new();
        let mut labels = Vec::new();
        let mut plain = true;
        for s in one {
            let vs: Vec<Value> = match (s.get("const"), s.get("enum")) {
                (Some(c), _) => vec![c.clone()],
                (None, Some(Value::Array(e))) => e.clone(),
                _ => {
                    plain = false;
                    break;
                }
            };
            for x in vs {
                let label = s.get("title").or(s.get("description")).and_then(Value::as_str).map(str::to_string);
                labels.push(label.map(Value::String).unwrap_or_else(|| x.clone()));
                values.push(x);
            }
        }
        if plain {
            o.remove("oneOf");
            o.insert("type".into(), "string".into());
            if labels != values {
                o.insert("x-enum-labels".into(), Value::Array(labels));
            }
            o.insert("enum".into(), Value::Array(values));
        }
    }
    // Doc comments are wrapped to fit the source: unwrap them (paragraphs stay).
    if let Some(d) = o.get("description").and_then(Value::as_str) {
        let unwrapped = d.split("\n\n").map(|p| p.split_whitespace().collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join("\n\n");
        o.insert("description".into(), unwrapped.into());
    }
    // Doc comments: the first paragraph is the title.
    if !o.contains_key("title") {
        if let Some(d) = o.get("description").and_then(Value::as_str).map(str::to_string) {
            match d.split_once("\n\n") {
                Some((title, rest)) => {
                    o.insert("title".into(), title.trim().into());
                    o.insert("description".into(), rest.trim().into());
                }
                None if d.len() <= 60 && !d.trim_end().ends_with('.') => {
                    o.insert("title".into(), d.trim().into());
                    o.remove("description");
                }
                None => {}
            }
        }
    }
    // `Option`s default to null: that's "not set", which the form shows anyway.
    if o.get("default").is_some_and(Value::is_null) {
        o.remove("default");
    }
    // Rust integer formats mean nothing to the form.
    if let Some(f) = o.get("format").and_then(Value::as_str) {
        if f.starts_with("int") || f.starts_with("uint") || f == "float" || f == "double" {
            o.remove("format");
        }
    }
    // JSON objects have no order: say which the fields come in (as declared).
    if let Some(Value::Object(props)) = o.get("properties") {
        let order: Vec<Value> = props.keys().map(|k| Value::String(k.clone())).collect();
        o.insert("x-order".into(), Value::Array(order));
    }
    for key in ["properties", "items"] {
        match o.get_mut(key) {
            Some(Value::Object(props)) if key == "properties" => props.values_mut().for_each(normalize),
            Some(items @ Value::Object(_)) => normalize(items),
            _ => {}
        }
    }
    // `"x-required": true` on a field → the object's `required` list.
    if let Some(Value::Object(props)) = o.get_mut("properties") {
        let marked: Vec<String> = props
            .iter_mut()
            .filter_map(|(k, f)| f.as_object_mut()?.remove("x-required").filter(|v| v == &Value::Bool(true)).map(|_| k.clone()))
            .collect();
        if !marked.is_empty() {
            let req = o.entry("required").or_insert_with(|| Value::Array(vec![]));
            if let Value::Array(r) = req {
                for k in marked {
                    if !r.contains(&Value::String(k.clone())) {
                        r.push(Value::String(k));
                    }
                }
            }
        }
    }
    // A config struct with `#[serde(default)]` needs nothing: drop an empty list.
    if o.get("required").and_then(Value::as_array).is_some_and(|r| r.is_empty()) {
        o.remove("required");
    }
}

/// A config with no settings.
#[derive(Clone, Copy, Debug, Default, serde::Deserialize)]
pub struct NoConfig {}

impl JsonSchema for NoConfig {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "NoConfig".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut m = Map::new();
        m.insert("type".into(), "object".into());
        m.insert("properties".into(), Value::Object(Map::new()));
        schemars::Schema::from(m)
    }
}

pub(crate) fn is_empty(schema: &Value) -> bool {
    schema.get("properties").and_then(Value::as_object).is_none_or(|p| p.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    #[derive(Serialize, Deserialize, JsonSchema, Default)]
    #[serde(rename_all = "camelCase", default)]
    #[allow(dead_code)]
    struct Settings {
        /// Upload server
        ///
        /// Where calls
        /// go.
        server: String,
        /// API key
        #[schemars(extend("x-secret" = true))]
        api_key: Option<String>,
        mode: Mode,
        #[schemars(range(min = 1, max = 10))]
        retries: u32,
    }

    #[derive(Serialize, Deserialize, JsonSchema, Default)]
    #[serde(rename_all = "lowercase")]
    #[allow(dead_code)]
    enum Mode {
        #[default]
        Fast,
        /// Slow and steady
        Slow,
    }

    #[derive(Serialize, Deserialize, JsonSchema, Default)]
    #[serde(rename_all = "camelCase", default)]
    #[allow(dead_code)]
    struct Marked {
        /// Server
        #[schemars(extend("x-required" = true))]
        server: String,
        streams: Vec<Stream>,
    }

    #[derive(Serialize, Deserialize, JsonSchema, Default)]
    #[serde(rename_all = "camelCase", default)]
    #[allow(dead_code)]
    struct Stream {
        /// System
        #[schemars(extend("x-system" = true, "x-required" = true))]
        short_name: String,
        port: u16,
    }

    #[test]
    fn required_and_system_fields_come_through() {
        let s = schema_for::<Marked>();
        assert_eq!(s["required"], json!(["server"]));
        let item = &s["properties"]["streams"]["items"];
        assert_eq!(item["properties"]["shortName"]["x-system"], true);
        assert_eq!(item["required"], json!(["shortName"]));
        assert!(item["properties"]["shortName"].get("x-required").is_none());
        assert!(item.get("required").is_some() && s["properties"]["streams"].get("required").is_none());
    }

    #[test]
    fn normalizes_what_schemars_makes() {
        let s = schema_for::<Settings>();
        let p = &s["properties"];
        assert_eq!(p["server"]["title"], "Upload server");
        assert_eq!(p["server"]["description"], "Where calls go.");
        assert_eq!(p["apiKey"]["type"], "string");
        assert_eq!(p["apiKey"]["title"], "API key");
        assert_eq!(p["apiKey"]["x-secret"], true);
        assert!(p["apiKey"].get("default").is_none());
        assert_eq!(p["mode"]["enum"], json!(["fast", "slow"]));
        assert_eq!(p["mode"]["x-enum-labels"], json!(["fast", "Slow and steady"]));
        assert_eq!(p["mode"]["default"], "fast");
        assert_eq!(p["retries"]["maximum"], 10);
        assert!(p["retries"].get("format").is_none());
        assert!(s.get("title").is_none());
        assert_eq!(s["x-order"], json!(["server", "apiKey", "mode", "retries"]));
    }
}
