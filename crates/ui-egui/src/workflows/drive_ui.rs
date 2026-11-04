//! File ▸ Save to Google Drive…: a folder of Google Drive for desktop, chosen here, and the
//! document saved into it as Save As would (`pdfcraft_platform::drive`). Drive uploads it; PdfCraft
//! never talks to Google. Folders are listed off the UI thread, since macOS may first ask the user
//! whether PdfCraft can read them. Desktop only: the web app has no Drive for desktop.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use pdfcraft_engine::DocId;
use pdfcraft_platform::drive::{self, Account, ListError, NameError};

use crate::{PdfCraftApp, widgets};

/// Saving to Drive: where to look for its folders, the folder last saved to, and the dialog.
#[derive(Default)]
pub struct DriveState {
    /// The home folder Drive's folders are under: the user's, set by the desktop app; tests point
    /// it elsewhere, and leave it unset for no Drive.
    pub home: Option<PathBuf>,
    /// The folder last saved to, kept in the settings.
    pub last: Option<PathBuf>,
    pub(crate) draft: Option<Draft>,
}

pub(crate) struct Draft {
    doc: DocId,
    accounts: Vec<Account>,
    account: usize,
    /// The folder shown, inside the account's root.
    folder: PathBuf,
    listing: Listing,
    /// Go into My Drive once the account's places are listed.
    enter_my_drive: bool,
    name: String,
    /// A folder being named, before it is made.
    new_folder: Option<String>,
    /// The file is already there and the user was asked whether to replace it.
    replace: bool,
    error: Option<String>,
}

enum Listing {
    Loading(PathBuf, mpsc::Receiver<Result<Vec<String>, ListError>>),
    Ready(Result<Vec<String>, ListError>),
}

/// What the dialog asks for once it has closed.
pub(crate) enum Chosen {
    Save(DocId, PathBuf),
    /// The system's own Save dialog, opened in this folder.
    Elsewhere(DocId, PathBuf, String),
}

impl Draft {
    fn root(&self) -> Option<&Path> {
        self.accounts.get(self.account).map(|a| a.root.as_path())
    }

    fn go(&mut self, folder: PathBuf, ctx: Option<&egui::Context>) {
        self.folder = folder.clone();
        self.new_folder = None;
        self.replace = false;
        self.error = None;
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.cloned();
        let path = folder.clone();
        let work = move || {
            let _ = tx.send(drive::folders(&path));
            if let Some(c) = &ctx {
                c.request_repaint();
            }
        };
        if std::thread::Builder::new().name("drive-list".into()).spawn(work).is_err() {
            self.listing = Listing::Ready(Err(ListError::Other(tl!("couldn't start listing the folder").into())));
            return;
        }
        self.listing = Listing::Loading(folder, rx);
    }

    fn poll(&mut self, ctx: Option<&egui::Context>) {
        let Listing::Loading(folder, rx) = &self.listing else { return };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(ListError::Other(tl!("listing stopped unexpectedly").into())),
        };
        // An answer for a folder no longer shown is dropped.
        if *folder != self.folder {
            return;
        }
        let at_root = self.root() == Some(self.folder.as_path());
        if self.enter_my_drive && at_root {
            self.enter_my_drive = false;
            if let Some(mine) = result.as_ref().ok().and_then(|places| drive::my_drive(places)) {
                let next = self.folder.join(mine);
                self.go(next, ctx);
                return;
            }
        }
        self.listing = Listing::Ready(result);
    }
}

impl PdfCraftApp {
    /// File ▸ Save to Google Drive…: open the dialog in the folder last saved to, else in My Drive.
    pub(crate) fn open_save_to_drive(&mut self) {
        let Some((_, doc)) = self.active_ids() else { return };
        let Some(name) = self.session.get(doc).map(|d| d.name.clone()) else { return };
        let home = self.drive.home.clone();
        let accounts = drive::accounts(home.as_deref());
        // The folder last saved to, while it is still there in one of the accounts.
        let last = self.drive.last.clone().filter(|l| l.is_dir());
        let found = last.as_ref().and_then(|l| accounts.iter().position(|a| l.starts_with(&a.root)));
        let account = found.unwrap_or(0);
        let start = found.and(last);
        let root = accounts.get(account).map(|a| a.root.clone());
        let mut draft = Draft {
            doc,
            accounts,
            account,
            folder: PathBuf::new(),
            listing: Listing::Ready(Ok(Vec::new())),
            enter_my_drive: start.is_none(),
            name: drive::pdf_name(&name).unwrap_or_else(|_| "Document.pdf".to_owned()),
            new_folder: None,
            replace: false,
            error: None,
        };
        if let Some(folder) = start.or(root) {
            draft.go(folder, self.ctx.as_ref());
        }
        self.drive.draft = Some(draft);
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
}

fn name_error(e: &NameError) -> String {
    match e {
        NameError::Empty => tl!("Type a name.").to_string(),
        NameError::Character(c) => crate::i18n::fmt(tl!("A name can't contain {c}."), &[("c", &c.escape_debug().to_string())]),
        NameError::Reserved => tl!("That name can't be used for a file in Drive.").to_string(),
        NameError::TooLong => tl!("That name is too long.").to_string(),
        NameError::Exists => tl!("There is already a folder with that name.").to_string(),
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

/// The dialog. Returns what to do once it closes, and whether it should close.
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp) -> (Option<Chosen>, bool) {
    let ctx = app.ctx.clone();
    let Some(d) = app.drive.draft.as_mut() else { return (None, true) };
    d.poll(ctx.as_ref());
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.set_width(520.0);
    ui.label(egui::RichText::new(tl!("Save to Google Drive")).font(crate::theme::semibold(18.0)));
    ui.add_space(8.0);
    if d.accounts.is_empty() {
        ui.label(tl!("Google Drive for desktop isn't set up on this computer. Install it and sign in to your Google account; its folders then show here, and what you save in them is uploaded."));
        ui.add_space(12.0);
        let mut close = false;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            close = widgets::pill_button(ui, tl!("Close"), true).clicked();
        });
        return (None, close);
    }

