//! What PdfCraft reads in a page: its title, snippets and links, outside code blocks. Lenient:
//! anything else, or anything malformed, is text it leaves alone.

use crate::{Snippet, Source, clean_title};

/// Lines looked at for the title.
const TITLE_LINES: usize = 64;
/// Snippets and links read from one page.
const MAX_SNIPPETS: usize = 2_000;
const MAX_LINKS: usize = 1_000;
/// Longest `[[link]]`, in bytes.
const MAX_LINK_BYTES: usize = 300;

/// The page's lines outside fenced code blocks and front matter, with their byte offsets.
pub(crate) fn lines(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut fence: Option<&str> = None;
    let mut offset = 0usize;
    let mut front = text.starts_with("---\n") || text.starts_with("---\r\n");
    for (i, raw) in text.split_inclusive('\n').enumerate() {
        let start = offset;
        offset = offset.saturating_add(raw.len());
        let line = raw.trim_end_matches(['\n', '\r']);
        if front {
            if i > 0 && line.trim_end() == "---" {
                front = false;
            }
            continue;
        }
        let trimmed = line.trim_start();
        let marker = if trimmed.starts_with("```") {
            Some("```")
        } else if trimmed.starts_with("~~~") {
            Some("~~~")
        } else {
            None
        };
        match (fence, marker) {
            (None, Some(m)) => fence = Some(m),
            (Some(f), Some(m)) if f == m => fence = None,
            (None, None) => out.push((start, line)),
            _ => {}
        }
    }
    out
}

/// The first `# ` heading.
pub fn title(text: &str) -> Option<String> {
    lines(text).into_iter().take(TITLE_LINES).find_map(|(_, l)| l.strip_prefix("# ").map(clean_title).filter(|t| !t.is_empty()))
}

/// The text after the title and before the first section.
pub(crate) fn summary(text: &str) -> &str {
    let ls = lines(text);
    let after_title = ls.iter().position(|(_, l)| l.starts_with("# ")).map_or(0, |i| i.saturating_add(1));
    let start = ls.get(after_title).map_or(text.len(), |(o, _)| *o);
    let end = ls.iter().skip(after_title).find(|(_, l)| l.starts_with("## ") || l.starts_with("# ")).map_or(text.len(), |(o, _)| *o);
    text.get(start..end.max(start)).unwrap_or_default().trim()
}

/// The pages linked with `[[Title]]` or `[[Title|shown]]` (`[[Title#Section]]` counts as Title),
/// each once, in order.
pub fn links(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (_, line) in lines(text) {
        let mut rest = line;
        while let Some(open) = rest.find("[[") {
            let after = rest.get(open.saturating_add(2)..).unwrap_or_default();
            let Some(close) = after.find("]]") else { break };
            let inner = after.get(..close).unwrap_or_default();
            rest = after.get(close.saturating_add(2)..).unwrap_or_default();
            if inner.len() > MAX_LINK_BYTES {
                continue;
            }
            let target = inner.split('|').next().unwrap_or_default();
            let target = clean_title(target.split('#').next().unwrap_or_default());
            if !target.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(&target)) {
                out.push(target);
                if out.len() >= MAX_LINKS {
                    return out;
                }
            }
        }
    }
    out
}

/// The snippets: block quotes whose last line is `— [title, p. N](pdfcraft://open?…)`.
pub fn snippets(text: &str) -> Vec<Snippet> {
    let mut out = Vec::new();
    let mut group: Vec<&str> = Vec::new();
    let ls = lines(text);
    let flush = |group: &mut Vec<&str>, out: &mut Vec<Snippet>| {
        if let Some((last, quote)) = group.split_last()
            && let Some(source) = source_line(last)
        {
            let quote = unquote(&quote.join(" "));
            if !quote.is_empty() && out.len() < MAX_SNIPPETS {
                out.push(Snippet { quote, source });
            }
        }
        group.clear();
    };
    for (_, line) in ls {
        match line.trim_start().strip_prefix('>') {
            Some(inner) => group.push(inner.strip_prefix(' ').unwrap_or(inner)),
            None => flush(&mut group, &mut out),
        }
    }
    flush(&mut group, &mut out);
    out
}

/// `— [label](link)` (an em or en dash, or one or two hyphens).
fn source_line(line: &str) -> Option<Source> {
    let line = line.trim();
    let line = ["—", "–", "--", "-"].iter().find_map(|d| line.strip_prefix(d))?.trim_start();
    let inner = line.strip_prefix('[')?;
    let (label, rest) = inner.split_once("](")?;
    let link = rest.strip_suffix(')')?;
    let title = match label.rsplit_once(", p. ") {
        Some((t, n)) if n.bytes().all(|b| b.is_ascii_digit()) => t,
        _ => label,
    };
    parse_source(title, link)
}

/// The quote without the quotation marks around it, on one line.
fn unquote(quote: &str) -> String {
    let flat = quote.split_whitespace().collect::<Vec<_>>().join(" ");
    for (open, close) in [('«', '»'), ('“', '”'), ('"', '"')] {
        if let Some(inner) = flat.strip_prefix(open).and_then(|s| s.strip_suffix(close)) {
            return inner.trim().to_owned();
        }
    }
    flat
}

pub(crate) fn parse_source(title: &str, link: &str) -> Option<Source> {
    let query = link.trim().strip_prefix("pdfcraft://open?")?;
    let (mut sha256, mut page, mut path) = (None, None, None);
    for pair in query.split('&').take(16) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "sha256" if value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) => {
                sha256 = Some(value.to_owned());
            }
            "page" if !value.is_empty() && value.len() <= 10 && value.bytes().all(|b| b.is_ascii_digit()) && !value.starts_with('0') => {
                page = value.parse::<u32>().ok().and_then(|p| p.checked_sub(1));
            }
            "path" => path = percent_decode(value),
            _ => {}
        }
    }
    Some(Source { title: clean_title(title), sha256: sha256?, page: page?, path })
}

/// A path in a link: letters, digits, `-_.~/` as they are, every other byte as `%XX`.
pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hex = s.get(i.saturating_add(1)..i.saturating_add(3))?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i = i.saturating_add(3);
        } else {
            out.push(b);
            i = i.saturating_add(1);
        }
    }
    let path = String::from_utf8(out).ok()?;
    (!path.is_empty() && !path.contains(['\0', '\n', '\r'])).then_some(path)
}

/// A snippet as a page holds it: the quote, then its source.
pub(crate) fn snippet_block(snippet: &Snippet) -> String {
    let quote = snippet.quote.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = clean_title(&snippet.source.title);
    let title = if title.is_empty() { "PDF".to_owned() } else { title };
    format!("> {quote}\n> — [{title}, p. {}]({})\n", snippet.source.page.saturating_add(1), snippet.source.link())
}
