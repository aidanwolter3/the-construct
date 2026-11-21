//! `docs/design.md` read as a course: parts, lessons, cards and a glossary
//! (decision `design-doc-and-lessons`).
//!
//! The Markdown is the source; this crate turns it into the JSON a pod's
//! collection fillers fetch (`docs/design-lessons.json`), so the course on a
//! pod follows the document on `main` and nobody keeps two copies. The shape
//! the document must have:
//!
//! - `## Part <n> · <title>` starts a part (a track). Its first paragraph is
//!   the part's summary.
//! - `### <n>.<m> <title>` starts a lesson of part `<n>`, numbered from 1
//!   without gaps. Its first paragraph is the lesson's summary. A quote
//!   `> **Why it matters:** …` or `> **Try it:** …` anywhere in the lesson is
//!   taken out of the text into the lesson's `why` and `try_it`.
//! - `#### <heading>` starts a card of the lesson. Text between the summary
//!   and the first card is a card without a heading. Deeper headings stay in
//!   the card's text. Every lesson has at least one card.
//! - `## Glossary` holds the glossary: list items `- **<term>**: <definition>`.
//! - Any other `##` section (a preface, how to keep the document current) is
//!   for readers of the file only.
//!
//! Relative links are made absolute (to the file on GitHub's `main`), because
//! a card is read on a device, away from the repository.

use serde_json::{Value, json};

/// Where relative links in the document point: the repository's `docs/`
/// folder on `main`.
pub const DOCS_URL: &str = "https://github.com/aidanwolter3/the-construct/blob/main/docs/";

/// The document's own address, for links to a lesson's section.
pub const DESIGN_URL: &str =
    "https://github.com/aidanwolter3/the-construct/blob/main/docs/design.md";

/// The longest card text, in bytes: a card is read on a phone.
pub const MAX_CARD_BYTES: usize = 6000;

/// The widest line of an untagged code fence (a diagram), in characters: as
/// wide as a phone shows monospace text in a card without wrapping.
pub const MAX_DIAGRAM_WIDTH: usize = 40;

/// The document read as a course.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Course {
    pub tracks: Vec<Track>,
    pub lessons: Vec<Lesson>,
    pub cards: Vec<Card>,
    pub glossary: Vec<Term>,
}

/// A part of the document.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// `part-<n>`.
    pub key: String,
    pub number: u32,
    pub title: String,
    pub summary: String,
    pub url: String,
}

/// A lesson: one `###` section.
#[derive(Debug, Clone, PartialEq)]
pub struct Lesson {
    /// The title as a slug, without its number, so progress on a pod
    /// survives renumbering.
    pub key: String,
    /// The track's key.
    pub track: String,
    pub track_title: String,
    /// `<n>.<m>`.
    pub number: String,
    pub title: String,
    pub summary: String,
    pub why: String,
    pub try_it: String,
    /// The lesson's place in the whole course, from 1.
    pub position: usize,
    pub url: String,
}

/// A card of a lesson: one `####` section.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    /// `<lesson key>/<position>`.
    pub key: String,
    /// The lesson's key.
    pub lesson: String,
    /// From 1 within the lesson.
    pub position: usize,
    pub heading: String,
    /// Markdown.
    pub body: String,
}

/// A glossary entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Term {
    pub term: String,
    pub definition: String,
    pub position: usize,
}

/// What is being read.
enum Mode {
    /// Before the first part, or in a `##` section for readers only.
    Outside,
    Track,
    Lesson,
    Glossary,
}

