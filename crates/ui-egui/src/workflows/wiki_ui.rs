//! The wiki beside the document (`pdfcraft_wiki`): Markdown pages in a folder the user chose (on
//! Google Drive by default, so it syncs and other editors can open it), listed, searched and read
//! with their snippets and links; the selection added to a page; and the AI assistant's proposed
//! changes, applied once the user accepts them, with the last batch undoable.
//!
//! - **The folder is the wiki.** Pages are read from it (off the UI thread) and written back
//!   atomically; edits made there in another editor are read back, and changes are applied to the
//!   pages as they are on disk, not as they were when proposed.
//! - **Nothing changes unasked.** The assistant's changes are a proposal; only the ones kept are
//!   written, and `index.md` and `log.md` are updated with them.

use std::io::Read;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use pdfcraft_ai::claude::{Options, Passage};
use pdfcraft_ai::source::SourceId;
use pdfcraft_engine::DocId;
use pdfcraft_engine::assistant::{self, Selected};
use pdfcraft_engine::wiki_assist::{self, EditRequest, WikiEdit, WikiIndex};
use pdfcraft_wiki::{Applied, Change, Headings, Snippet, Source, Wiki, apply};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// Files looked at in the folder.
const MAX_ENTRIES: usize = 20_000;
/// Wiki passages sent with a question.
pub(crate) const WIKI_PASSAGES: usize = 4;
/// Pages listed in the panel at once.
const LISTED: usize = 200;
/// The folder made for the wiki on Google Drive or on this computer.
pub const WIKI_FOLDER: &str = "PdfCraft Wiki";
/// What `index.md` carries when PdfCraft wrote it: an index of the user's own is never written over.
const INDEX_MARK: &str = "pdfcraft:index";
/// `log.md` past this size is left alone: it is only ever added to, never cut.
const MAX_LOG_BYTES: usize = pdfcraft_wiki::MAX_PAGE_BYTES / 2;

type Loaded = Result<(Wiki, WikiIndex), String>;

/// The wiki as the panel shows it.
#[derive(Default)]
pub struct WikiState {
    pub(crate) wiki: Option<Wiki>,
    pub(crate) index: Option<WikiIndex>,
    /// The folder the pages shown came from.
    loaded_from: Option<PathBuf>,
    loading: Option<(PathBuf, mpsc::Receiver<Loaded>)>,
    pub(crate) error: Option<String>,
    /// The file of the page shown; `None` lists the pages.
    pub page: Option<String>,
    pub(crate) search: String,
    pub(crate) request: String,
    /// The selection waiting for a page to go on.
    pub(crate) pending: Option<Snippet>,
    pub(crate) review: Option<Review>,
    running: Option<Running>,
    /// The last batch written, to undo.
    undo: Vec<Applied>,
}

pub(crate) struct Review {
    pub(crate) edit: WikiEdit,
    pub(crate) keep: Vec<bool>,
}

struct Running {
    rx: mpsc::Receiver<Result<WikiEdit, String>>,
    cancel: Arc<AtomicBool>,
}

impl WikiState {
    pub fn pages(&self) -> usize {
        self.wiki.as_ref().map_or(0, |w| w.pages.len())
    }

    pub fn page_shown(&self) -> Option<&str> {
        self.page.as_deref()
    }

    pub fn has_review(&self) -> bool {
        self.review.is_some()
    }

    pub fn running(&self) -> bool {
        self.running.is_some()
    }

    pub fn loading(&self) -> bool {
        self.loading.is_some()
    }
}

/// What the panel knows of the app.
pub(crate) struct Here {
    pub folder: Option<PathBuf>,
    pub drive: bool,
    /// Text is selected in a document that allows copying.
    pub can_add: bool,
    pub has_doc: bool,
}

pub(crate) enum Action {
    Open(String),
    Back,
    OpenSource(Source),
    AddSelection,
    AddPendingTo(String),
    AddPendingToNew(String),
    CancelPending,
    Propose,
    FromPage,
    Check,
    Stop,
    Apply,
    Discard,
    Undo,
    UseDrive,
    ChooseFolder,
    UseLocal,
    Reload,
}

/// The section headings PdfCraft adds, in the interface's language.
fn headings() -> Headings {
    Headings { snippets: tl!("Snippets").to_owned(), related: tl!("Related").to_owned() }
}

/// The wiki's pages from `dir`: its Markdown files, each read only so far as a page may be long.
fn read_folder(dir: &Path) -> Result<Wiki, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total = 0usize;
    for entry in entries.take(MAX_ENTRIES) {
        let Ok(entry) = entry else { continue };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        if !name.to_lowercase().ends_with(".md") || name.starts_with('.') {
            continue;
        }
        // Not following links: a page is a file in this folder.
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        if files.len() > pdfcraft_wiki::MAX_PAGES || total > pdfcraft_wiki::MAX_TOTAL_BYTES {
            break;
        }
        let mut bytes = Vec::new();
        let limit = u64::try_from(pdfcraft_wiki::MAX_PAGE_BYTES).unwrap_or(u64::MAX).saturating_add(1);
        if let Ok(f) = std::fs::File::open(entry.path()) {
            let _ = f.take(limit).read_to_end(&mut bytes);
        }
        total = total.saturating_add(bytes.len());
        files.push((name, bytes));
    }
    Ok(Wiki::read(files))
}

