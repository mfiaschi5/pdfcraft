//! File ▸ Save to Google Drive…, against a folder laid out as Google Drive for desktop keeps
//! them: it opens in My Drive, goes into folders and makes new ones, saves the document there as
//! Save As would, remembers the folder across a restart, asks before replacing a file, refuses
//! names that could leave the folder, and says so when Drive isn't there or can't be read.

use std::path::{Path, PathBuf};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::{Dialog, PdfCraftApp};

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
    assert!(support::step_until(&mut h, |h| h.query_by_label("No folders here").is_some()), "inside Papers");
    // A new folder, made where we are and gone into.
    h.get_by_label("New folder").click();
    h.run_steps(2);
    let field = h.get_by_role_and_label(Role::TextInput, "Folder name");
    field.focus();
    field.type_text("Week 1");
    h.run_steps(1);
    h.get_by_label("Create").click();
    assert!(support::step_until(&mut h, |h| h.query_by_label("Week 1").is_some() && h.query_by_label("No folders here").is_some()));
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
    assert!(support::step_until(&mut h, |h| h.query_by_label("No folders here").is_some()));
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
