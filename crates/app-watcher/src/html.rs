//! Just enough HTML: a page's readable text (for the LLM extractor, event
//! descriptions and keyword rules) and its JSON-LD blocks. No DOM: pages
//! are read as text, which is all the extractors need.

/// The `<script type="application/ld+json">` blocks of `html`.
pub fn ld_json_blocks(html: &str) -> Vec<&str> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = lower[at..].find("<script") {
        let start = at + i;
        let Some(gt) = lower[start..].find('>') else {
            break;
        };
        let open_end = start + gt + 1;
        let Some(close) = lower[open_end..].find("</script") else {
            break;
        };
        let attrs = &lower[start..open_end];
        if attrs.contains("application/ld+json") {
            out.push(&html[open_end..open_end + close]);
        }
        at = open_end + close;
    }
    out
}

/// Remove `<tag ...>...</tag>` elements (case-insensitive) whose content is
/// not text: scripts, styles, templates.
fn drop_elements(html: &str, tags: &[&str]) -> String {
    let mut s = html.to_owned();
    for tag in tags {
        let open = format!("<{tag}");
        let close = format!("</{tag}");
        loop {
            let lower = s.to_ascii_lowercase();
            let Some(start) = lower.find(&open) else {
                break;
            };
            // `<s` must not match `<script>` when dropping `s`: the next
            // byte ends the name.
            let next = lower.as_bytes().get(start + open.len()).copied();
            if !matches!(next, Some(b'>' | b' ' | b'\t' | b'\n' | b'\r' | b'/')) {
                // Not this element: blank the `<` so the search moves on.
                s.replace_range(start..start + 1, " ");
                continue;
            }
            let end = match lower[start..].find(&close) {
                Some(c) => {
                    let c = start + c;
                    lower[c..].find('>').map_or(lower.len(), |g| c + g + 1)
                }
                None => lower.len(),
            };
            s.replace_range(start..end, " ");
        }
    }
    s
}

/// The text of an HTML fragment or page: elements that hold no text
/// dropped, block ends as line breaks, entities decoded, blank runs
/// collapsed.
pub fn to_text(html: &str) -> String {
    let s = drop_elements(
        html,
        &["script", "style", "noscript", "template", "svg", "head"],
    );
    let mut out = String::with_capacity(s.len() / 2);
    let mut rest = s.as_str();
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let Some(j) = rest[i..].find('>') else {
            rest = "";
            break;
        };
        let tag = rest[i + 1..i + j]
            .trim_start_matches('/')
            .to_ascii_lowercase();
        let name: String = tag
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if matches!(
            name.as_str(),
            "br" | "p"
                | "div"
                | "li"
                | "tr"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "section"
                | "article"
                | "header"
                | "footer"
                | "ul"
                | "ol"
                | "table"
                | "time"
        ) {
            out.push('\n');
        } else if !matches!(
            name.as_str(),
            "a" | "b"
                | "i"
                | "em"
                | "strong"
                | "span"
                | "small"
                | "u"
                | "abbr"
                | "mark"
                | "sup"
                | "sub"
        ) {
            out.push(' ');
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    let decoded = decode_entities(&out);
    let mut lines: Vec<String> = Vec::new();
    for line in decoded.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !line.is_empty() {
            lines.push(line);
        }
    }
    lines.join("\n")
}

/// Decode the common named entities and every numeric one.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let end = after.find(';').filter(|e| *e <= 10);
        let decoded = end.and_then(|e| {
            let name = &after[..e];
            let c = if let Some(num) = name.strip_prefix('#') {
                let n = match num.strip_prefix(['x', 'X']) {
                    Some(h) => u32::from_str_radix(h, 16).ok()?,
                    None => num.parse().ok()?,
                };
                char::from_u32(n)?
            } else {
                match name {
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    "apos" => '\'',
                    "nbsp" => ' ',
                    "rsquo" | "lsquo" => '\'',
                    "rdquo" | "ldquo" => '"',
                    "ndash" => '–',
                    "mdash" => '—',
                    "hellip" => '…',
                    "middot" => '·',
                    _ => return None,
                }
            };
            Some((c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &after[e + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// At most `max` characters of `s` (on a character boundary).
pub fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_of_a_page() {
        let html = "<html><head><title>x</title><style>p{}</style></head><body>\
            <h1>Shows</h1><p>Budget&nbsp;Cuts &amp; friends</p><script>var a='<p>no</p>';</script>\
            <ul><li>Fri<br>7:30</li></ul><s>gone?</s><b>Bold</b>, <a href=x>link</a></body></html>";
        assert_eq!(
            to_text(html),
            "Shows\nBudget Cuts & friends\nFri\n7:30\ngone? Bold, link"
        );
    }

    #[test]
    fn ld_json() {
        let html = r#"<script>x</script><SCRIPT type="application/ld+json">{"a":1}</SCRIPT>
            <script type='application/ld+json'>[2]</script>"#;
        assert_eq!(ld_json_blocks(html), vec![r#"{"a":1}"#, "[2]"]);
    }

    #[test]
    fn entities() {
        assert_eq!(
            decode_entities("a &#39;b&#x27; &bogus; &"),
            "a 'b' &bogus; &"
        );
        assert_eq!(truncate("héllo", 2), "hé…");
    }
}
