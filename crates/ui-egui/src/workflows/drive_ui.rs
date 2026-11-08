//! Google Drive for desktop's folders, worked in from PdfCraft (`pdfcraft_platform::drive`).
//! PdfCraft never talks to Google: it opens and saves in the folders Drive for desktop keeps in
//! sync, and Drive uploads what changes. A PDF opened from them is an ordinary file, so Save
//! writes the synced copy, and the file watcher notices a newer copy that Drive brings down.
//!
//! - File ▸ Open from Google Drive… and File ▸ Save to Google Drive…: dialogs that go through the
//!   accounts' folders.
//! - The Google Drive panel beside the document shows the folder the document is in, or the one
//!   last used. Its folders and PDFs open with a click. New folders, Rename, Reveal in Finder and
//!   Pin to Home are there too, and Save in this folder… opens the Save dialog in it.
//!
//! Folders are listed off the UI thread, since macOS may first ask the user whether PdfCraft can
//! read them, and an online-only folder can be slow. Desktop only: the web app has no Drive for
//! desktop.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use egui::{CornerRadius, Rect, Sense, vec2};
use pdfcraft_engine::DocId;
use pdfcraft_platform::drive::{self, Account, Entry, ListError, NameError};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons, widgets};

/// While the panel shows, its folder is listed again at most this often, so what Drive brings
/// down shows up.
const RELIST: Duration = Duration::from_secs(5);
/// A folder's or PDF's row in a listing.
const ROW_HEIGHT: f32 = 40.0;

/// Google Drive in the app: where to look for its folders, the folder last used, the dialogs and
/// the panel.
#[derive(Default)]
pub struct DriveState {
    /// The home folder Drive's folders are under: the user's, set by the desktop app; tests point
    /// it elsewhere, and leave it unset for no Drive.
    pub home: Option<PathBuf>,
    /// The Drive folder last opened from or saved to, kept in the settings.
    pub last: Option<PathBuf>,
    /// File ▸ Save to Google Drive…, while it is open.
    pub(crate) draft: Option<Draft>,
    /// File ▸ Open from Google Drive…, while it is open.
    pub(crate) open: Option<Browser>,
    /// The Google Drive panel, once it has been shown.
    pub(crate) panel: Option<Panel>,
}

type Listed = Result<Vec<Entry>, ListError>;

/// A Drive folder being looked at: the accounts, which one, the folder, and what it holds.
pub(crate) struct Browser {
    accounts: Vec<Account>,
    account: usize,
    /// The folder shown, inside the account's folder.
    folder: PathBuf,
    /// What the folder held when last listed; `None` until the first listing of it is in.
    entries: Option<Listed>,
    /// A listing on its way, and the folder it is for.
    loading: Option<(PathBuf, mpsc::Receiver<Listed>)>,
    /// Go into My Drive once the account's places are listed.
    enter_my_drive: bool,
    /// When the last listing came in.
    listed: Option<Instant>,
}

impl Browser {
    /// Look in `start` when it is a folder in one of `accounts`, else in the first account's My
    /// Drive.
    fn new(accounts: Vec<Account>, start: Option<PathBuf>, ctx: Option<&egui::Context>) -> Self {
        let start = start.and_then(|s| drive::place(&s, &accounts).map(|p| (p.account, s))).filter(|(_, s)| s.is_dir());
        let account = start.as_ref().map_or(0, |(a, _)| *a);
        let mut b = Self { accounts, account, folder: PathBuf::new(), entries: None, loading: None, enter_my_drive: start.is_none(), listed: None };
        if let Some(folder) = start.map(|(_, s)| s).or_else(|| b.root().map(Path::to_path_buf)) {
            b.go(folder, ctx);
        }
        b
    }

    fn root(&self) -> Option<&Path> {
        self.accounts.get(self.account).map(|a| a.root.as_path())
    }

    /// At the account's folder, which holds its places (My Drive …), not folders of one's own.
    fn at_root(&self) -> bool {
        self.root() == Some(self.folder.as_path())
    }

    /// The folder is listed and is one of one's own, so something can be made or saved in it.
    fn ready(&self) -> bool {
        !self.at_root() && matches!(self.entries, Some(Ok(_)))
    }

    /// Show `folder`, listing it afresh.
    fn go(&mut self, folder: PathBuf, ctx: Option<&egui::Context>) {
        self.folder = folder;
        self.entries = None;
        self.list(ctx);
    }

    /// List the folder again, showing what it held until the new listing is in.
    fn refresh(&mut self, ctx: Option<&egui::Context>) {
        if self.loading.is_none() {
            self.list(ctx);
        }
    }

