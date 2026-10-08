//! OIV `<xml>` commands: XPath-based editing of XML files.

use xee_xpath::{Documents, Queries, Query};

use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Append {
    /// As the first child of the selected node.
    First,
    /// As the last child of the selected node.
    Last,
    Before,
    After,
}

impl Append {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "first" => Some(Self::First),
            "last" => Some(Self::Last),
            "before" => Some(Self::Before),
            "after" => Some(Self::After),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum XmlOp {
    /// `content` is an XML fragment.
    Add { xpath: String, append: Append, content: String },
    Replace { xpath: String, content: String },
    Remove { xpath: String },
}

impl XmlOp {
    fn xpath(&self) -> &str {
        match self {
            XmlOp::Add { xpath, .. } | XmlOp::Replace { xpath, .. } | XmlOp::Remove { xpath } => xpath,
        }
    }
}

/// Applies the operations to an XML document. Returns the new document and
/// one warning per operation whose XPath matched nothing.
pub fn apply(input: &[u8], ops: &[XmlOp]) -> Result<(Vec<u8>, Vec<String>), Error> {
    let bom = input.starts_with(b"\xEF\xBB\xBF");
    let text = std::str::from_utf8(if bom { &input[3..] } else { input }).map_err(|_| Error::Xml("not UTF-8".into()))?;

    let mut docs = Documents::new();
    let doc = docs.add_string_without_uri(text).map_err(|e| Error::Xml(e.to_string()))?;
    let queries = Queries::default();
    let mut warnings = Vec::new();

    for op in ops {
        let query = queries
            .many(op.xpath(), |_, item| Ok(item.to_node()?))
            .map_err(|e| Error::Xml(format!("{}: {e}", op.xpath())))?;
        let nodes = query.execute(&mut docs, doc).map_err(|e| Error::Xml(format!("{}: {e}", op.xpath())))?;
        // Removing what is not there leaves the document as wanted: no warning.
        if nodes.is_empty() && !matches!(op, XmlOp::Remove { .. }) {
            warnings.push(format!("no match for {}", op.xpath()));
            continue;
        }
        let xot = docs.xot_mut();
        for node in nodes {
            let xml_err = |e: xot::Error| Error::Xml(e.to_string());
            match op {
                XmlOp::Remove { .. } => {
                    // Take the node's indentation with it.
                    if let Some(ws) = xot.previous_sibling(node).filter(|&p| is_blank(xot, p)) {
                        xot.remove(ws).map_err(xml_err)?;
                    }
                    xot.remove(node).map_err(xml_err)?;
                }
                XmlOp::Replace { content, .. } => {
                    let new = fragment(xot, content)?;
                    let indent = indent_before(xot, node);
                    let mut prev = node;
                    for (i, n) in new.into_iter().enumerate() {
                        if i == 0 {
                            xot.replace(node, n).map_err(xml_err)?;
                        } else {
                            insert_after(xot, prev, n, indent.as_deref()).map_err(xml_err)?;
                        }
                        prev = n;
                    }
                }
                XmlOp::Add { append, content, .. } => {
                    for n in fragment(xot, content)? {
                        add(xot, node, n, *append).map_err(xml_err)?;
                    }
                }
            }
        }
    }

    let root = docs.document_node(doc).unwrap();
    let element = docs.xot().document_element(root).map_err(|e| Error::Xml(e.to_string()))?;
    let body = docs.xot().to_string(element).map_err(|e| Error::Xml(e.to_string()))?;
    // Keep the original prolog (declaration, comments) and trailing whitespace.
    let start = text.match_indices('<').find(|(i, _)| !matches!(text.as_bytes().get(i + 1), Some(b'?' | b'!'))).map_or(0, |(i, _)| i);
    let tail = &text[text.trim_end().len()..];
    let mut out = format!("{}{body}{tail}", &text[..start]);
    if text.contains("\r\n") {
        out = out.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    let mut bytes = if bom { b"\xEF\xBB\xBF".to_vec() } else { Vec::new() };
    bytes.extend_from_slice(out.as_bytes());
    Ok((bytes, warnings))
}

/// Indents every line by its depth in the element tree, with the file's own
/// indentation unit (a tab, or its smallest step of spaces). Only whitespace
/// at the start and end of lines changes: content, blank lines, line endings
/// and lines inside comments or CDATA stay as they are.
pub fn reindent(text: &str) -> String {
    let unit = indent_unit(text);
    let mut out = String::with_capacity(text.len());
    let (mut depth, mut inside) = (0usize, None::<&str>);
    for line in text.split_inclusive('\n') {
        let body = line.trim_start_matches([' ', '\t']);
        let content = body.trim_end_matches(['\r', '\n']);
        if inside.is_none() && !content.is_empty() {
            let level = if content.starts_with("</") { depth.saturating_sub(1) } else { depth };
            out.push_str(&unit.repeat(level));
            out.push_str(content.trim_end_matches([' ', '\t']));
            out.push_str(&body[content.len()..]);
        } else {
            out.push_str(line);
        }
        // Follow the tags on the line.
        let mut rest = content;
        while !rest.is_empty() {
            if let Some(end) = inside {
                match rest.find(end) {
                    Some(i) => {
                        rest = &rest[i + end.len()..];
                        inside = None;
                    }
                    None => break,
                }
                continue;
            }
            let Some(i) = rest.find('<') else { break };
            rest = &rest[i..];
            let skip = [("<!--", "-->"), ("<![CDATA[", "]]>"), ("<?", "?>"), ("<!", ">")].into_iter().find(|(start, _)| rest.starts_with(start));
            if let Some((start, end)) = skip {
                rest = &rest[start.len()..];
                inside = Some(end);
                continue;
            }
            // A tag, to its '>' outside quotes.
            let mut quote = None;
            let end = rest.char_indices().skip(1).find(|&(_, c)| match quote {
                Some(q) => {
                    if c == q {
                        quote = None;
                    }
                    false
                }
                None if c == '"' || c == '\'' => {
                    quote = Some(c);
                    false
                }
                None => c == '>',
            });
            let Some((end, _)) = end else { break };
            let tag = &rest[..=end];
            if tag.starts_with("</") {
                depth = depth.saturating_sub(1);
            } else if !tag.ends_with("/>") {
                depth += 1;
            }
            rest = &rest[end + 1..];
        }
    }
    out
}

/// A tab if most indented lines use one, else the smallest indentation
/// of spaces (2 if none).
fn indent_unit(text: &str) -> String {
    let indents: Vec<&str> = text.lines().map(|l| &l[..l.len() - l.trim_start_matches([' ', '\t']).len()]).filter(|i| !i.is_empty()).collect();
    let tabs = indents.iter().filter(|i| i.contains('\t')).count();
    if tabs * 2 > indents.len() {
        return "\t".into();
    }
    let spaces = indents.iter().filter(|i| i.bytes().all(|b| b == b' ')).map(|i| i.len()).min().unwrap_or(2);
    " ".repeat(spaces.clamp(1, 8))
}

/// Adds `new` relative to `node`, indented like its future siblings.
fn add(xot: &mut xot::Xot, node: xot::Node, new: xot::Node, append: Append) -> Result<(), xot::Error> {
    match append {
        Append::Last => {
            let last_element = xot.children(node).filter(|&c| xot.is_element(c)).last();
            let indent = last_element.and_then(|c| indent_before(xot, c));
            match (xot.last_child(node), indent) {
                (Some(last), Some(indent)) if is_blank(xot, last) => {
                    xot.insert_before(last, new)?;
                    let ws = xot.new_text(&indent);
                    xot.insert_before(new, ws)
                }
                _ => xot.append(node, new),
            }
        }
        Append::First => match xot.first_child(node) {
            Some(first) if is_blank(xot, first) => {
                let indent = line_indent(xot.text_str(first).unwrap());
                xot.insert_after(first, new)?;
                let ws = xot.new_text(&indent);
                xot.insert_after(new, ws)
            }
            _ => xot.prepend(node, new),
        },
        Append::Before => {
            xot.insert_before(node, new)?;
            if let Some(indent) = indent_before(xot, new) {
                let ws = xot.new_text(&indent);
                xot.insert_before(node, ws)?;
            }
            Ok(())
        }
        Append::After => {
            let indent = indent_before(xot, node);
            insert_after(xot, node, new, indent.as_deref())
        }
    }
}

fn insert_after(xot: &mut xot::Xot, node: xot::Node, new: xot::Node, indent: Option<&str>) -> Result<(), xot::Error> {
    xot.insert_after(node, new)?;
    if let Some(indent) = indent {
        let ws = xot.new_text(indent);
        xot.insert_before(new, ws)?;
    }
    Ok(())
}

fn is_blank(xot: &xot::Xot, node: xot::Node) -> bool {
    xot.text_str(node).is_some_and(|t| t.trim().is_empty())
}

/// The newline and indentation in front of `node`, if it starts a line.
fn indent_before(xot: &xot::Xot, node: xot::Node) -> Option<String> {
    let prev = xot.previous_sibling(node)?;
    let text = xot.text_str(prev)?;
    let tail = &text[text.trim_end().len()..];
    tail.contains('\n').then(|| line_indent(tail))
}

fn line_indent(ws: &str) -> String {
    format!("\n{}", ws.rsplit('\n').next().unwrap_or(""))
}

/// Parses an XML fragment and detaches its top-level nodes, skipping
/// whitespace-only text between them.
fn fragment(xot: &mut xot::Xot, content: &str) -> Result<Vec<xot::Node>, Error> {
    let frag = xot.parse_fragment(content).map_err(|e| Error::Xml(e.to_string()))?;
    let nodes: Vec<_> = xot
        .children(frag)
        .filter(|&n| xot.text_str(n).is_none_or(|t| !t.trim().is_empty()))
        .collect();
    for &n in &nodes {
        xot.detach(n).map_err(|e| Error::Xml(e.to_string()))?;
    }
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reindenting() {
        // dlclist.xml as a mod installer left it: tabs, spaces, nothing, mixed endings.
        let messy = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n\r\n<SMandatoryPacksData>\r\n\t<Paths>\r\n        <Item>dlcpacks:/spMapFixes/</Item>\n<Item>dlcpacks:/greenhouses/</Item>\r\n  \t\t<Item>platform:/dlcPacks/mpBeach/</Item>\t\t\r\n\r\n<!-- <Item>\r\n   old </Item> -->\r\n\t\t<col value=\"a>b\" />\r\n\t</Paths>\r\n</SMandatoryPacksData>\r\n";
        let tidy = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n\r\n<SMandatoryPacksData>\r\n\t<Paths>\r\n\t\t<Item>dlcpacks:/spMapFixes/</Item>\n\t\t<Item>dlcpacks:/greenhouses/</Item>\r\n\t\t<Item>platform:/dlcPacks/mpBeach/</Item>\r\n\r\n\t\t<!-- <Item>\r\n   old </Item> -->\r\n\t\t<col value=\"a>b\" />\r\n\t</Paths>\r\n</SMandatoryPacksData>\r\n";
        assert_eq!(reindent(messy), tidy);
        assert_eq!(reindent(tidy), tidy);
        // Spaces when the file uses them, by its own step.
        assert_eq!(reindent("<a>\n  <b>\n<c/>\n</b>\n    </a>\n"), "<a>\n<b>\n<c/>\n</b>\n</a>\n".replace("<b>", "  <b>").replace("<c/>", "    <c/>").replace("</b>", "  </b>"));
    }

    const DLCLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<SMandatoryPacksData>
	<Paths>
		<Item>dlcpacks:/mpbeach/</Item>
		<Item>dlcpacks:/mpheist/</Item>
		<Item>dlcpacks:/patchday21ng/</Item>
	</Paths>
</SMandatoryPacksData>
"#;

    fn run(ops: &[XmlOp]) -> String {
        let (out, warnings) = apply(DLCLIST.as_bytes(), ops).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        String::from_utf8(out).unwrap()
    }

    fn items(xml: &str) -> Vec<&str> {
        xml.split("<Item>").skip(1).map(|s| s.split("</Item>").next().unwrap()).collect()
    }

    #[test]
    fn add_modes() {
        let add = |append, xpath: &str, v: &str| XmlOp::Add { xpath: xpath.into(), append, content: format!("<Item>{v}</Item>") };
        let out = run(&[
            add(Append::Last, "/SMandatoryPacksData/Paths", "last"),
            add(Append::First, "/SMandatoryPacksData/Paths", "first"),
            add(Append::Before, "/SMandatoryPacksData/Paths/Item[.='dlcpacks:/mpheist/']", "before"),
            add(Append::After, "/SMandatoryPacksData/Paths/Item[.='dlcpacks:/patchday21ng/']", "after"),
        ]);
        assert!(out.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert_eq!(
            items(&out),
            ["first", "dlcpacks:/mpbeach/", "before", "dlcpacks:/mpheist/", "dlcpacks:/patchday21ng/", "after", "last"]
        );
    }

    #[test]
    fn replace_and_remove() {
        let out = run(&[
            XmlOp::Replace { xpath: "/SMandatoryPacksData/Paths/Item[2]".into(), content: "<Item>new</Item>".into() },
            XmlOp::Remove { xpath: "/SMandatoryPacksData/Paths/Item[1]".into() },
        ]);
        assert_eq!(items(&out), ["new", "dlcpacks:/patchday21ng/"]);
    }

    #[test]
    fn no_match_warns() {
        let (_, warnings) = apply(DLCLIST.as_bytes(), &[XmlOp::Replace { xpath: "/Nope".into(), content: "<a/>".into() }]).unwrap();
        assert_eq!(warnings.len(), 1);
        // Nothing to remove is what a removal wants.
        let (_, warnings) = apply(DLCLIST.as_bytes(), &[XmlOp::Remove { xpath: "/Nope".into() }]).unwrap();
        assert!(warnings.is_empty());
    }
}
