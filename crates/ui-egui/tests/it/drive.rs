//! Google Drive for desktop's folders, against a folder laid out as Drive for desktop keeps them.
//!
//! - File ▸ Save to Google Drive… opens in My Drive, or in the document's own Drive folder. It goes
//!   into folders and makes new ones, and saves the document there as Save As would. It remembers
//!   the folder across a restart, asks before replacing a file, refuses names that could leave the
//!   folder, and says so when Drive isn't there or can't be read.
//! - File ▸ Open from Google Drive… (also on Home) lists folders and PDFs, opens the PDF clicked,
//!   and remembers its folder.
//! - The Google Drive panel shows the folder the document is in and follows the document in
//!   front. Its PDFs open in tabs, and it makes folders and renames files, never one open in
//!   PdfCraft. Save in this folder… opens the Save dialog there.

use std::path::{Path, PathBuf};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::{Dialog, PdfCraftApp, RightPanel};

use crate::support;

const FIXTURE: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] >> endobj
trailer << /Root 1 0 R >>
%%EOF";

/// A home folder with two Drive accounts, the second one's My Drive named in Italian.
fn home(name: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("pdfcraft-ui-drive-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let cloud = home.join("Library/CloudStorage");
    std::fs::create_dir_all(cloud.join("GoogleDrive-a@example.com/Il mio Drive/Papers")).unwrap();
    std::fs::create_dir_all(cloud.join("GoogleDrive-a@example.com/Il mio Drive/Archive")).unwrap();
    std::fs::create_dir_all(cloud.join("GoogleDrive-a@example.com/Altri computer")).unwrap();
    std::fs::create_dir_all(cloud.join("GoogleDrive-b@example.com/My Drive")).unwrap();
    home
}

fn my_drive(home: &Path) -> PathBuf {
    home.join("Library/CloudStorage/GoogleDrive-a@example.com/Il mio Drive")
}

fn app(home: &Path, persisted: Option<&str>) -> Harness<'static, PdfCraftApp> {
    let home = home.to_path_buf();
    let persisted = persisted.map(str::to_owned);
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_eframe(move |_| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        if let Some(p) = &persisted {
            app.restore(p);
        }
        app.drive.home = Some(home.clone());
        app.open_bytes("notes.pdf", None, FIXTURE.to_vec()).unwrap();
        app
    });
    h.run_steps(2);
    h
}

fn open_dialog(h: &mut Harness<'static, PdfCraftApp>) {
    h.state_mut().run_command("file.save_to_drive");
    let shown = support::step_until(h, |h| h.query_by_label("Loading…").is_none() && h.query_by_label("Save to Google Drive").is_some());
    assert!(shown, "the dialog lists its folder");
    h.run_steps(2);
}

