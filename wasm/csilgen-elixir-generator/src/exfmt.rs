//! Mix-format-faithful rendering for the Elixir expressions the generator emits.
//!
//! The generated files must pass `mix format --check-formatted` byte-identically,
//! and the formatter's wrapping decisions depend on the nesting context a value
//! appears in — so field (de)serializer expressions are built as a small doc tree
//! and rendered with the same fit rules `mix format` applies, instead of as flat
//! strings the formatter would rewrap.

/// Mix's default line width. Fit checks measure the would-be line including any
/// trailing closers/separators that land on it, matching the formatter's lookahead.
pub(crate) const MAX_WIDTH: usize = 98;

pub(crate) fn indent(n: usize) -> String {
    " ".repeat(n)
}

/// One emitted Elixir expression, structured only as deeply as the formatter's
/// wrapping needs: leaves stay flat text, containers know their mix break style.
pub(crate) enum Doc {
    /// Text with no break opportunities the emitter relies on.
    Flat(String),
    /// `{a, b}` — mix packs tuple elements greedily, continuing at the column
    /// one past the opening brace.
    Tuple(Vec<Doc>),
    /// `[a, b]` — mix breaks lists all-or-nothing, one element per line at +2.
    List(Vec<Doc>),
    /// `fun(a, b)` or `fun(a, fn p -> body end)`. With a trailing fn, mix keeps
    /// the positional args on the header line and breaks only the fn body.
    Call {
        fun: String,
        args: Vec<Doc>,
        tail_fn: Option<(String, Box<Doc>)>,
    },
    /// `fun(head, k: v, ...)` — a call whose trailing args are keywords (`if/2`).
    /// Mix keeps the head on the opening line and breaks the keywords at +2;
    /// `head` is itself a doc so a long guard condition (a regex match, say)
    /// can keep breaking rather than spilling past the width unbroken.
    KwCall {
        fun: String,
        head: Box<Doc>,
        kwargs: Vec<(String, Doc)>,
    },
    /// `a or b` — mix's guard-condition break: `a` stays on the opening line,
    /// `or` trails it, and `b` continues at `col + 2` (breaking further there
    /// if it still doesn't fit). Only ever built from an `is_nil(...)` left
    /// operand in this generator, which is always short enough to stay flat.
    Or(Box<Doc>, Box<Doc>),
    /// `case subject do ... end` — always multi-line.
    Case {
        subject: String,
        clauses: Vec<(String, Doc)>,
    },
    /// `cond do ... end` — always multi-line.
    Cond { clauses: Vec<(String, Doc)> },
}

impl Doc {
    pub(crate) fn flat(s: impl Into<String>) -> Doc {
        Doc::Flat(s.into())
    }

    /// Whether the doc can never render on one line (do/end blocks).
    pub(crate) fn is_hard(&self) -> bool {
        match self {
            Doc::Case { .. } | Doc::Cond { .. } => true,
            Doc::Flat(_) => false,
            Doc::Tuple(items) | Doc::List(items) => items.iter().any(Doc::is_hard),
            Doc::Call { args, tail_fn, .. } => {
                args.iter().any(Doc::is_hard)
                    || tail_fn.as_ref().is_some_and(|(_, body)| body.is_hard())
            }
            Doc::KwCall { head, kwargs, .. } => {
                head.is_hard() || kwargs.iter().any(|(_, v)| v.is_hard())
            }
            Doc::Or(a, b) => a.is_hard() || b.is_hard(),
        }
    }

