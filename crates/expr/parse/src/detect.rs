/// Returns true if the string looks like an expression (starts with an identifier
/// immediately followed by `(`). Used to distinguish expressions from plain string values.
///
/// Examples:
/// - `"env(\"KEY\")"` → true
/// - `"concat(a, b)"` → true
/// - `"hello world"` → false
/// - `"(not a func)"` → false
pub fn is_expression(s: &str) -> bool {
    let trimmed = s.trim();
    for stmt in trimmed.split([';', '\n']) {
        let stmt = stmt.trim();
        if stmt.is_empty() {
            continue;
        }
        if statement_looks_like_expression(stmt) {
            return true;
        }
    }
    false
}

fn statement_looks_like_expression(stmt: &str) -> bool {
    let Some(stop) = stmt.find(['(', '=']) else {
        return false;
    };
    if stop == 0 {
        return false;
    }
    let prefix = stmt[..stop].trim_end();
    if prefix.is_empty() {
        return false;
    }
    let first_byte = prefix.as_bytes()[0];
    first_byte.is_ascii_alphabetic()
        && prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Returns true if the string contains interpolation markers `{...}` that are
/// not escaped as `{{`. Used to detect strings needing interpolation evaluation.
pub fn has_interpolation(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                i += 2;
            } else {
                return true;
            }
        } else {
            i += 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_function_calls() {
        assert!(is_expression("env(\"KEY\")"));
        assert!(is_expression("concat(a, b)"));
        assert!(is_expression("if(true, 1, 2)"));
        assert!(is_expression("sha256('hello')"));
        assert!(is_expression("  env(\"X\")  "));
        assert!(is_expression("multiIf(c1, v1, c2, v2, default)"));
        assert!(is_expression("objectId(\"507f1f77bcf86cd799439011\")"));
    }

    #[test]
    fn rejects_non_expressions() {
        assert!(!is_expression("hello world"));
        assert!(!is_expression("just a string"));
        assert!(!is_expression("(parenthesized)"));
        assert!(!is_expression("123"));
        assert!(!is_expression(""));
        assert!(!is_expression("has spaces(x)"));
        assert!(!is_expression("123func(x)"));
        assert!(!is_expression("true"));
        assert!(!is_expression("null"));
    }

    #[test]
    fn detects_statements() {
        assert!(is_expression("x = 5; x + 1"));
        assert!(is_expression("x = add(1, 2); x"));
        assert!(is_expression("  x = 5; x"));
        assert!(is_expression("name = 'value'"));
    }

    #[test]
    fn detects_newline_separated_statements() {
        assert!(is_expression("x = 5\nx + 1"));
        assert!(is_expression("x = add(1, 2)\ny = mul(x, 2)\ny"));
        assert!(is_expression("\nx = 5\n"));
    }

    #[test]
    fn rejects_assignment_like_strings() {
        assert!(!is_expression("123 = 5"));
        assert!(!is_expression("= bad"));
        assert!(!is_expression("has spaces = bad"));
    }

    #[test]
    fn detects_interpolation() {
        assert!(has_interpolation("hello {name}"));
        assert!(has_interpolation("{x}"));
        assert!(has_interpolation("a {1 + 1} b"));
        assert!(has_interpolation("prefix {1 + 1} suffix"));
    }

    #[test]
    fn rejects_escaped_braces() {
        assert!(!has_interpolation("hello {{world}}"));
        assert!(!has_interpolation("no interpolation"));
        assert!(!has_interpolation(""));
        assert!(!has_interpolation("plain text"));
    }

    #[test]
    fn mixed_braces() {
        assert!(has_interpolation("{{escaped}} but {real}"));
    }
}