    /// [`Self::refresh`] when the folder was last listed [`RELIST`] ago or more.
    fn keep_fresh(&mut self, ctx: Option<&egui::Context>) {
        if self.listed.is_some_and(|t| t.elapsed() >= RELIST) {
            self.refresh(ctx);
        }
    }

    fn list(&mut self, ctx: Option<&egui::Context>) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.cloned();
        let path = self.folder.clone();
        let work = move || {
            let _ = tx.send(drive::entries(&path));
            if let Some(c) = &ctx {
                c.request_repaint();
            }
        };
        if std::thread::Builder::new().name("drive-list".into()).spawn(work).is_err() {
            self.loading = None;
            self.entries = Some(Err(ListError::Other(tl!("couldn't start listing the folder").into())));
            return;
        }
        self.loading = Some((self.folder.clone(), rx));
    }

    /// Take a listing that has come in.
    fn poll(&mut self, ctx: Option<&egui::Context>) {
        let Some((folder, rx)) = &self.loading else { return };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(ListError::Other(tl!("listing stopped unexpectedly").into())),
        };
        let shown = *folder == self.folder;
        self.loading = None;
        // An answer for a folder no longer shown is dropped.
        if !shown {
            return;
        }
        self.listed = Some(Instant::now());
        if self.enter_my_drive && self.at_root() {
            self.enter_my_drive = false;
            let places: Vec<String> = result.as_ref().map(|l| l.iter().filter(|e| e.folder).map(|e| e.name.clone()).collect()).unwrap_or_default();
            if let Some(mine) = drive::my_drive(&places) {
                let next = self.folder.join(mine);
                self.go(next, ctx);
                return;
            }
        }
        self.entries = Some(result);
    }
}

/// Where `path` is in Drive, in words: the account when there are several, then the folders from
/// its place (My Drive …) down, without the file itself when `file` is true.
fn where_in_drive(accounts: &[Account], place: &drive::Place, file: bool) -> String {
    let parts = if file { place.parts.split_last().map_or(&[][..], |(_, rest)| rest) } else { &place.parts[..] };
    let mut text = parts.join(" › ");
    if accounts.len() > 1
        && let Some(a) = accounts.get(place.account)
    {
        text = if text.is_empty() { a.name.clone() } else { format!("{} · {text}", a.name) };
    }
    text
}

/// The account (a choice when there are several), then where in it, each part a button back to
/// it. Returns the folder to go to.
fn nav(ui: &mut egui::Ui, t: &Tokens, b: &mut Browser) -> Option<PathBuf> {
    let mut go_to = None;
    if b.accounts.len() > 1 {
        let mut account = b.account;
        let width = ui.available_width().min(320.0);
        craft_ui::ComboBox::from_id_salt("drive-account")
            .selected_text(b.accounts.get(account).map(|a| a.name.as_str()).unwrap_or_default())
            .width(width)
            .show_ui(ui, |ui| {
                for (i, a) in b.accounts.iter().enumerate() {
                    ui.selectable_value(&mut account, i, &a.name);
                }
            });
        if account != b.account {
            b.account = account;
            b.enter_my_drive = true;
            go_to = b.root().map(Path::to_path_buf);
        }
    } else if let Some(a) = b.accounts.first() {
        ui.label(egui::RichText::new(&a.name).color(t.text_muted));
    }
    ui.add_space(6.0);
    if let Some(root) = b.root().map(Path::to_path_buf) {
        ui.horizontal_wrapped(|ui| {
            if ui.small_button(tl!("Google Drive")).clicked() {
                go_to = Some(root.clone());
            }
            let mut at = root.clone();
            for part in b.folder.strip_prefix(&root).map(|p| p.components().collect::<Vec<_>>()).unwrap_or_default() {
                at = at.join(part);
                ui.label(egui::RichText::new(">").color(t.text_faint));
                if ui.small_button(part.as_os_str().to_string_lossy()).clicked() {
                    go_to = Some(at.clone());
                }
            }
        });
    }
    go_to
}

/// What was clicked in a folder's listing.
enum Clicked {
    Folder(PathBuf),
    File(PathBuf),
    /// From a row's menu.
    Rename(PathBuf),
    Reveal(PathBuf),
    Pin(PathBuf),
}

/// How a listing shows its rows.
struct Rows<'a> {
    /// The document's own file, marked when it is listed.
    current: Option<&'a Path>,
    /// PDFs are shown faint (the Save dialog, where a click only takes the name).
    faint_files: bool,
    /// Each row has a menu: Rename…, Reveal in Finder, and Pin to Home for a folder.
    menu: bool,
    height: f32,
}

