//! A small Markdown reader for `core://Text` `markdown` (feature
//! `text_markdown`, decision `text-markdown`): text that arrives as
//! Markdown, such as an agent's report, read into [`Block`]s that the
//! layout draws with its own boxes and texts, and inline [`Span`]s that the
//! text engine shapes in one buffer.
//!
//! It reads what agents and people write, not all of CommonMark: ATX and
//! setext headings, paragraphs, bulleted, numbered and task lists (nested
//! by indentation), block quotes, fenced code blocks, GitHub tables and
//! thematic breaks; inside them `**bold**`, `*italic*`, `` `code` ``,
//! `~~strike~~`, `[links](url)`, `<autolinks>`, bare `https://` addresses
//! and backslash escapes. HTML is shown as written. A single line break
//! stays a line break (as in a GitHub comment). Nothing it reads can fail:
//! anything else is text.

/// How a run of inline text is styled. Marks combine (bold and italic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mark(u8);

impl Mark {
    pub const BOLD: Mark = Mark(1);
    pub const ITALIC: Mark = Mark(2);
    pub const CODE: Mark = Mark(4);
    pub const STRIKE: Mark = Mark(8);
    pub const LINK: Mark = Mark(16);

    pub fn has(self, m: Mark) -> bool {
        self.0 & m.0 == m.0 && m.0 != 0
    }

    pub fn with(self, m: Mark) -> Mark {
        Mark(self.0 | m.0)
    }

    pub fn is_plain(self) -> bool {
        self.0 == 0
    }
}

/// A styled byte range of an [`Inline`]'s text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub mark: Mark,
}

/// Text with its styled ranges (in order, not overlapping, never plain).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Inline {
    pub text: String,
    pub spans: Vec<Span>,
}

impl Inline {
    fn push(&mut self, s: &str, mark: Mark) {
        if s.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(s);
        if mark.is_plain() {
            return;
        }
        match self.spans.last_mut() {
            Some(last) if last.end == start && last.mark == mark => last.end = self.text.len(),
            _ => self.spans.push(Span {
                start,
                end: self.text.len(),
                mark,
            }),
        }
    }

    fn append(&mut self, other: &Inline, extra: Mark) {
        let mut at = 0;
        for s in &other.spans {
            self.push(&other.text[at..s.start], extra);
            self.push(&other.text[s.start..s.end], s.mark.with(extra));
            at = s.end;
        }
        self.push(&other.text[at..], extra);
    }

    /// Its text with surrounding white space trimmed, spans kept in place.
    fn trimmed(self) -> Inline {
        let start = self.text.len() - self.text.trim_start().len();
        let end = self.text.trim_end().len().max(start);
        let spans = self
            .spans
            .iter()
            .filter_map(|s| {
                let (a, b) = (s.start.max(start), s.end.min(end));
                (a < b).then_some(Span {
                    start: a - start,
                    end: b - start,
                    mark: s.mark,
                })
            })
            .collect();
        Inline {
            text: self.text[start..end].to_owned(),
            spans,
        }
    }
}

/// One item of a list: its task box (`- [x]`), if any, and what it holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

/// A block of a Markdown text.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        text: Inline,
    },
    Paragraph(Inline),
    List {
        /// The first number of a numbered list; `None` for bullets.
        start: Option<u64>,
        items: Vec<Item>,
    },
    Quote(Vec<Block>),
    Code(String),
    Table {
        head: Vec<Inline>,
        rows: Vec<Vec<Inline>>,
    },
    Rule,
}

/// The most blocks nested in one another (lists in quotes in lists…);
/// deeper ones are read as text.
const MAX_DEPTH: usize = 8;

/// `text` read as Markdown.
pub fn parse(text: &str) -> Vec<Block> {
    let normal = text.replace("\r\n", "\n").replace('\t', "    ");
    let lines: Vec<&str> = normal.lines().collect();
    blocks(&lines, 0)
}

