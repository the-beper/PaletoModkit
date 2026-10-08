//! OIV `<text>` commands: line-based editing of text files.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    Equal,
    StartWith,
    /// `*` matches any run of characters, `?` a single one.
    Mask,
}

impl Condition {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "equal" => Some(Self::Equal),
            "startwith" => Some(Self::StartWith),
            "mask" => Some(Self::Mask),
            _ => None,
        }
    }

    fn matches(self, line: &str, pattern: &str) -> bool {
        match self {
            Self::Equal => line == pattern,
            Self::StartWith => line.starts_with(pattern),
            Self::Mask => glob(line.as_bytes(), pattern.as_bytes()),
        }
    }
}

#[derive(Debug, Clone)]
pub enum TextOp {
    /// Append a line at the end.
    Add(String),
    /// Insert before or after the first matching line.
    Insert { before: bool, line: String, condition: Condition, text: String },
    /// Replace every matching line.
    Replace { line: String, condition: Condition, text: String },
    /// Delete every matching line.
    Delete { condition: Condition, line: String },
}

/// Applies the operations, keeping the file's line endings.
pub fn apply(text: &str, ops: &[TextOp]) -> String {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing = text.ends_with('\n');
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    for op in ops {
        match op {
            TextOp::Add(t) => lines.push(t.clone()),
            TextOp::Insert { before, line, condition, text } => {
                if let Some(i) = lines.iter().position(|l| condition.matches(l, line)) {
                    lines.insert(if *before { i } else { i + 1 }, text.clone());
                }
            }
            TextOp::Replace { line, condition, text } => {
                for l in lines.iter_mut().filter(|l| condition.matches(l, line)) {
                    *l = text.clone();
                }
            }
            TextOp::Delete { condition, line } => lines.retain(|l| !condition.matches(l, line)),
        }
    }
    let mut out = lines.join(eol);
    if trailing || text.is_empty() {
        out.push_str(eol);
    }
    out
}

fn glob(s: &[u8], p: &[u8]) -> bool {
    match (p.first(), s.first()) {
        (None, _) => s.is_empty(),
        (Some(b'*'), _) => glob(s, &p[1..]) || (!s.is_empty() && glob(&s[1..], p)),
        (Some(b'?'), Some(_)) => glob(&s[1..], &p[1..]),
        (Some(a), Some(b)) if a == b => glob(&s[1..], &p[1..]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_example() {
        let input = "Line 1\r\nLine 2\r\nLine 3\r\nLine 4 old\r\nLine 5\r\n# comment\r\n";
        let ops = [
            TextOp::Add("This line is added".into()),
            TextOp::Delete { condition: Condition::Equal, line: "Line 3".into() },
            TextOp::Replace { line: "Line 4".into(), condition: Condition::StartWith, text: "THIS IS NEW LINE".into() },
            TextOp::Insert { before: false, line: "Line 5".into(), condition: Condition::Equal, text: "After 5".into() },
            TextOp::Insert { before: true, line: "Line 1".into(), condition: Condition::Equal, text: "First".into() },
            TextOp::Delete { condition: Condition::Mask, line: "#*".into() },
        ];
        assert_eq!(
            apply(input, &ops),
            "First\r\nLine 1\r\nLine 2\r\nTHIS IS NEW LINE\r\nLine 5\r\nAfter 5\r\nThis line is added\r\n"
        );
    }

    #[test]
    fn mask() {
        assert!(glob(b"a LINE b", b"*LINE*"));
        assert!(glob(b"abc", b"a?c"));
        assert!(!glob(b"abd", b"a?c"));
    }
}
