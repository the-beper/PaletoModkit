//! Metadata values as XML, in the style of CodeWalker's `MetaXml.cs`:
//! numbers in `value` attributes, vectors in `x`/`y`/`z`/`w`, names and
//! strings as text, array items as `<Item>`, pointed-to structures with a
//! `type` attribute.

use std::fmt::Write;

use super::{Kind, Schema, Value};
use crate::Error;
use crate::names::Names;

pub(super) fn write(schema: &Schema, names: &Names, root_type: u32, root: &Value) -> String {
    let mut w = XmlWriter { schema, names, out: String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n") };
    let name = names.name(root_type).into_owned();
    w.open(0, &name, "");
    w.struct_body(1, root_type, root);
    w.close(0, &name);
    w.out
}

struct XmlWriter<'a> {
    schema: &'a Schema,
    names: &'a Names,
    out: String,
}

impl XmlWriter<'_> {
    fn indent(&mut self, n: usize) {
        self.out.extend(std::iter::repeat_n(' ', n));
    }

    fn open(&mut self, indent: usize, name: &str, attrs: &str) {
        self.indent(indent);
        let _ = writeln!(self.out, "<{name}{attrs}>");
    }

    fn close(&mut self, indent: usize, name: &str) {
        self.indent(indent);
        let _ = writeln!(self.out, "</{name}>");
    }

    fn empty(&mut self, indent: usize, name: &str, attrs: &str) {
        self.indent(indent);
        let _ = writeln!(self.out, "<{name}{attrs} />");
    }

    fn text(&mut self, indent: usize, name: &str, attrs: &str, text: &str) {
        if text.is_empty() {
            return self.empty(indent, name, attrs);
        }
        self.indent(indent);
        let _ = writeln!(self.out, "<{name}{attrs}>{}</{name}>", escape(text));
    }

    fn struct_body(&mut self, indent: usize, ty: u32, value: &Value) {
        let (Some(info), Value::Struct(values)) = (self.schema.structs.get(&ty), value) else { return };
        for (field, value) in info.fields.iter().zip(values) {
            let name = self.names.name(field.name).into_owned();
            self.field(indent, &name, &field.kind, value);
        }
    }

    fn field(&mut self, indent: usize, name: &str, kind: &Kind, value: &Value) {
        match (kind, value) {
            (Kind::Struct(ty), v) => {
                let has_fields = self.schema.structs.get(ty).is_some_and(|s| !s.fields.is_empty());
                if has_fields {
                    self.open(indent, name, "");
                    self.struct_body(indent + 1, *ty, v);
                    self.close(indent, name);
                } else {
                    self.empty(indent, name, "");
                }
            }
            (Kind::StructPtr, Value::Ptr(None)) => self.empty(indent, name, ""),
            (Kind::StructPtr, Value::Ptr(Some((ty, v)))) => {
                let attrs = format!(" type=\"{}\"", self.names.name(*ty));
                self.open(indent, name, &attrs);
                self.struct_body(indent + 1, *ty, v);
                self.close(indent, name);
            }
            (Kind::Array { elem, .. }, Value::Array(items)) => {
                let attrs = match &**elem {
                    Kind::Struct(ty) => format!(" itemType=\"{}\"", self.names.name(*ty)),
                    _ => String::new(),
                };
                if items.is_empty() {
                    return self.empty(indent, name, &attrs);
                }
                if is_number(elem) {
                    // Numbers as text, ten per row.
                    let numbers: Vec<String> = items.iter().map(|v| self.scalar(elem, v)).collect();
                    if numbers.len() <= 10 {
                        return self.text(indent, name, &attrs, &numbers.join(" "));
                    }
                    self.open(indent, name, &attrs);
                    for row in numbers.chunks(10) {
                        self.indent(indent + 1);
                        self.out.push_str(&row.join(" "));
                        self.out.push('\n');
                    }
                    return self.close(indent, name);
                }
                self.open(indent, name, &attrs);
                for item in items {
                    self.field(indent + 1, "Item", elem, item);
                }
                self.close(indent, name);
            }
            (Kind::Map, Value::Map(ty, entries)) => {
                if entries.is_empty() {
                    return self.empty(indent, name, "");
                }
                self.open(indent, name, "");
                for entry in entries {
                    self.map_entry(indent + 1, *ty, entry);
                }
                self.close(indent, name);
            }
            (Kind::Bytes, Value::Bytes(bytes)) => {
                if bytes.is_empty() {
                    return self.empty(indent, name, "");
                }
                self.open(indent, name, "");
                for row in bytes.chunks(32) {
                    self.indent(indent + 1);
                    let hex: Vec<String> = row.iter().map(|b| format!("{b:02X}")).collect();
                    self.out.push_str(&hex.join(" "));
                    self.out.push('\n');
                }
                self.close(indent, name);
            }
            (Kind::Vec2 | Kind::Vec3 | Kind::Vec3Padded | Kind::Vec4, Value::Vec(v)) => {
                let mut attrs = String::new();
                for (axis, c) in ["x", "y", "z", "w"].iter().zip(v).take(super::vec_len(kind)) {
                    let _ = write!(attrs, " {axis}=\"{}\"", f32::from_bits(*c));
                }
                self.empty(indent, name, &attrs);
            }
            (Kind::Hash | Kind::Enum { .. } | Kind::Chars(_) | Kind::CharPtr | Kind::StrPtr, v) => {
                let text = self.scalar(kind, v);
                self.text(indent, name, "", &text);
            }
            (Kind::Flags { enumeration, .. }, v) if enumeration.is_some_and(|e| self.schema.enums.contains_key(&e)) => {
                let text = self.scalar(kind, v);
                self.text(indent, name, "", &text);
            }
            (kind, v) => {
                let attrs = format!(" value=\"{}\"", self.scalar(kind, v));
                self.empty(indent, name, &attrs);
            }
        }
    }

    /// A PSO map entry: `<Item key="...">` with the item's content.
    fn map_entry(&mut self, indent: usize, ty: u32, entry: &Value) {
        let (Some(info), Value::Struct(values)) = (self.schema.structs.get(&ty), entry) else { return };
        let (Some(key), Some(item)) = (map_part(info, "Key"), map_part(info, "Item")) else { return };
        let key_text = escape(&self.scalar(&info.fields[key].kind, &values[key]));
        let item_kind = info.fields[item].kind.clone();
        match (&item_kind, &values[item]) {
            (Kind::Struct(t), v) => {
                let attrs = format!(" type=\"{}\" key=\"{key_text}\"", self.names.name(*t));
                self.open(indent, "Item", &attrs);
                self.struct_body(indent + 1, *t, v);
                self.close(indent, "Item");
            }
            (Kind::StructPtr, Value::Ptr(Some((t, v)))) => {
                let attrs = format!(" type=\"{}\" key=\"{key_text}\"", self.names.name(*t));
                self.open(indent, "Item", &attrs);
                self.struct_body(indent + 1, *t, v);
                self.close(indent, "Item");
            }
            (kind, v) => {
                let text = self.scalar(kind, v);
                self.text(indent, "Item", &format!(" type=\"String\" key=\"{key_text}\""), &text);
            }
        }
    }

    /// A value as text: numbers, names or strings.
    fn scalar(&self, kind: &Kind, value: &Value) -> String {
        match (kind, value) {
            (_, Value::Bool(b)) => b.to_string(),
            (Kind::HexU32, Value::Int(v)) => format!("0x{:08X}", *v as u32),
            (Kind::U64, Value::Int(v)) => (*v as u64).to_string(),
            (Kind::Enum { enumeration, .. }, Value::Int(v)) => {
                let found = self.schema.enums.get(enumeration).and_then(|vals| vals.iter().find(|(_, x)| x == v));
                found.map_or_else(|| v.to_string(), |(n, _)| self.names.name(*n).into_owned())
            }
            (Kind::Flags { enumeration: Some(e), sep, .. }, Value::Int(v)) if self.schema.enums.contains_key(e) => {
                let mut rest = *v as u64;
                let mut parts = Vec::new();
                for (name, bit) in &self.schema.enums[e] {
                    let mask = 1u64.checked_shl(*bit as u32).unwrap_or(0);
                    if mask != 0 && rest & mask != 0 {
                        parts.push(self.names.name(*name).into_owned());
                        rest &= !mask;
                    }
                }
                // Bits without a name keep their value.
                if rest != 0 {
                    parts.push(rest.to_string());
                }
                parts.join(sep)
            }
            (_, Value::Int(v)) => v.to_string(),
            (_, Value::Float(bits)) => f32::from_bits(*bits).to_string(),
            (_, Value::Hash(h)) => self.names.name(*h).into_owned(),
            (_, Value::Str(s)) => s.clone().unwrap_or_default(),
            _ => String::new(),
        }
    }
}