/// Write `index.md`: every page, linked. Only over an index PdfCraft wrote (or none): a page of
/// the user's own by that name, as in an Obsidian vault, is left as it is.
fn write_index(dir: &Path, wiki: &Wiki) {
    let path = dir.join(pdfcraft_wiki::INDEX_FILE);
    match std::fs::read(&path) {
        Ok(bytes) if !String::from_utf8_lossy(&bytes).contains(INDEX_MARK) => return,
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return,
        _ => {}
    }
    let note = format!("{INDEX_MARK} · {}", tl!("PdfCraft keeps this page up to date; changes made here are replaced."));
    if let Err(e) = crate::editing::write_atomically(&path.to_string_lossy(), wiki.index_text(tl!("Index"), &note).as_bytes()) {
        log::warn!("couldn't write the wiki's index: {e}");
    }
}

/// A page file's path in `dir`, if `file` is a plain name of a Markdown file.
fn page_path(dir: &Path, file: &str) -> Option<PathBuf> {
    let plain = !file.contains(['/', '\\']) && !file.starts_with('.') && file.to_lowercase().ends_with(".md");
    plain.then(|| dir.join(file))
}

impl PdfCraftApp {
    /// The wiki's folder, from the settings.
    pub(crate) fn wiki_folder(&self) -> Option<PathBuf> {
        self.assistant.prefs.wiki_folder.as_deref().filter(|f| !f.is_empty()).map(PathBuf::from)
    }

    /// Start reading the wiki when its folder changed or it hasn't been read yet.
    pub(crate) fn ensure_wiki(&mut self) {
        let Some(dir) = self.wiki_folder() else { return };
        // Read once per folder; after a failure, again only when asked (Reload).
        let fresh = self.wiki.loaded_from.as_deref() == Some(dir.as_path());
        let busy = self.wiki.loading.as_ref().is_some_and(|(d, _)| *d == dir);
        if fresh || busy {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        let path = dir.clone();
        let work = move || {
            let loaded = read_folder(&path).map(|w| {
                let index = WikiIndex::build(&w);
                (w, index)
            });
            let _ = tx.send(loaded);
            if let Some(c) = &ctx {
                c.request_repaint();
            }
        };
        self.wiki.loading = Some((dir, rx));
        #[cfg(not(target_arch = "wasm32"))]
        if let Err(e) = std::thread::Builder::new().name("wiki-read".into()).spawn(work) {
            self.wiki.loading = None;
            self.wiki.error = Some(e.to_string());
        }
        #[cfg(target_arch = "wasm32")]
        work();
    }

    /// Take the pages once read, and the assistant's proposal once it has come.
    pub(crate) fn poll_wiki(&mut self) {
        if let Some((dir, rx)) = &self.wiki.loading {
            let got = match rx.try_recv() {
                Ok(r) => Some(r),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(tl!("Reading the wiki stopped unexpectedly.").to_owned())),
            };
            if let Some(result) = got {
                let dir = dir.clone();
                self.wiki.loading = None;
                // A folder changed meanwhile is read again by `ensure_wiki`.
                if self.wiki_folder().as_deref() == Some(dir.as_path()) {
                    match result {
                        Ok((w, index)) => {
                            for warning in &w.warnings {
                                log::info!("wiki: {warning}");
                            }
                            self.wiki.wiki = Some(w);
                            self.wiki.index = Some(index);
                            self.wiki.error = None;
                        }
                        Err(e) => {
                            self.wiki.wiki = None;
                            self.wiki.index = None;
                            self.wiki.error = Some(e);
                        }
                    }
                    self.wiki.loaded_from = Some(dir);
                }
            }
        }
        self.poll_wiki_edit();
    }