    /// The single-line form. Only meaningful when `!is_hard()`; hard docs render
    /// empty here, and every caller checks hardness before measuring.
    pub(crate) fn flat_text(&self) -> String {
        match self {
            Doc::Flat(s) => s.clone(),
            Doc::Tuple(items) => format!("{{{}}}", join_flat(items)),
            Doc::List(items) => format!("[{}]", join_flat(items)),
            Doc::Call { fun, args, tail_fn } => {
                let mut inner = join_flat(args);
                if let Some((params, body)) = tail_fn {
                    if !inner.is_empty() {
                        inner.push_str(", ");
                    }
                    inner.push_str(&format!("fn {params} -> {} end", body.flat_text()));
                }
                format!("{fun}({inner})")
            }
            Doc::KwCall { fun, head, kwargs } => {
                let kws: Vec<String> = kwargs
                    .iter()
                    .map(|(k, v)| format!("{k}: {}", v.flat_text()))
                    .collect();
                format!("{fun}({}, {})", head.flat_text(), kws.join(", "))
            }
            Doc::Or(a, b) => format!("{} or {}", a.flat_text(), b.flat_text()),
            Doc::Case { .. } | Doc::Cond { .. } => String::new(),
        }
    }

    pub(crate) fn flat_len(&self) -> usize {
        self.flat_text().len()
    }

    /// Whether any text anywhere in the doc mentions `needle` (used to decide
    /// if a bound variable is actually read).
    pub(crate) fn mentions(&self, needle: &str) -> bool {
        match self {
            Doc::Flat(s) => s.contains(needle),
            Doc::Tuple(items) | Doc::List(items) => items.iter().any(|i| i.mentions(needle)),
            Doc::Call { fun, args, tail_fn } => {
                fun.contains(needle)
                    || args.iter().any(|a| a.mentions(needle))
                    || tail_fn
                        .as_ref()
                        .is_some_and(|(p, b)| p.contains(needle) || b.mentions(needle))
            }
            Doc::KwCall { fun, head, kwargs } => {
                fun.contains(needle)
                    || head.mentions(needle)
                    || kwargs
                        .iter()
                        .any(|(k, v)| k.contains(needle) || v.mentions(needle))
            }
            Doc::Or(a, b) => a.mentions(needle) || b.mentions(needle),
            Doc::Case { subject, clauses } => {
                subject.contains(needle)
                    || clauses
                        .iter()
                        .any(|(p, b)| p.contains(needle) || b.mentions(needle))
            }
            Doc::Cond { clauses } => clauses
                .iter()
                .any(|(p, b)| p.contains(needle) || b.mentions(needle)),
        }
    }