/// `blocks` run together as one inline text: a short preview (a
/// `core://Text` with `max_lines`). Headings are bold; blocks are separated
/// by a space, list items start with their bullet or number.
pub fn flatten(blocks: &[Block]) -> Inline {
    let mut out = Inline::default();
    flatten_into(blocks, &mut out);
    out.trimmed()
}

fn flatten_into(blocks: &[Block], out: &mut Inline) {
    for b in blocks {
        if !out.text.is_empty() && !out.text.ends_with(' ') {
            out.push(" ", Mark::default());
        }
        match b {
            Block::Heading { text, .. } => out.append(text, Mark::BOLD),
            Block::Paragraph(t) => out.append(&one_line(t), Mark::default()),
            Block::List { start, items } => {
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(" ", Mark::default());
                    }
                    out.push(&marker(*start, i, item.task), Mark::default());
                    out.push(" ", Mark::default());
                    flatten_into(&item.blocks, out);
                }
            }
            Block::Quote(inner) => flatten_into(inner, out),
            Block::Code(code) => out.push(&code.replace('\n', " "), Mark::CODE),
            Block::Table { head, rows } => {
                for row in std::iter::once(head).chain(rows) {
                    let cells: Vec<&str> = row.iter().map(|c| c.text.as_str()).collect();
                    out.push(&cells.join(" · "), Mark::default());
                    out.push(" ", Mark::default());
                }
            }
            Block::Rule => {}
        }
    }
}

/// `t` with its line breaks as spaces (same length, so spans stay put).
fn one_line(t: &Inline) -> Inline {
    Inline {
        text: t.text.replace('\n', " "),
        spans: t.spans.clone(),
    }
}

/// What a list item starts with: `•`, `3.`, or a task's box.
pub fn marker(start: Option<u64>, index: usize, task: Option<bool>) -> String {
    match (task, start) {
        (Some(true), _) => "☑".into(),
        (Some(false), _) => "☐".into(),
        (None, Some(n)) => format!("{}.", n.saturating_add(index as u64)),
        (None, None) => "•".into(),
    }
}

/// How far `line` is indented (spaces), and the rest of it.
fn indent(line: &str) -> (usize, &str) {
    let rest = line.trim_start_matches(' ');
    (line.len() - rest.len(), rest)
}

fn blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// A fence's character and length, when `line` opens (or closes) one.
fn fence(line: &str) -> Option<(char, usize)> {
    let (n, rest) = indent(line);
    if n > 3 {
        return None;
    }
    let c = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = rest.chars().take_while(|x| *x == c).count();
    // A backtick fence's info string has no backticks.
    (len >= 3 && !(c == '`' && rest[len..].contains('`'))).then_some((c, len))
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let (n, rest) = indent(line);
    if n > 3 {
        return None;
    }
    let level = rest.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let after = &rest[level..];
    if !(after.is_empty() || after.starts_with(' ')) {
        return None;
    }
    // A closing run of `#`s goes, when a space stands before it.
    let body = after.trim();
    let stripped = body.trim_end_matches('#');
    let body = if stripped.is_empty() {
        ""
    } else if stripped.ends_with(' ') {
        stripped.trim_end()
    } else {
        body
    };
    Some((level as u8, body))
}

fn rule(line: &str) -> bool {
    let (n, rest) = indent(line);
    if n > 3 {
        return false;
    }
    let Some(c) = rest.chars().next().filter(|c| matches!(c, '-' | '*' | '_')) else {
        return false;
    };
    let mut count = 0;
    for x in rest.chars() {
        if x == c {
            count += 1;
        } else if x != ' ' {
            return false;
        }
    }
    count >= 3
}