    fn poll_wiki_edit(&mut self) {
        let Some(running) = &self.wiki.running else { return };
        let result = match running.rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(tl!("The request stopped unexpectedly.").to_owned()),
        };
        let cancelled = running.cancel.load(Ordering::Relaxed);
        self.wiki.running = None;
        match result {
            _ if cancelled => {}
            Ok(edit) if edit.proposal.changes.is_empty() => {
                let why = edit.errors.first().cloned().unwrap_or_else(|| edit.summary.trim().to_owned());
                if why.is_empty() {
                    self.notify_tr("The assistant proposed no changes to the wiki.");
                } else {
                    self.notify_fmt("The assistant proposed no changes to the wiki: {why}", &[("why", &why)]);
                }
            }
            Ok(edit) => {
                let keep = vec![true; edit.proposal.changes.len()];
                self.wiki.review = Some(Review { edit, keep });
                self.choose_right_panel(Some(crate::RightPanel::Wiki));
            }
            Err(e) => self.notify(e),
        }
    }

    /// Read the wiki again from its folder (after changes there, or a new folder).
    pub(crate) fn reload_wiki(&mut self) {
        self.wiki.wiki = None;
        self.reread_wiki();
    }

    /// Read the folder again from scratch: a read already under way may have started before the
    /// last write, so its result is dropped.
    fn reread_wiki(&mut self) {
        self.wiki.loading = None;
        self.wiki.loaded_from = None;
        self.ensure_wiki();
    }

    /// Keep the wiki in `dir`, made if needed.
    pub(crate) fn set_wiki_folder(&mut self, dir: PathBuf) {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.notify_fmt("Couldn't use that folder for the wiki: {e}", &[("e", &e.to_string())]);
            return;
        }
        self.assistant.prefs.wiki_folder = Some(dir.to_string_lossy().into_owned());
        self.wiki.page = None;
        self.wiki.undo.clear();
        self.reload_wiki();
    }

    /// The wiki's folder in My Drive of the first Google account Drive for desktop syncs.
    #[cfg(not(target_arch = "wasm32"))]
    fn use_drive_for_wiki(&mut self) {
        use pdfcraft_platform::drive;
        let home = self.drive.home.clone();
        let Some(account) = drive::accounts(home.as_deref()).into_iter().next() else {
            self.notify_tr("Google Drive for desktop isn't set up on this computer.");
            return;
        };
        match drive::folders(&account.root) {
            Ok(places) => match drive::my_drive(&places) {
                Some(mine) => self.set_wiki_folder(account.root.join(mine).join(WIKI_FOLDER)),
                None => self.notify_tr("My Drive wasn't found in Google Drive; choose the folder yourself."),
            },
            Err(drive::ListError::NotAllowed) => {
                self.notify_tr("PdfCraft isn't allowed to open Google Drive yet. Allow it when macOS asks, or choose the folder yourself.")
            }
            Err(_) => self.notify_tr("Google Drive couldn't be read; choose the folder yourself."),
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn use_drive_for_wiki(&mut self) {
        self.notify_tr("The wiki needs the desktop app");
    }

    /// The selection, as a snippet for the wiki, following the assistant's rules (nothing from a
    /// document that forbids copying, nothing under unapplied redaction marks).
    pub(crate) fn add_selection_to_wiki(&mut self, index: usize) {
        let Some(view) = self.views.get(index) else { return };
        let id = view.id;
        let Some((page, selected)) = view.assistant_selection() else {
            self.notify_tr("Select some text first.");
            return;
        };
        let ctx = match self.session.assistant_context(id, page, &selected) {
            Ok(ctx) => ctx,
            Err(e) => {
                self.notify(e.to_string());
                return;
            }
        };
        let quote = ctx.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if quote.is_empty() {
            self.notify_tr("There is no text there to add.");
            return;
        }
        let quote = pdfcraft_ai::text::capped(&quote, pdfcraft_wiki::MAX_CHANGE_BYTES / 2);
        let source = self.wiki_source(id, u32::try_from(page).unwrap_or(u32::MAX));
        self.wiki.pending = Some(Snippet { quote, source });
        self.wiki.search.clear();
        self.choose_right_panel(Some(crate::RightPanel::Wiki));
    }

    fn wiki_source(&self, id: DocId, page: u32) -> Source {
        Source {
            title: self.session.document_title(id).unwrap_or_default(),
            sha256: self.session.document_sha256(id).map(|h| h.to_hex()).unwrap_or_default(),
            page,
            path: self.session.get(id).and_then(|d| d.path.clone()),
        }
    }

    /// Write `changes` to the wiki's folder, applied to the pages as they are on disk now. Returns
    /// the files written, in order; the changes that couldn't be made are reported.
    fn write_wiki_changes(&mut self, changes: &[Change], kind: &str) -> Vec<String> {
        let Some(dir) = self.wiki_folder() else { return Vec::new() };
        let mut wiki = match read_folder(&dir) {
            Ok(w) => w,
            Err(e) => {
                self.notify_fmt("Couldn't read the wiki: {e}", &[("e", &e)]);
                return Vec::new();
            }
        };
        let headings = headings();
        let mut written: Vec<Applied> = Vec::new();
        let mut failed: Vec<String> = Vec::new();
        for change in changes {
            let result = apply(&wiki, change, &headings).map_err(|e| e.to_string()).and_then(|a| {
                let path = page_path(&dir, &a.file).ok_or_else(|| tl!("not a page's file name").to_owned())?;
                // A new page never writes over a file that appeared meanwhile.
                if a.before.is_none() && path.exists() {
                    return Err(tl!("a file of that name is already there").to_owned());
                }
                crate::editing::write_atomically(&path.to_string_lossy(), a.after.as_bytes()).map_err(|e| e.to_string())?;
                Ok(a)
            });
            match result {
                Ok(a) => {
                    wiki.set(&a.file, a.after.clone());
                    written.push(a);
                }
                Err(e) => failed.push(format!("{}: {e}", change.page())),
            }
        }
        if !written.is_empty() {
            self.write_index_and_log(&dir, &wiki, &written, kind);
            self.wiki.undo = written.clone();
        }
        if let Some(first) = failed.first() {
            self.notify_fmt("{n} changes couldn't be made: {first}", &[("n", &failed.len().to_string()), ("first", first)]);
        }
        self.reread_wiki();
        written.into_iter().map(|a| a.file).collect()
    }

    fn write_index_and_log(&mut self, dir: &Path, wiki: &Wiki, written: &[Applied], kind: &str) {
        write_index(dir, wiki);
        let log_path = dir.join(pdfcraft_wiki::LOG_FILE);
        let mut log = match std::fs::read(&log_path) {
            Ok(bytes) if bytes.len() > MAX_LOG_BYTES => {
                log::info!("the wiki's log is past {MAX_LOG_BYTES} bytes; nothing more is added to it");
                return;
            }
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => format!("# {}\n", tl!("Log")),
            Err(e) => {
                log::warn!("couldn't read the wiki's log: {e}");
                return;
            }
        };
        let date = chrono::Local::now().format("%Y-%m-%d");
        log.push_str(&format!("\n## [{date}] {kind}\n\n"));
        for a in written {
            let title = a.file.trim_end_matches(".md");
            let what = if a.before.is_none() { tl!("new page") } else { tl!("changed") };
            log.push_str(&format!("- [[{title}]] ({what})\n"));
        }
        if let Err(e) = crate::editing::write_atomically(&log_path.to_string_lossy(), log.as_bytes()) {
            log::warn!("couldn't write the wiki's log: {e}");
        }
    }

    /// Put back what the last batch changed, where the files are still as it left them.
    fn undo_wiki(&mut self) {
        let Some(dir) = self.wiki_folder() else { return };
        let batch = std::mem::take(&mut self.wiki.undo);
        let mut kept = 0usize;
        for a in batch.iter().rev() {
            let Some(path) = page_path(&dir, &a.file) else { continue };
            if std::fs::read_to_string(&path).ok().as_deref() != Some(a.after.as_str()) {
                kept = kept.saturating_add(1);
                continue;
            }
            let done = match &a.before {
                Some(before) => crate::editing::write_atomically(&path.to_string_lossy(), before.as_bytes()).is_ok(),
                None => std::fs::remove_file(&path).is_ok(),
            };
            if !done {
                kept = kept.saturating_add(1);
            }
        }
        if let Ok(wiki) = read_folder(&dir) {
            write_index(&dir, &wiki);
        }
        if kept > 0 {
            self.notify_fmt("{n} pages changed since, so they were left as they are.", &[("n", &kept.to_string())]);
        } else {
            self.notify_tr("The wiki's last changes were undone.");
        }
        if self.wiki.page.as_ref().is_some_and(|p| !dir.join(p).exists()) {
            self.wiki.page = None;
        }
        self.reread_wiki();
    }

    fn add_pending(&mut self, title: &str, new_page: bool) {
        let Some(snippet) = self.wiki.pending.take() else { return };
        let title = pdfcraft_wiki::clean_title(title);
        if title.is_empty() {
            self.wiki.pending = Some(snippet);
            self.notify_tr("Type the new page's title.");
            return;
        }
        let mut changes = Vec::new();
        if new_page {
            changes.push(Change::NewPage { title: title.clone(), text: String::new() });
        }
        changes.push(Change::AddSnippet { page: title.clone(), snippet, note: None });
        let written = self.write_wiki_changes(&changes, tl!("snippet added"));
        if written.len() == changes.len() {
            self.notify_fmt("Added to the wiki page “{page}”", &[("page", &title)]);
            self.wiki.page = written.last().cloned();
            self.wiki.search.clear();
        }
    }

    fn apply_wiki_review(&mut self) {
        let Some(review) = self.wiki.review.take() else { return };
        let changes: Vec<Change> =
            review.edit.proposal.changes.iter().zip(&review.keep).filter(|(_, keep)| **keep).map(|(p, _)| p.change.clone()).collect();
        if changes.is_empty() {
            return;
        }
        let n = self.write_wiki_changes(&changes, tl!("assistant's changes")).len();
        if n > 0 {
            self.notify_fmt("{n} changes made to the wiki", &[("n", &n.to_string())]);
        }
    }

    /// Ask the assistant for changes to the wiki: `request`, with the page being read, the
    /// library's and the wiki's passages about it.
    pub(crate) fn start_wiki_edit(&mut self, request: String, extra: Vec<Passage>) {
        let Some(service) = self.ai_service.clone() else {
            self.notify_tr("The AI assistant needs the desktop app.");
            return;
        };
        if self.wiki.running.is_some() || request.trim().is_empty() {
            return;
        }
        if !self.assistant.prefs.enabled {
            self.notify_tr("Turn the AI assistant on in Preferences first.");
            return;
        }
        if !self.assistant.prefs.consented {
            self.notify_tr("Ask a first question in the AI assistant panel to allow requests, then try again.");
            return;
        }
        let (transport, key) = match service.route(&self.assistant.prefs) {
            Ok(route) => route,
            Err(e) => {
                self.notify(e);
                return;
            }
        };
        let Some(wiki) = self.wiki.wiki.clone() else {
            self.notify_tr("The wiki is still being read; try again in a moment.");
            return;
        };
        let mut passages = extra;
        let active = self.active.and_then(|i| self.views.get(i)).map(|v| (v.id, v.current, v.assistant_selection()));
        let doc = active.as_ref().map(|(id, ..)| *id);
        let mut doc_source: Option<Source> = None;
        if let Some((id, page, selection)) = active {
            let (page, selected) = selection.unwrap_or((page, Selected::Page));
            match self.session.assistant_context(id, page, &selected) {
                Ok(ctx) => {
                    passages.extend(assistant::context_passages(&ctx));
                    doc_source = Some(self.wiki_source(id, 0));
                }
                Err(e) => log::info!("the wiki request leaves out the open page: {e}"),
            }
        }
        let (library, entries) = self.library_for_map(doc, &request);
        passages.extend(library);
        if let Some(index) = &self.wiki.index {
            passages.extend(index.passages(&request, WIKI_PASSAGES));
        }
        passages.truncate(pdfcraft_ai::claude::MAX_PASSAGES);
        let model = self.assistant.prefs.model();
        let effort = self.assistant.prefs.effort;
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        let headings = headings();
        let work = move || {
            let source = move |p: &Passage| -> Option<Source> {
                match p.id {
                    SourceId::ThisDocument { page } => doc_source.clone().map(|s| Source { page, ..s }),
                    SourceId::Library { sha256, page } => {
                        let hex = sha256.to_hex();
                        let (_, paths, title) = entries.iter().find(|(h, ..)| *h == hex)?.clone();
                        Some(Source { title, sha256: hex, page, path: paths.into_iter().next() })
                    }
                    SourceId::Wiki { .. } => None,
                }
            };
            let req = EditRequest {
                options: Options { effort, ..Options::new(model) },
                key: &key,
                wiki: &wiki,
                passages,
                request: &request,
                headings,
                source: &source,
            };
            let result = pdfcraft_engine::guard(|| {
                wiki_assist::propose(transport.as_ref(), &req, &mut |_| {
                    if stop.load(Ordering::Relaxed) { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
                })
            })
            .unwrap_or_else(|panic| Err(format!("the request failed unexpectedly ({panic})")));
            let _ = tx.send(result);
            if let Some(c) = &ctx {
                c.request_repaint();
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        if let Err(e) = std::thread::Builder::new().name("ai-wiki".into()).spawn(work) {
            log::warn!("couldn't start the wiki assistant's worker: {e}");
        }
        #[cfg(target_arch = "wasm32")]
        work();
        self.wiki.running = Some(Running { rx, cancel });
    }

    /// A snippet's source: its document at the page, opened if it isn't.
    fn open_wiki_source(&mut self, source: &Source) {
        let Some(sha) = pdfcraft_ai::source::Sha256::from_hex(&source.sha256) else { return };
        let page = usize::try_from(source.page).unwrap_or(usize::MAX);
        if let Some(i) = self.views.iter().position(|v| self.session.document_sha256(v.id) == Some(sha)) {
            self.active = Some(i);
            if let Some(view) = self.views.get_mut(i)
                && page < view.page_count()
            {
                view.go_to_page(page);
            }
            return;
        }
        let path = source.path.clone().or_else(|| self.library_mut().entry(&sha).and_then(|e| e.paths.first().cloned()));
        match path {
            Some(path) => self.open_library_document(path, page, sha),
            None => self.notify_tr("That document isn't open, and the wiki doesn't say where it is."),
        }
    }

    /// The wiki page a citation names, shown in the panel.
    pub(crate) fn open_wiki_page_by_id(&mut self, page: &pdfcraft_ai::source::Sha256) {
        let file = self.wiki.index.as_ref().and_then(|i| i.file(page)).map(str::to_owned);
        match file {
            Some(file) => {
                self.wiki.page = Some(file);
                self.choose_right_panel(Some(crate::RightPanel::Wiki));
            }
            None => self.notify_tr("That wiki page isn't there any more."),
        }
    }
}

/// Ask for the wiki's folder in the system's dialog.
pub(crate) fn choose_folder(app: &mut PdfCraftApp) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut dialog = rfd::AsyncFileDialog::new().set_title(tl!("Choose the wiki's folder").to_string());
        if let Some(dir) = app.wiki_folder() {
            dialog = dialog.set_directory(dir);
        }
        app.ask_one(crate::pickers::Ask::Folder(dialog), None, |app, dir| app.set_wiki_folder(dir));
    }
    #[cfg(target_arch = "wasm32")]
    app.notify_tr("The wiki needs the desktop app");
}

/// Run what the panel asked for.
pub(crate) fn run_panel(app: &mut PdfCraftApp, actions: Vec<Action>) {
    for action in actions {
        run(app, action);
    }
}

fn run(app: &mut PdfCraftApp, action: Action) {
    match action {
        Action::Open(title) => {
            let file = app.wiki.wiki.as_ref().and_then(|w| w.find(&title)).map(|p| p.file.clone());
            match file {
                Some(f) => app.wiki.page = Some(f),
                None => app.notify_fmt("There is no wiki page “{page}” yet.", &[("page", &title)]),
            }
        }
        Action::Back => app.wiki.page = None,
        Action::OpenSource(source) => app.open_wiki_source(&source),
        Action::AddSelection => {
            if let Some(i) = app.active {
                app.add_selection_to_wiki(i);
            }
        }
        Action::AddPendingTo(title) => app.add_pending(&title, false),
        Action::AddPendingToNew(title) => app.add_pending(&title, true),
        Action::CancelPending => app.wiki.pending = None,
        Action::Propose => {
            let request = app.wiki.request.trim().to_owned();
            app.start_wiki_edit(request, Vec::new());
        }
        Action::FromPage => app.start_wiki_edit(tl!("Add what matters on the page being read to the wiki: quote the key passages as snippets on the pages they belong to, making pages for topics the wiki doesn't cover yet, and link related pages.").to_owned(), Vec::new()),
        Action::Check => app.start_wiki_edit(tl!("Check the wiki: find claims that contradict each other, pages that should link to each other, pages nothing links to, and statements without a snippet behind them, and propose fixes.").to_owned(), Vec::new()),
        Action::Stop => {
            if let Some(r) = &app.wiki.running {
                r.cancel.store(true, Ordering::Relaxed);
            }
        }
        Action::Apply => app.apply_wiki_review(),
        Action::Discard => app.wiki.review = None,
        Action::Undo => app.undo_wiki(),
        Action::UseDrive => app.use_drive_for_wiki(),
        Action::ChooseFolder => choose_folder(app),
        Action::UseLocal => {
            if let Some(dir) = crate::assistant_ui::assistant_dir() {
                app.set_wiki_folder(dir.join("Wiki"));
            }
        }
        Action::Reload => app.reload_wiki(),
    }
}

/// The Wiki panel.
pub(crate) fn panel(ui: &mut egui::Ui, t: &Tokens, state: &mut WikiState, here: &Here) -> Vec<Action> {
    let mut out = Vec::new();
    let Some(folder) = &here.folder else {
        setup(ui, t, here, &mut out);
        return out;
    };
    ui.horizontal(|ui| {
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        ui.add(icons_image("folder", t));
        ui.label(egui::RichText::new(name).small().color(t.text_muted)).on_hover_text(folder.to_string_lossy());
        if ui.small_button(tl!("Change…")).clicked() {
            out.push(Action::ChooseFolder);
        }
        if ui.small_button(tl!("Reload")).clicked() {
            out.push(Action::Reload);
        }
    });
    if let Some(e) = &state.error {
        ui.label(
            egui::RichText::new(crate::i18n::fmt(tl!("The wiki's folder couldn't be read: {e}"), &[("e", e)])).color(ui.visuals().error_fg_color),
        );
        return out;
    }
    let Some(wiki) = state.wiki.as_ref() else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(tl!("Reading the wiki…"));
        });
        return out;
    };
    if let Some(review) = state.review.as_mut() {
        review_ui(ui, t, review, &mut out);
        return out;
    }
    if let Some(snippet) = &state.pending {
        pending_ui(ui, t, wiki, snippet, &mut state.search, &mut out);
        return out;
    }
    match state.page.as_deref().and_then(|f| wiki.page(f)) {
        Some(page) => page_ui(ui, t, wiki, page, &mut out),
        None => list_ui(ui, t, wiki, &mut state.search, here, &mut out),
    }
    ui.separator();
    assistant_ui(ui, t, state, here, &mut out);
    out
}

