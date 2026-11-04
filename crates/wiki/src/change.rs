//! Changes to the wiki: the assistant proposes them and the user accepts the ones to keep.
//! [`apply`] works on the pages as they are on disk now, so edits made meanwhile in another
//! editor are kept; a change that no longer fits (its page gone, the words it replaces changed)
//! fails with the reason instead of being forced.

use std::fmt;

use crate::markdown::{self, lines, snippet_block};
use crate::{INDEX_FILE, LOG_FILE, MAX_PAGE_BYTES, Snippet, Wiki, clean_title, file_name};

/// Changes one proposal may hold.
pub const MAX_CHANGES: usize = 200;
/// Longest text one change may add, in bytes.
pub const MAX_CHANGE_BYTES: usize = 64 * 1024;

/// Section names PdfCraft recognises in a page (any language it was written in).
const SNIPPET_SECTIONS: &[&str] = &["snippets", "snippet", "quotes", "citazioni", "citas", "citations", "zitate"];
const RELATED_SECTIONS: &[&str] =
    &["related", "see also", "collegata a", "collegate", "vedi anche", "pagine collegate", "relacionadas", "voir aussi", "siehe auch"];

/// The headings of the sections PdfCraft adds, in the interface's language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Headings {
    pub snippets: String,
    pub related: String,
}

impl Default for Headings {
    fn default() -> Self {
        Self { snippets: "Snippets".into(), related: "Related".into() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// A new page titled `title` holding `text`.
    NewPage { title: String, text: String },
    /// A snippet at the end of the page's snippets, with an optional line about it.
    AddSnippet { page: String, snippet: Snippet, note: Option<String> },
    /// A link to `to` at the end of the page's related pages.
    AddLink { page: String, to: String, note: Option<String> },
    /// A paragraph at the end of what the page says, before its sections.
    AddText { page: String, text: String },
    /// `old`, which occurs once in the page, replaced by `new`. Snippets keep their words.
    Replace { page: String, old: String, new: String },
}

impl Change {
    /// The title of the page it changes.
    pub fn page(&self) -> &str {
        match self {
            Self::NewPage { title, .. } => title,
            Self::AddSnippet { page, .. } | Self::AddLink { page, .. } | Self::AddText { page, .. } | Self::Replace { page, .. } => page,
        }
    }
}

/// One proposed change and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposed {
    pub change: Change,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Proposal {
    pub changes: Vec<Proposed>,
}

impl Proposal {
    /// Add a change; `false` once the proposal holds [`MAX_CHANGES`].
    pub fn push(&mut self, change: Change, reason: &str) -> bool {
        if self.changes.len() >= MAX_CHANGES {
            return false;
        }
        self.changes.push(Proposed { change, reason: crate::clean_title(reason).chars().take(500).collect() });
        true
    }

    /// Whether the proposal makes a page titled `title` (ignoring case).
    pub fn makes(&self, title: &str) -> bool {
        self.changes.iter().any(|p| matches!(&p.change, Change::NewPage { title: t, .. } if t.eq_ignore_ascii_case(title.trim())))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyError {
    NoPage(String),
    PageExists(String),
    Empty,
    TooLarge,
    /// The page already links there, or already holds that snippet.
    AlreadyThere,
    /// The words to replace aren't in the page (any more), or are there more than once.
    NotFound,
    Ambiguous,
    /// The replacement would change a snippet's quote or its source.
    SnippetChanged,
    /// The page was too large to read, so it isn't changed.
    Unread,
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPage(t) => write!(f, "there is no page {t}"),
            Self::PageExists(t) => write!(f, "there is already a page {t}; add to it instead"),
            Self::Empty => write!(f, "there is nothing to add"),
            Self::TooLarge => write!(f, "the page would be too large"),
            Self::AlreadyThere => write!(f, "the page already has it"),
            Self::NotFound => write!(f, "the words to replace aren't in the page"),
            Self::Ambiguous => write!(f, "the words to replace are in the page more than once"),
            Self::SnippetChanged => write!(f, "snippets keep the document's words"),
            Self::Unread => write!(f, "the page is too large to change here"),
        }
    }
}

/// What applying a change does to one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub file: String,
    /// The file's text before; `None` when the change made it.
    pub before: Option<String>,
    pub after: String,
}

/// Apply `change` to `wiki`, the pages as they are now. Returns the file to write and its new
/// text; [`Wiki::set`] it before applying the next change of a batch.
pub fn apply(wiki: &Wiki, change: &Change, headings: &Headings) -> Result<Applied, ApplyError> {
    if let Change::NewPage { title, text } = change {
        let title = clean_title(title);
        if title.is_empty() {
            return Err(ApplyError::Empty);
        }
        if wiki.find(&title).is_some() {
            return Err(ApplyError::PageExists(title));
        }
        let mut taken: Vec<String> = wiki.pages.iter().map(|p| p.file.clone()).collect();
        taken.extend([INDEX_FILE.to_owned(), LOG_FILE.to_owned()]);
        let body = text.trim();
        if body.len() > MAX_CHANGE_BYTES {
            return Err(ApplyError::TooLarge);
        }
        let after = if body.is_empty() { format!("# {title}\n") } else { format!("# {title}\n\n{body}\n") };
        return Ok(Applied { file: file_name(&title, &taken), before: None, after });
    }
    let page = wiki.find(change.page()).ok_or_else(|| ApplyError::NoPage(clean_title(change.page())))?;
    if page.skipped {
        return Err(ApplyError::Unread);
    }
    let text = page.text.as_str();
    let after = match change {
        Change::NewPage { .. } => return Err(ApplyError::Empty),
        Change::AddSnippet { snippet, note, .. } => {
            if snippet.quote.trim().is_empty() {
                return Err(ApplyError::Empty);
            }
            let flat = snippet.quote.split_whitespace().collect::<Vec<_>>().join(" ");
            if page.snippets().iter().any(|s| s.quote == flat && s.source.sha256 == snippet.source.sha256 && s.source.page == snippet.source.page) {
                return Err(ApplyError::AlreadyThere);
            }
            let mut block = snippet_block(snippet);
            if let Some(n) = note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                block.push('\n');
                block.push_str(n);
                block.push('\n');
            }
            into_section(text, SNIPPET_SECTIONS, &headings.snippets, &block)
        }
        Change::AddLink { to, note, .. } => {
            let to = clean_title(to);
            if to.is_empty() {
                return Err(ApplyError::Empty);
            }
            if page.links().iter().any(|l| l.eq_ignore_ascii_case(&to)) || to.eq_ignore_ascii_case(&page.title) {
                return Err(ApplyError::AlreadyThere);
            }
            let item = match note.as_deref().map(|n| n.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|n| !n.is_empty()) {
                Some(n) => format!("- [[{to}]] — {n}\n"),
                None => format!("- [[{to}]]\n"),
            };
            into_section(text, RELATED_SECTIONS, &headings.related, &item)
        }
        Change::AddText { text: add, .. } => {
            let add = add.trim();
            if add.is_empty() {
                return Err(ApplyError::Empty);
            }
            before_sections(text, add)
        }
        Change::Replace { old, new, .. } => {
            if old.is_empty() {
                return Err(ApplyError::Empty);
            }
            match text.matches(old.as_str()).count() {
                0 => return Err(ApplyError::NotFound),
                1 => {}
                _ => return Err(ApplyError::Ambiguous),
            }
            let after = text.replacen(old.as_str(), new, 1);
            if markdown::snippets(&after) != page.snippets() {
                return Err(ApplyError::SnippetChanged);
            }
            after
        }
    };
    let added = after.len().saturating_sub(text.len());
    if after.len() > MAX_PAGE_BYTES || added > MAX_CHANGE_BYTES {
        return Err(ApplyError::TooLarge);
    }
    Ok(Applied { file: page.file.clone(), before: Some(text.to_owned()), after })
}