/// Read the document. An error names the line and what is wrong.
pub fn parse(markdown: &str) -> Result<Course, String> {
    let mut course = Course::default();
    let mut mode = Mode::Outside;
    // The section being read: its lines (outside code fences, headings
    // included) until the next heading that ends it.
    let mut buf: Vec<&str> = Vec::new();
    let mut fence: Option<&str> = None;
    let mut lesson_count = 0u32;

    for (i, line) in markdown.lines().enumerate() {
        let at = i + 1;
        if let Some(f) = fence {
            if line.trim_start().starts_with(f) {
                fence = None;
            }
            buf.push(line);
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = Some(&trimmed[..3]);
            buf.push(line);
            continue;
        }
        let level = heading_level(line);
        if level == 2 || (level == 3 && !matches!(mode, Mode::Outside | Mode::Glossary)) {
            finish(&mut course, &mode, &buf)?;
            buf.clear();
        }
        if level == 2 {
            let title = line[3..].trim();
            if let Some(rest) = title.strip_prefix("Part ") {
                let (n, name) = split_number(rest)
                    .ok_or_else(|| format!("line {at}: a part is `## Part <n> · <title>`"))?;
                let n: u32 = n
                    .parse()
                    .map_err(|_| format!("line {at}: `{n}` isn't a part number"))?;
                if n as usize != course.tracks.len() + 1 {
                    return Err(format!(
                        "line {at}: part {n} follows part {}",
                        course.tracks.len()
                    ));
                }
                course.tracks.push(Track {
                    key: format!("part-{n}"),
                    number: n,
                    title: name.to_string(),
                    summary: String::new(),
                    url: format!("{DESIGN_URL}#{}", anchor(title)),
                });
                lesson_count = 0;
                mode = Mode::Track;
            } else if title == "Glossary" {
                mode = Mode::Glossary;
            } else {
                mode = Mode::Outside;
            }
            continue;
        }
        if level == 3 && !matches!(mode, Mode::Outside | Mode::Glossary) {
            let title = line[4..].trim();
            let track = course.tracks.last().expect("a lesson's mode is in a part");
            let (number, name) = split_number(title)
                .ok_or_else(|| format!("line {at}: a lesson is `### <n>.<m> <title>`"))?;
            let expected = format!("{}.{}", track.number, lesson_count + 1);
            if number != expected {
                return Err(format!("line {at}: lesson {number} should be {expected}"));
            }
            lesson_count += 1;
            let key = slug(name);
            if course.lessons.iter().any(|l| l.key == key) {
                return Err(format!("line {at}: a second lesson titled `{name}`"));
            }
            course.lessons.push(Lesson {
                key,
                track: track.key.clone(),
                track_title: track.title.clone(),
                number,
                title: name.to_string(),
                summary: String::new(),
                why: String::new(),
                try_it: String::new(),
                position: course.lessons.len() + 1,
                url: format!("{DESIGN_URL}#{}", anchor(title)),
            });
            mode = Mode::Lesson;
            continue;
        }
        buf.push(line);
    }
    if fence.is_some() {
        return Err("a code fence is never closed".to_string());
    }
    finish(&mut course, &mode, &buf)?;
    if course.lessons.is_empty() {
        return Err(
            "no lessons: a part is `## Part <n> · <title>`, a lesson `### <n>.<m> <title>`".into(),
        );
    }
    Ok(course)
}

