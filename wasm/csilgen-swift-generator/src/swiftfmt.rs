//! Brings the emitted Swift into the shape `swift format` (Swift 6, default
//! configuration) leaves unchanged, so a consumer that runs the formatter on checked-in
//! generated files gets no diff. The WASM sandbox cannot run the formatter itself.
//!
//! The emitters write one statement per line with four-space indentation, which is
//! easy to read in this crate. This pass halves the indentation (swift-format indents
//! by two), groups long integer literals, moves the access level of an extension to
//! its members, and breaks each line longer than [`LINE_LENGTH`]. The formatter keeps
//! a line break that the source already has at a legal break point, so a broken line
//! does not have to match the formatter's own packing: one element per line inside a
//! broken bracket is a shape the formatter accepts as it is. The places where the
//! formatter does not keep a break (after `try`, after the `(` of a parenthesized
//! expression) are broken only where the formatter itself must break them.

/// The default `lineLength` of swift-format.
pub const LINE_LENGTH: usize = 100;

/// Return `text` in the shape swift-format leaves unchanged, with exactly one final
/// newline.
pub fn tidy(text: &str) -> String {
    let mut out = String::new();
    let mut blank = false;
    // The indent of the `public extension` being moved, if one is open.
    let mut extension: Option<usize> = None;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            // swift-format keeps at most one blank line in a row.
            if !blank && !out.is_empty() {
                out.push('\n');
            }
            blank = true;
            continue;
        }
        blank = false;
        let spaces = line.len() - line.trim_start_matches(' ').len();
        let indent = " ".repeat(spaces / 2);
        let mut code = line.trim_start().to_string();
        // swift-format moves the access level of an extension to its members
        // (`NoAccessLevelOnExtensionDeclaration`).
        if let Some(rest) = code.strip_prefix("public extension ") {
            code = format!("extension {rest}");
            extension = Some(spaces);
        } else if let Some(open) = extension {
            if spaces == open && code.starts_with('}') {
                extension = None;
            } else if spaces == open + 4 && is_member_declaration(&code) {
                code = format!("public {code}");
            }
        }
        if !code.starts_with("//") {
            code = group_numbers(&code);
        }
        break_line(&indent, &code, &mut out);
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

fn is_member_declaration(code: &str) -> bool {
    [
        "func ",
        "init(",
        "init?(",
        "static ",
        "var ",
        "let ",
        "subscript",
    ]
    .iter()
    .any(|keyword| code.starts_with(keyword))
}

/// Insert `_` separators the way swift-format's `GroupNumericLiterals` does: decimal
/// integers of 7 or more digits in groups of 3, hexadecimal of 8 or more in groups
/// of 4. Strings, comments, identifiers and floating-point literals are left alone.
fn group_numbers(code: &str) -> String {
    let bytes = code.as_bytes();
    let mut out = String::with_capacity(code.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' {
            let end = skip_string(bytes, i).unwrap_or(bytes.len() - 1);
            out.push_str(&code[i..=end]);
            i = end + 1;
            continue;
        }
        if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            out.push_str(&code[i..]);
            break;
        }
        let starts_word = i == 0 || !is_word(bytes[i - 1]);
        if c.is_ascii_digit() && starts_word {
            let hex = c == b'0' && matches!(bytes.get(i + 1), Some(b'x' | b'X'));
            let start = if hex { i + 2 } else { i };
            let mut end = start;
            while end < bytes.len() && is_word(bytes[end]) {
                end += 1;
            }
            let digits = &code[start..end];
            let float = !hex
                && bytes.get(end) == Some(&b'.')
                && bytes.get(end + 1).is_some_and(u8::is_ascii_digit);
            let (size, min) = if hex { (4, 8) } else { (3, 7) };
            let plain = if hex {
                digits.bytes().all(|d| d.is_ascii_hexdigit())
            } else {
                digits.bytes().all(|d| d.is_ascii_digit())
            };
            out.push_str(&code[i..start]);
            if plain && !float && digits.len() >= min {
                let head = digits.len() % size;
                let mut parts = Vec::new();
                if head > 0 {
                    parts.push(&digits[..head]);
                }
                let mut at = head;
                while at < digits.len() {
                    parts.push(&digits[at..at + size]);
                    at += size;
                }
                out.push_str(&parts.join("_"));
            } else {
                out.push_str(digits);
            }
            i = end;
            continue;
        }
        let len = utf8_len(c);
        out.push_str(&code[i..i + len]);
        i += len;
    }
    out
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