fn icons_image(name: &str, t: &Tokens) -> egui::Image<'static> {
    crate::icons::image(name, 14.0, t.icon)
}

fn setup(ui: &mut egui::Ui, t: &Tokens, here: &Here, out: &mut Vec<Action>) {
    ui.label(tl!("Your wiki is a folder of Markdown pages, one per topic, with snippets from your PDFs and links between pages. The AI assistant reads it when it answers, and proposes what to add; you choose what is kept."));
    ui.add_space(8.0);
    if here.drive && ui.button(tl!("Keep it on Google Drive")).clicked() {
        out.push(Action::UseDrive);
    }
    if ui.button(tl!("Choose a folder…")).clicked() {
        out.push(Action::ChooseFolder);
    }
    if ui.button(tl!("Keep it on this computer")).clicked() {
        out.push(Action::UseLocal);
    }
    ui.add_space(4.0);
    ui.label(egui::RichText::new(tl!("Other editors, such as Obsidian, can open the same folder.")).small().color(t.text_faint));
}

fn list_ui(ui: &mut egui::Ui, t: &Tokens, wiki: &Wiki, search: &mut String, here: &Here, out: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.add(widgets::line(search).hint_text(tl!("Find a page")).desired_width(ui.available_width() - 4.0));
    });
    if here.can_add && ui.button(tl!("Add the selection to a page")).clicked() {
        out.push(Action::AddSelection);
    }
    if wiki.pages.is_empty() {
        ui.label(
            egui::RichText::new(tl!("No pages yet. Add a selection, or ask the assistant to start from the page you are reading."))
                .color(t.text_faint),
        );
        return;
    }
    let needle = search.trim().to_lowercase();
    let shown: Vec<_> = wiki
        .pages
        .iter()
        .filter(|p| needle.is_empty() || p.title.to_lowercase().contains(&needle) || p.text.to_lowercase().contains(&needle))
        .take(LISTED)
        .collect();
    egui::ScrollArea::vertical().id_salt("wiki-pages").max_height(ui.available_height() * 0.6).auto_shrink([false, true]).show(ui, |ui| {
        for p in shown {
            let snippets = p.snippets().len();
            let label = if snippets > 0 { format!("{}  ·  {snippets}", p.title) } else { p.title.clone() };
            if widgets::ghost_button_sized(ui, "file-text", &label, 26.0).clicked() {
                out.push(Action::Open(p.title.clone()));
            }
        }
    });
}