/// The folder's folders and PDFs, in a scrolling box that draws only the rows in view.
fn listing(ui: &mut egui::Ui, t: &Tokens, b: &Browser, rows: &Rows<'_>) -> Option<Clicked> {
    let mut clicked = None;
    egui::Frame::new().stroke(egui::Stroke::new(1.0, t.border)).corner_radius(t.radius).inner_margin(4.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let area =
            egui::ScrollArea::vertical().id_salt("drive-list").max_height(rows.height).min_scrolled_height(rows.height).auto_shrink([false, false]);
        match &b.entries {
            Some(Ok(list)) if !list.is_empty() => {
                area.show_rows(ui, ROW_HEIGHT, list.len(), |ui, range| {
                    for e in list.get(range).unwrap_or_default() {
                        if let Some(c) = row(ui, t, &b.folder, e, rows) {
                            clicked = Some(c);
                        }
                    }
                });
            }
            entries => {
                area.show(ui, |ui| match entries {
                    None => {
                        ui.label(egui::RichText::new(tl!("Loading…")).color(t.text_muted));
                    }
                    Some(Err(e)) => {
                        ui.label(egui::RichText::new(list_error(e)).color(ui.visuals().error_fg_color));
                        if ui.small_button(tl!("Try again")).clicked() {
                            clicked = Some(Clicked::Folder(b.folder.clone()));
                        }
                    }
                    Some(Ok(_)) => {
                        ui.label(egui::RichText::new(tl!("This folder is empty")).color(t.text_muted));
                    }
                });
            }
        }
    });
    clicked
}

/// One folder or PDF: a click goes into it or opens it.
fn row(ui: &mut egui::Ui, t: &Tokens, folder: &Path, e: &Entry, rows: &Rows<'_>) -> Option<Clicked> {
    let path = folder.join(&e.name);
    let current = !e.folder && rows.current == Some(path.as_path());
    let faint = !e.folder && rows.faint_files;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), &e.name));
    if current {
        ui.painter().rect_filled(rect, CornerRadius::same(6), t.accent_soft);
    } else if ui.rect_contains_pointer(rect) {
        ui.painter().rect_filled(rect, CornerRadius::same(6), t.hover);
    }
    let (icon, color) = if e.folder {
        ("folder", t.icon)
    } else if faint {
        ("file-text", t.text_faint)
    } else {
        ("file-text", egui::Color32::from_rgb(0xE0, 0x3E, 0x3E))
    };
    icons::paint(ui, Rect::from_center_size(egui::pos2(rect.left() + 15.0, rect.center().y), vec2(18.0, 18.0)), icon, 17.0, color);
    let mut clicked = None;
    let mut right = rect.right() - 8.0;
    if rows.menu {
        let more_rect = Rect::from_center_size(rect.right_center() - vec2(16.0, 0.0), vec2(26.0, 26.0));
        right = more_rect.left() - 4.0;
        let more = ui.interact(more_rect, ui.scope_id().with(("drive-more", &e.name)), Sense::click());
        let label = crate::i18n::fmt(tl!("More for {name}"), &[("name", &e.name)]);
        more.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
        if more.hovered() {
            ui.painter().rect_filled(more_rect, CornerRadius::same(6), t.pressed);
        }
        icons::paint(ui, more_rect.shrink(5.0), "ellipsis", 16.0, t.icon);
        egui::Popup::menu(&more).show(|ui| {
            ui.set_min_width(180.0);
            if ui.button(tl!("Rename…")).clicked() {
                clicked = Some(Clicked::Rename(path.clone()));
                ui.close();
            }
            let reveal = if cfg!(target_os = "macos") { tl!("Reveal in Finder") } else { tl!("Reveal in File Explorer") };
            if ui.button(reveal).clicked() {
                clicked = Some(Clicked::Reveal(path.clone()));
                ui.close();
            }
            if e.folder && ui.button(tl!("Pin to Home")).clicked() {
                clicked = Some(Clicked::Pin(path.clone()));
                ui.close();
            }
        });
    }
    // The name has the row's width; a PDF's age and size go on a second line under it.
    let left = rect.left() + 30.0;
    let width = (right - left).max(0.0);
    let text_color = if faint { t.text_muted } else { t.text };
    if e.folder {
        widgets::row_text(ui, egui::pos2(left, rect.center().y), e.name.as_str(), theme::medium(13.0), text_color, width);
    } else {
        let size = crate::panels::human_size(usize::try_from(e.size).unwrap_or(usize::MAX));
        let when = e.modified.map(crate::combine_ui::ago).unwrap_or_default();
        let detail = if when.is_empty() { size } else { format!("{when} · {size}") };
        widgets::row_text(ui, egui::pos2(left, rect.center().y - 8.0), e.name.as_str(), theme::medium(13.0), text_color, width);
        widgets::row_text(ui, egui::pos2(left, rect.center().y + 9.0), detail, theme::regular(11.0), t.text_faint, width);
    }
    let hover = if current { crate::i18n::fmt(tl!("{name} is the document shown"), &[("name", &e.name)]) } else { e.name.clone() };
    if clicked.is_none() && resp.on_hover_text(hover).clicked() {
        clicked = Some(if e.folder { Clicked::Folder(path) } else { Clicked::File(path) });
    }
    clicked
}