fn map_part(info: &super::StructInfo, name: &str) -> Option<usize> {
    let hash = crate::hash::joaat(name.as_bytes());
    info.fields.iter().position(|f| f.name == hash)
}

fn is_number(kind: &Kind) -> bool {
    matches!(
        kind,
        Kind::Bool | Kind::I8 | Kind::U8 | Kind::I16 | Kind::U16 | Kind::I32 | Kind::U32 | Kind::HexU32 | Kind::U64 | Kind::F32 | Kind::Half
    )
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// `map_types`: entry structures of the original file's maps, preferred
/// when several fit.
pub(super) fn read(schema: &Schema, names: &Names, root_type: u32, text: &str, map_types: &[u32]) -> Result<Value, Error> {
    let doc = roxmltree::Document::parse(text).map_err(|e| Error::Edit(format!("XML: {e}")))?;
    let root = doc.root_element();
    if names.hash(root.tag_name().name()) != root_type {
        return Err(Error::Edit(format!("the root element must be <{}>", names.name(root_type))));
    }
    XmlReader { schema, names, map_types }.read_struct(root, root_type)
}

struct XmlReader<'a> {
    schema: &'a Schema,
    names: &'a Names,
    map_types: &'a [u32],
}

type Node<'a, 'i> = roxmltree::Node<'a, 'i>;