fn pending_ui(ui: &mut egui::Ui, t: &Tokens, wiki: &Wiki, snippet: &Snippet, search: &mut String, out: &mut Vec<Action>) {
    ui.label(egui::RichText::new(tl!("Add the selection to a page")).font(theme::semibold(13.0)));
    ui.label(egui::RichText::new(format!("“{}”", pdfcraft_ai::text::prefix(&snippet.quote, 280))).italics().color(t.text_muted));
    let label = ui.label(tl!("Page"));
    ui.add(widgets::line(search).hint_text(tl!("Find a page, or type a new one's title")).desired_width(f32::INFINITY)).labelled_by(label.id);
    let needle = search.trim().to_lowercase();
    let mut matching = 0usize;
    egui::ScrollArea::vertical().id_salt("wiki-pick").max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
        for p in wiki.pages.iter().filter(|p| needle.is_empty() || p.title.to_lowercase().contains(&needle)).take(LISTED) {
            matching = matching.saturating_add(1);
            if widgets::ghost_button_sized(ui, "file-text", &p.title, 26.0).clicked() {
                out.push(Action::AddPendingTo(p.title.clone()));
            }
        }
    });
    let exact = wiki.find(search.trim()).is_some();
    ui.horizontal(|ui| {
        if !needle.is_empty() && !exact && ui.button(crate::i18n::fmt(tl!("New page “{title}”"), &[("title", search.trim())])).clicked() {
            out.push(Action::AddPendingToNew(search.trim().to_owned()));
        }
        if ui.button(tl!("Cancel")).clicked() {
            out.push(Action::CancelPending);
        }
    });
    if matching == 0 && needle.is_empty() {
        ui.label(egui::RichText::new(tl!("Type the new page's title.")).small().color(t.text_faint));
    }
}