/// Naming a new folder in the browser's folder: the field and its buttons, or the button that
/// starts it. Returns the folder made.
fn new_folder_row(ui: &mut egui::Ui, b: &Browser, naming: &mut Option<String>, error: &mut Option<String>) -> Option<PathBuf> {
    let mut made = None;
    ui.horizontal_wrapped(|ui| match naming {
        Some(name) => {
            let label = ui.label(tl!("Folder name"));
            let field = ui.add(widgets::line(name).desired_width(180.0)).labelled_by(label.id);
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button(tl!("Create")).clicked() || enter {
                match drive::new_folder(&b.folder, name) {
                    Ok(path) => made = Some(path),
                    Err(e) => *error = Some(name_error(&e, false)),
                }
            }
            if ui.button(tl!("Cancel")).clicked() {
                *naming = None;
                *error = None;
            }
        }
        None => {
            if ui.add_enabled(b.ready(), egui::Button::new(tl!("New folder"))).clicked() {
                *naming = Some(String::new());
            }
        }
    });
    if made.is_some() {
        *naming = None;
        *error = None;
    }
    made
}

fn name_error(e: &NameError, file: bool) -> String {
    match e {
        NameError::Empty => tl!("Type a name.").to_string(),
        NameError::Character(c) => crate::i18n::fmt(tl!("A name can't contain {c}."), &[("c", &c.escape_debug().to_string())]),
        NameError::Reserved => tl!("That name can't be used for a file in Drive.").to_string(),
        NameError::TooLong => tl!("That name is too long.").to_string(),
        NameError::Exists if file => tl!("There is already something with that name here.").to_string(),
        NameError::Exists => tl!("There is already a folder with that name.").to_string(),
        NameError::Other(e) if file => crate::i18n::fmt(tl!("Couldn't rename it: {e}"), &[("e", e)]),
        NameError::Other(e) => crate::i18n::fmt(tl!("Couldn't make the folder: {e}"), &[("e", e)]),
    }
}

fn list_error(e: &ListError) -> String {
    match e {
        ListError::NotAllowed => {
            tl!("PdfCraft isn't allowed to open this Drive yet. Allow it when macOS asks, or in System Settings > Privacy & Security > Files and Folders; or use Other location… to choose the folder in the system's dialog.").to_string()
        }
        ListError::Missing => tl!("This folder isn't there any more.").to_string(),
        ListError::Other(e) => crate::i18n::fmt(tl!("Couldn't open this folder: {e}"), &[("e", e)]),
    }
}

/// Google Drive for desktop isn't on this computer: what to do, in a dialog or the panel.
fn not_set_up(ui: &mut egui::Ui) {
    ui.label(tl!("Google Drive for desktop isn't set up on this computer. Install it and sign in to your Google account; its folders then show here, and what you save in them is uploaded."));
}

// ---- File ▸ Save to Google Drive… ----

pub(crate) struct Draft {
    doc: DocId,
    browser: Browser,
    name: String,
    /// A folder being named, before it is made.
    new_folder: Option<String>,
    /// The file is already there and the user was asked whether to replace it.
    replace: bool,
    error: Option<String>,
}

/// What the Save dialog asks for once it has closed.
pub(crate) enum Chosen {
    Save(DocId, PathBuf),
    /// The system's own Save dialog, opened in this folder.
    Elsewhere(DocId, PathBuf, String),
}

// ---- File ▸ Open from Google Drive… ----

/// What the Open dialog asks for once it has closed.
pub(crate) enum Opened {
    File(PathBuf),
    /// The system's own Open dialog, opened in this folder.
    Elsewhere(PathBuf),
}

// ---- The Google Drive panel ----