    /// Render starting at column `col` (the caller has already emitted `col`
    /// spaces or preceding text). `reserve` is how many bytes the caller will
    /// tack onto the doc's *last* rendered line (a separating comma, a closing
    /// keyword) — it must count against the fit check or a flat line that just
    /// barely fits would let that trailing byte spill past the width mix
    /// actually allows.
    pub(crate) fn render(&self, col: usize, reserve: usize) -> String {
        if !self.is_hard() && col + self.flat_len() + reserve <= MAX_WIDTH {
            return self.flat_text();
        }
        match self {
            Doc::Flat(s) => s.clone(),
            Doc::Tuple(items) => render_tuple(items, col, reserve),
            Doc::List(items) => {
                let mut out = String::from("[\n");
                for (i, item) in items.iter().enumerate() {
                    let last = i + 1 == items.len();
                    out.push_str(&indent(col + 2));
                    out.push_str(&item.render(col + 2, usize::from(!last)));
                    if !last {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&indent(col));
                out.push(']');
                out
            }
            Doc::Call { fun, args, tail_fn } => {
                render_call(fun, args, tail_fn.as_ref(), col, reserve)
            }
            Doc::KwCall { fun, head, kwargs } => {
                // The head shares the `fun(` line when it fits there (leaving room
                // for its trailing comma); otherwise `fun(` opens alone and the
                // head gets its own line at `col + 2`, breaking further there if
                // it still doesn't fit — mix's guard-condition ladder.
                let open_len = fun.len() + 1;
                let mut out = if !head.is_hard() && col + open_len + head.flat_len() < MAX_WIDTH {
                    format!("{fun}({},\n", head.flat_text())
                } else {
                    format!("{fun}(\n{}{},\n", indent(col + 2), head.render(col + 2, 1))
                };
                for (i, (k, v)) in kwargs.iter().enumerate() {
                    let last = i + 1 == kwargs.len();
                    // The closing `)` lands on its own line, so only a middle
                    // kwarg's separating comma needs reserving here.
                    let after = usize::from(!last);
                    out.push_str(&indent(col + 2));
                    let vcol = col + 2 + k.len() + 2;
                    if !v.is_hard() && vcol + v.flat_len() + after <= MAX_WIDTH {
                        out.push_str(&format!("{k}: {}", v.flat_text()));
                    } else {
                        out.push_str(&format!("{k}:\n"));
                        out.push_str(&indent(col + 4));
                        out.push_str(&v.render(col + 4, after));
                    }
                    if !last {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&indent(col));
                out.push(')');
                out
            }
            Doc::Or(a, b) => {
                // `a` is always a short `is_nil(...)` guard in this generator, so
                // it never needs its own break here — only whether `b` still fits
                // trailing it on the same line is in question at this point (the
                // top-level flat check already ruled that out), so `a` renders
                // flat and `b` drops to `col + 2`.
                format!(
                    "{} or\n{}{}",
                    a.flat_text(),
                    indent(col + 2),
                    b.render(col + 2, reserve)
                )
            }
            Doc::Case { subject, clauses } => {
                let mut out = format!("case {subject} do\n");
                out.push_str(&render_clauses(clauses, col + 2));
                out.push('\n');
                out.push_str(&indent(col));
                out.push_str("end");
                out
            }
            Doc::Cond { clauses } => {
                let mut out = String::from("cond do\n");
                out.push_str(&render_clauses(clauses, col + 2));
                out.push('\n');
                out.push_str(&indent(col));
                out.push_str("end");
                out
            }
        }
    }
}

fn join_flat(items: &[Doc]) -> String {
    items
        .iter()
        .map(Doc::flat_text)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Greedy tuple fill: elements flow onto the current line while their own flat
/// text fits, otherwise continue at `col + 1` — and a multi-line element always
/// sends the next element to a fresh line. `reserve` is whatever the tuple's
/// caller will append right after the closing `}` (e.g. a separating comma).
fn render_tuple(items: &[Doc], col: usize, reserve: usize) -> String {
    let mut out = String::from("{");
    let mut cur = col + 1;
    let mut prev_multiline = false;
    for (i, item) in items.iter().enumerate() {
        let first = i == 0;
        let last = i + 1 == items.len();
        if !first {
            out.push(',');
            cur += 1;
        }
        let sep = usize::from(!first);
        // Whatever immediately follows this element on the same line — a
        // separating comma for a middle element, or the tuple's own closing
        // `}` (plus the caller's reserve) for the last — must be reserved,
        // or an element that just barely fits would let that trailing byte
        // spill past the width mix actually allows.
        let after = if last { 1 + reserve } else { 1 };
        let fits_here =
            !item.is_hard() && !prev_multiline && cur + sep + item.flat_len() + after <= MAX_WIDTH;
        if first && fits_here {
            out.push_str(&item.flat_text());
            cur += item.flat_len();
            prev_multiline = false;
        } else if fits_here {
            out.push(' ');
            out.push_str(&item.flat_text());
            cur += 1 + item.flat_len();
            prev_multiline = false;
        } else {
            if !first {
                out.push('\n');
                out.push_str(&indent(col + 1));
            }
            // `after` only gates the flat-vs-break choice above; once an element
            // is committed to its own multi-line rendering, mix does not keep
            // reserving the ancestor's trailing bytes against it — only the
            // element's own immediate closer matters from here down.
            let rendered = item.render(col + 1, 0);
            let last_line_len = rendered.split('\n').next_back().map_or(0, str::len);
            prev_multiline = rendered.contains('\n');
            cur = if prev_multiline {
                last_line_len
            } else {
                col + 1 + last_line_len
            };
            out.push_str(&rendered);
        }
    }
    out.push('}');
    out
}

// `reserve` isn't threaded further here: whichever branch renders, the last
// physical line is always a lone closing token (`end)` / `)`) at a column far
// short of 98, so nothing the caller appends after it can ever overflow.
fn render_call(
    fun: &str,
    args: &[Doc],
    tail_fn: Option<&(String, Box<Doc>)>,
    col: usize,
    _reserve: usize,
) -> String {
    match tail_fn {
        Some((params, body)) => {
            let mut header = format!("{fun}(");
            for a in args {
                header.push_str(&a.flat_text());
                header.push_str(", ");
            }
            header.push_str(&format!("fn {params} ->"));
            if col + header.len() <= MAX_WIDTH {
                let mut out = header;
                out.push('\n');
                out.push_str(&indent(col + 2));
                out.push_str(&body.render(col + 2, 0));
                out.push('\n');
                out.push_str(&indent(col));
                out.push_str("end)");
                out
            } else {
                let mut out = format!("{fun}(\n");
                for a in args {
                    out.push_str(&indent(col + 2));
                    out.push_str(&a.render(col + 2, 1));
                    out.push_str(",\n");
                }
                out.push_str(&indent(col + 2));
                out.push_str(&format!("fn {params} ->\n"));
                out.push_str(&indent(col + 4));
                out.push_str(&body.render(col + 4, 0));
                out.push('\n');
                out.push_str(&indent(col + 2));
                out.push_str("end\n");
                out.push_str(&indent(col));
                out.push(')');
                out
            }
        }
        None => {
            let mut out = format!("{fun}(\n");
            for (i, a) in args.iter().enumerate() {
                let last = i + 1 == args.len();
                out.push_str(&indent(col + 2));
                out.push_str(&a.render(col + 2, usize::from(!last)));
                if !last {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&indent(col));
            out.push(')');
            out
        }
    }
}

/// Case/cond clause bodies break all-or-nothing: every clause stays `pat -> body`
/// only when they all fit; otherwise every body drops to its own line and blank
/// lines separate the clauses — exactly mix's behavior.
fn render_clauses(clauses: &[(String, Doc)], ccol: usize) -> String {
    let all_fit = clauses
        .iter()
        .all(|(pat, body)| !body.is_hard() && ccol + pat.len() + 4 + body.flat_len() <= MAX_WIDTH);
    if all_fit {
        clauses
            .iter()
            .map(|(pat, body)| format!("{}{pat} -> {}", indent(ccol), body.flat_text()))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        clauses
            .iter()
            .map(|(pat, body)| {
                format!(
                    "{}{pat} ->\n{}{}",
                    indent(ccol),
                    indent(ccol + 2),
                    body.render(ccol + 2, 0)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// Render `head doc` at `indent_n` (e.g. `resp = call(...)`, `:ok <- if(...)`,
/// `atom: expr`), breaking after the operator/keyword when the flat form
/// overflows — the value then continues on its own line at +2, mix-style.
/// `head` carries its trailing space (`"resp = "`, `"amount: "`). `reserve` is
/// whatever the caller will append right after this binding on its last line
/// (a separating comma, the ` do` of a `with` clause's final step) — it counts
/// against the *first* fit check (deciding whether to break at all), same as
/// `Doc::render`. It is deliberately dropped once the value has already moved
/// to its own line: empirically, mix does not reserve a `key:`/`pattern <-`
/// binding's trailing separator against the *moved* value's own fit decision
/// (verified against real `mix format` output — a value that exactly fills
/// the width on its own line stays flat even when the next token, appended
/// right after it, pushes the physical line past 98). Only a value sitting
/// *inside* a call's parens (an arg, or a `KwCall` head) reserves for its own
/// trailing comma — that's `render_call`/`Doc::KwCall`'s concern, not this one.
pub(crate) fn render_binding(indent_n: usize, head: &str, doc: &Doc, reserve: usize) -> String {
    let col = indent_n + head.len();
    if !doc.is_hard() && col + doc.flat_len() + reserve <= MAX_WIDTH {
        return format!("{}{head}{}", indent(indent_n), doc.flat_text());
    }
    format!(
        "{}{}\n{}{}",
        indent(indent_n),
        head.trim_end(),
        indent(indent_n + 2),
        doc.render(indent_n + 2, 0)
    )
}

/// Render `def head, do: expr`, moving `do:` to a continuation line when flat
/// overflows (mix wraps one-line def clauses exactly this way). Nothing ever
/// trails a def clause on the same line, so there is no reserve to thread.
pub(crate) fn render_def_kw(indent_n: usize, head: &str, doc: &Doc) -> String {
    let flat_head = format!("{}{head}, do: ", indent(indent_n));
    if !doc.is_hard() && flat_head.len() + doc.flat_len() <= MAX_WIDTH {
        return format!("{flat_head}{}", doc.flat_text());
    }
    format!(
        "{}{head},\n{}",
        indent(indent_n),
        render_binding(indent_n + 2, "do: ", doc, 0)
    )
}

/// Render an attribute-prefixed list (`@enforce_keys [...]`, `@wire_keys [...]`):
/// flat when it fits, otherwise one element per line at +2 with no trailing
/// comma — mix's strict list break. Module-attribute assignment (`@attr expr`)
/// is never a call, so a keyword-shaped list still keeps its brackets here; see
/// `defstruct_list` for the one construct that elides them.
pub(crate) fn attr_list(indent_n: usize, prefix: &str, items: &[String]) -> String {
    let flat = format!("{}{prefix}[{}]", indent(indent_n), items.join(", "));
    if flat.len() <= MAX_WIDTH {
        return flat;
    }
    let mut out = format!("{}{prefix}[\n", indent(indent_n));
    for (i, item) in items.iter().enumerate() {
        out.push_str(&indent(indent_n + 2));
        out.push_str(item);
        if i + 1 < items.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str(&indent(indent_n));
    out.push(']');
    out
}

/// Whether `item` is a keyword-pair entry (`key: value`) rather than a bare
/// list element (`:atom`) — the shape Elixir's grammar requires for a call's
/// trailing arguments to auto-collect into a keyword list.
fn is_keyword_item(item: &str) -> bool {
    let Some((key, _)) = item.split_once(": ") else {
        return false;
    };
    !key.is_empty()
        && key.starts_with(|c: char| c.is_alphabetic() || c == '_')
        && key.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Render `defstruct [...]`. Elixir auto-collects a call's trailing `key: value`
/// arguments into a list, so `defstruct(offset: 0, limit: 20)` and
/// `defstruct([offset: 0, limit: 20])` parse identically — mix always prefers the
/// bracket-free form when every field carries a default. A struct with any bare
/// (no-default) field can't take that shorthand (a bare atom isn't a keyword
/// pair, so the arguments wouldn't collect into one list), and renders as an
/// ordinary bracketed list via `attr_list` instead. Continuation lines align
/// under the first field, one column past `defstruct `.
pub(crate) fn defstruct_list(indent_n: usize, items: &[String]) -> String {
    let prefix = "defstruct ";
    if items.is_empty() || !items.iter().all(|item| is_keyword_item(item)) {
        return attr_list(indent_n, prefix, items);
    }
    let flat = format!("{}{prefix}{}", indent(indent_n), items.join(", "));
    if flat.len() <= MAX_WIDTH {
        return flat;
    }
    let cont = indent_n + prefix.len();
    let mut out = format!("{}{prefix}", indent(indent_n));
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(&indent(cont));
        }
        out.push_str(item);
        if i + 1 < items.len() {
            out.push_str(",\n");
        }
    }
    out
}

/// Render a `@spec`/`@callback` line: `attr name(args) :: ret1 | ret2`.
///
/// Mix's break ladder, reproduced exactly: the whole line flat when ≤ 98; else
/// break after `::` — the head line may run to 98 *excluding* the trailing
/// ` ::` (mix's lookahead stops at the already-broken `::`); else the head's
/// own args break one per line. The return union then fits on one line or
/// breaks at `|` with the operator leading each continuation.
pub(crate) fn attr_spec(
    indent_n: usize,
    attr: &str,
    fun: &str,
    args: &[String],
    ret: &[String],
) -> String {
    let head = format!("{fun}({})", args.join(", "));
    let ret_flat = ret.join(" | ");
    let one_line = format!("{}{attr} {head} :: {ret_flat}", indent(indent_n));
    if one_line.len() <= MAX_WIDTH {
        return one_line;
    }
    let exprcol = indent_n + attr.len() + 1;
    let cont = exprcol + 2;
    let head_line = format!("{}{attr} {head} ::", indent(indent_n));
    let mut out = if head_line.len().saturating_sub(3) <= MAX_WIDTH {
        format!("{head_line}\n")
    } else {
        let mut broken = format!("{}{attr} {fun}(\n", indent(indent_n));
        for (i, arg) in args.iter().enumerate() {
            broken.push_str(&indent(cont));
            broken.push_str(arg);
            if i + 1 < args.len() {
                broken.push(',');
            }
            broken.push('\n');
        }
        broken.push_str(&indent(exprcol));
        broken.push_str(") ::\n");
        broken
    };
    if cont + ret_flat.len() <= MAX_WIDTH {
        out.push_str(&indent(cont));
        out.push_str(&ret_flat);
    } else {
        out.push_str(&indent(cont));
        out.push_str(&render_ret_piece(&ret[0], cont));
        for piece in &ret[1..] {
            out.push('\n');
            out.push_str(&indent(cont));
            out.push_str("| ");
            // The `| ` prefix moves this piece's own content two columns in.
            out.push_str(&render_ret_piece(piece, cont + 2));
        }
    }
    out
}

/// Render one `::` return alternative at the column its first character lands
/// on. A bare type is left as-is (mix cannot break a single token, and does
/// not try). A `{:tag, A | B | ...}` success tuple gets mix's tuple-of-a-union
/// treatment when it doesn't fit flat: the tag opens the tuple, and the union
/// continues one column past `{`, itself staying flat or breaking at `|`
/// depending on whether it fits there.
fn render_ret_piece(piece: &str, col: usize) -> String {
    if col + piece.len() <= MAX_WIDTH {
        return piece.to_string();
    }
    let Some(body) = piece.strip_prefix('{').and_then(|s| s.strip_suffix('}')) else {
        return piece.to_string();
    };
    let Some((tag, inner)) = body.split_once(", ") else {
        return piece.to_string();
    };
    if !inner.contains(" | ") {
        return piece.to_string();
    }
    let member_col = col + 1;
    let members: Vec<&str> = inner.split(" | ").collect();
    let members_flat = members.join(" | ");
    let mut out = format!("{{{tag},\n{}", indent(member_col));
    if member_col + members_flat.len() <= MAX_WIDTH {
        out.push_str(&members_flat);
    } else {
        out.push_str(members[0]);
        for m in &members[1..] {
            out.push('\n');
            out.push_str(&indent(member_col));
            out.push_str("| ");
            out.push_str(m);
        }
    }
    out.push('}');
    out
}

/// Mix collapses runs of blank lines to one and removes blank lines directly
/// before a block-closing `end`, so assembled files are normalized once here
/// rather than teaching every emitter about its neighbors. Also pins the file
/// to exactly one trailing newline.
pub(crate) fn normalize_blank_lines(content: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in content.split('\n') {
        if line.is_empty() {
            if out.last().is_none_or(|l| l.is_empty()) {
                continue;
            }
            out.push(line);
            continue;
        }
        if line.trim() == "end" {
            while out.last().is_some_and(|l| l.is_empty()) {
                out.pop();
            }
        }
        out.push(line);
    }
    let mut s = out.join("\n");
    while s.ends_with('\n') {
        s.pop();
    }
    s.push('\n');
    s
}