#[test]
fn a_document_is_saved_into_a_drive_folder_and_the_folder_is_remembered() {
    let home = home("save");
    let mut h = app(&home, None);
    open_dialog(&mut h);
    assert_eq!(h.state().dialog, Some(Dialog::SaveToDrive));
    // It opened in the first account's My Drive, folders sorted.
    h.get_by_label("Il mio Drive");
    let archive = h.get_by_label("Archive").rect().min.y;
    assert!(archive < h.get_by_label("Papers").rect().min.y);
    h.get_by_label("Papers").click();
    assert!(support::step_until(&mut h, |h| h.query_by_label("This folder is empty").is_some()), "inside Papers");
    // A new folder, made where we are and gone into.
    h.get_by_label("New folder").click();
    h.run_steps(2);
    let field = h.get_by_role_and_label(Role::TextInput, "Folder name");
    field.focus();
    field.type_text("Week 1");
    h.run_steps(1);
    h.get_by_label("Create").click();
    assert!(support::step_until(&mut h, |h| h.query_by_label("Week 1").is_some() && h.query_by_label("This folder is empty").is_some()));
    let week = my_drive(&home).join("Papers/Week 1");
    assert!(week.is_dir());
    h.get_by_label("Save").click();
    h.run_steps(3);
    let saved = week.join("notes.pdf");
    assert!(std::fs::read(&saved).unwrap().starts_with(b"%PDF"), "written");
    assert_eq!(h.state().dialog, None);
    assert_eq!(h.state().drive.last.as_deref(), Some(week.as_path()));
    let s = h.state();
    assert_eq!(s.session.get(s.views[0].id).unwrap().path.as_deref(), saved.to_str(), "the document's file is now the Drive copy");

    // After a restart it opens where it last saved; the same name asks before replacing.
    let persisted = h.state().persist();
    drop(h);
    let mut h = app(&home, Some(&persisted));
    open_dialog(&mut h);
    h.get_by_label("Week 1");
    h.get_by_label("Save").click();
    h.run_steps(2);
    assert!(h.query_by_label_contains("is already in this folder. Replace it?").is_some());
    assert_eq!(h.state().dialog, Some(Dialog::SaveToDrive), "nothing written yet");
    h.get_by_label("Replace").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn names_that_could_leave_the_folder_are_refused() {
    let home = home("names");
    let mut h = app(&home, None);
    open_dialog(&mut h);
    h.get_by_label("Papers").click();
    assert!(support::step_until(&mut h, |h| h.query_by_label("This folder is empty").is_some()));
    h.get_by_role_and_label(Role::TextInput, "Name").focus();
    h.run_steps(1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_steps(1);
    h.get_by_role_and_label(Role::TextInput, "Name").type_text("../escape");
    h.run_steps(2);
    h.get_by_label("A name can't contain /.");
    h.get_by_label("Save").click();
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::SaveToDrive), "Save does nothing");
    assert!(!my_drive(&home).join("escape.pdf").exists() && !my_drive(&home).join("Papers/../escape.pdf").exists());
    // Nor a folder named so.
    h.get_by_label("New folder").click();
    h.run_steps(2);
    let field = h.get_by_role_and_label(Role::TextInput, "Folder name");
    field.focus();
    field.type_text("..");
    h.run_steps(1);
    h.get_by_label("Create").click();
    h.run_steps(2);
    h.get_by_label("That name can't be used for a file in Drive.");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn without_drive_or_permission_the_dialog_says_what_to_do() {
    let empty = std::env::temp_dir().join(format!("pdfcraft-ui-drive-{}-none", std::process::id()));
    let _ = std::fs::remove_dir_all(&empty);
    std::fs::create_dir_all(&empty).unwrap();
    let mut h = app(&empty, None);
    h.state_mut().run_command("file.save_to_drive");
    h.run_steps(3);
    assert!(h.query_by_label_contains("Google Drive for desktop isn't set up on this computer").is_some());
    h.get_by_label("Close").click();
    h.run_steps(2);
    assert_eq!(h.state().dialog, None);
    let _ = std::fs::remove_dir_all(&empty);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let home = home("closed");
        let mine = my_drive(&home);
        std::fs::set_permissions(&mine, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read_dir(&mine).is_err() {
            let mut h = app(&home, None);
            h.state_mut().run_command("file.save_to_drive");
            // The account's places list, then My Drive can't be read.
            let said = support::step_until(&mut h, |h| h.query_by_label_contains("PdfCraft isn't allowed to open this Drive yet").is_some());
            assert!(said, "the reason is shown");
            h.get_by_label("Try again");
            h.get_by_label("Other location…");
        }
        std::fs::set_permissions(&mine, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&home);
    }
}

/// Two PDFs and a text file in My Drive › Papers, and a PDF in My Drive › Archive.
fn papers(home: &Path) -> PathBuf {
    let papers = my_drive(home).join("Papers");
    for f in ["a.pdf", "b.pdf"] {
        std::fs::write(papers.join(f), FIXTURE).unwrap();
    }
    std::fs::write(papers.join("notes.txt"), b"not a PDF").unwrap();
    std::fs::write(my_drive(home).join("Archive/x.pdf"), FIXTURE).unwrap();
    papers
}

/// The file of the document in front.
fn front(h: &Harness<'static, PdfCraftApp>) -> Option<PathBuf> {
    let s = h.state();
    let view = s.views.get(s.active?)?;
    s.session.get(view.id)?.path.as_deref().map(PathBuf::from)
}

fn open(h: &mut Harness<'static, PdfCraftApp>, path: &Path) {
    let tabs = h.state().views.len();
    h.state_mut().open_path(path.to_str().unwrap());
    assert!(support::step_until(h, |h| h.state().views.len() == tabs + 1), "{} opened", path.display());
    h.run_steps(2);
}

fn retype(h: &mut Harness<'static, PdfCraftApp>, field: &str, text: &str) {
    h.get_by_role_and_label(Role::TextInput, field).focus();
    h.run_steps(1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_steps(1);
    h.get_by_role_and_label(Role::TextInput, field).type_text(text);
    h.run_steps(2);
}

#[test]
fn a_pdf_is_opened_from_a_drive_folder_and_the_folder_is_remembered() {
    let home = home("open");
    let papers = papers(&home);
    let mut h = app(&home, None);
    h.state_mut().run_command("file.open_from_drive");
    assert!(support::step_until(&mut h, |h| h.query_by_label("Open from Google Drive").is_some() && h.query_by_label("Papers").is_some()));
    assert_eq!(h.state().dialog, Some(Dialog::OpenFromDrive));
    h.get_by_label("Papers").click();
    assert!(support::step_until(&mut h, |h| h.query_by_role_and_label(Role::Button, "b.pdf").is_some()), "inside Papers, its PDFs listed");
    assert!(h.query_by_role_and_label(Role::Button, "a.pdf").is_some());
    assert!(h.query_by_role_and_label(Role::Button, "notes.txt").is_none(), "only PDFs");
    h.get_by_role_and_label(Role::Button, "b.pdf").click();
    assert!(support::step_until(&mut h, |h| h.state().views.len() == 2), "opened in a tab of its own");
    assert_eq!(h.state().dialog, None);
    assert_eq!(front(&h), Some(papers.join("b.pdf")));
    assert_eq!(h.state().drive.last.as_deref(), Some(papers.as_path()));

    // Next time it opens in Papers, and a file open already comes to the front instead.
    h.state_mut().active = Some(0);
    h.state_mut().run_command("file.open_from_drive");
    assert!(support::step_until(&mut h, |h| h.query_by_role_and_label(Role::Button, "b.pdf").is_some()), "opened in the folder last used");
    h.get_by_role_and_label(Role::Button, "b.pdf").click();
    h.run_steps(3);
    assert_eq!(h.state().views.len(), 2, "no second tab");
    assert_eq!(front(&h), Some(papers.join("b.pdf")));

    // Other location… hands over to the system's dialog; Cancel just closes.
    h.state_mut().run_command("file.open_from_drive");
    assert!(support::step_until(&mut h, |h| h.query_by_label("Open from Google Drive").is_some()));
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert_eq!(h.state().dialog, None);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn home_opens_from_google_drive() {
    let home = home("home");
    let home_dir = home.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_eframe(move |_| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.drive.home = Some(home_dir.clone());
        app
    });
    h.run_steps(2);
    h.get_by_label("Open from Google Drive").click();
    assert!(support::step_until(&mut h, |h| h.state().dialog == Some(Dialog::OpenFromDrive) && h.query_by_label("Papers").is_some()));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_drive_panel_works_in_the_folder_of_the_document_in_front() {
    let home = home("panel");
    let papers = papers(&home);
    let mut h = app(&home, None);
    open(&mut h, &papers.join("a.pdf"));
    h.state_mut().choose_right_panel(Some(RightPanel::Drive));
    assert!(support::step_until(&mut h, |h| h.query_by_role_and_label(Role::Button, "b.pdf").is_some()), "the panel lists the document's folder");
    h.get_by_label("In Google Drive");
    // Two accounts: the account comes first.
    h.get_by_label("a@example.com · Il mio Drive › Papers");
    assert!(h.query_by_role_and_label(Role::Button, "notes.txt").is_none(), "only PDFs");

    // Another PDF of the folder opens in a tab of its own.
    h.get_by_role_and_label(Role::Button, "b.pdf").click();
    assert!(support::step_until(&mut h, |h| h.state().views.len() == 3));
    assert_eq!(front(&h), Some(papers.join("b.pdf")));

    // A document from another folder brings the panel there; back on the first, back to Papers.
    open(&mut h, &my_drive(&home).join("Archive/x.pdf"));
    assert!(
        support::step_until(&mut h, |h| h.query_by_role_and_label(Role::Button, "x.pdf").is_some()
            && h.query_by_role_and_label(Role::Button, "b.pdf").is_none()),
        "the panel follows"
    );
    h.state_mut().active = Some(1);
    assert!(support::step_until(&mut h, |h| h.query_by_role_and_label(Role::Button, "b.pdf").is_some()), "back in Papers");

    // A new folder, made where the panel is and gone into.
    h.get_by_label("New folder").click();
    h.run_steps(2);
    retype(&mut h, "Folder name", "Drafts");
    h.get_by_label("Create").click();
    assert!(support::step_until(&mut h, |h| h.query_by_label("This folder is empty").is_some()), "inside Drafts");
    assert!(papers.join("Drafts").is_dir());
    h.get_by_label("Papers").click();
    assert!(support::step_until(&mut h, |h| h.query_by_role_and_label(Role::Button, "a.pdf").is_some() && h.query_by_label("Drafts").is_some()));

    // A file open in PdfCraft isn't renamed: its tab would lose it.
    h.get_by_role_and_label(Role::Button, "More for a.pdf").click();
    h.run_steps(2);
    h.get_by_label("Rename…").click();
    h.run_steps(2);
    retype(&mut h, "New name", "Renamed");
    h.get_by_label("Rename").click();
    h.run_steps(2);
    h.get_by_label("a.pdf is open in PdfCraft. Close it first, then rename it.");
    assert!(papers.join("a.pdf").is_file() && !papers.join("Renamed.pdf").exists());
    h.get_by_label("Cancel").click();
    h.run_steps(2);

    // One that isn't open is, keeping .pdf; a file Drive brings down shows on Refresh.
    std::fs::write(papers.join("c.pdf"), FIXTURE).unwrap();
    h.get_by_label("Refresh").click();
    assert!(support::step_until(&mut h, |h| h.query_by_role_and_label(Role::Button, "c.pdf").is_some()));
    h.get_by_role_and_label(Role::Button, "More for c.pdf").click();
    h.run_steps(2);
    h.get_by_label("Rename…").click();
    h.run_steps(2);
    retype(&mut h, "New name", "Chapter 3");
    h.get_by_label("Rename").click();
    assert!(support::step_until(&mut h, |h| h.query_by_role_and_label(Role::Button, "Chapter 3.pdf").is_some()), "listed under its new name");
    assert!(papers.join("Chapter 3.pdf").is_file() && !papers.join("c.pdf").exists());

    // Save in this folder… opens the Save dialog in it: the document's own name is taken there.
    h.get_by_label("Save in this folder…").click();
    let shown = support::step_until(&mut h, |h| {
        h.state().dialog == Some(Dialog::SaveToDrive) && h.query_by_label("Save to Google Drive").is_some() && h.query_by_label("Loading…").is_none()
    });
    assert!(shown, "the Save dialog lists its folder");
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(2);
    assert!(h.query_by_label_contains("a.pdf is already in this folder. Replace it?").is_some(), "the dialog is in Papers");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn save_to_drive_starts_in_the_documents_own_drive_folder() {
    let home = home("own");
    let papers = papers(&home);
    let mut h = app(&home, None);
    open(&mut h, &papers.join("a.pdf"));
    h.state_mut().run_command("file.save_to_drive");
    assert!(support::step_until(&mut h, |h| h.query_by_role_and_label(Role::Button, "b.pdf").is_some()), "in Papers, not My Drive");
    assert!(h.query_by_label("Archive").is_none());
    // A PDF already there gives its name, and Save then asks before replacing it.
    h.get_by_role_and_label(Role::Button, "b.pdf").click();
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(2);
    assert!(h.query_by_label_contains("b.pdf is already in this folder. Replace it?").is_some());
    assert_eq!(std::fs::read(papers.join("b.pdf")).unwrap(), FIXTURE, "nothing written yet");
    let _ = std::fs::remove_dir_all(&home);
}