pub(crate) struct Panel {
    browser: Browser,
    /// The document the panel last followed: when another one comes to the front, the panel goes
    /// to that one's folder if it is in Drive.
    doc: Option<DocId>,
    new_folder: Option<String>,
    renaming: Option<Renaming>,
    error: Option<String>,
}

struct Renaming {
    path: PathBuf,
    name: String,
    /// Give the field the keyboard on its first frame.
    focus: bool,
}

impl Panel {
    /// Leave what was being named or renamed in the folder left.
    fn forget_edits(&mut self) {
        self.new_folder = None;
        self.renaming = None;
        self.error = None;
    }
}

/// What the panel is shown beside.
pub(crate) struct Here {
    pub doc: DocId,
    /// The document's file, if it has one.
    pub path: Option<PathBuf>,
    /// The files open in tabs: one of them, or a folder holding one, isn't renamed.
    pub open: Vec<PathBuf>,
}

/// What the panel asks the app to do.
pub(crate) enum Action {
    Open(PathBuf),
    Reveal(PathBuf),
    Pin(PathBuf),
    /// File ▸ Save to Google Drive… for this document, opened in this folder.
    SaveIn(DocId, PathBuf),
}

impl PdfCraftApp {
    /// File ▸ Save to Google Drive…: open the dialog in the document's own Drive folder, else in
    /// the one last used, else in My Drive.
    pub(crate) fn open_save_to_drive(&mut self) {
        self.open_save_to_drive_in(None);
    }

    /// The Save dialog, opened in `folder` when that is a Drive folder.
    fn open_save_to_drive_in(&mut self, folder: Option<PathBuf>) {
        let Some((_, doc)) = self.active_ids() else { return };
        let Some((name, path)) = self.session.get(doc).map(|d| (d.name.clone(), d.path.clone())) else { return };
        let accounts = drive::accounts(self.drive.home.as_deref());
        let own = path.as_deref().and_then(|p| Path::new(p).parent()).map(Path::to_path_buf).filter(|f| drive::place(f, &accounts).is_some());
        let start = folder.or(own).or_else(|| self.drive.last.clone());
        let browser = Browser::new(accounts, start, self.ctx.as_ref());
        self.drive.draft = Some(Draft {
            doc,
            browser,
            name: drive::pdf_name(&name).unwrap_or_else(|_| "Document.pdf".to_owned()),
            new_folder: None,
            replace: false,
            error: None,
        });
        self.dialog = Some(crate::Dialog::SaveToDrive);
    }

    /// Save where the dialog chose, and keep the folder for next time.
    pub(crate) fn finish_save_to_drive(&mut self, chosen: Chosen) {
        match chosen {
            Chosen::Save(doc, dest) => {
                let folder = dest.parent().map(Path::to_path_buf);
                self.save_doc_as(doc, &dest, move |app| app.drive.last = folder);
            }
            Chosen::Elsewhere(doc, folder, name) => {
                let dialog = rfd::AsyncFileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(name).set_directory(&folder);
                self.ask_one(crate::pickers::Ask::Save(dialog), None, move |app, dest| {
                    let folder = dest.parent().map(Path::to_path_buf);
                    app.save_doc_as(doc, &dest, move |app| app.drive.last = folder);
                });
            }
        }
    }

    /// File ▸ Open from Google Drive…: open the dialog in the Drive folder last used, else in My
    /// Drive.
    pub(crate) fn open_from_drive(&mut self) {
        let accounts = drive::accounts(self.drive.home.as_deref());
        self.drive.open = Some(Browser::new(accounts, self.drive.last.clone(), self.ctx.as_ref()));
        self.dialog = Some(crate::Dialog::OpenFromDrive);
    }

    /// Open what the dialog chose, and keep the folder for next time.
    pub(crate) fn finish_open_from_drive(&mut self, opened: Opened) {
        match opened {
            Opened::File(path) => self.open_from_drive_folder(&path),
            Opened::Elsewhere(folder) => self.pick(crate::pickers::PickFor::Open, Self::open_picker().set_directory(&folder), true),
        }
    }

    /// Open a PDF from a Drive folder (or bring its tab to the front), and keep the folder.
    fn open_from_drive_folder(&mut self, path: &Path) {
        self.drive.last = path.parent().map(Path::to_path_buf);
        self.open_recent(&path.to_string_lossy());
    }

