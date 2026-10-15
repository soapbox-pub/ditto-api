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

/// Format a number the way JavaScript's `String(n)` does for the values that
/// appear in generated SVG: integers without a fraction, and no negative zero.
pub fn num(n: f64) -> String {
    if n == 0.0 {
        return "0".to_string();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    format!("{n}")
}

/// `Math.round`, which rounds halves up rather than away from zero.
pub fn round(n: f64) -> f64 {
    (n + 0.5).floor()
}

/// `Math.min` of two numbers, which is NaN if either is (unlike `f64::min`).
pub fn min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.min(b)
    }
}

/// `Math.max` of two numbers, which is NaN if either is (unlike `f64::max`).
pub fn max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.max(b)
    }
}

/// `Number(s)` for strings: blank is zero, anything unparseable is NaN.
pub fn to_number(s: &str) -> f64 {
    let s = s.trim();
    if s.is_empty() {
        return 0.0;
    }
    match s.trim_start_matches(['+', '-']) {
        "Infinity" => {
            return if s.starts_with('-') {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            };
        }
        t if t
            .chars()
            .any(|c| c.is_ascii_alphabetic() && c != 'e' && c != 'E') =>
        {
            return f64::NAN;
        }
        _ => {}
    }
    s.parse().unwrap_or(f64::NAN)
}

/// `Number.parseInt(s, 16)`: the longest hex prefix, NaN if there is none.
pub fn parse_hex_int(s: &str) -> f64 {
    let s = s.trim_start();
    let (negative, s) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    let end = s.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(s.len());
    match u64::from_str_radix(&s[..end], 16) {
        Ok(n) if negative => -(n as f64),
        Ok(n) => n as f64,
        Err(_) => f64::NAN,
    }
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
    fn formats_numbers_like_js() {
        assert_eq!(num(540.0), "540");
        assert_eq!(num(157.5), "157.5");
        assert_eq!(num(-0.0), "0");
        assert_eq!(num(630.0 / 7.0), "90");
        assert_eq!(num(1200.0 / 7.0), "171.42857142857142");
    }

    #[test]
    fn converts_strings_like_number() {
        assert_eq!(to_number(""), 0.0);
        assert_eq!(to_number(" 12.5 "), 12.5);
        assert_eq!(to_number("1e2"), 100.0);
        assert!(to_number("12px").is_nan());
        assert!(to_number("inf").is_nan());
        assert!(to_number("NaN").is_nan());
    }

    #[test]
    fn parses_hex_like_parse_int() {
        assert_eq!(parse_hex_int("ff"), 255.0);
        assert_eq!(parse_hex_int("1z"), 1.0);
        assert!(parse_hex_int("zz").is_nan());
        assert!(parse_hex_int("").is_nan());
    }

    #[test]
    fn min_and_max_propagate_nan() {
        assert!(min(1.0, f64::NAN).is_nan());
        assert!(max(f64::NAN, 1.0).is_nan());
        assert_eq!(min(1.0, 2.0), 1.0);
    }

    #[test]
    fn rounds_halves_up() {
        assert_eq!(round(2.5), 3.0);
        assert_eq!(round(-2.5), -2.0);
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