/// Close the section `buf` holds.
fn finish(course: &mut Course, mode: &Mode, buf: &[&str]) -> Result<(), String> {
    match mode {
        Mode::Outside => Ok(()),
        Mode::Track => {
            let (summary, _) = first_paragraph(buf);
            let track = course.tracks.last_mut().expect("in a part");
            if summary.is_empty() {
                return Err(format!("part {} has no summary paragraph", track.number));
            }
            track.summary = summary;
            Ok(())
        }
        Mode::Glossary => {
            read_glossary(course, buf);
            Ok(())
        }
        Mode::Lesson => {
            let lesson = course.lessons.last_mut().expect("in a lesson");
            let mut lines: Vec<&str> = Vec::new();
            // Take the two special quotes out, wherever they are.
            let mut i = 0;
            while i < buf.len() {
                let line = buf[i];
                let quote = [("> **Why it matters:**", 0), ("> **Try it:**", 1)]
                    .into_iter()
                    .find(|(p, _)| line.trim_start().starts_with(p));
                if let Some((prefix, which)) = quote {
                    let mut text = line.trim_start()[prefix.len()..].trim().to_string();
                    i += 1;
                    while i < buf.len() && buf[i].trim_start().starts_with('>') {
                        let more = buf[i].trim_start()[1..].trim();
                        if !more.is_empty() {
                            text.push(' ');
                            text.push_str(more);
                        }
                        i += 1;
                    }
                    let text = absolute_links(&text);
                    if which == 0 {
                        lesson.why = text;
                    } else {
                        lesson.try_it = text;
                    }
                    continue;
                }
                lines.push(line);
                i += 1;
            }
            let (summary, rest) = first_paragraph(&lines);
            if summary.is_empty() {
                return Err(format!("lesson {} has no summary paragraph", lesson.number));
            }
            lesson.summary = absolute_links(&summary);
            let lesson_key = lesson.key.clone();
            let number = lesson.number.clone();
            let mut cards: Vec<(String, Vec<&str>)> = vec![(String::new(), Vec::new())];
            let mut fence = false;
            for line in rest {
                let t = line.trim_start();
                if t.starts_with("```") || t.starts_with("~~~") {
                    fence = !fence;
                }
                if !fence && heading_level(line) == 4 {
                    cards.push((line[5..].trim().to_string(), Vec::new()));
                    continue;
                }
                cards.last_mut().expect("one card").1.push(line);
            }
            let mut position = 0;
            for (heading, body) in cards {
                let body = unwrap(&absolute_links(body.join("\n").trim()));
                if let Some(wide) = wide_diagram_line(&body) {
                    return Err(format!(
                        "lesson {number}: card `{heading}` has a diagram line {wide} characters wide, over \
                         {MAX_DIAGRAM_WIDTH}: a phone wraps it (draw it narrower, or tag the fence with a language)"
                    ));
                }
                if heading.is_empty() && body.is_empty() {
                    continue;
                }
                if body.is_empty() {
                    return Err(format!("lesson {number}: card `{heading}` has no text"));
                }
                if body.len() > MAX_CARD_BYTES {
                    return Err(format!(
                        "lesson {number}: card `{heading}` is {} bytes, over {MAX_CARD_BYTES}: split it",
                        body.len()
                    ));
                }
                position += 1;
                course.cards.push(Card {
                    key: format!("{lesson_key}/{position}"),
                    lesson: lesson_key.clone(),
                    position,
                    heading,
                    body,
                });
            }
            if position == 0 {
                return Err(format!("lesson {number} has no cards"));
            }
            Ok(())
        }
    }
}

/// `- **term**: definition` items, a definition running on over indented
/// lines.
fn read_glossary(course: &mut Course, buf: &[&str]) {
    for line in buf {
        let t = line.trim_start();
        if let Some(item) = t.strip_prefix("- **") {
            if let Some((term, rest)) = item.split_once("**") {
                let definition = rest.trim_start_matches(':').trim();
                course.glossary.push(Term {
                    term: term.trim_end_matches(':').trim().to_string(),
                    definition: definition.to_string(),
                    position: course.glossary.len() + 1,
                });
            }
        } else if !t.is_empty()
            && line.starts_with(' ')
            && let Some(last) = course.glossary.last_mut()
        {
            last.definition.push(' ');
            last.definition.push_str(t);
        }
    }
    for term in &mut course.glossary {
        term.definition = absolute_links(&term.definition);
    }
}