    /// Before the Google Drive panel is drawn: make it on first use (in the document's Drive
    /// folder, else the one last used, else My Drive), follow the document in front, and keep the
    /// folder's listing fresh.
    pub(crate) fn ensure_drive_panel(&mut self) {
        let ctx = self.ctx.clone();
        let doc = self.active_ids().map(|(_, id)| id);
        let folder =
            doc.and_then(|id| self.session.get(id)).and_then(|d| d.path.as_deref()).and_then(|p| Path::new(p).parent()).map(Path::to_path_buf);
        // Drawn again by then even with nothing else going on, so the listing stays fresh.
        if let Some(c) = &ctx {
            c.request_repaint_after(RELIST);
        }
        match &mut self.drive.panel {
            Some(p) if p.doc == doc => p.browser.keep_fresh(ctx.as_ref()),
            Some(p) => {
                p.doc = doc;
                if let Some(folder) = folder.filter(|f| drive::place(f, &p.browser.accounts).is_some() && *f != p.browser.folder) {
                    p.forget_edits();
                    p.browser = Browser::new(std::mem::take(&mut p.browser.accounts), Some(folder), ctx.as_ref());
                }
            }
            None => {
                let accounts = drive::accounts(self.drive.home.as_deref());
                let start = folder.filter(|f| drive::place(f, &accounts).is_some()).or_else(|| self.drive.last.clone());
                let browser = Browser::new(accounts, start, ctx.as_ref());
                self.drive.panel = Some(Panel { browser, doc, new_folder: None, renaming: None, error: None });
            }
        }
    }

    /// The Google Drive panel's place beside the document: what it shows it is beside.
    pub(crate) fn drive_here(&self, doc: DocId) -> Here {
        let open = self.views.iter().filter_map(|v| self.session.get(v.id)).filter_map(|d| d.path.as_deref()).map(PathBuf::from).collect();
        Here { doc, path: self.session.get(doc).and_then(|d| d.path.as_deref()).map(PathBuf::from), open }
    }
}

/// File ▸ Save to Google Drive…. Returns what to do once it closes, and whether it should close.
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp) -> (Option<Chosen>, bool) {
    let ctx = app.ctx.clone();
    let Some(d) = app.drive.draft.as_mut() else { return (None, true) };
    d.browser.poll(ctx.as_ref());
    let t = Tokens::get(ui.ctx());
    ui.set_width(520.0);
    ui.label(egui::RichText::new(tl!("Save to Google Drive")).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    if d.browser.accounts.is_empty() {
        not_set_up(ui);
        return (None, close_button(ui));
    }
    let mut go_to = nav(ui, &t, &mut d.browser);
    ui.add_space(4.0);
    match listing(ui, &t, &d.browser, &Rows { current: None, faint_files: true, menu: false, height: 200.0 }) {
        Some(Clicked::Folder(f)) => go_to = Some(f),
        // A PDF already there: its name, to save over it (Save then asks).
        Some(Clicked::File(f)) => {
            if let Some(name) = f.file_name() {
                d.name = name.to_string_lossy().into_owned();
                d.replace = false;
            }
        }
        _ => {}
    }
    if let Some(made) = new_folder_row(ui, &d.browser, &mut d.new_folder, &mut d.error) {
        go_to = Some(made);
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let label = ui.label(tl!("Name"));
        if ui.add(widgets::line(&mut d.name).desired_width(f32::INFINITY)).labelled_by(label.id).changed() {
            d.replace = false;
        }
    });
    let file = drive::pdf_name(&d.name);
    let message = d.error.clone().or_else(|| file.as_ref().err().map(|e| name_error(e, true)));
    if let Some(m) = &message {
        ui.label(egui::RichText::new(m).small().color(ui.visuals().error_fg_color));
    }
    let dest = file.ok().map(|f| d.browser.folder.join(f));
    let exists = dest.as_ref().is_some_and(|p| p.exists());
    if d.replace && exists {
        let name = dest.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        ui.label(crate::i18n::fmt(tl!("{name} is already in this folder. Replace it?"), &[("name", &name)]));
    }
    ui.add_space(12.0);
    let mut chosen = None;
    let mut close = false;
    let ready = d.browser.ready() && d.error.is_none();
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let label = if d.replace && exists { tl!("Replace") } else { tl!("Save") };
        if ui.add_enabled_ui(ready && dest.is_some(), |ui| widgets::pill_button(ui, label, true)).inner.clicked()
            && let Some(dest) = dest.clone()
        {
            if exists && !d.replace {
                d.replace = true;
            } else {
                chosen = Some(Chosen::Save(d.doc, dest));
                close = true;
            }
        }
        if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
            close = true;
        }
        if widgets::pill_button(ui, tl!("Other location…"), false).clicked() {
            chosen = Some(Chosen::Elsewhere(d.doc, d.browser.folder.clone(), d.name.clone()));
            close = true;
        }
    });
    if let Some(folder) = go_to {
        d.new_folder = None;
        d.replace = false;
        d.error = None;
        d.browser.go(folder, ctx.as_ref());
    }
    (chosen, close)
}

