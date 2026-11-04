//! The LLM wiki: the user's own notes as Markdown pages, one per topic, in a folder they choose,
//! with snippets quoted from their PDFs (each linking back to its document and page) and
//! `[[links]]` between pages. The AI assistant reads the pages that bear on a question and
//! proposes new pages, snippets, links and wording; nothing changes until the user accepts it.
//!
//! The pages are plain Markdown, so another editor (Obsidian, a text editor) can open and change
//! them, and PdfCraft reads back what it finds there. It understands three things in a page:
//!
//! - the **title**, the first `# ` heading (else the file name);
//! - **snippets**, a block quote whose last line links to `pdfcraft://open?sha256=…&page=…`
//!   ([`Source::link`]);
//! - **links**, `[[Title]]` or `[[Title|shown text]]`.
//!
//! Everything else is kept as it is. Sans-IO: the caller reads the folder into [`Wiki::read`] and
//! writes back what [`apply`] returns. Inputs are bounded ([`MAX_PAGES`], [`MAX_PAGE_BYTES`],
//! [`MAX_TOTAL_BYTES`]), and file names made from titles ([`file_name`]) can't leave the folder.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod change;
mod markdown;

pub use change::{Applied, ApplyError, Change, Headings, MAX_CHANGE_BYTES, MAX_CHANGES, Proposal, Proposed, apply};
pub use markdown::{links, snippets, title};

/// Pages read from one folder.
pub const MAX_PAGES: usize = 5_000;
/// A page larger than this is listed but not read.
pub const MAX_PAGE_BYTES: usize = 1 << 20;
/// Most text read from one folder.
pub const MAX_TOTAL_BYTES: usize = 64 << 20;
/// Longest title, in bytes.
pub const MAX_TITLE_BYTES: usize = 200;
/// Longest file name made from a title, in bytes, before `.md`.
const MAX_STEM_BYTES: usize = 120;
/// The pages PdfCraft keeps itself: the list of pages and the record of changes.
pub const INDEX_FILE: &str = "index.md";
pub const LOG_FILE: &str = "log.md";

/// Where a snippet came from: a document, by its content hash, and a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// The document's title or file name, as shown.
    pub title: String,
    /// SHA-256 of the document's bytes, lowercase hex.
    pub sha256: String,
    /// Page index (0-based); the link shows it 1-based.
    pub page: u32,
    /// Where the document was, to open it when the library doesn't know it.
    pub path: Option<String>,
}

/// A quote from a document, as a page holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    pub quote: String,
    pub source: Source,
}

/// One page of the wiki.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    /// Its file name in the folder (`Monetary policy.md`).
    pub file: String,
    pub title: String,
    /// The whole Markdown text; empty when the page was too large to read ([`Page::skipped`]).
    pub text: String,
    pub skipped: bool,
}

impl Page {
    pub fn snippets(&self) -> Vec<Snippet> {
        snippets(&self.text)
    }

    pub fn links(&self) -> Vec<String> {
        links(&self.text)
    }

    /// The text before the first section, without the title: what the page says it is about.
    pub fn summary(&self) -> &str {
        markdown::summary(&self.text)
    }
}

/// The pages of a wiki folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Wiki {
    pub pages: Vec<Page>,
    /// What was left out while reading, for the user.
    pub warnings: Vec<String>,
}

impl Wiki {
    /// The wiki from a folder's Markdown files, as (file name, bytes) pairs. Files PdfCraft keeps
    /// itself ([`INDEX_FILE`], [`LOG_FILE`]), names that aren't plain file names, and anything
    /// past the caps are left out, with a warning.
    pub fn read(files: Vec<(String, Vec<u8>)>) -> Self {
        let mut wiki = Self::default();
        let mut total = 0usize;
        let mut files: Vec<(String, Vec<u8>)> = files.into_iter().filter(|(name, _)| is_page_file(name)).collect();
        files.sort_by_cached_key(|(name, _)| name.to_lowercase());
        for (file, bytes) in files {
            if wiki.pages.len() >= MAX_PAGES {
                wiki.warnings.push(format!("only the first {MAX_PAGES} pages are read"));
                break;
            }
            let stem = file.get(..file.len().saturating_sub(3)).unwrap_or_default().to_owned();
            let too_big = bytes.len() > MAX_PAGE_BYTES || total.saturating_add(bytes.len()) > MAX_TOTAL_BYTES;
            if too_big {
                wiki.warnings.push(format!("{file} is too large to read"));
                wiki.pages.push(Page { file, title: stem, text: String::new(), skipped: true });
                continue;
            }
            total = total.saturating_add(bytes.len());
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let title = title(&text).unwrap_or(stem);
            wiki.pages.push(Page { file, title, text, skipped: false });
        }
        wiki
    }