/// A list item's marker: its indentation, `None` for a bullet or its
/// number, and where its content starts (a byte offset in `line`).
fn list_marker(line: &str) -> Option<(usize, Option<u64>, usize)> {
    let (n, rest) = indent(line);
    let (number, width) = match rest.chars().next()? {
        '-' | '*' | '+' => (None, 1),
        c if c.is_ascii_digit() => {
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            if digits > 9 || !matches!(rest[digits..].chars().next(), Some('.' | ')')) {
                return None;
            }
            (rest[..digits].parse().ok(), digits + 1)
        }
        _ => return None,
    };
    let after = &rest[width..];
    if after.is_empty() {
        return Some((n, number, line.len()));
    }
    if !after.starts_with(' ') {
        return None;
    }
    let spaces = after.len() - after.trim_start_matches(' ').len();
    // Five spaces or more: the content starts after one (it is indented code
    // in CommonMark, read here as text).
    let gap = if spaces > 4 { 1 } else { spaces };
    Some((n, number, n + width + gap))
}

/// Whether `line` is a table's delimiter row (`| --- | :-: |`).
fn delimiter_row(line: &str) -> bool {
    let cells = cells(line);
    !cells.is_empty()
        && line.contains('-')
        && cells.iter().all(|c| {
            let c = c.trim();
            let c = c.strip_prefix(':').unwrap_or(c);
            let c = c.strip_suffix(':').unwrap_or(c);
            !c.is_empty() && c.chars().all(|x| x == '-')
        })
}

/// A table row's cells, split at the pipes that aren't escaped or in code.
fn cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = if t.ends_with('|') && !t.ends_with("\\|") {
        &t[..t.len() - 1]
    } else {
        t
    };
    let mut out = vec![String::new()];
    let mut code = false;
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                out.last_mut().expect("one cell").push('|');
                chars.next();
            }
            '`' => {
                code = !code;
                out.last_mut().expect("one cell").push(c);
            }
            '|' if !code => out.push(String::new()),
            _ => out.last_mut().expect("one cell").push(c),
        }
    }
    out.into_iter().map(|c| c.trim().to_owned()).collect()
}

fn table_start(lines: &[&str], i: usize) -> bool {
    lines[i].contains('|')
        && lines.get(i + 1).is_some_and(|l| delimiter_row(l))
        && cells(lines[i]).len() == cells(lines[i + 1]).len()
}