fn utf8_len(first: u8) -> usize {
    match first {
        0xf0..=0xff => 4,
        0xe0..=0xef => 3,
        0xc0..=0xdf => 2,
        _ => 1,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Paren,
    Bracket,
    Brace,
}

struct Group {
    open: usize,
    close: usize,
    kind: Kind,
}

fn fits(indent: &str, code: &str) -> bool {
    indent.len() + code.chars().count() <= LINE_LENGTH
}

fn push(out: &mut String, indent: &str, code: &str) {
    out.push_str(indent);
    out.push_str(code);
    out.push('\n');
}

/// Push `indent` + `code` as one or more lines.
fn break_line(indent: &str, code: &str, out: &mut String) {
    if fits(indent, code) || code.starts_with("//") {
        push(out, indent, code);
        return;
    }
    let inner = format!("{indent}  ");
    // A `case` label whose statement does not fit puts the statement on the next
    // line, one level in.
    if let Some(label) = case_label(code) {
        push(out, indent, &code[..label]);
        break_line(&inner, code[label..].trim(), out);
        return;
    }
    if let Some(condition) = code
        .strip_prefix("guard ")
        .and_then(|c| c.strip_suffix(" else {"))
    {
        push(out, indent, "guard");
        break_line(&inner, condition, out);
        push(out, indent, "else {");
        return;
    }
    // The formatter breaks before the body brace of an `if` first, and only breaks
    // the condition when the condition alone does not fit either.
    if code.starts_with("if ")
        && let Some(condition) = code.strip_suffix(" {")
        && fits(indent, condition)
    {
        push(out, indent, condition);
        push(out, indent, "{");
        return;
    }
    if let Some(condition) = code.strip_prefix("if ").and_then(|c| c.strip_suffix(" {"))
        && split_top_level(condition, " && ").len() > 1
    {
        let parts = split_top_level(condition, " && ");
        break_line(indent, &format!("if {}", parts[0]), out);
        for part in &parts[1..] {
            break_line(&inner, &format!("&& {part}"), out);
        }
        push(out, indent, "{");
        return;
    }
    if let Some(rest) = code.strip_prefix("&& ")
        && split_top_level(rest, " && ").len() > 1
    {
        for part in split_top_level(rest, " && ") {
            break_line(indent, &format!("&& {part}"), out);
        }
        return;
    }
    if let Some(eq) = code.find(" = if ")
        && (code.starts_with("let ") || code.starts_with("var "))
    {
        let value = if typed_head(indent, &code[..eq + 2], out) {
            format!("{inner}  ")
        } else {
            inner.clone()
        };
        break_line(&value, &code[eq + 3..], out);
        return;
    }
    if let Some(rest) = code.strip_prefix("return ")
        && !fits(indent, head(code))
    {
        push(out, indent, "return");
        continuation(&inner, rest, out);
        return;
    }
    if type_declaration(indent, code, out) || function_declaration(indent, code, out) {
        return;
    }
    // A typed declaration without a value breaks after its colon.
    let decl = strip_modifiers(code);
    if (decl.starts_with("let ") || decl.starts_with("var "))
        && find_top_level(code, " = ").is_none()
        && let Some(colon) = find_top_level(code, ": ")
    {
        push(out, indent, &code[..=colon]);
        break_line(&inner, code[colon + 1..].trim(), out);
        return;
    }
    let groups = top_level_groups(code).unwrap_or_default();
    let breakable_group = groups
        .iter()
        .any(|g| breakable(code, g) || is_block(code, g));
    if !fits(indent, head(code)) || !breakable_group {
        if let Some(eq) = find_top_level(code, " = ") {
            if !fits(indent, &code[..eq + 2])
                && let Some(colon) = label_colon(&code[..eq])
            {
                push(out, indent, &code[..=colon]);
                break_line(&inner, code[colon + 1..].trim(), out);
                return;
            }
            push(out, indent, &code[..eq + 2]);
            continuation(&inner, code[eq + 3..].trim(), out);
            return;
        }
        if let Some(rest) = code.strip_prefix("try ") {
            push(out, indent, "try");
            break_line(&inner, rest, out);
            return;
        }
        if let Some(colon) = label_colon(code) {
            push(out, indent, &code[..=colon]);
            break_line(&inner, code[colon + 1..].trim(), out);
            return;
        }
    }
    if !break_groups(indent, code, &groups, out) {
        push(out, indent, code);
    }
}

/// Push `head`, a declaration up to and including its ` =`. When it does not fit, the
/// type annotation goes one level in below the name, and the result is `true`: the
/// value then belongs one level further in than the type.
fn typed_head(indent: &str, head: &str, out: &mut String) -> bool {
    if !fits(indent, head)
        && let Some(colon) = find_top_level(head, ": ")
    {
        push(out, indent, &head[..=colon]);
        push(out, &format!("{indent}  "), head[colon + 1..].trim());
        return true;
    }
    push(out, indent, head);
    false
}

/// A line that follows a break after `return` or `=`: a `try` that cannot share the
/// line with the callee's opening bracket stays alone, at the same indent.
fn continuation(indent: &str, code: &str, out: &mut String) {
    if let Some(rest) = code.strip_prefix("try ")
        && !fits(indent, head(code))
    {
        push(out, indent, "try");
        break_line(indent, rest, out);
    } else {
        break_line(indent, code, out);
    }
}

fn is_block(code: &str, g: &Group) -> bool {
    g.kind == Kind::Brace && !code[g.open + 1..g.close].trim().is_empty()
}

/// Break the brackets of `code`: every inline block or closure, or else the last
/// argument list or collection literal. `false` when there is nothing to break.
fn break_groups(indent: &str, code: &str, groups: &[Group], out: &mut String) -> bool {
    let inner = format!("{indent}  ");
    let blocks: Vec<&Group> = groups.iter().filter(|g| is_block(code, g)).collect();
    if !blocks.is_empty() {
        // An inline block or closure becomes one statement per line, the shape the
        // formatter gives it when it does not fit.
        let mut rest = 0;
        for g in blocks {
            break_line(indent, code[rest..=g.open].trim_end(), out);
            break_line(&inner, code[g.open + 1..g.close].trim(), out);
            rest = g.close;
        }
        break_line(indent, &code[rest..], out);
        return true;
    }
    let Some(g) = groups.iter().rev().find(|g| breakable(code, g)) else {
        return false;
    };
    break_line(indent, &code[..=g.open], out);
    break_items(
        &inner,
        &code[g.open + 1..g.close],
        g.kind == Kind::Bracket,
        out,
    );
    break_line(indent, &code[g.close..], out);
    true
}

/// One element per line. A collection literal on several lines with more than one
/// element ends with a comma; the formatter adds it when it is absent and removes it
/// from a single element.
fn break_items(indent: &str, items: &str, collection: bool, out: &mut String) {
    let items = split_top_level(items, ",");
    let count = items.len();
    for (i, item) in items.iter().enumerate() {
        let comma = if i + 1 < count || (collection && count > 1) {
            ","
        } else {
            ""
        };
        break_line(indent, &format!("{item}{comma}"), out);
    }
}

/// An argument or parameter list, a tuple, or a collection literal: a bracket whose
/// break the formatter keeps. The `(` of a parenthesized expression is not one.
fn breakable(code: &str, g: &Group) -> bool {
    let inside = &code[g.open + 1..g.close];
    if inside.trim().is_empty() {
        return false;
    }
    match g.kind {
        Kind::Brace => false,
        Kind::Bracket => is_literal(code, g.open),
        Kind::Paren => !is_literal(code, g.open) || split_top_level(inside, ",").len() > 1,
    }
}

/// `code` up to and including its first bracket, the part that must fit on the first
/// line when the bracket is broken.
fn head(code: &str) -> &str {
    match top_level_groups(code).unwrap_or_default().first() {
        Some(g) => &code[..=g.open],
        None => code,
    }
}

const MODIFIERS: &[&str] = &["public ", "static ", "indirect ", "final ", "mutating "];

fn strip_modifiers(code: &str) -> &str {
    let mut rest = code;
    while let Some(m) = MODIFIERS.iter().find(|m| rest.starts_with(**m)) {
        rest = &rest[m.len()..];
    }
    rest
}

/// `public struct Name: A, B {` as the formatter breaks it: the conformances one
/// level in and `{` alone, and the name one level in as well when even
/// `public struct Name:` does not fit.
fn type_declaration(indent: &str, code: &str, out: &mut String) -> bool {
    let rest = strip_modifiers(code);
    let Some(keyword) = ["struct ", "enum ", "protocol ", "extension ", "class "]
        .into_iter()
        .find(|k| rest.starts_with(k))
    else {
        return false;
    };
    let Some(body) = code.strip_suffix(" {") else {
        return false;
    };
    let lead = &code[..code.len() - rest.len() + keyword.len() - 1];
    let declared = body[lead.len()..].trim();
    let (name, conformances) = match declared.find(": ") {
        Some(colon) => (&declared[..=colon], Some(declared[colon + 2..].trim())),
        None => (declared, None),
    };
    let inner = format!("{indent}  ");
    let first = format!("{lead} {name}");
    let name_indent = if fits(indent, &first) {
        push(out, indent, &first);
        inner.clone()
    } else {
        push(out, indent, lead);
        push(out, &inner, name);
        format!("{inner}  ")
    };
    if let Some(conformances) = conformances {
        push(out, &name_indent, conformances);
    }
    push(out, indent, "{");
    true
}

/// A `func` or `init` declaration whose line does not fit. The parameters go one
/// per line; when `public func name(` alone does not fit, the name moves one level in
/// and the body brace stands alone; when the effects and the result type do not fit
/// after `)`, the result type moves to its own line.
fn function_declaration(indent: &str, code: &str, out: &mut String) -> bool {
    let rest = strip_modifiers(code);
    let is_func = rest.starts_with("func ");
    if !is_func && !rest.starts_with("init(") && !rest.starts_with("init?(") {
        return false;
    }
    let groups = top_level_groups(code).unwrap_or_default();
    let inner = format!("{indent}  ");
    // A body on the declaration's line moves to its own lines first.
    if let Some(body) = groups.last()
        && is_block(code, body)
        && body.close == code.len() - 1
    {
        break_line(indent, &code[..=body.open], out);
        break_line(&inner, code[body.open + 1..body.close].trim(), out);
        push(out, indent, "}");
        return true;
    }
    let Some(params) = groups.into_iter().find(|g| g.kind == Kind::Paren) else {
        return false;
    };
    let name_broken = is_func && !fits(indent, &code[..=params.open]);
    let decl_indent = if name_broken {
        let func = code.len() - rest.len() + "func".len();
        push(out, indent, &code[..func]);
        push(out, &inner, code[func..=params.open].trim());
        inner.clone()
    } else {
        push(out, indent, &code[..=params.open]);
        indent.to_string()
    };
    let list = &code[params.open + 1..params.close];
    if !list.trim().is_empty() {
        break_items(&format!("{decl_indent}  "), list, false, out);
    }
    let tail = &code[params.close..];
    let (signature, brace) = match tail.strip_suffix(" {") {
        Some(signature) => (signature, true),
        None => (tail, false),
    };
    if !name_broken && fits(indent, tail) {
        push(out, indent, tail);
        return true;
    }
    if fits(&decl_indent, signature) {
        push(out, &decl_indent, signature);
    } else if let Some(arrow) = signature.find(" -> ") {
        push(out, &decl_indent, &signature[..arrow]);
        let arrow_indent = if name_broken {
            decl_indent.clone()
        } else {
            format!("{decl_indent}  ")
        };
        push(out, &arrow_indent, &signature[arrow + 1..]);
    } else {
        push(out, &decl_indent, signature);
    }
    if brace {
        push(out, indent, "{");
    }
    true
}

/// The end of the label of a `case` line that also holds a statement.
fn case_label(code: &str) -> Option<usize> {
    if !code.starts_with("case ") && !code.starts_with("default:") {
        return None;
    }
    let colon = find_top_level(code, ": ")?;
    (!code[colon + 1..].trim().is_empty()).then_some(colon + 1)
}

/// The colon after a leading argument label, parameter name or dictionary key.
fn label_colon(code: &str) -> Option<usize> {
    let colon = find_top_level(code, ": ")?;
    let label = &code[..colon];
    let simple = label
        .split(' ')
        .all(|w| !w.is_empty() && w.bytes().all(|c| is_word(c) || c == b'`'));
    let key = label.starts_with('"') && label.ends_with('"');
    (simple || key).then_some(colon)
}

/// A `[` opens a collection literal, and a `(` a tuple or a parenthesized
/// expression, when no value or name comes right before it.
fn is_literal(code: &str, open: usize) -> bool {
    !code[..open]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | ')' | ']' | '?' | '!' | '>'))
}

