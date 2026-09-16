//! A document as a JSON value, for a schema to validate, with where in the
//! text each part of it is: every JSON pointer mapped to its key's bytes (when
//! it has a key) and its value's. JSON is read with the editor's own parser,
//! YAML as YAML 1.2 (core schema, anchors and `<<` merges resolved), TOML with
//! its dates as strings.

use crate::json::{self, Lit, Options, Sink};
use saphyr_parser::{Event, Parser, ScalarStyle};
use serde_json::{Map, Number, Value};
use std::collections::HashMap;
use std::ops::Range;

/// Where a part of the document is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub key: Option<Range<usize>>,
    pub value: Range<usize>,
}

/// A document and where its parts are.
#[derive(Debug, Default)]
pub struct Doc {
    pub value: Value,
    pub places: HashMap<String, Place>,
}

/// `name` as one step of a JSON pointer.
fn step(name: &str) -> String {
    name.replace('~', "~0").replace('/', "~1")
}

/// Values in the making, innermost last.
enum Frame {
    Object {
        map: Map<String, Value>,
        pointer: String,
        start: usize,
        key: Option<(String, Range<usize>)>,
    },
    Array {
        items: Vec<Value>,
        pointer: String,
        start: usize,
    },
}

/// Builds a [`Doc`] from nodes as they complete.
#[derive(Default)]
struct Builder {
    stack: Vec<Frame>,
    root: Option<Value>,
    places: HashMap<String, Place>,
}

impl Builder {
    /// Where the next node goes, and the key it goes under.
    fn next_pointer(&self) -> (String, Option<Range<usize>>) {
        match self.stack.last() {
            Some(Frame::Object { pointer, key, .. }) => match key {
                Some((k, span)) => (format!("{pointer}/{}", step(k)), Some(span.clone())),
                None => (pointer.clone(), None),
            },
            Some(Frame::Array { items, pointer, .. }) => {
                (format!("{pointer}/{}", items.len()), None)
            }
            None => (String::new(), None),
        }
    }

    fn open(&mut self, object: bool, start: usize) {
        let (pointer, _) = self.next_pointer();
        self.stack.push(if object {
            Frame::Object { map: Map::new(), pointer, start, key: None }
        } else {
            Frame::Array { items: Vec::new(), pointer, start }
        });
    }

    /// Close the innermost container at `end`; a copy of it when `keep`.
    fn close(&mut self, end: usize, keep: bool) -> Option<Value> {
        let frame = self.stack.pop()?;
        let (value, start) = match frame {
            Frame::Object { map, start, .. } => (Value::Object(map), start),
            Frame::Array { items, start, .. } => (Value::Array(items), start),
        };
        let copy = keep.then(|| value.clone());
        self.put(value, start..end);
        copy
    }

    fn key(&mut self, name: String, span: Range<usize>) {
        if let Some(Frame::Object { key, .. }) = self.stack.last_mut() {
            *key = Some((name, span));
        }
    }

    /// A finished value at `span`: into its container, or as the document.
    fn put(&mut self, value: Value, span: Range<usize>) {
        let (pointer, key_span) = self.next_pointer();
        self.places.insert(pointer, Place { key: key_span, value: span });
        match self.stack.last_mut() {
            Some(Frame::Object { map, key, .. }) => {
                if let Some((k, _)) = key.take() {
                    map.insert(k, value);
                }
            }
            Some(Frame::Array { items, .. }) => items.push(value),
            None => self.root = Some(value),
        }
    }

    fn doc(self) -> Option<Doc> {
        Some(Doc { value: self.root?, places: self.places })
    }
}