fn quote_line(line: &str) -> Option<&str> {
    let (n, rest) = indent(line);
    if n > 3 {
        return None;
    }
    let rest = rest.strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// Whether `lines[i]` starts a block other than a paragraph (so it ends one).
fn starts_block(lines: &[&str], i: usize) -> bool {
    let l = lines[i];
    heading(l).is_some()
        || fence(l).is_some()
        || quote_line(l).is_some()
        || rule(l)
        || list_marker(l).is_some_and(|(_, _, c)| !blank(&l[c..]))
        || table_start(lines, i)
}

fn blocks(lines: &[&str], depth: usize) -> Vec<Block> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if blank(line) {
            i += 1;
            continue;
        }
        if depth >= MAX_DEPTH {
            let text: Vec<&str> = lines[i..].iter().map(|l| l.trim()).collect();
            out.push(Block::Paragraph(inline(text.join("\n").trim())));
            break;
        }
        if let Some((c, len)) = fence(line) {
            let (pad, _) = indent(line);
            let mut code = Vec::new();
            i += 1;
            while i < lines.len() {
                if let Some((c2, len2)) = fence(lines[i])
                    && c2 == c
                    && len2 >= len
                    && lines[i].trim().chars().all(|x| x == c)
                {
                    i += 1;
                    break;
                }
                let l = lines[i];
                let (n, _) = indent(l);
                code.push(&l[n.min(pad)..]);
                i += 1;
            }
            out.push(Block::Code(code.join("\n")));
            continue;
        }
        if let Some((level, body)) = heading(line) {
            out.push(Block::Heading {
                level,
                text: inline(body),
            });
            i += 1;
            continue;
        }
        if rule(line) {
            out.push(Block::Rule);
            i += 1;
            continue;
        }
        if quote_line(line).is_some() {
            let mut inner = Vec::new();
            while i < lines.len() {
                match quote_line(lines[i]) {
                    Some(rest) => inner.push(rest),
                    // A lazy line carries on the quote's paragraph.
                    None if !blank(lines[i])
                        && !starts_block(lines, i)
                        && inner.last().is_some_and(|l| !blank(l)) =>
                    {
                        inner.push(lines[i].trim_start())
                    }
                    None => break,
                }
                i += 1;
            }
            out.push(Block::Quote(blocks(&inner, depth + 1)));
            continue;
        }
        if let Some((_, number, _)) = list_marker(line)
            && !blank(&line[list_marker(line).map_or(0, |m| m.2)..])
        {
            let (list, next) = list(lines, i, number, depth);
            out.push(list);
            i = next;
            continue;
        }
        if table_start(lines, i) {
            let head = cells(lines[i])
                .iter()
                .map(|c| inline(c))
                .collect::<Vec<_>>();
            let width = head.len();
            i += 2;
            let mut rows = Vec::new();
            while i < lines.len() && !blank(lines[i]) && lines[i].contains('|') {
                let mut row: Vec<Inline> = cells(lines[i]).iter().map(|c| inline(c)).collect();
                row.resize(width, Inline::default());
                rows.push(row);
                i += 1;
            }
            out.push(Block::Table { head, rows });
            continue;
        }
        // A paragraph: up to a blank line or another block. A line of `=`
        // or `-` under it makes it a heading (setext).
        let mut para = vec![line.trim()];
        i += 1;
        let mut level = None;
        while i < lines.len() && !blank(lines[i]) {
            let t = lines[i].trim();
            if indent(lines[i]).0 <= 3 && !t.is_empty() && t.chars().all(|c| c == '=') {
                level = Some(1);
                i += 1;
                break;
            }
            if indent(lines[i]).0 <= 3 && t.len() >= 2 && t.chars().all(|c| c == '-') {
                level = Some(2);
                i += 1;
                break;
            }
            if starts_block(lines, i) {
                break;
            }
            para.push(t);
            i += 1;
        }
        let text = inline(&para.join("\n"));
        out.push(match level {
            Some(level) => Block::Heading {
                level,
                text: one_line(&text),
            },
            None => Block::Paragraph(text),
        });
    }
    out
}

/// The list starting at `lines[i]` (a bullet when `number` is `None`), and
/// the line after it.
fn list(lines: &[&str], mut i: usize, number: Option<u64>, depth: usize) -> (Block, usize) {
    let ordered = number.is_some();
    let mut items = Vec::new();
    let base = indent(lines[i]).0;
    while i < lines.len() {
        let Some((n, num, content)) = list_marker(lines[i]) else {
            break;
        };
        if num.is_some() != ordered || n + 3 < base || n > base + 3 || blank(&lines[i][content..]) {
            break;
        }
        let width = content - n;
        let mut body = vec![&lines[i][content..]];
        i += 1;
        while i < lines.len() {
            let l = lines[i];
            let (ind, _) = indent(l);
            if blank(l) {
                // A blank line stays in the item when more of it follows.
                let more = lines[i + 1..]
                    .iter()
                    .find(|l| !blank(l))
                    .is_some_and(|l| indent(l).0 >= n + width);
                if !more {
                    break;
                }
                body.push("");
            } else if ind >= n + width {
                body.push(&l[n + width..]);
            } else if list_marker(l).is_some_and(|(m, _, c)| m > n && !blank(&l[c..])) {
                // A list less indented than this item's content but more
                // than its marker: nested all the same.
                body.push(&l[(n + 1).min(ind)..]);
            } else if list_marker(l).is_none()
                && !starts_block(lines, i)
                && body.last().is_some_and(|b| !blank(b))
            {
                // A lazy line carries on the item's paragraph.
                body.push(l.trim_start());
            } else {
                break;
            }
            i += 1;
        }
        let mut task = None;
        if let Some(first) = body.first_mut() {
            for (box_, done) in [("[ ] ", false), ("[x] ", true), ("[X] ", true)] {
                if let Some(rest) = first.strip_prefix(box_) {
                    *first = rest;
                    task = Some(done);
                }
            }
            if matches!(*first, "[ ]" | "[x]" | "[X]") {
                task = Some(*first != "[ ]");
                *first = "";
            }
        }
        items.push(Item {
            task,
            blocks: blocks(&body, depth + 1),
        });
        // Blank lines between items.
        while i < lines.len()
            && blank(lines[i])
            && lines[i + 1..]
                .iter()
                .find(|l| !blank(l))
                .and_then(|l| list_marker(l))
                .is_some_and(|(m, num, _)| {
                    num.is_some() == ordered && m + 3 >= base && m <= base + 3
                })
        {
            i += 1;
        }
    }
    (
        Block::List {
            start: number,
            items,
        },
        i,
    )
}