/// The byte range of the section headed `## <one of names>`: from its heading to the next heading
/// of its level or above.
fn section(text: &str, names: &[&str]) -> Option<(usize, usize)> {
    let ls = lines(text);
    let at = ls.iter().position(|(_, l)| l.strip_prefix("## ").is_some_and(|h| names.contains(&h.trim().to_lowercase().as_str())))?;
    let start = ls.get(at)?.0;
    let end = ls.iter().skip(at.saturating_add(1)).find(|(_, l)| l.starts_with("# ") || l.starts_with("## ")).map_or(text.len(), |(o, _)| *o);
    Some((start, end))
}

/// `block` at the end of the section named one of `names`, or in a new section `heading` at the
/// end of the page.
fn into_section(text: &str, names: &[&str], heading: &str, block: &str) -> String {
    let lower: Vec<String> = names.iter().map(|n| (*n).to_owned()).chain([heading.trim().to_lowercase()]).collect();
    let lower: Vec<&str> = lower.iter().map(String::as_str).collect();
    match section(text, &lower) {
        Some((_, end)) => {
            let (head, tail) = text.split_at(end.min(text.len()));
            let head = head.trim_end();
            let sep = if block.starts_with("- ") && head.lines().last().is_some_and(|l| l.starts_with("- ")) { "\n" } else { "\n\n" };
            let tail = if tail.is_empty() { String::new() } else { format!("\n{tail}") };
            format!("{head}{sep}{}{tail}", block.trim_end_matches('\n').to_owned() + "\n")
        }
        None => format!("{}\n\n## {}\n\n{}", text.trim_end(), heading.trim(), block.trim_end_matches('\n').to_owned() + "\n"),
    }
}

/// `add` as a paragraph after what the page says and before its first section.
fn before_sections(text: &str, add: &str) -> String {
    let ls = lines(text);
    let first_section = ls.iter().find(|(_, l)| l.starts_with("## ")).map(|(o, _)| *o);
    match first_section {
        Some(at) => {
            let (head, tail) = text.split_at(at.min(text.len()));
            format!("{}\n\n{add}\n\n{tail}", head.trim_end())
        }
        None => format!("{}\n\n{add}\n", text.trim_end()),
    }
}

impl Wiki {
    /// Put `text` as file `file`'s, adding the page if it is new: how a batch sees the changes
    /// already applied.
    pub fn set(&mut self, file: &str, text: String) {
        let stem = file.get(..file.len().saturating_sub(3)).unwrap_or(file).to_owned();
        let title = crate::title(&text).unwrap_or(stem);
        match self.pages.iter_mut().find(|p| p.file == file) {
            Some(p) => {
                p.title = title;
                p.text = text;
                p.skipped = false;
            }
            None => {
                self.pages.push(crate::Page { file: file.to_owned(), title, text, skipped: false });
                self.pages.sort_by_cached_key(|p| p.file.to_lowercase());
            }
        }
    }

    /// Take a page out (an undone new page).
    pub fn remove(&mut self, file: &str) {
        self.pages.retain(|p| p.file != file);
    }
}