impl Sink for Builder {
    fn begin_object(&mut self, at: usize) {
        self.open(true, at);
    }
    fn end_object(&mut self, span: Range<usize>) {
        self.close(span.end, false);
    }
    fn begin_array(&mut self, at: usize) {
        self.open(false, at);
    }
    fn end_array(&mut self, span: Range<usize>) {
        self.close(span.end, false);
    }
    fn key(&mut self, raw: &str, span: Range<usize>) {
        Builder::key(self, json::unescape(raw).into_owned(), span);
    }
    fn string(&mut self, raw: &str, span: Range<usize>) {
        self.put(Value::String(json::unescape(raw).into_owned()), span);
    }
    fn number(&mut self, raw: &str, span: Range<usize>) {
        let n = raw
            .parse::<Number>()
            .ok()
            .or_else(|| raw.parse::<f64>().ok().and_then(Number::from_f64))
            .map_or(Value::Null, Value::Number);
        self.put(n, span);
    }
    fn literal(&mut self, lit: Lit, span: Range<usize>) {
        let v = match lit {
            Lit::True => Value::Bool(true),
            Lit::False => Value::Bool(false),
            Lit::Null => Value::Null,
        };
        self.put(v, span);
    }
}

/// A JSON document (JSONC allowed, as its options say); `None` when it has
/// syntax errors or is several documents.
pub fn json(text: &str, opts: Options) -> Option<Doc> {
    if opts.multiple_roots {
        return None;
    }
    let mut b = Builder::default();
    if !json::parse(text, opts, &mut b).is_empty() {
        return None;
    }
    b.doc()
}

/// A plain YAML scalar as the YAML 1.2 core schema reads it.
fn yaml_scalar(s: &str) -> Value {
    match s {
        "" | "~" | "null" | "Null" | "NULL" => return Value::Null,
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" | "-.inf" | "-.Inf" | "-.INF"
        | ".nan" | ".NaN" | ".NAN" => return Value::String(s.to_string()),
        _ => {}
    }
    let int = if let Some(h) = s.strip_prefix("0x") {
        i64::from_str_radix(h, 16).ok()
    } else if let Some(o) = s.strip_prefix("0o") {
        i64::from_str_radix(o, 8).ok()
    } else if s
        .bytes()
        .enumerate()
        .all(|(i, b)| b.is_ascii_digit() || (i == 0 && matches!(b, b'-' | b'+')))
        && s.bytes().any(|b| b.is_ascii_digit())
    {
        s.parse::<i64>().ok()
    } else {
        None
    };
    if let Some(i) = int {
        return Value::Number(i.into());
    }
    let floaty = s.bytes().any(|b| b.is_ascii_digit())
        && s.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'-' | b'+'));
    match s.parse::<f64>().ok().filter(|_| floaty).and_then(Number::from_f64) {
        Some(n) => Value::Number(n),
        None => Value::String(s.to_string()),
    }
}

/// Most nodes an alias may expand a YAML document to.
const MAX_NODES: usize = 1_000_000;

fn count_nodes(v: &Value) -> usize {
    match v {
        Value::Array(a) => 1 + a.iter().map(count_nodes).sum::<usize>(),
        Value::Object(o) => 1 + o.values().map(count_nodes).sum::<usize>(),
        _ => 1,
    }
}