/// A piece of inline text before emphasis is matched.
enum Atom {
    Text(String, Mark),
    /// A run of `*`, `_` or `~`: how many are left unmatched, and whether
    /// it can open or close emphasis.
    Delim {
        ch: char,
        left: usize,
        open: bool,
        close: bool,
    },
}

/// `s` read for inline Markdown.
pub fn inline(s: &str) -> Inline {
    inline_at(s, 0)
}

fn inline_at(s: &str, depth: usize) -> Inline {
    let atoms = atoms(s, depth);
    emphasis(atoms)
}

fn atoms(s: &str, depth: usize) -> Vec<Atom> {
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let at = |k: usize| chars.get(k).map(|c| c.1);
    let byte = |k: usize| chars.get(k).map_or(s.len(), |c| c.0);
    let mut out: Vec<Atom> = Vec::new();
    let mut text = String::new();
    let flush = |text: &mut String, out: &mut Vec<Atom>| {
        if !text.is_empty() {
            out.push(Atom::Text(std::mem::take(text), Mark::default()));
        }
    };
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k].1;
        // Backslash escapes; a backslash at a line's end is a line break.
        if c == '\\' {
            match at(k + 1) {
                Some(n) if n.is_ascii_punctuation() => {
                    text.push(n);
                    k += 2;
                    continue;
                }
                Some('\n') => {
                    text.push('\n');
                    k += 2;
                    continue;
                }
                _ => {}
            }
        }
        if c == '`' {
            let run = chars[k..].iter().take_while(|x| x.1 == '`').count();
            // The closing run of exactly as many backticks.
            let mut j = k + run;
            let mut close = None;
            while j < chars.len() {
                if chars[j].1 == '`' {
                    let r = chars[j..].iter().take_while(|x| x.1 == '`').count();
                    if r == run {
                        close = Some(j);
                        break;
                    }
                    j += r;
                } else {
                    j += 1;
                }
            }
            if let Some(j) = close {
                let mut code = s[byte(k + run)..byte(j)].replace('\n', " ");
                if code.len() >= 2
                    && code.starts_with(' ')
                    && code.ends_with(' ')
                    && !code.trim().is_empty()
                {
                    code = code[1..code.len() - 1].to_owned();
                }
                flush(&mut text, &mut out);
                out.push(Atom::Text(code, Mark::CODE));
                k = j + run;
                continue;
            }
            text.push_str(&s[byte(k)..byte(k + run)]);
            k += run;
            continue;
        }
        // Links and images: `[text](url)`, `![alt](url)`.
        if (c == '[' || (c == '!' && at(k + 1) == Some('['))) && depth < 4 {
            let open = if c == '!' { k + 1 } else { k };
            if let Some((label_end, after)) = link_end(&chars, open) {
                let label = &s[byte(open + 1)..byte(label_end)];
                flush(&mut text, &mut out);
                let inner = inline_at(label, depth + 1);
                let mut sp = 0;
                for span in &inner.spans {
                    out.push(Atom::Text(
                        inner.text[sp..span.start].to_owned(),
                        Mark::LINK,
                    ));
                    out.push(Atom::Text(
                        inner.text[span.start..span.end].to_owned(),
                        span.mark.with(Mark::LINK),
                    ));
                    sp = span.end;
                }
                out.push(Atom::Text(inner.text[sp..].to_owned(), Mark::LINK));
                k = after;
                continue;
            }
        }
        // Autolinks: `<https://…>`.
        if c == '<'
            && let Some(len) = chars[k + 1..].iter().position(|x| x.1 == '>' || x.1 == ' ')
            && at(k + 1 + len) == Some('>')
        {
            let inner = &s[byte(k + 1)..byte(k + 1 + len)];
            if inner.starts_with("http://") || inner.starts_with("https://") || inner.contains('@')
            {
                flush(&mut text, &mut out);
                out.push(Atom::Text(inner.to_owned(), Mark::LINK));
                k += len + 2;
                continue;
            }
        }
        // Bare addresses, without the punctuation that ends a sentence.
        if (c == 'h' && (k == 0 || !chars[k - 1].1.is_alphanumeric()))
            && (s[byte(k)..].starts_with("https://") || s[byte(k)..].starts_with("http://"))
        {
            let mut j = k;
            while j < chars.len() && !chars[j].1.is_whitespace() && chars[j].1 != '<' {
                j += 1;
            }
            while j > k
                && matches!(
                    chars[j - 1].1,
                    '.' | ',' | ';' | ':' | '!' | '?' | ')' | '\'' | '"' | '*' | '_' | '~'
                )
            {
                j -= 1;
            }
            flush(&mut text, &mut out);
            out.push(Atom::Text(s[byte(k)..byte(j)].to_owned(), Mark::LINK));
            k = j;
            continue;
        }
        if matches!(c, '*' | '_' | '~') {
            let run = chars[k..].iter().take_while(|x| x.1 == c).count();
            let before = k.checked_sub(1).map(|b| chars[b].1);
            let after = at(k + run);
            let space = |x: Option<char>| x.is_none_or(char::is_whitespace);
            let punct = |x: Option<char>| x.is_some_and(|x| x.is_ascii_punctuation());
            // CommonMark's flanking rules.
            let left = !space(after) && (!punct(after) || space(before) || punct(before));
            let right = !space(before) && (!punct(before) || space(after) || punct(after));
            let (open, close) = if c == '_' {
                (
                    left && (!right || punct(before)),
                    right && (!left || punct(after)),
                )
            } else {
                (left, right)
            };
            if (c != '~' || run == 2) && (open || close) {
                flush(&mut text, &mut out);
                out.push(Atom::Delim {
                    ch: c,
                    left: run,
                    open,
                    close,
                });
            } else {
                text.push_str(&s[byte(k)..byte(k + run)]);
            }
            k += run;
            continue;
        }
        text.push(c);
        k += 1;
    }
    flush(&mut text, &mut out);
    out
}