/// File ▸ Open from Google Drive…. Returns what to do once it closes, and whether it should close.
pub(crate) fn open_body(ui: &mut egui::Ui, app: &mut PdfCraftApp) -> (Option<Opened>, bool) {
    let ctx = app.ctx.clone();
    let Some(b) = app.drive.open.as_mut() else { return (None, true) };
    b.poll(ctx.as_ref());
    let t = Tokens::get(ui.ctx());
    ui.set_width(520.0);
    ui.label(egui::RichText::new(tl!("Open from Google Drive")).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    if b.accounts.is_empty() {
        not_set_up(ui);
        return (None, close_button(ui));
    }
    let mut go_to = nav(ui, &t, b);
    ui.add_space(4.0);
    let mut chosen = None;
    let mut close = false;
    match listing(ui, &t, b, &Rows { current: None, faint_files: false, menu: false, height: 280.0 }) {
        Some(Clicked::Folder(f)) => go_to = Some(f),
        Some(Clicked::File(f)) => {
            chosen = Some(Opened::File(f));
            close = true;
        }
        _ => {}
    }
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(tl!("Click a PDF to open it. Saving it writes the copy in Drive, and Google Drive for desktop uploads it."))
            .small()
            .color(t.text_muted),
    );
    ui.add_space(12.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
            close = true;
        }
        if widgets::pill_button(ui, tl!("Other location…"), false).clicked() {
            chosen = Some(Opened::Elsewhere(b.folder.clone()));
            close = true;
        }
    });
    if let Some(folder) = go_to {
        b.go(folder, ctx.as_ref());
    }
    (chosen, close)
}

fn close_button(ui: &mut egui::Ui) -> bool {
    ui.add_space(12.0);
    let mut close = false;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        close = widgets::pill_button(ui, tl!("Close"), true).clicked();
    });
    close
}