/// The first document of a YAML file; `None` when it has a syntax error.
pub fn yaml(text: &str) -> Option<Doc> {
    let line_starts: Vec<usize> =
        std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let offset = |m: &saphyr_parser::Marker| {
        let line_start = line_starts.get(m.line().saturating_sub(1)).copied().unwrap_or(text.len());
        let line = &text[line_start..];
        line_start + line.char_indices().nth(m.col()).map_or(line.len(), |(i, _)| i)
    };
    let mut b = Builder::default();
    // Anchored values by id, and the anchor each open container takes.
    let mut anchors: HashMap<usize, Value> = HashMap::new();
    let mut open_anchors: Vec<usize> = Vec::new();
    // Whether the next node in each open mapping is a key.
    let mut want_key: Vec<Option<bool>> = Vec::new();
    let mut nodes = 0usize;
    let mut parser = Parser::new_from_str(text);
    let mut documents = 0;
    while let Some(next) = parser.next_event() {
        let (event, span) = next.ok()?;
        let range = offset(&span.start)..offset(&span.end).max(offset(&span.start));
        let key_slot = matches!(
            event,
            Event::Scalar(..)
                | Event::Alias(_)
                | Event::SequenceStart(..)
                | Event::MappingStart(..)
        ) && match want_key.last_mut() {
            Some(Some(w)) => {
                let k = *w;
                *w = !*w;
                k
            }
            _ => false,
        };
        nodes += 1;
        if nodes > MAX_NODES {
            return None;
        }
        match event {
            Event::DocumentStart(_) => {
                documents += 1;
                if documents > 1 {
                    break;
                }
            }
            Event::DocumentEnd => break,
            Event::Scalar(v, style, anchor, tag) => {
                let as_string = style != ScalarStyle::Plain
                    || tag
                        .as_ref()
                        .is_some_and(|t| t.suffix == "str" || t.suffix.ends_with(":str"));
                if key_slot {
                    b.key(v.to_string(), range);
                    continue;
                }
                let value = if as_string { Value::String(v.to_string()) } else { yaml_scalar(&v) };
                if anchor != 0 {
                    anchors.insert(anchor, value.clone());
                }
                merge_or_put(&mut b, value, range);
            }
            Event::Alias(id) => {
                let value = anchors.get(&id).cloned().unwrap_or(Value::Null);
                nodes += count_nodes(&value);
                if nodes > MAX_NODES {
                    return None;
                }
                if key_slot {
                    b.key(value.as_str().unwrap_or("").to_string(), range);
                } else {
                    merge_or_put(&mut b, value, range);
                }
            }
            Event::MappingStart(anchor, _) | Event::SequenceStart(anchor, _) => {
                if key_slot {
                    // A complex key: named by its position, it can't be validated.
                    b.key("?".into(), range.clone());
                }
                let object = matches!(event, Event::MappingStart(..));
                b.open(object, range.start);
                open_anchors.push(anchor);
                want_key.push(object.then_some(true));
            }
            Event::MappingEnd | Event::SequenceEnd => {
                want_key.pop();
                let anchor = open_anchors.pop().unwrap_or(0);
                if let Some(v) = b.close(range.end.max(range.start), anchor != 0) {
                    anchors.insert(anchor, v);
                }
            }
            _ => {}
        }
    }
    b.doc()
}