/// Where a link's label ends and the index after its `(url)`, when
/// `chars[open]` is `[` and a link follows.
fn link_end(chars: &[(usize, char)], open: usize) -> Option<(usize, usize)> {
    let mut nest = 0usize;
    let mut j = open + 1;
    let label_end = loop {
        match chars.get(j)?.1 {
            '\\' => j += 1,
            '[' => nest += 1,
            ']' if nest == 0 => break j,
            ']' => nest -= 1,
            '\n' if chars.get(j + 1).is_some_and(|c| c.1 == '\n') => return None,
            _ => {}
        }
        j += 1;
    };
    if chars.get(label_end + 1)?.1 != '(' {
        return None;
    }
    let mut parens = 0usize;
    let mut j = label_end + 2;
    loop {
        match chars.get(j)?.1 {
            '(' => parens += 1,
            ')' if parens == 0 => return Some((label_end, j + 1)),
            ')' => parens -= 1,
            '\n' => return None,
            _ => {}
        }
        j += 1;
    }
}

/// Matches emphasis delimiters (CommonMark's "process emphasis", without
/// the rule of three) and styles the text between each pair.
fn emphasis(mut atoms: Vec<Atom>) -> Inline {
    // (opener, closer, mark): the atoms strictly between take the mark.
    let mut pairs: Vec<(usize, usize, Mark)> = Vec::new();
    let mut closer = 0;
    while closer < atoms.len() {
        let (ch, left, close) = match &atoms[closer] {
            Atom::Delim {
                ch, left, close, ..
            } => (*ch, *left, *close),
            Atom::Text(..) => {
                closer += 1;
                continue;
            }
        };
        if !close || left == 0 {
            closer += 1;
            continue;
        }
        let opener = (0..closer).rev().find(|&o| {
            matches!(&atoms[o], Atom::Delim { ch: c, left, open: true, .. } if *c == ch && *left > 0)
        });
        let Some(opener) = opener else {
            closer += 1;
            continue;
        };
        let Atom::Delim { left: ol, .. } = atoms[opener] else {
            unreachable!("found as a delimiter")
        };
        let (used, mark) = match ch {
            '~' => (2, Mark::STRIKE),
            _ if ol >= 2 && left >= 2 => (2, Mark::BOLD),
            _ => (1, Mark::ITALIC),
        };
        if ch == '~' && (ol < 2 || left < 2) {
            closer += 1;
            continue;
        }
        if let Atom::Delim { left, .. } = &mut atoms[opener] {
            *left -= used;
        }
        if let Atom::Delim { left, .. } = &mut atoms[closer] {
            *left -= used;
        }
        // Delimiters between them can no longer match.
        for a in &mut atoms[opener + 1..closer] {
            if let Atom::Delim { open, close, .. } = a {
                *open = false;
                *close = false;
            }
        }
        pairs.push((opener, closer, mark));
        // The closer may close another (`***a***`); else move on.
    }
    let mut out = Inline::default();
    for (i, a) in atoms.iter().enumerate() {
        let mut mark = Mark::default();
        for (o, c, m) in &pairs {
            if *o < i && i < *c {
                mark = mark.with(*m);
            }
        }
        match a {
            Atom::Text(t, base) => out.push(t, mark.with(*base)),
            Atom::Delim { ch, left, .. } => {
                out.push(&ch.to_string().repeat(*left), mark);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(i: &Inline) -> Vec<(&str, Mark)> {
        i.spans
            .iter()
            .map(|s| (&i.text[s.start..s.end], s.mark))
            .collect()
    }

    #[test]
    fn inline_styles() {
        let i = inline(
            "Fixed **the redirect** in `auth.rs`, see *notes* and ~~old~~ [the PR](https://x.example/1).",
        );
        assert_eq!(
            i.text,
            "Fixed the redirect in auth.rs, see notes and old the PR."
        );
        assert_eq!(
            styled(&i),
            [
                ("the redirect", Mark::BOLD),
                ("auth.rs", Mark::CODE),
                ("notes", Mark::ITALIC),
                ("old", Mark::STRIKE),
                ("the PR", Mark::LINK),
            ]
        );
    }

    #[test]
    fn nested_and_unmatched_emphasis() {
        let i = inline(
            "***both*** and **bold *and italic*** and 2 * 3 * 4 and snake_case_name and **open",
        );
        assert_eq!(
            i.text,
            "both and bold and italic and 2 * 3 * 4 and snake_case_name and **open"
        );
        assert_eq!(
            styled(&i),
            [
                ("both", Mark::BOLD.with(Mark::ITALIC)),
                ("bold ", Mark::BOLD),
                ("and italic", Mark::BOLD.with(Mark::ITALIC)),
            ]
        );
    }

    #[test]
    fn code_keeps_its_markers_and_escapes_work() {
        let i = inline(r"`**not bold**` and \*not italic\* and ``a ` b``");
        assert_eq!(i.text, "**not bold** and *not italic* and a ` b");
        assert_eq!(
            styled(&i),
            [("**not bold**", Mark::CODE), ("a ` b", Mark::CODE)]
        );
    }

    #[test]
    fn bare_and_auto_links() {
        let i = inline("Opened https://github.com/o/r/pull/7. Also <https://a.example>.");
        assert_eq!(
            styled(&i),
            [
                ("https://github.com/o/r/pull/7", Mark::LINK),
                ("https://a.example", Mark::LINK)
            ]
        );
    }

    #[test]
    fn blocks_of_a_report() {
        let md = "## Summary\n\nFixed the **login** redirect.\nSecond line.\n\n- one\n- two\n  - nested\n- [x] done\n- [ ] todo\n\n1. first\n2. second\n\n> quoted\n> more\n\n```rust\nfn main() {}\n```\n\n| a | b |\n|---|:-:|\n| 1 | `2` |\n\n---\nEnd";
        let b = parse(md);
        assert_eq!(b.len(), 9, "{b:#?}");
        assert!(matches!(&b[0], Block::Heading { level: 2, text } if text.text == "Summary"));
        assert!(
            matches!(&b[1], Block::Paragraph(t) if t.text == "Fixed the login redirect.\nSecond line.")
        );
        let Block::List { start: None, items } = &b[2] else {
            panic!("{:?}", b[2])
        };
        assert_eq!(items.len(), 4);
        assert!(matches!(&items[1].blocks[1], Block::List { items, .. } if items.len() == 1));
        assert_eq!(items[2].task, Some(true));
        assert_eq!(items[3].task, Some(false));
        assert!(matches!(&b[3], Block::List { start: Some(1), items } if items.len() == 2));
        assert!(matches!(&b[4], Block::Quote(q) if q.len() == 1));
        assert_eq!(b[5], Block::Code("fn main() {}".into()));
        assert!(matches!(&b[6], Block::Table { head, rows } if head.len() == 2 && rows.len() == 1));
        assert_eq!(b[7], Block::Rule);
        assert!(matches!(&b[8], Block::Paragraph(t) if t.text == "End"));
    }

    #[test]
    fn setext_headings_and_lists_under_numbers() {
        let b = parse("Title\n=====\n1. step\n   - detail\n2. next\n  - two spaces in");
        assert!(matches!(&b[0], Block::Heading { level: 1, .. }));
        let Block::List { items, .. } = &b[1] else {
            panic!("{b:?}")
        };
        assert_eq!(items.len(), 2);
        assert!(matches!(items[0].blocks.last(), Some(Block::List { .. })));
        assert!(
            matches!(items[1].blocks.last(), Some(Block::List { .. })),
            "{items:?}"
        );
    }

    #[test]
    fn plain_text_stays_text() {
        let b = parse("Waiting on CI.\nThen deploy.");
        assert_eq!(
            b,
            [Block::Paragraph(inline("Waiting on CI.\nThen deploy."))]
        );
        assert!(parse("").is_empty());
        // An unclosed fence runs to the end.
        assert_eq!(parse("```\ncode"), [Block::Code("code".into())]);
    }

    #[test]
    fn flatten_runs_blocks_together() {
        let f = flatten(&parse("# Done\n\n- **one**\n- two\n\nAll good."));
        assert_eq!(f.text, "Done • one • two All good.");
        assert_eq!(styled(&f), [("Done", Mark::BOLD), ("one", Mark::BOLD)]);
    }

    #[test]
    fn deep_nesting_is_capped() {
        let md = ">".repeat(500) + " deep";
        let b = parse(&md);
        assert_eq!(b.len(), 1);
    }
}