/// The bracket pairs at depth zero, or `None` when the line has an unbalanced closer
/// or a comment, which this pass does not break.
fn top_level_groups(code: &str) -> Option<Vec<Group>> {
    let bytes = code.as_bytes();
    let mut groups = Vec::new();
    let mut stack: Vec<(usize, Kind)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => i = skip_string(bytes, i)?,
            b'/' if bytes.get(i + 1) == Some(&b'/') => return None,
            b'(' => stack.push((i, Kind::Paren)),
            b'[' => stack.push((i, Kind::Bracket)),
            b'{' => stack.push((i, Kind::Brace)),
            b')' | b']' | b'}' => {
                let (open, kind) = stack.pop()?;
                if stack.is_empty() {
                    groups.push(Group {
                        open,
                        close: i,
                        kind,
                    });
                }
            }
            _ => {}
        }
        i += 1;
    }
    // An opener left on the stack is a block that continues on the next line; the
    // groups before it are still complete.
    Some(groups)
}

/// The byte index of the first `needle` outside brackets and strings.
fn find_top_level(code: &str, needle: &str) -> Option<usize> {
    let bytes = code.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => i = skip_string(bytes, i)?,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            _ if depth == 0 && code[i..].starts_with(needle) => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// The index of the closing quote of the string literal that opens at `start`, with
/// `\(...)` interpolations skipped as nested code.
fn skip_string(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if bytes.get(i + 1) == Some(&b'(') => {
                let mut depth = 0;
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'"' => i = skip_string(bytes, i)?,
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
            }
            b'\\' => i += 1,
            b'"' => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// `code` split at each `separator` outside brackets and strings, trimmed, with no
/// empty last part.
fn split_top_level(code: &str, separator: &str) -> Vec<String> {
    let bytes = code.as_bytes();
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                if let Some(end) = skip_string(bytes, i) {
                    i = end;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            _ if depth == 0 && code[i..].starts_with(separator) => {
                items.push(code[start..i].trim().to_string());
                i += separator.len();
                start = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    let last = code[start..].trim();
    if !last.is_empty() {
        items.push(last.to_string());
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_lines_only_lose_half_their_indent() {
        assert_eq!(
            tidy("struct A {\n    let a: Int\n}\n\n\n"),
            "struct A {\n  let a: Int\n}\n"
        );
    }

    #[test]
    fn long_integer_literals_are_grouped() {
        assert_eq!(
            tidy(
                "a > 4294967295 && b < 100000 && c == 0x10000000000 && d == 12345678.5 && e1234567\n"
            ),
            "a > 4_294_967_295 && b < 100000 && c == 0x100_0000_0000 && d == 12345678.5 && e1234567\n"
        );
        assert_eq!(
            tidy("let s = \"1234567\" // 1234567\n"),
            "let s = \"1234567\" // 1234567\n"
        );
    }

    #[test]
    fn a_long_call_gets_one_argument_per_line() {
        let long = format!(
            "        let r = try transport.call(service: \"{}\", op: \"x\", request: r)\n",
            "S".repeat(70)
        );
        let want = format!(
            "    let r = try transport.call(\n      service: \"{}\",\n      op: \"x\",\n      request: r\n    )\n",
            "S".repeat(70)
        );
        assert_eq!(tidy(&long), want);
    }

    #[test]
    fn a_long_inline_block_gets_one_statement_per_line() {
        let long = format!(
            "if let v = self.{} {{ entries.append((.text(\"a\"), v)) }}\n",
            "a".repeat(70)
        );
        let want = format!(
            "if let v = self.{} {{\n  entries.append((.text(\"a\"), v))\n}}\n",
            "a".repeat(70)
        );
        assert_eq!(tidy(&long), want);
    }

    #[test]
    fn a_long_collection_literal_ends_with_a_comma() {
        let long = format!("let a = [{}, {}]\n", "x".repeat(50), "y".repeat(50));
        let want = format!(
            "let a = [\n  {},\n  {},\n]\n",
            "x".repeat(50),
            "y".repeat(50)
        );
        assert_eq!(tidy(&long), want);
    }

    #[test]
    fn a_long_conformance_list_moves_to_its_own_line() {
        let name = "N".repeat(80);
        assert_eq!(
            tidy(&format!(
                "public struct {name}: Equatable, Sendable {{\n}}\n"
            )),
            format!("public struct {name}:\n  Equatable, Sendable\n{{\n}}\n")
        );
        let name = "N".repeat(100);
        assert_eq!(
            tidy(&format!("public enum {name}: Equatable {{\n}}\n")),
            format!("public enum\n  {name}:\n    Equatable\n{{\n}}\n")
        );
        assert_eq!(
            tidy(&format!("public extension {name} {{\n}}\n")),
            format!("extension\n  {name}\n{{\n}}\n")
        );
    }

    #[test]
    fn a_long_function_declaration_breaks_like_swift_format() {
        let ty = "T".repeat(90);
        assert_eq!(
            tidy(&format!(
                "    func f(_ r: Short) throws -> {ty} {{\n    }}\n"
            )),
            format!("  func f(\n    _ r: Short\n  ) throws\n    -> {ty}\n  {{\n  }}\n")
        );
        let name = "n".repeat(95);
        let ty = "T".repeat(95);
        assert_eq!(
            tidy(&format!(
                "public func {name}(_ b: [UInt8]) throws -> {ty} {{\n}}\n"
            )),
            format!("public func\n  {name}(\n    _ b: [UInt8]\n  ) throws\n  -> {ty}\n{{\n}}\n")
        );
        assert_eq!(
            tidy(&format!(
                "public func {name}(_ v: {ty}) throws -> [UInt8] {{\n}}\n"
            )),
            format!(
                "public func\n  {name}(\n    _ v:\n      {ty}\n  ) throws -> [UInt8]\n{{\n}}\n"
            )
        );
    }

    #[test]
    fn a_long_one_line_function_body_moves_to_its_own_line() {
        let ty = "T".repeat(40);
        assert_eq!(
            tidy(&format!(
                "    static func fromCbor(_ b: [UInt8]) throws -> {ty} {{ try {ty}(cborValue: b) }}\n"
            )),
            format!(
                "  static func fromCbor(_ b: [UInt8]) throws -> {ty} {{\n    try {ty}(cborValue: b)\n  }}\n"
            )
        );
    }

    #[test]
    fn a_long_return_or_assignment_breaks_after_the_keyword() {
        let name = "N".repeat(95);
        assert_eq!(
            tidy(&format!("    return try {name}(cborValue: v)\n")),
            format!("  return\n    try\n    {name}(\n      cborValue: v\n    )\n")
        );
        assert_eq!(
            tidy(&format!("let x = try {name}(cborValue: v)\n")),
            format!("let x =\n  try\n  {name}(\n    cborValue: v\n  )\n")
        );
        let short = "N".repeat(80);
        assert_eq!(
            tidy(&format!("let x = try {short}(cborValue: v)\n")),
            format!("let x = try {short}(\n  cborValue: v\n)\n")
        );
        assert_eq!(
            tidy(&format!("self.{name} = {name}\n")),
            format!("self.{name} =\n  {name}\n")
        );
        assert_eq!(
            tidy(&format!("    try {name}(cborValue: v)\n")),
            format!("  try\n    {name}(\n      cborValue: v\n    )\n")
        );
    }

    #[test]
    fn a_long_labeled_element_breaks_after_its_colon() {
        let name = "n".repeat(60);
        assert_eq!(
            tidy(&format!("f({name}: {name}, b: 1)\n")),
            format!("f(\n  {name}:\n    {name},\n  b: 1\n)\n")
        );
        assert_eq!(
            tidy(&format!("public let {name}: {name}\n")),
            format!("public let {name}:\n  {name}\n")
        );
    }

    #[test]
    fn long_conditions_break_before_each_and() {
        let a = "a".repeat(60);
        assert_eq!(
            tidy(&format!("if s == \"{a}\" && o == \"{a}\" {{\n}}\n")),
            format!("if s == \"{a}\"\n  && o == \"{a}\"\n{{\n}}\n")
        );
        let ty = "F".repeat(80);
        assert_eq!(
            tidy(&format!("guard let v = {ty}(rawValue: s) else {{\n}}\n")),
            format!("guard\n  let v = {ty}(\n    rawValue: s\n  )\nelse {{\n}}\n")
        );
    }

    #[test]
    fn a_public_extension_moves_its_access_level_to_the_members() {
        assert_eq!(
            tidy(
                "public extension A {\n    /// Doc.\n    func f() {}\n    static func g() {\n        let x = 1\n    }\n}\nstruct B {\n    func h() {}\n}\n"
            ),
            "extension A {\n  /// Doc.\n  public func f() {}\n  public static func g() {\n    let x = 1\n  }\n}\nstruct B {\n  func h() {}\n}\n"
        );
    }

    #[test]
    fn a_long_case_statement_moves_below_its_label() {
        let long = format!(
            "case \"E\": throw ERefusal(value: try E(cborValue: {}))\n",
            "p".repeat(60)
        );
        let want = format!(
            "case \"E\":\n  throw ERefusal(\n    value: try E(cborValue: {})\n  )\n",
            "p".repeat(60)
        );
        assert_eq!(tidy(&long), want);
    }

    #[test]
    fn a_long_if_expression_moves_below_its_assignment() {
        let long = format!(
            "let a: T? = if let v = m(c, \"{}\") {{ d(v) }} else {{ nil }}\n",
            "k".repeat(70)
        );
        let want = format!(
            "let a: T? =\n  if let v = m(c, \"{}\") {{\n    d(v)\n  }} else {{\n    nil\n  }}\n",
            "k".repeat(70)
        );
        assert_eq!(tidy(&long), want);
    }

    #[test]
    fn a_long_typed_declaration_moves_its_type_below_the_name() {
        let name = "n".repeat(38);
        let ty = "T".repeat(50);
        let key = "k".repeat(60);
        assert_eq!(
            tidy(&format!(
                "        let {name}: {ty}? = if let v = m(c, \"{key}\") {{ d(v) }} else {{ nil }}\n"
            )),
            format!(
                "    let {name}:\n      {ty}? =\n        if let v = m(c, \"{key}\") {{\n          d(v)\n        }} else {{\n          nil\n        }}\n"
            )
        );
        let fitting = "n".repeat(37);
        assert_eq!(
            tidy(&format!(
                "        let {fitting}: {ty}? = if let v = m(c, \"{key}\") {{ d(v) }} else {{ nil }}\n"
            )),
            format!(
                "    let {fitting}: {ty}? =\n      if let v = m(c, \"{key}\") {{\n        d(v)\n      }} else {{\n        nil\n      }}\n"
            )
        );
        let name = "n".repeat(42);
        assert_eq!(
            tidy(&format!(
                "    init(\n        {name}: {ty}? = nil,\n        b: Int\n    ) {{\n"
            )),
            format!("  init(\n    {name}:\n      {ty}? = nil,\n    b: Int\n  ) {{\n")
        );
        let fitting = "n".repeat(41);
        assert_eq!(
            tidy(&format!("        {fitting}: {ty}? = nil,\n")),
            format!("    {fitting}: {ty}? =\n      nil,\n")
        );
    }

    #[test]
    fn an_if_whose_condition_fits_alone_puts_its_brace_on_the_next_line() {
        let key = "x".repeat(56);
        assert_eq!(
            tidy(&format!(
                "    if let v = CsilCbor.mapGet(cborValue, \"{key}\") {{\n"
            )),
            format!("  if let v = CsilCbor.mapGet(cborValue, \"{key}\")\n  {{\n")
        );
        let key = "x".repeat(59);
        assert_eq!(
            tidy(&format!(
                "    if let v = CsilCbor.mapGet(cborValue, \"{key}\") {{\n"
            )),
            format!("  if let v = CsilCbor.mapGet(\n    cborValue,\n    \"{key}\"\n  ) {{\n")
        );
    }

    #[test]
    fn brackets_in_strings_and_interpolations_are_not_breaks() {
        let code = format!("f(\"({}\", \"\\(g(\"[\"))\")", "x".repeat(100));
        assert_eq!(
            tidy(&code),
            format!("f(\n  \"({}\",\n  \"\\(g(\"[\"))\"\n)\n", "x".repeat(100))
        );
    }
}