/// Put `value`, or — under a `<<` key — merge its mapping (or mappings) into
/// the one being built, the keys it already has winning.
fn merge_or_put(b: &mut Builder, value: Value, span: Range<usize>) {
    let merging =
        matches!(b.stack.last(), Some(Frame::Object { key: Some((k, _)), .. }) if k == "<<");
    if !merging {
        b.put(value, span);
        return;
    }
    let Some(Frame::Object { map, key, pointer, .. }) = b.stack.last_mut() else { return };
    let key_span = key.take().map(|(_, s)| s);
    let sources = match value {
        Value::Object(o) => vec![o],
        Value::Array(a) => a
            .into_iter()
            .filter_map(|v| match v {
                Value::Object(o) => Some(o),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let pointer = pointer.clone();
    for source in sources {
        for (k, v) in source {
            if !map.contains_key(&k) {
                b.places.insert(
                    format!("{pointer}/{}", step(&k)),
                    Place { key: key_span.clone(), value: span.clone() },
                );
                map.insert(k, v);
            }
        }
    }
}

/// A TOML document, its dates as strings; `None` when it has errors.
pub fn toml(text: &str) -> Option<Doc> {
    use toml::de::{DeTable, DeValue};
    let (table, errors) = DeTable::parse_recoverable(text);
    if !errors.is_empty() {
        return None;
    }
    let mut places = HashMap::new();
    fn convert(
        v: &DeValue,
        span: Range<usize>,
        pointer: String,
        key: Option<Range<usize>>,
        places: &mut HashMap<String, Place>,
    ) -> Value {
        let out = match v {
            DeValue::String(s) => Value::String(s.to_string()),
            DeValue::Integer(i) => i64::from_str_radix(i.as_str(), i.radix())
                .map(|n| Value::Number(n.into()))
                .unwrap_or_else(|_| Value::String(i.as_str().to_string())),
            DeValue::Float(f) => {
                let raw = f.as_str().replace('_', "");
                raw.parse::<f64>()
                    .ok()
                    .and_then(Number::from_f64)
                    .map_or_else(|| Value::String(raw.clone()), Value::Number)
            }
            DeValue::Boolean(b) => Value::Bool(*b),
            DeValue::Datetime(d) => Value::String(d.to_string()),
            DeValue::Array(items) => Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| {
                        convert(item.get_ref(), item.span(), format!("{pointer}/{i}"), None, places)
                    })
                    .collect(),
            ),
            DeValue::Table(t) => convert_table(t, &pointer, places),
        };
        places.insert(pointer, Place { key, value: span });
        out
    }
    fn convert_table(t: &DeTable, pointer: &str, places: &mut HashMap<String, Place>) -> Value {
        Value::Object(
            t.iter()
                .map(|(k, item)| {
                    let name = k.get_ref().to_string();
                    let child = format!("{pointer}/{}", step(&name));
                    let value = convert(item.get_ref(), item.span(), child, Some(k.span()), places);
                    (name, value)
                })
                .collect(),
        )
    }
    let value = convert_table(table.get_ref(), "", &mut places);
    places.insert(String::new(), Place { key: None, value: 0..text.len() });
    Some(Doc { value, places })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at<'a>(text: &'a str, doc: &Doc, pointer: &str) -> (&'a str, Option<&'a str>) {
        let p =
            doc.places.get(pointer).unwrap_or_else(|| panic!("no {pointer} in {:?}", doc.places));
        (&text[p.value.clone()], p.key.clone().map(|k| &text[k]))
    }

    #[test]
    fn json_is_read_as_serde_would_with_every_part_placed() {
        let text = r#"{"a~b": [1, 2.5e1, "x\n"], "c/d": {"e": null}}"#;
        let doc = json(text, Options::default()).unwrap();
        assert_eq!(doc.value, serde_json::from_str::<Value>(text).unwrap());
        assert_eq!(at(text, &doc, "/a~0b/1"), ("2.5e1", None));
        assert_eq!(at(text, &doc, "/c~1d/e"), ("null", Some("\"e\"")));
        assert_eq!(at(text, &doc, "").0, text);
        assert!(json("{", Options::default()).is_none());
    }

    #[test]
    fn yaml_reads_by_the_core_schema_with_anchors_and_merges() {
        let text = "base: &base\n  image: nginx\n  ports: [80]\nweb:\n  <<: *base\n  ports: [8080]\n  replicas: 3\n  enabled: yes\n  ratio: 1.5\n  label: '3'\n  empty:\n";
        let doc = yaml(text).unwrap();
        let web = &doc.value["web"];
        assert_eq!(web["image"], "nginx", "merged from the anchor");
        assert_eq!(web["ports"], serde_json::json!([8080]), "the mapping's own key wins");
        assert_eq!(web["replicas"], 3);
        assert_eq!(web["enabled"], "yes", "YAML 1.2: yes is a string");
        assert_eq!(web["ratio"], 1.5);
        assert_eq!(web["label"], "3", "quoted stays a string");
        assert_eq!(web["empty"], Value::Null);
        assert_eq!(at(text, &doc, "/web/replicas"), ("3", Some("replicas")));
        assert_eq!(at(text, &doc, "/web/image").1, Some("<<"), "a merged key points at the merge");
        assert!(yaml("a: [1, 2\n").is_none());
    }

    #[test]
    fn toml_reads_with_dates_as_strings_and_every_key_placed() {
        let text = "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = 2024\nwhen = 1979-05-27T07:32:00Z\n\n[dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\n";
        let doc = toml(text).unwrap();
        assert_eq!(doc.value["package"]["edition"], 2024);
        assert_eq!(doc.value["package"]["when"], "1979-05-27T07:32:00Z");
        assert_eq!(doc.value["dependencies"]["serde"]["features"][0], "derive");
        assert_eq!(at(text, &doc, "/package/edition"), ("2024", Some("edition")));
        assert_eq!(at(text, &doc, "/dependencies/serde/features/0").0, "\"derive\"");
        assert!(toml("a = \n").is_none());
    }
}
