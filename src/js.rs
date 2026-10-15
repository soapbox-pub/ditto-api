//! Small reimplementations of JavaScript built-ins whose exact behavior the
//! API's output depends on. ditto-server was written in TypeScript, and
//! clients and caches were built against what these produce.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

/// Characters `encodeURIComponent` leaves alone besides ASCII alphanumerics.
const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

/// `encodeURIComponent`.
pub fn encode_uri_component(s: &str) -> String {
    utf8_percent_encode(s, URI_COMPONENT).to_string()
}

/// `Number.parseInt(s, 10) || undefined`: leading whitespace, an optional
/// sign and as many digits as follow. Zero and garbage both yield `None`.
pub fn parse_int(s: &str) -> Option<i64> {
    let s = s.trim_start();
    let (negative, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    let n: i64 = digits[..end].parse().ok()?;
    let n = if negative { -n } else { n };
    (n != 0).then_some(n)
}

/// `escapeUTF8` from the `entities` package: escapes the five XML specials.
pub fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// `a || b || c` over optional strings: the first non-empty value, or else
/// whatever the last operand is (which may be an empty string).
pub fn or_chain(values: &[Option<&String>]) -> Option<String> {
    values
        .iter()
        .flatten()
        .find(|v| !v.is_empty())
        .or(values.last().and_then(|v| v.as_ref()))
        .map(|v| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_encode_uri_component() {
        assert_eq!(
            encode_uri_component("https://a.example/x y?q=1&r=ü!*'()~"),
            "https%3A%2F%2Fa.example%2Fx%20y%3Fq%3D1%26r%3D%C3%BC!*'()~"
        );
    }

    #[test]
    fn parses_ints_like_parse_int() {
        assert_eq!(parse_int("1200"), Some(1200));
        assert_eq!(parse_int("  630px"), Some(630));
        assert_eq!(parse_int("-5"), Some(-5));
        assert_eq!(parse_int("1.5e3"), Some(1));
        assert_eq!(parse_int("0"), None);
        assert_eq!(parse_int("abc"), None);
        assert_eq!(parse_int(""), None);
    }

    #[test]
    fn chains_like_logical_or() {
        let empty = String::new();
        let a = "a".to_string();
        assert_eq!(or_chain(&[None, Some(&a)]), Some("a".into()));
        assert_eq!(or_chain(&[Some(&empty), Some(&a)]), Some("a".into()));
        assert_eq!(or_chain(&[Some(&a), Some(&empty)]), Some("a".into()));
        assert_eq!(or_chain(&[None, Some(&empty)]), Some("".into()));
        assert_eq!(or_chain(&[Some(&empty), None]), None);
    }
}