fn elements<'a, 'i>(node: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    node.children().filter(|n| n.is_element())
}

fn text_of(node: Node) -> String {
    node.text().unwrap_or("").trim().to_string()
}

/// Where a problem is, for error messages: `<a>/<b>` with a line number.
fn at(node: Node) -> String {
    let pos = node.document().text_pos_at(node.range().start);
    format!("<{}> (line {})", node.tag_name().name(), pos.row)
}

impl XmlReader<'_> {
    fn read_struct(&self, node: Node, ty: u32) -> Result<Value, Error> {
        let info = self.schema.structs.get(&ty).ok_or_else(|| Error::Edit(format!("unknown structure {}", self.names.name(ty))))?;
        let children: Vec<Node> = elements(node).collect();
        for child in &children {
            let hash = self.names.hash(child.tag_name().name());
            if !info.fields.iter().any(|f| f.name == hash) {
                return Err(Error::Edit(format!("{}: no such field in {}", at(*child), self.names.name(ty))));
            }
        }
        let mut values = Vec::with_capacity(info.fields.len());
        for field in &info.fields {
            let child = children.iter().find(|c| self.names.hash(c.tag_name().name()) == field.name).copied();
            values.push(match child {
                Some(child) => self.read_field(child, &field.kind)?,
                None => self.default(&field.kind)?,
            });
        }
        Ok(Value::Struct(values))
    }

    fn default(&self, kind: &Kind) -> Result<Value, Error> {
        Ok(match kind {
            Kind::Bool => Value::Bool(false),
            Kind::F32 => Value::Float(0),
            Kind::Vec2 | Kind::Vec3 | Kind::Vec3Padded | Kind::Vec4 => Value::Vec([0; 4]),
            Kind::Hash => Value::Hash(0),
            Kind::Chars(_) | Kind::CharPtr | Kind::StrPtr => Value::Str(None),
            Kind::Struct(ty) => {
                let info = self.schema.structs.get(ty).ok_or_else(|| Error::Edit(format!("unknown structure {}", self.names.name(*ty))))?;
                Value::Struct(info.fields.iter().map(|f| self.default(&f.kind)).collect::<Result<_, _>>()?)
            }
            Kind::StructPtr => Value::Ptr(None),
            Kind::Array { .. } => Value::Array(Vec::new()),
            Kind::Bytes => Value::Bytes(Vec::new()),
            Kind::Map => Value::Map(0, Vec::new()),
            _ => Value::Int(0),
        })
    }

    fn read_field(&self, node: Node, kind: &Kind) -> Result<Value, Error> {
        let bad = |what: &str| Error::Edit(format!("{}: {what}", at(node)));
        Ok(match kind {
            Kind::Struct(ty) => self.read_struct(node, *ty)?,
            Kind::StructPtr => match node.attribute("type") {
                None if elements(node).next().is_none() => Value::Ptr(None),
                None => return Err(bad("a type=\"...\" attribute is needed")),
                Some(t) => {
                    let ty = self.names.hash(t);
                    Value::Ptr(Some((ty, Box::new(self.read_struct(node, ty)?))))
                }
            },
            Kind::Array { elem, .. } if is_number(elem) => {
                let text = text_of(node);
                let items = text.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty());
                Value::Array(items.map(|s| self.scalar(node, elem, s)).collect::<Result<_, _>>()?)
            }
            Kind::Array { elem, .. } => {
                let mut items = Vec::new();
                for child in elements(node) {
                    if child.tag_name().name() != "Item" {
                        return Err(Error::Edit(format!("{}: expected <Item>", at(child))));
                    }
                    items.push(self.read_field(child, elem)?);
                }
                Value::Array(items)
            }
            Kind::Map => self.read_map(node)?,
            Kind::Bytes => {
                let text = text_of(node);
                let bytes = text.split_whitespace().map(|b| u8::from_str_radix(b, 16)).collect::<Result<_, _>>();
                Value::Bytes(bytes.map_err(|_| bad("expected hexadecimal bytes"))?)
            }
            Kind::Vec2 | Kind::Vec3 | Kind::Vec3Padded | Kind::Vec4 => {
                let mut v = [0u32; 4];
                for (axis, c) in ["x", "y", "z", "w"].iter().zip(v.iter_mut()).take(super::vec_len(kind)) {
                    let s = node.attribute(*axis).unwrap_or("0");
                    *c = parse_float(s).ok_or_else(|| bad(&format!("invalid {axis}=\"{s}\"")))?.to_bits();
                }
                Value::Vec(v)
            }
            // Strings keep their spaces.
            Kind::Chars(_) | Kind::CharPtr | Kind::StrPtr => Value::Str(node.text().filter(|t| !t.is_empty()).map(str::to_string)),
            Kind::Hash | Kind::Enum { .. } => {
                let text = node.attribute("value").map(str::to_string).unwrap_or_else(|| text_of(node));
                self.scalar(node, kind, &text)?
            }
            Kind::Flags { .. } => {
                let text = node.attribute("value").map(str::to_string).unwrap_or_else(|| text_of(node));
                self.scalar(node, kind, &text)?
            }
            _ => {
                let text = node.attribute("value").ok_or_else(|| bad("a value=\"...\" attribute is needed"))?;
                self.scalar(node, kind, text)?
            }
        })
    }

    fn read_map(&self, node: Node) -> Result<Value, Error> {
        let mut entries = Vec::new();
        let mut entry_type = 0;
        for child in elements(node) {
            let key = child.attribute("key").ok_or_else(|| Error::Edit(format!("{}: a key=\"...\" attribute is needed", at(child))))?;
            // The entry structure: the one with Key and Item fields whose item matches.
            let ty = self.map_entry_type(child)?;
            entry_type = ty;
            let info = &self.schema.structs[&ty];
            let (k, i) = (map_part(info, "Key").unwrap(), map_part(info, "Item").unwrap());
            let mut values: Vec<Value> = info.fields.iter().map(|f| self.default(&f.kind)).collect::<Result<_, _>>()?;
            values[k] = self.scalar(child, &info.fields[k].kind, key)?;
            values[i] = match &info.fields[i].kind {
                Kind::Struct(t) => self.read_struct(child, *t)?,
                Kind::StructPtr => {
                    let t = self.names.hash(child.attribute("type").unwrap_or(""));
                    Value::Ptr(Some((t, Box::new(self.read_struct(child, t)?))))
                }
                kind => self.scalar(child, kind, &text_of(child))?,
            };
            entries.push(Value::Struct(values));
        }
        Ok(Value::Map(entry_type, entries))
    }

    fn map_entry_type(&self, item: Node) -> Result<u32, Error> {
        let item_type = item.attribute("type").map(|t| self.names.hash(t));
        let (key, item_name) = (crate::hash::joaat(b"Key"), crate::hash::joaat(b"Item"));
        let fits = |info: &super::StructInfo| {
            let field = |h: u32| info.fields.iter().find(|f| f.name == h);
            match (field(key), field(item_name), item_type) {
                (Some(_), Some(f), Some(t)) if t == crate::hash::joaat(b"String") => {
                    matches!(f.kind, Kind::Hash | Kind::Chars(_) | Kind::CharPtr | Kind::StrPtr)
                }
                (Some(_), Some(f), Some(t)) => f.kind == Kind::Struct(t) || f.kind == Kind::StructPtr,
                _ => false,
            }
        };
        let mut found: Vec<u32> = self.schema.structs.iter().filter(|(_, s)| fits(s)).map(|(t, _)| *t).collect();
        found.sort_by_key(|t| (!self.map_types.contains(t), *t));
        found.first().copied().ok_or_else(|| Error::Edit(format!("{}: no map entry structure fits this item", at(item))))
    }

    fn scalar(&self, node: Node, kind: &Kind, text: &str) -> Result<Value, Error> {
        let bad = || Error::Edit(format!("{}: invalid value \"{text}\"", at(node)));
        let text = text.trim();
        Ok(match kind {
            Kind::Bool => Value::Bool(match text {
                "true" | "True" | "1" => true,
                "false" | "False" | "0" | "" => false,
                _ => return Err(bad()),
            }),
            Kind::F32 => Value::Float(parse_float(text).ok_or_else(bad)?.to_bits()),
            Kind::Hash => Value::Hash(self.names.hash(text)),
            Kind::Chars(_) | Kind::CharPtr | Kind::StrPtr => Value::Str((!text.is_empty()).then(|| text.to_string())),
            Kind::Enum { enumeration, .. } => {
                let hash = self.names.hash(text);
                let named = self.schema.enums.get(enumeration).and_then(|vals| vals.iter().find(|(n, _)| *n == hash));
                match named {
                    Some((_, v)) => Value::Int(*v),
                    None => Value::Int(parse_int(text).ok_or_else(bad)?),
                }
            }
            Kind::Flags { enumeration, .. } => {
                let values = enumeration.and_then(|e| self.schema.enums.get(&e));
                let mut bits = 0u64;
                for part in text.split(|c: char| c.is_whitespace() || c == ',' || c == '|').filter(|s| !s.is_empty()) {
                    let hash = self.names.hash(part);
                    match values.and_then(|vals| vals.iter().find(|(n, _)| *n == hash)) {
                        Some((_, bit)) => bits |= 1u64.checked_shl(*bit as u32).unwrap_or(0),
                        None => bits |= parse_int(part).ok_or_else(bad)? as u64,
                    }
                }
                Value::Int(bits as i64)
            }
            _ => Value::Int(parse_int(text).ok_or_else(bad)?),
        })
    }
}

fn parse_int(text: &str) -> Option<i64> {
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok().map(|v| v as i64),
        None => text.parse::<i64>().ok().or_else(|| text.parse::<u64>().ok().map(|v| v as i64)),
    }
}

fn parse_float(text: &str) -> Option<f32> {
    match text.trim() {
        "NaN" => Some(f32::NAN),
        t => t.parse().ok(),
    }
}
