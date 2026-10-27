//! RBF metadata (`RBF0`): XML stored as a stream of typed records, following
//! CodeWalker's `Rbf.cs` (Copyright (c) 2015 Neodymium, MIT) and its XML
//! conversion (`MetaXml.cs`, `XmlRbf.cs`).
//!
//! Records name their element through a table of names built as the stream
//! goes. Elements have attributes (their first records) and children: values
//! (unsigned, float, boolean, vector, string) and raw bytes (text content).

use std::fmt::Write;

use crate::Error;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Node {
    Element { name: String, attributes: Vec<Node>, children: Vec<Node> },
    U32 { name: String, value: u32 },
    Bool { name: String, value: bool },
    F32 { name: String, value: f32 },
    Vec3 { name: String, value: [f32; 3] },
    Str { name: String, value: String },
    /// Text content: a NUL-terminated string, or numbers when the element
    /// has a `content` attribute.
    Bytes(Vec<u8>),
}

const MAGIC: &[u8; 4] = b"RBF0";

/// Deepest element nesting accepted. Real files nest a few levels; the tree
/// is converted (and dropped) recursively, so it must stay well off the stack's end.
const MAX_DEPTH: usize = 64;

pub(super) fn parse(data: &[u8]) -> Result<Node, Error> {
    if !data.starts_with(MAGIC) {
        return Err(Error::Format("not RBF metadata".into()));
    }
    let bad = || Error::Format("RBF metadata is truncated or invalid".into());
    let mut at = 4;
    let mut take = |n: usize| -> Result<&[u8], Error> {
        let b = data.get(at..at + n).ok_or_else(bad)?;
        at += n;
        Ok(b)
    };
    let mut names: Vec<(String, u8)> = Vec::new();
    // Every record copies its name: a long name reused by many short records
    // would multiply the file's size.
    let mut name_bytes = data.len().saturating_mul(8).saturating_add(1 << 20);
    // Open elements, each with the attributes still to come.
    let mut stack: Vec<(String, Vec<Node>, Vec<Node>, u16)> = Vec::new();
    loop {
        let index = take(1)?[0];
        let push = |stack: &mut Vec<(String, Vec<Node>, Vec<Node>, u16)>, node: Node| -> Result<(), Error> {
            let top = stack.last_mut().ok_or_else(bad)?;
            if top.3 > 0 {
                top.3 -= 1;
                top.1.push(node);
            } else {
                top.2.push(node);
            }
            Ok(())
        };
        match index {
            0xFF => {
                if take(1)?[0] != 0xFF {
                    return Err(bad());
                }
                let (name, attributes, children, _) = stack.pop().ok_or_else(bad)?;
                let element = Node::Element { name, attributes, children };
                if stack.is_empty() {
                    return if at == data.len() { Ok(element) } else { Err(bad()) };
                }
                push(&mut stack, element)?;
            }
            0xFD => {
                if take(1)?[0] != 0xFF {
                    return Err(bad());
                }
                let len = u32::from_le_bytes(take(4)?.try_into().unwrap()) as usize;
                let bytes = take(len)?.to_vec();
                push(&mut stack, Node::Bytes(bytes))?;
            }
            index => {
                let ty = take(1)?[0];
                if index as usize == names.len() {
                    let len = u16::from_le_bytes(take(2)?.try_into().unwrap()) as usize;
                    names.push((String::from_utf8_lossy(take(len)?).into_owned(), ty));
                }
                let name = names.get(index as usize).ok_or_else(bad)?.0.clone();
                name_bytes = name_bytes.checked_sub(name.len()).ok_or_else(|| Error::Format("RBF metadata repeats its names too much".into()))?;
                let f32_at = |b: &[u8]| f32::from_le_bytes(b.try_into().unwrap());
                let node = match ty {
                    0x00 => {
                        if stack.len() >= MAX_DEPTH {
                            return Err(Error::Format("RBF metadata nests too deeply".into()));
                        }
                        take(4)?;
                        let attributes = u16::from_le_bytes(take(2)?.try_into().unwrap());
                        stack.push((name, Vec::new(), Vec::new(), attributes));
                        continue;
                    }
                    0x10 => Node::U32 { name, value: u32::from_le_bytes(take(4)?.try_into().unwrap()) },
                    0x20 => Node::Bool { name, value: true },
                    0x30 => Node::Bool { name, value: false },
                    0x40 => Node::F32 { name, value: f32_at(take(4)?) },
                    0x50 => {
                        let b = take(12)?;
                        Node::Vec3 { name, value: [f32_at(&b[0..4]), f32_at(&b[4..8]), f32_at(&b[8..12])] }
                    }
                    0x60 => {
                        let len = u16::from_le_bytes(take(2)?.try_into().unwrap()) as usize;
                        Node::Str { name, value: String::from_utf8_lossy(take(len)?).into_owned() }
                    }
                    other => return Err(Error::Unsupported(format!("RBF record type {other:#x}"))),
                };
                push(&mut stack, node)?;
            }
        }
    }
}

