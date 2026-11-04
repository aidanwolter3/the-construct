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

/// Ticket sellers whose links count as a "Get tickets" link anywhere in a
/// description.
const TICKET_HOSTS: [&str; 10] = [
    "eventbrite.",
    "humanitix.",
    "ticketmaster.",
    "dice.fm",
    "axs.com",
    "etix.com",
    "seetickets.",
    "tixr.com",
    "ticketweb.",
    "comiccents.",
];

/// The tickets link of a description: a Markdown link whose text mentions
/// tickets, else the first link to a ticket seller. `None` if neither.
pub fn tickets_url(description: &str) -> Option<String> {
    let mut first_seller: Option<String> = None;
    let mut rest = description;
    while let Some(i) = rest.find("](") {
        let text_start = rest[..i].rfind('[').map_or(0, |j| j + 1);
        let text = rest[text_start..i].to_lowercase();
        let after = &rest[i + 2..];
        let end = after.find(')').unwrap_or(after.len());
        let url = after[..end].trim();
        if url.starts_with("http://") || url.starts_with("https://") {
            if text.contains("ticket") {
                return Some(url.to_owned());
            }
            if first_seller.is_none() && TICKET_HOSTS.iter().any(|h| url.contains(h)) {
                first_seller = Some(url.to_owned());
            }
        }
        rest = &after[end.min(after.len())..];
    }
    if first_seller.is_none() {
        for word in
            description.split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == '"')
        {
            if (word.starts_with("https://") || word.starts_with("http://"))
                && TICKET_HOSTS.iter().any(|h| word.contains(h))
            {
                return Some(word.trim_end_matches(['.', ',', ';']).to_owned());
            }
        }
    }
    first_seller
}

/// A title with whitespace runs as one space.
pub fn clean_title(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Plain text from a description written in Markdown (Meetup's are):
/// headings, bold and italics markers dropped, links as their text, at
/// most one blank line in a row.
pub fn markdown_to_text(s: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in s.lines() {
        let mut l = line.trim_end().to_owned();
        let t = l.trim_start();
        if t.starts_with('#') {
            l = t.trim_start_matches('#').trim_start().to_owned();
        }
        // `[text](url)` -> `text`.
        let mut out = String::with_capacity(l.len());
        let mut rest = l.as_str();
        while let Some(i) = rest.find('[') {
            let after = &rest[i + 1..];
            match after.find("](") {
                Some(j) if !after[..j].contains('[') => match after[j + 2..].find(')') {
                    Some(k) => {
                        out.push_str(&rest[..i]);
                        out.push_str(&after[..j]);
                        rest = &after[j + 2 + k + 1..];
                    }
                    None => {
                        out.push_str(&rest[..=i]);
                        rest = after;
                    }
                },
                _ => {
                    out.push_str(&rest[..=i]);
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        let out = out.replace("**", "").replace("__", "");
        let blank = out.trim().is_empty();
        if blank && lines.last().is_none_or(|p| p.is_empty()) {
            continue;
        }
        lines.push(if blank { String::new() } else { out });
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
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
            <h1>Shows</h1><p>Brass&nbsp;Lanterns &amp; friends</p><script>var a='<p>no</p>';</script>\
            <ul><li>Fri<br>7:30</li></ul><s>gone?</s><b>Bold</b>, <a href=x>link</a></body></html>";
        assert_eq!(
            to_text(html),
            "Shows\nBrass Lanterns & friends\nFri\n7:30\ngone? Bold, link"
        );
    }

    #[test]
    fn ld_json() {
        let html = r#"<script>x</script><SCRIPT type="application/ld+json">{"a":1}</SCRIPT>
            <script type='application/ld+json'>[2]</script>"#;
        assert_eq!(ld_json_blocks(html), vec![r#"{"a":1}"#, "[2]"]);
    }

    #[test]
    fn markdown() {
        let md = "Improv Millbrook\n## Time & Location\n\n\n**[Get Tickets Here!](https://x.example/a)** now\nSee [a [b]](c) and [x]\n\n";
        assert_eq!(
            markdown_to_text(md),
            "Improv Millbrook\nTime & Location\n\nGet Tickets Here! now\nSee [a b] and [x]"
        );
        assert_eq!(clean_title(" Show:  Nov 6th "), "Show: Nov 6th");
        assert_eq!(
            tickets_url(
                "Join! **[Get Tickets Here!](https://x.example/t)** or [map](https://m.example)"
            )
            .as_deref(),
            Some("https://x.example/t")
        );
        assert_eq!(
            tickets_url("[Info](https://a.example) then https://www.eventbrite.com/e/1.")
                .as_deref(),
            Some("https://www.eventbrite.com/e/1")
        );
        assert_eq!(tickets_url("[Info](https://a.example)"), None);
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