/// Join the lines a paragraph, list item or quote was wrapped over into one:
/// the renderer keeps a single line break, as GitHub comments do, while the
/// file wraps its prose. Code fences, tables and headings stay as written.
pub fn unwrap(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut fence = false;
    // Whether the last line written can take the next one.
    let mut open = false;
    let mut quote = false;
    for line in text.split('\n') {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
            out.push(line.to_string());
            open = false;
            continue;
        }
        if fence {
            out.push(line.to_string());
            continue;
        }
        if t.is_empty() {
            out.push(String::new());
            open = false;
            continue;
        }
        let is_quote = t.starts_with('>');
        let starts_block = is_quote
            || t.starts_with('|')
            || t.starts_with('#')
            || t.starts_with("- ")
            || t.starts_with("* ")
            || t.starts_with("+ ")
            || ordered_item(t);
        if open && is_quote && quote {
            let last = out.last_mut().expect("open means a line");
            last.push(' ');
            last.push_str(t[1..].trim());
            continue;
        }
        if open && !starts_block {
            let last = out.last_mut().expect("open means a line");
            last.push(' ');
            last.push_str(t);
            continue;
        }
        out.push(line.to_string());
        open = !(t.starts_with('|') || t.starts_with('#'));
        quote = is_quote;
    }
    out.join("\n")
}

/// `1. ` or `12) `: an ordered list item.
fn ordered_item(t: &str) -> bool {
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") "))
}

/// The width of the widest line of an untagged fence, when over
/// [`MAX_DIAGRAM_WIDTH`].
fn wide_diagram_line(text: &str) -> Option<usize> {
    let mut diagram: Option<bool> = None;
    let mut widest = 0;
    for line in text.split('\n') {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            diagram = match diagram {
                Some(_) => None,
                None => Some(matches!(t[3..].trim(), "" | "text")),
            };
            continue;
        }
        if diagram == Some(true) {
            widest = widest.max(line.trim_end().chars().count());
        }
    }
    (widest > MAX_DIAGRAM_WIDTH).then_some(widest)
}

/// The first paragraph (joined into one line) and the lines after it.
fn first_paragraph<'a>(lines: &[&'a str]) -> (String, Vec<&'a str>) {
    let start = lines.iter().position(|l| !l.trim().is_empty());
    let Some(start) = start else {
        return (String::new(), Vec::new());
    };
    let first = lines[start].trim_start();
    if heading_level(lines[start]) > 0 || first.starts_with("```") || first.starts_with('>') {
        return (String::new(), lines[start..].to_vec());
    }
    let end = lines[start..]
        .iter()
        .position(|l| l.trim().is_empty())
        .map_or(lines.len(), |n| start + n);
    let text = lines[start..end]
        .iter()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join(" ");
    (text, lines[end..].to_vec())
}

/// The level of an ATX heading (`## x` is 2), 0 for any other line.
fn heading_level(line: &str) -> usize {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
        hashes
    } else {
        0
    }
}

/// `1 · Title` or `1.2 Title` into the number and the title.
fn split_number(text: &str) -> Option<(String, &str)> {
    let end = text.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let number = text[..end].trim_end_matches('.');
    if number.is_empty() {
        return None;
    }
    let title = text[end..]
        .trim_start_matches([' ', '·', ':', '—', '-'])
        .trim();
    (!title.is_empty()).then(|| (number.to_string(), title))
}

/// A title as a key: lowercase letters and digits joined by `-`.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c == '\'' || c == '’' {
            continue;
        }
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// The anchor GitHub gives a heading: lowercase, punctuation dropped, each
/// space a `-`.
pub fn anchor(heading: &str) -> String {
    heading
        .chars()
        .flat_map(char::to_lowercase)
        .filter_map(|c| match c {
            ' ' => Some('-'),
            '-' | '_' => Some(c),
            c if c.is_alphanumeric() => Some(c),
            _ => None,
        })
        .collect()
}