/// The Google Drive panel's contents. Returns what the app is to do.
pub(crate) fn panel(ui: &mut egui::Ui, t: &Tokens, state: &mut DriveState, here: &Here, ctx: Option<&egui::Context>) -> Vec<Action> {
    let mut actions = Vec::new();
    let Some(p) = state.panel.as_mut() else { return actions };
    p.browser.poll(ctx);
    if p.browser.accounts.is_empty() {
        not_set_up(ui);
        return actions;
    }
    let mut go_to = None;
    // Where the document is.
    let place = here.path.as_deref().and_then(|path| drive::place(path, &p.browser.accounts));
    let own_folder = here.path.as_deref().and_then(Path::parent).filter(|_| place.is_some()).map(Path::to_path_buf);
    egui::Frame::NONE.fill(t.card).stroke(egui::Stroke::new(1.0, t.border)).corner_radius(t.radius).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        match &place {
            Some(place) => {
                ui.horizontal(|ui| {
                    let (icon, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                    icons::paint(ui, icon, "cloud", 17.0, t.accent_text);
                    ui.label(egui::RichText::new(tl!("In Google Drive")).font(theme::semibold(13.5)));
                });
                widgets::label_ltr(ui, where_in_drive(&p.browser.accounts, place, true), theme::regular(12.0), t.text_muted);
                let key = crate::commands::command_shortcut_label(ui.ctx(), "file.save");
                ui.label(
                    egui::RichText::new(crate::i18n::fmt(
                        tl!("Save ({key}) updates it there, and Google Drive for desktop uploads the change."),
                        &[("key", &key)],
                    ))
                    .small()
                    .color(t.text_faint),
                );
                if let Some(own) = own_folder.as_ref().filter(|f| **f != p.browser.folder)
                    && ui.small_button(tl!("Go to its folder")).clicked()
                {
                    go_to = Some(own.clone());
                }
            }
            None => {
                ui.label(egui::RichText::new(tl!("This document isn't in Google Drive.")).font(theme::semibold(13.5)));
                ui.label(
                    egui::RichText::new(tl!("Open a PDF below to work on it in Drive, or save this one in a folder.")).small().color(t.text_faint),
                );
            }
        }
    });
    ui.add_space(8.0);
    if let Some(f) = nav(ui, t, &mut p.browser) {
        go_to = Some(f);
    }
    ui.add_space(4.0);
    let height = (ui.available_height() - 150.0).clamp(160.0, 900.0);
    match listing(ui, t, &p.browser, &Rows { current: here.path.as_deref(), faint_files: false, menu: true, height }) {
        Some(Clicked::Folder(f)) => go_to = Some(f),
        Some(Clicked::File(f)) => actions.push(Action::Open(f)),
        Some(Clicked::Rename(path)) => {
            let shown = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            // A PDF's name without its extension, which the rename keeps.
            let name =
                if path.is_file() && drive::is_pdf(&shown) { shown.get(..shown.len().saturating_sub(4)).unwrap_or(&shown).to_owned() } else { shown };
            p.new_folder = None;
            p.error = None;
            p.renaming = Some(Renaming { path, name, focus: true });
        }
        Some(Clicked::Reveal(path)) => actions.push(Action::Reveal(path)),
        Some(Clicked::Pin(path)) => actions.push(Action::Pin(path)),
        None => {}
    }
    // Renaming a file or folder of this one.
    let mut renamed = false;
    if let Some(r) = p.renaming.as_mut() {
        let shown = r.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        ui.add_space(4.0);
        ui.label(crate::i18n::fmt(tl!("Rename {name}"), &[("name", &shown)]));
        ui.horizontal_wrapped(|ui| {
            let label = ui.label(tl!("New name"));
            let field = ui.add(widgets::line(&mut r.name).desired_width(170.0)).labelled_by(label.id);
            if std::mem::take(&mut r.focus) {
                field.request_focus();
            }
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button(tl!("Rename")).clicked() || enter {
                // A document open in PdfCraft would lose its file: it is closed first.
                if here.open.iter().any(|o| o.starts_with(&r.path)) {
                    p.error = Some(crate::i18n::fmt(tl!("{name} is open in PdfCraft. Close it first, then rename it."), &[("name", &shown)]));
                } else {
                    match drive::rename(&r.path, &r.name) {
                        Ok(_) => renamed = true,
                        Err(e) => p.error = Some(name_error(&e, true)),
                    }
                }
            }
            if ui.button(tl!("Cancel")).clicked() {
                renamed = true;
            }
        });
    }
    if renamed {
        p.renaming = None;
        p.error = None;
        p.browser.refresh(ctx);
    }
    if p.renaming.is_none()
        && let Some(made) = new_folder_row(ui, &p.browser, &mut p.new_folder, &mut p.error)
    {
        go_to = Some(made);
    }
    if let Some(m) = &p.error {
        ui.label(egui::RichText::new(m).small().color(ui.visuals().error_fg_color));
    }
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(p.browser.ready(), egui::Button::new(tl!("Save in this folder…"))).clicked() {
            actions.push(Action::SaveIn(here.doc, p.browser.folder.clone()));
        }
        if ui.button(tl!("Refresh")).clicked() {
            p.browser.accounts = drive::accounts(state.home.as_deref());
            p.browser.refresh(ctx);
        }
    });
    if let Some(folder) = go_to {
        p.forget_edits();
        p.browser.go(folder, ctx);
    }
    actions
}

/// Do what the panel asked.
pub(crate) fn run_panel(app: &mut PdfCraftApp, actions: Vec<Action>) {
    for action in actions {
        match action {
            Action::Open(path) => app.open_from_drive_folder(&path),
            Action::Reveal(path) => {
                if let Err(e) = crate::tab_context::reveal(&path) {
                    app.notify_error(e);
                }
            }
            Action::Pin(path) => {
                app.pin_folder(&path.to_string_lossy());
            }
            Action::SaveIn(doc, folder) => {
                if app.active_ids().is_some_and(|(_, id)| id == doc) {
                    app.open_save_to_drive_in(Some(folder));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn where_a_file_is_reads_from_its_place_down_with_the_account_when_there_are_several() {
        let a = Account { name: "a@example.com".into(), root: PathBuf::from("/a") };
        let b = Account { name: "b@example.com".into(), root: PathBuf::from("/b") };
        let place = drive::Place { account: 1, parts: vec!["My Drive".into(), "Papers".into(), "x.pdf".into()] };
        assert_eq!(where_in_drive(&[a.clone(), b.clone()], &place, true), "b@example.com · My Drive › Papers");
        assert_eq!(where_in_drive(std::slice::from_ref(&b), &drive::Place { account: 0, ..place.clone() }, true), "My Drive › Papers");
        assert_eq!(where_in_drive(std::slice::from_ref(&b), &drive::Place { account: 0, ..place.clone() }, false), "My Drive › Papers › x.pdf");
        assert_eq!(where_in_drive(&[a, b], &drive::Place { account: 0, parts: Vec::new() }, true), "a@example.com");
    }
}