    /// The page called `title` (ignoring case), or whose file is named so.
    pub fn find(&self, title: &str) -> Option<&Page> {
        let want = title.trim().to_lowercase();
        self.pages
            .iter()
            .find(|p| p.title.to_lowercase() == want)
            .or_else(|| self.pages.iter().find(|p| p.file.get(..p.file.len().saturating_sub(3)).is_some_and(|s| s.to_lowercase() == want)))
    }

    pub fn page(&self, file: &str) -> Option<&Page> {
        self.pages.iter().find(|p| p.file == file)
    }

    /// Pages that link to `title`.
    pub fn backlinks(&self, title: &str) -> Vec<&Page> {
        let want = title.trim().to_lowercase();
        self.pages.iter().filter(|p| p.links().iter().any(|l| l.to_lowercase() == want)).collect()
    }

    /// [`INDEX_FILE`]'s text: every page, linked, with the start of what it says.
    pub fn index_text(&self, heading: &str, note: &str) -> String {
        let mut out = format!("# {heading}\n\n<!-- {note} -->\n\n");
        for p in &self.pages {
            let about = first_sentence(p.summary());
            if about.is_empty() {
                out.push_str(&format!("- [[{}]]\n", p.title));
            } else {
                out.push_str(&format!("- [[{}]] — {about}\n", p.title));
            }
        }
        out
    }
}

/// A Markdown file of the wiki's own (not one PdfCraft keeps, not hidden, no folder in its name).
fn is_page_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".md") && lower != INDEX_FILE && lower != LOG_FILE && !name.starts_with('.') && !name.contains(['/', '\\']) && name.len() > 3
}

/// The first sentence of `text`, on one line, at most 160 bytes.
fn first_sentence(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = flat.find(". ").map_or(flat.len(), |i| i.saturating_add(1));
    let mut out = flat.get(..end).unwrap_or(&flat).to_owned();
    if out.len() > 160 {
        let mut cut = 157;
        while !out.is_char_boundary(cut) {
            cut = cut.saturating_sub(1);
        }
        out.truncate(cut);
        out.push('…');
    }
    out
}

/// A title as one line: whitespace collapsed, `[`, `]`, `|` and `#` (which would break links and
/// headings) left out, at most [`MAX_TITLE_BYTES`].
pub fn clean_title(title: &str) -> String {
    let flat: String = title.chars().map(|c| if c.is_control() { ' ' } else { c }).filter(|c| !matches!(c, '[' | ']' | '|' | '#')).collect();
    let mut out = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut cut = out.len().min(MAX_TITLE_BYTES);
    while !out.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    out.truncate(cut);
    out
}

/// The file name for a page titled `title`, not one of `taken` (compared ignoring case): the
/// title with characters no synced file system takes in a name replaced, never a hidden or device
/// name or one with a folder in it, and numbered (`Title 2.md`) when needed.
pub fn file_name(title: &str, taken: &[String]) -> String {
    let mut stem: String =
        clean_title(title).chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { ' ' } else { c }).collect();
    // Words of dots only (`..`) are what a path climbs with; they say nothing in a name.
    stem = stem.split_whitespace().filter(|w| !w.chars().all(|c| c == '.')).collect::<Vec<_>>().join(" ");
    let mut cut = stem.len().min(MAX_STEM_BYTES);
    while !stem.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    stem.truncate(cut);
    let stem = stem.trim_start_matches('.').trim_start().trim_end_matches(['.', ' ']).to_owned();
    let device = {
        let upper = stem.split('.').next().unwrap_or_default().to_ascii_uppercase();
        matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "INDEX" | "LOG")
            || (upper.len() == 4 && (upper.starts_with("COM") || upper.starts_with("LPT")) && upper.ends_with(|c: char| c.is_ascii_digit()))
    };
    let stem = if stem.is_empty() {
        "Untitled".to_owned()
    } else if device {
        format!("{stem} page")
    } else {
        stem
    };
    let taken: Vec<String> = taken.iter().map(|t| t.to_lowercase()).collect();
    let mut name = format!("{stem}.md");
    let mut n = 2u32;
    while taken.contains(&name.to_lowercase()) && n < 10_000 {
        name = format!("{stem} {n}.md");
        n = n.saturating_add(1);
    }
    name
}

impl Source {
    /// The link a snippet's last line carries.
    pub fn link(&self) -> String {
        let mut link = format!("pdfcraft://open?sha256={}&page={}", self.sha256, self.page.saturating_add(1));
        if let Some(path) = &self.path {
            link.push_str("&path=");
            link.push_str(&markdown::percent_encode(path));
        }
        link
    }

    /// Read a link [`Source::link`] wrote, with the title shown for it.
    pub fn parse(title: &str, link: &str) -> Option<Self> {
        markdown::parse_source(title, link)
    }
}

#[cfg(test)]
mod tests;