fn page_ui(ui: &mut egui::Ui, t: &Tokens, wiki: &Wiki, page: &pdfcraft_wiki::Page, out: &mut Vec<Action>) {
    if ui.small_button(tl!("< All pages")).clicked() {
        out.push(Action::Back);
    }
    ui.label(egui::RichText::new(&page.title).font(theme::semibold(16.0)));
    if page.skipped {
        ui.label(egui::RichText::new(tl!("This page is too large to show here.")).color(t.text_muted));
        return;
    }
    egui::ScrollArea::vertical().id_salt(("wiki-page", &page.file)).max_height(ui.available_height() * 0.6).auto_shrink([false, true]).show(
        ui,
        |ui| {
            markdown(ui, t, &page.text, out);
            let back = wiki.backlinks(&page.title);
            if !back.is_empty() {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(tl!("Linked from")).font(theme::semibold(12.5)).color(t.text_muted));
                ui.horizontal_wrapped(|ui| {
                    for p in back.into_iter().take(50) {
                        if ui.link(&p.title).clicked() {
                            out.push(Action::Open(p.title.clone()));
                        }
                    }
                });
            }
        },
    );
}

/// The page's Markdown: headings, paragraphs, lists, code, and snippets with their sources;
/// `[[links]]` and sources are clickable.
fn markdown(ui: &mut egui::Ui, t: &Tokens, text: &str, out: &mut Vec<Action>) {
    let mut quote: Vec<&str> = Vec::new();
    let mut fenced = false;
    let mut title_seen = false;
    for line in text.lines().take(5_000) {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            ui.label(egui::RichText::new(line).monospace().small());
            continue;
        }
        if trimmed.starts_with('>') {
            quote.push(line);
            continue;
        }
        if !quote.is_empty() {
            quote_ui(ui, t, &quote, out);
            quote.clear();
        }
        if let Some(h) = line.strip_prefix("# ") {
            // The title is shown above.
            if !title_seen {
                title_seen = true;
                continue;
            }
            ui.label(egui::RichText::new(h).font(theme::semibold(15.0)));
        } else if let Some(h) = line.strip_prefix("## ").or_else(|| line.strip_prefix("### ")) {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(h).font(theme::semibold(13.5)));
        } else if let Some(item) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            ui.horizontal_wrapped(|ui| {
                ui.label("•");
                inline(ui, item, out);
            });
        } else if line.trim().is_empty() {
            ui.add_space(4.0);
        } else if !line.trim_start().starts_with("<!--") {
            ui.horizontal_wrapped(|ui| inline(ui, line, out));
        }
    }
    if !quote.is_empty() {
        quote_ui(ui, t, &quote, out);
    }
}