    // The account, then where in it.
    let mut go_to: Option<PathBuf> = None;
    if d.accounts.len() > 1 {
        let mut account = d.account;
        craft_ui::ComboBox::from_id_salt("drive-account")
            .selected_text(d.accounts.get(account).map(|a| a.name.as_str()).unwrap_or_default())
            .width(320.0)
            .show_ui(ui, |ui| {
                for (i, a) in d.accounts.iter().enumerate() {
                    ui.selectable_value(&mut account, i, &a.name);
                }
            });
        if account != d.account {
            d.account = account;
            d.enter_my_drive = true;
            go_to = d.root().map(Path::to_path_buf);
        }
    } else if let Some(a) = d.accounts.first() {
        ui.label(egui::RichText::new(&a.name).color(t.text_muted));
    }
    ui.add_space(6.0);
    if let Some(root) = d.root().map(Path::to_path_buf) {
        ui.horizontal_wrapped(|ui| {
            if ui.small_button(tl!("Google Drive")).clicked() {
                go_to = Some(root.clone());
            }
            let mut at = root.clone();
            for part in d.folder.strip_prefix(&root).map(|p| p.components().collect::<Vec<_>>()).unwrap_or_default() {
                at = at.join(part);
                ui.label(egui::RichText::new(">").color(t.text_faint));
                if ui.small_button(part.as_os_str().to_string_lossy()).clicked() {
                    go_to = Some(at.clone());
                }
            }
        });
    }
    ui.add_space(4.0);
    egui::Frame::new().stroke(egui::Stroke::new(1.0, t.border)).corner_radius(t.radius).inner_margin(6.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().max_height(200.0).min_scrolled_height(200.0).auto_shrink([false, false]).show(ui, |ui| match &d.listing {
            Listing::Loading(..) => {
                ui.label(egui::RichText::new(tl!("Loading…")).color(t.text_muted));
            }
            Listing::Ready(Err(e)) => {
                ui.label(egui::RichText::new(list_error(e)).color(ui.visuals().error_fg_color));
                if ui.small_button(tl!("Try again")).clicked() {
                    go_to = Some(d.folder.clone());
                }
            }
            Listing::Ready(Ok(folders)) if folders.is_empty() => {
                ui.label(egui::RichText::new(tl!("No folders here")).color(t.text_muted));
            }
            Listing::Ready(Ok(folders)) => {
                for f in folders {
                    if widgets::ghost_button_sized(ui, "folder", f, 26.0).clicked() {
                        go_to = Some(d.folder.join(f));
                    }
                }
            }
        });
    });
    // A new folder here.
    let at_root = d.root() == Some(d.folder.as_path());
    ui.horizontal(|ui| match &mut d.new_folder {
        Some(name) => {
            let label = ui.label(tl!("Folder name"));
            let field = ui.add(widgets::line(name).desired_width(200.0)).labelled_by(label.id);
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button(tl!("Create")).clicked() || enter {
                match drive::new_folder(&d.folder, name) {
                    Ok(made) => go_to = Some(made),
                    Err(e) => d.error = Some(name_error(&e)),
                }
            }
            if ui.button(tl!("Cancel")).clicked() {
                d.new_folder = None;
                d.error = None;
            }
        }
        None => {
            // The account's root holds its places (My Drive …), not folders of one's own.
            let can = !at_root && matches!(d.listing, Listing::Ready(Ok(_)));
            if ui.add_enabled(can, egui::Button::new(tl!("New folder"))).clicked() {
                d.new_folder = Some(String::new());
            }
        }
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let label = ui.label(tl!("Name"));
        if ui.add(widgets::line(&mut d.name).desired_width(f32::INFINITY)).labelled_by(label.id).changed() {
            d.replace = false;
        }
    });
    let file = drive::pdf_name(&d.name);
    let message = d.error.clone().or_else(|| file.as_ref().err().map(name_error));
    if let Some(m) = &message {
        ui.label(egui::RichText::new(m).small().color(ui.visuals().error_fg_color));
    }
    let dest = file.ok().map(|f| d.folder.join(f));
    let exists = dest.as_ref().is_some_and(|p| p.exists());
    if d.replace && exists {
        let name = dest.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        ui.label(crate::i18n::fmt(tl!("{name} is already in this folder. Replace it?"), &[("name", &name)]));
    }
    ui.add_space(12.0);
    let mut chosen = None;
    let mut close = false;
    let ready = !at_root && matches!(d.listing, Listing::Ready(Ok(_))) && d.error.is_none();
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
            chosen = Some(Chosen::Elsewhere(d.doc, d.folder.clone(), d.name.clone()));
            close = true;
        }
    });
    if let Some(folder) = go_to {
        d.go(folder, ctx.as_ref());
    }
    (chosen, close)
}