/// Make the targets of Markdown links absolute: `layouts.md#x` to the file on
/// GitHub, `#x` to a section of the design document. Code spans and links
/// that are already absolute are left alone.
pub fn absolute_links(text: &str) -> String {
    // Line by line, so fenced code stays as written.
    let mut in_fence = false;
    text.split('\n')
        .map(|line| {
            let t = line.trim_start();
            if t.starts_with("```") || t.starts_with("~~~") {
                in_fence = !in_fence;
                return line.to_string();
            }
            if in_fence {
                line.to_string()
            } else {
                line_links(line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn line_links(line: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    let mut last = 0;
    for (i, c) in line.char_indices() {
        if c == '`' {
            in_code = !in_code;
            continue;
        }
        if in_code || c != ']' || i < last || !line[i + 1..].starts_with('(') {
            continue;
        }
        let start = i + 2;
        let Some(len) = line[start..].find(')') else {
            continue;
        };
        let target = &line[start..start + len];
        if let Some(abs) = absolute(target) {
            out.push_str(&line[last..start]);
            out.push_str(&abs);
            last = start + len;
        }
    }
    out.push_str(&line[last..]);
    out
}

/// A relative link target made absolute, or `None` to keep it.
fn absolute(target: &str) -> Option<String> {
    if target.contains("://") || target.starts_with("mailto:") || target.is_empty() {
        return None;
    }
    if let Some(section) = target.strip_prefix('#') {
        return Some(format!("{DESIGN_URL}#{section}"));
    }
    // Relative to docs/: `../crates/x` climbs out of it.
    let mut base: Vec<&str> = DOCS_URL.trim_end_matches('/').split('/').collect();
    let mut path = target;
    while let Some(up) = path.strip_prefix("../") {
        base.pop();
        path = up;
    }
    let path = path.trim_start_matches("./");
    Some(format!("{}/{path}", base.join("/")))
}

/// The course as the JSON the pod's fillers read, ending in a newline.
pub fn to_json(course: &Course) -> String {
    let value = json!({
        "note": "Generated from docs/design.md by crates/design-lessons; don't edit by hand. \
                 Regenerate: UPDATE_DESIGN_LESSONS=1 cargo test -p design-lessons",
        "source": DESIGN_URL,
        "tracks": course.tracks.iter().map(|t| json!({
            "key": t.key, "number": t.number, "title": t.title, "summary": t.summary,
            "position": t.number, "url": t.url,
        })).collect::<Vec<Value>>(),
        "lessons": course.lessons.iter().map(|l| json!({
            "key": l.key, "track": l.track, "track_title": l.track_title, "number": l.number,
            "title": l.title, "summary": l.summary, "why": l.why, "try_it": l.try_it,
            "position": l.position, "url": l.url,
        })).collect::<Vec<Value>>(),
        "cards": course.cards.iter().map(|c| json!({
            "key": c.key, "lesson": c.lesson, "position": c.position, "heading": c.heading,
            "body": c.body,
        })).collect::<Vec<Value>>(),
        "glossary": course.glossary.iter().map(|g| json!({
            "term": g.term, "definition": g.definition, "position": g.position,
        })).collect::<Vec<Value>>(),
    });
    let mut text = serde_json::to_string_pretty(&value).expect("JSON values serialize");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "# Title\n\nPreface, for readers of the file.\n\n\
        ## Part 1 · Goals\n\nWhy it exists.\n\n\
        ### 1.1 What it is for\n\nThe summary,\nover two lines.\n\n\
        > **Why it matters:** Because.\n\n\
        An intro card, see [layouts](layouts.md#bindings).\n\n\
        #### First card\n\nText with `code](x)`.\n\n```\n#### not a card\n```\n\n\
        #### Second card\n\nMore.\n\n> **Try it:** Run it,\n> twice.\n\n\
        ## Part 2 · Pieces\n\nThe big pieces.\n\n\
        ### 2.1 The pod\n\nThe server.\n\n#### Only card\n\nSee [crates](../crates/pod-core) and [above](#part-1--goals).\n\n\
        ## Keeping it current\n\n### Not a lesson\n\nText.\n\n\
        ## Glossary\n\n- **Pod**: the server,\n  one per user.\n- **Jack**: an ability.\n";

    #[test]
    fn reads_parts_lessons_cards_and_terms() {
        let c = parse(DOC).unwrap();
        assert_eq!(c.tracks.len(), 2);
        assert_eq!(c.tracks[0].summary, "Why it exists.");
        assert_eq!(c.tracks[0].url, format!("{DESIGN_URL}#part-1--goals"));
        assert_eq!(c.lessons.len(), 2);
        let l = &c.lessons[0];
        assert_eq!(
            (l.key.as_str(), l.number.as_str()),
            ("what-it-is-for", "1.1")
        );
        assert_eq!(l.summary, "The summary, over two lines.");
        assert_eq!(
            c.cards[1].body.lines().next(),
            Some("Text with `code](x)`.")
        );
        assert_eq!(l.why, "Because.");
        assert_eq!(l.try_it, "Run it, twice.");
        assert_eq!(l.url, format!("{DESIGN_URL}#11-what-it-is-for"));
        let cards: Vec<_> = c.cards.iter().filter(|c| c.lesson == l.key).collect();
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[0].heading, "");
        assert!(
            cards[0]
                .body
                .contains(&format!("({DOCS_URL}layouts.md#bindings)"))
        );
        assert_eq!(cards[1].heading, "First card");
        assert!(cards[1].body.contains("#### not a card"));
        assert!(cards[1].body.contains("`code](x)`"));
        assert_eq!(cards[2].body, "More.");
        assert_eq!(cards[2].key, "what-it-is-for/3");
        let only = c.cards.last().unwrap();
        assert!(
            only.body.contains(
                "(https://github.com/aidanwolter3/the-construct/blob/main/crates/pod-core)"
            )
        );
        assert!(only.body.contains(&format!("({DESIGN_URL}#part-1--goals)")));
        assert_eq!(c.lessons[1].position, 2);
        assert_eq!(c.glossary.len(), 2);
        assert_eq!(c.glossary[0].definition, "the server, one per user.");
    }

    #[test]
    fn refuses_wrong_numbers_and_empty_lessons() {
        let skipped = "## Part 1 · A\n\nS.\n\n### 1.2 B\n\nS.\n\nText.\n";
        assert!(parse(skipped).unwrap_err().contains("should be 1.1"));
        let empty = "## Part 1 · A\n\nS.\n\n### 1.1 B\n\nOnly a summary.\n";
        assert!(parse(empty).unwrap_err().contains("no cards"));
        let part = "## Part 2 · A\n\nS.\n";
        assert!(parse(part).unwrap_err().contains("follows part 0"));
    }

    #[test]
    fn unwraps_prose_but_not_code_or_tables() {
        let text =
            "One\ntwo.\n\n- a\n  b\n- c\n1. d\n   e\n\n> q\n> r\n\n| x |\n| y |\n```\nk\nl\n```";
        assert_eq!(
            unwrap(text),
            "One two.\n\n- a b\n- c\n1. d e\n\n> q r\n\n| x |\n| y |\n```\nk\nl\n```"
        );
    }

    #[test]
    fn refuses_wide_diagrams_only() {
        let wide = "x".repeat(MAX_DIAGRAM_WIDTH + 1);
        assert_eq!(
            wide_diagram_line(&format!("```\n{wide}\n```")),
            Some(MAX_DIAGRAM_WIDTH + 1)
        );
        assert_eq!(wide_diagram_line(&format!("```json\n{wide}\n```")), None);
    }

    #[test]
    fn anchors_match_github() {
        assert_eq!(
            anchor("2.3 The engine (client-core)"),
            "23-the-engine-client-core"
        );
        assert_eq!(anchor("Part 3 · Inside the pod"), "part-3--inside-the-pod");
        assert_eq!(slug("The engine (client-core)"), "the-engine-client-core");
        assert_eq!(slug("The pod's job"), "the-pods-job");
    }
}