/// A block quote: a snippet with its source when its last line links to one.
fn quote_ui(ui: &mut egui::Ui, t: &Tokens, lines: &[&str], out: &mut Vec<Action>) {
    let joined = lines.join("\n");
    let snippet = pdfcraft_wiki::snippets(&joined).into_iter().next();
    egui::Frame::new().fill(t.accent_soft).corner_radius(4).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        match snippet {
            Some(s) => {
                ui.label(egui::RichText::new(&s.quote).italics());
                let label =
                    crate::i18n::fmt(tl!("{title}, page {n}"), &[("title", &s.source.title), ("n", &(u64::from(s.source.page) + 1).to_string())]);
                if ui.link(label).on_hover_text(tl!("Open the document at this page")).clicked() {
                    out.push(Action::OpenSource(s.source));
                }
            }
            None => {
                let text: Vec<&str> = lines.iter().map(|l| l.trim_start().trim_start_matches('>').trim_start()).collect();
                ui.label(egui::RichText::new(text.join(" ")).italics());
            }
        }
    });
}

/// A line's text with its `[[links]]` as links; Markdown links show their text.
fn inline(ui: &mut egui::Ui, line: &str, out: &mut Vec<Action>) {
    let mut rest = line;
    while let Some(open) = rest.find("[[") {
        let (before, after) = rest.split_at(open);
        let after = after.get(2..).unwrap_or_default();
        let Some(close) = after.find("]]") else { break };
        if !before.is_empty() {
            ui.label(plain(before));
        }
        let inner = after.get(..close).unwrap_or_default();
        let target = inner.split('|').next().unwrap_or_default().split('#').next().unwrap_or_default().trim().to_owned();
        let shown = inner.split('|').nth(1).unwrap_or(&target).trim().to_owned();
        if !target.is_empty() && ui.link(shown).clicked() {
            out.push(Action::Open(target));
        }
        rest = after.get(close.saturating_add(2)..).unwrap_or_default();
    }
    if !rest.is_empty() {
        ui.label(plain(rest));
    }
}