pub(super) fn write(root: &Node) -> Result<Vec<u8>, Error> {
    let mut out = MAGIC.to_vec();
    let mut names: Vec<String> = Vec::new();
    write_node(root, &mut out, &mut names)?;
    Ok(out)
}

fn record(name: &str, ty: u8, out: &mut Vec<u8>, names: &mut Vec<String>) -> Result<(), Error> {
    match names.iter().position(|n| n == name) {
        Some(i) => out.extend_from_slice(&[i as u8, ty]),
        None => {
            // Indexes 0xFD and 0xFF mark bytes and closing records.
            if names.len() >= 0xFD {
                return Err(Error::Unsupported("RBF metadata with more than 253 names".into()));
            }
            out.extend_from_slice(&[names.len() as u8, ty]);
            out.extend_from_slice(&len16(name.len(), "bytes in a name")?);
            out.extend_from_slice(name.as_bytes());
            names.push(name.to_string());
        }
    }
    Ok(())
}

/// A length as a 16-bit field, refused when it does not fit.
fn len16(len: usize, what: &str) -> Result<[u8; 2], Error> {
    u16::try_from(len).map(u16::to_le_bytes).map_err(|_| Error::Edit(format!("RBF: too many {what} ({len}, at most 65535)")))
}

fn write_node(node: &Node, out: &mut Vec<u8>, names: &mut Vec<String>) -> Result<(), Error> {
    match node {
        Node::Element { name, attributes, children } => {
            record(name, 0x00, out, names)?;
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&len16(attributes.len(), "attributes")?);
            for n in attributes.iter().chain(children) {
                write_node(n, out, names)?;
            }
            out.extend_from_slice(&[0xFF, 0xFF]);
        }
        Node::U32 { name, value } => {
            record(name, 0x10, out, names)?;
            out.extend_from_slice(&value.to_le_bytes());
        }
        Node::Bool { name, value } => record(name, if *value { 0x20 } else { 0x30 }, out, names)?,
        Node::F32 { name, value } => {
            record(name, 0x40, out, names)?;
            out.extend_from_slice(&value.to_le_bytes());
        }
        Node::Vec3 { name, value } => {
            record(name, 0x50, out, names)?;
            for v in value {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        Node::Str { name, value } => {
            record(name, 0x60, out, names)?;
            out.extend_from_slice(&len16(value.len(), "bytes in a string")?);
            out.extend_from_slice(value.as_bytes());
        }
        Node::Bytes(bytes) => {
            out.extend_from_slice(&[0xFD, 0xFF]);
            let len = u32::try_from(bytes.len()).map_err(|_| Error::Edit("RBF: text longer than 4 GB".into()))?;
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(bytes);
        }
    }
    Ok(())
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A value as attribute text: `0x…` unsigned, floats, `True`/`False`, strings.
fn value_text(node: &Node) -> Option<(&str, String)> {
    Some(match node {
        Node::U32 { name, value } => (name, format!("0x{value:08X}")),
        Node::Bool { name, value } => (name, if *value { "True" } else { "False" }.to_string()),
        Node::F32 { name, value } => (name, value.to_string()),
        Node::Str { name, value } => (name, value.clone()),
        _ => return None,
    })
}

pub(super) fn to_xml(root: &Node) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    write_xml(root, 0, &mut out);
    out
}

fn write_xml(node: &Node, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match node {
        Node::Element { name, attributes, children } => {
            let mut attrs = String::new();
            for a in attributes {
                if let Some((n, v)) = value_text(a) {
                    let _ = write!(attrs, " {n}=\"{}\"", escape(&v));
                }
            }
            if children.is_empty() {
                let _ = writeln!(out, "{pad}<{name}{attrs} />");
                return;
            }
            let content = attributes.iter().find_map(|a| match a {
                Node::Str { name, value } if name == "content" => Some(value.as_str()),
                _ => None,
            });
            // Text alone: on one line.
            if let ([Node::Bytes(bytes)], None) = (&children[..], content) {
                let text = String::from_utf8_lossy(bytes.strip_suffix(&[0]).unwrap_or(bytes));
                let _ = writeln!(out, "{pad}<{name}{attrs}>{}</{name}>", escape(&text));
                return;
            }
            let _ = writeln!(out, "{pad}<{name}{attrs}>");
            for child in children {
                match (child, content) {
                    (Node::Bytes(bytes), Some("char_array")) => {
                        for b in bytes {
                            let _ = writeln!(out, "{pad} {b}");
                        }
                    }
                    (Node::Bytes(bytes), Some("short_array")) => {
                        for s in bytes.as_chunks::<2>().0 {
                            let _ = writeln!(out, "{pad} {}", u16::from_le_bytes(*s));
                        }
                    }
                    (Node::Bytes(bytes), _) => {
                        let text = String::from_utf8_lossy(bytes.strip_suffix(&[0]).unwrap_or(bytes));
                        let _ = writeln!(out, "{pad} {}", escape(&text));
                    }
                    (Node::Vec3 { name, value: [x, y, z] }, _) => {
                        let _ = writeln!(out, "{pad} <{name} x=\"{x}\" y=\"{y}\" z=\"{z}\" />");
                    }
                    (Node::Element { .. }, _) => write_xml(child, indent + 1, out),
                    (Node::Str { name, value }, _) => {
                        let _ = writeln!(out, "{pad} <{name}>{}</{name}>", escape(value));
                    }
                    (value, _) => {
                        if let Some((n, v)) = value_text(value) {
                            let _ = writeln!(out, "{pad} <{n} value=\"{}\" />", escape(&v));
                        }
                    }
                }
            }
            let _ = writeln!(out, "{pad}</{name}>");
        }
        other => write_xml(&Node::Element { name: "value".into(), attributes: Vec::new(), children: vec![other.clone()] }, indent, out),
    }
}

/// A value from attribute text, as CodeWalker reads it back.
fn value_node(name: &str, text: &str) -> Node {
    let name = name.to_string();
    match text {
        "True" => Node::Bool { name, value: true },
        "False" => Node::Bool { name, value: false },
        _ => {
            if let Some(hex) = text.strip_prefix("0x")
                && let Ok(value) = u32::from_str_radix(hex, 16)
            {
                return Node::U32 { name, value };
            }
            match text.parse::<f32>() {
                Ok(value) if !text.is_empty() => Node::F32 { name, value },
                _ => Node::Str { name, value: text.to_string() },
            }
        }
    }
}

pub(super) fn from_xml(text: &str) -> Result<Node, Error> {
    let doc = roxmltree::Document::parse(text).map_err(|e| Error::Edit(format!("XML: {e}")))?;
    read_xml(doc.root_element(), 1)
}

fn read_xml(element: roxmltree::Node, depth: usize) -> Result<Node, Error> {
    let name = element.tag_name().name().to_string();
    if depth > MAX_DEPTH {
        return Err(Error::Edit(format!("<{name}>: elements nest more than {MAX_DEPTH} deep")));
    }
    let attrs: Vec<_> = element.attributes().collect();
    let elements: Vec<_> = element.children().filter(|n| n.is_element()).collect();
    if let Some(value) = element.attribute("value").filter(|v| !v.is_empty()) {
        return Ok(value_node(&name, value));
    }
    if attrs.len() == 3 && ["x", "y", "z"].iter().all(|a| element.attribute(*a).is_some()) {
        let axis = |a: &str| element.attribute(a).unwrap().parse::<f32>().map_err(|_| Error::Edit(format!("<{name}>: invalid {a}")));
        return Ok(Node::Vec3 { name: name.clone(), value: [axis("x")?, axis("y")?, axis("z")?] });
    }
    let attributes = attrs.iter().map(|a| value_node(a.name(), a.value())).collect();
    let content = element.attribute("content");
    let mut children = Vec::new();
    for child in element.children() {
        if child.is_element() {
            children.push(read_xml(child, depth + 1)?);
        } else if let Some(text) = child.text().filter(|t| !(!elements.is_empty() && t.trim().is_empty())) {
            let numbers = || text.split_whitespace();
            let bytes = match content {
                Some("char_array") => numbers().map(|n| n.parse::<u8>()).collect::<Result<Vec<u8>, _>>(),
                Some("short_array") => numbers().map(|n| n.parse::<u16>().map(u16::to_le_bytes)).collect::<Result<Vec<_>, _>>().map(|v| v.concat()),
                _ => {
                    let text = if elements.is_empty() && attrs.is_empty() { text.to_string() } else { text.trim().to_string() };
                    Ok([text.as_bytes(), &[0]].concat())
                }
            };
            let bytes = bytes.map_err(|_| Error::Edit(format!("<{name}>: invalid numbers")))?;
            if !(content.is_some() && bytes.is_empty()) {
                children.push(Node::Bytes(bytes));
            }
        }
    }
    Ok(Node::Element { name, attributes, children })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let root = Node::Element {
            name: "Root".into(),
            attributes: vec![Node::Str { name: "type".into(), value: "test".into() }],
            children: vec![
                Node::F32 { name: "speed".into(), value: 1.5 },
                Node::U32 { name: "flags".into(), value: 0xA },
                Node::Bool { name: "on".into(), value: true },
                Node::Vec3 { name: "pos".into(), value: [1.0, -2.0, 0.25] },
                Node::Element { name: "label".into(), attributes: vec![], children: vec![Node::Bytes(b"hello & bye\0".to_vec())] },
                Node::Element {
                    name: "ids".into(),
                    attributes: vec![Node::Str { name: "content".into(), value: "short_array".into() }],
                    children: vec![Node::Bytes(vec![1, 0, 2, 0])],
                },
                Node::Element { name: "empty".into(), attributes: vec![], children: vec![] },
            ],
        };
        let bin = write(&root).unwrap();
        assert_eq!(parse(&bin).unwrap(), root);
        let xml = to_xml(&root);
        assert!(xml.contains("<speed value=\"1.5\" />") && xml.contains("<label>hello &amp; bye</label>"), "{xml}");
        assert_eq!(from_xml(&xml).unwrap(), root);
    }
}