/// Text without Markdown's emphasis marks, and links as their text.
fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let (before, after) = rest.split_at(open);
        out.push_str(before);
        match after.find("](").zip(after.find(')')) {
            Some((mid, end)) if mid < end => {
                out.push_str(after.get(1..mid).unwrap_or_default());
                rest = after.get(end.saturating_add(1)..).unwrap_or_default();
            }
            _ => {
                out.push('[');
                rest = after.get(1..).unwrap_or_default();
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "").replace("__", "")
}

fn review_ui(ui: &mut egui::Ui, t: &Tokens, review: &mut Review, out: &mut Vec<Action>) {
    ui.label(egui::RichText::new(tl!("The assistant proposes")).font(theme::semibold(13.5)));
    if !review.edit.summary.trim().is_empty() {
        ui.label(egui::RichText::new(review.edit.summary.trim()).color(t.text_muted));
    }
    egui::ScrollArea::vertical().id_salt("wiki-review").max_height(ui.available_height() * 0.7).auto_shrink([false, true]).show(ui, |ui| {
        for (p, keep) in review.edit.proposal.changes.iter().zip(review.keep.iter_mut()) {
            ui.add(widgets::checkbox(keep, describe(&p.change)));
            if !p.reason.is_empty() {
                ui.label(egui::RichText::new(&p.reason).small().color(t.text_faint));
            }
            ui.add_space(2.0);
        }
    });
    let kept = review.keep.iter().filter(|k| **k).count();
    ui.horizontal(|ui| {
        let label = crate::i18n::fmt(tl!("Apply {n}"), &[("n", &kept.to_string())]);
        if ui.add_enabled(kept > 0, egui::Button::new(label)).clicked() {
            out.push(Action::Apply);
        }
        if ui.button(tl!("Discard")).clicked() {
            out.push(Action::Discard);
        }
    });
}

/// One proposed change, in a line or two.
fn describe(change: &Change) -> String {
    let short = |s: &str| pdfcraft_ai::text::prefix(&s.split_whitespace().collect::<Vec<_>>().join(" "), 160).to_owned();
    match change {
        Change::NewPage { title, text } => crate::i18n::fmt(tl!("New page “{page}”: {text}"), &[("page", title), ("text", &short(text))]),
        Change::AddSnippet { page, snippet, .. } => crate::i18n::fmt(
            tl!("On “{page}”, quote {source}: “{quote}”"),
            &[
                ("page", page),
                (
                    "source",
                    &crate::i18n::fmt(
                        tl!("{title}, page {n}"),
                        &[("title", &snippet.source.title), ("n", &(u64::from(snippet.source.page) + 1).to_string())],
                    ),
                ),
                ("quote", &short(&snippet.quote)),
            ],
        ),
        Change::AddLink { page, to, .. } => crate::i18n::fmt(tl!("Link “{page}” to “{to}”"), &[("page", page), ("to", to)]),
        Change::AddText { page, text } => crate::i18n::fmt(tl!("On “{page}”, add: {text}"), &[("page", page), ("text", &short(text))]),
        Change::Replace { page, old, new } => {
            crate::i18n::fmt(tl!("On “{page}”, replace “{old}” with “{new}”"), &[("page", page), ("old", &short(old)), ("new", &short(new))])
        }
    }
}

fn assistant_ui(ui: &mut egui::Ui, t: &Tokens, state: &mut WikiState, here: &Here, out: &mut Vec<Action>) {
    if state.running.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(tl!("The assistant is working on the wiki…"));
            if ui.small_button(tl!("Stop")).clicked() {
                out.push(Action::Stop);
            }
        });
        return;
    }
    let label = ui.label(egui::RichText::new(tl!("Ask the assistant to update the wiki")).small().color(t.text_muted));
    let field = ui.add(widgets::block(&mut state.request).desired_rows(2).desired_width(f32::INFINITY)).labelled_by(label.id);
    let enter = field.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(!state.request.trim().is_empty(), egui::Button::new(tl!("Propose changes"))).clicked() || enter {
            out.push(Action::Propose);
        }
        if ui.add_enabled(here.has_doc, egui::Button::new(tl!("From this page"))).clicked() {
            out.push(Action::FromPage);
        }
        if ui.button(tl!("Check the wiki")).clicked() {
            out.push(Action::Check);
        }
        if !state.undo.is_empty() && ui.button(tl!("Undo last changes")).clicked() {
            out.push(Action::Undo);
        }
    });
}
