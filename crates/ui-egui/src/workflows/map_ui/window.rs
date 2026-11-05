//! The concept map in a window of its own, to put on another screen while the document stays in
//! the main window: the graph filling it (`graph_view`, with previews rendered from the document
//! being read), the map's toolbar above, and the node picked beside it, to connect and follow.
//! Clicking a node there goes to its passage in the main window. Where a platform can't open
//! another window, egui shows it as a window inside the main one.

use egui::{ViewportBuilder, ViewportId};

use super::graph_view::{self, Doc, Host};
use super::{Action, Here, Picked, focus_ui, heading, hint, run};
use crate::PdfCraftApp;
use crate::theme::Tokens;

impl PdfCraftApp {
    /// ⌘⇧E: the keyboard to the map's graph, in its window when that is open, else beside the
    /// document.
    pub(crate) fn focus_map_graph(&mut self) {
        if !self.maps.available() {
            self.notify_tr("Concept maps need the desktop app");
            return;
        }
        if self.maps.window {
            self.maps.window_view.focus_now = true;
        } else {
            self.ensure_map();
            self.choose_right_panel(Some(crate::RightPanel::Map));
            self.maps.panel_view.focus_now = true;
        }
    }

    /// Open or close the map's own window.
    pub(crate) fn set_map_window(&mut self, open: bool) {
        if open && !self.maps.available() {
            self.notify_tr("Concept maps need the desktop app");
            return;
        }
        if open {
            self.ensure_map();
            self.maps.window_view.focus_now = true;
        }
        self.maps.window = open;
    }
}

/// The map's window, while it is open. Drawn after the main window's content, so what it asks
/// for (a passage, a connection) happens in the same frame.
pub(crate) fn show(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if !app.maps.window {
        return;
    }
    let title = crate::i18n::fmt(tl!("Concept map: {map}"), &[("map", &app.maps.graph.title)]);
    let builder = ViewportBuilder::default().with_title(title).with_inner_size([1000.0, 720.0]).with_min_inner_size([480.0, 360.0]);
    // The document being read, beside which the passages are marked and from which previews render.
    let active = app.active;
    let here = active
        .and_then(|i| app.views.get(i))
        .map(|v| Here {
            sha256: app.session.document_sha256(v.id).map(|h| h.to_hex()),
            path: app.session.get(v.id).and_then(|d| d.path.clone()),
            page: v.current,
            can_add: false,
        })
        .unwrap_or(Here { sha256: None, path: None, page: 0, can_add: false });
    let mut doc = match (active, here.sha256.clone()) {
        (Some(i), Some(sha256)) => {
            let session = &app.session;
            app.views.get_mut(i).and_then(|view| session.get(view.id).map(|d| Doc { sha256, info: &d.info, view }))
        }
        _ => None,
    };
    let maps = &mut app.maps;
    let mut actions: Vec<Action> = Vec::new();
    let mut close = false;
    ctx.show_viewport_immediate(ViewportId::from_hash_of("pdfcraft-map-window"), builder, |ui, _class| {
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            close = true;
        }
        let t = Tokens::get(ui.ctx());
        egui::Panel::right("map-window-side")
            .resizable(false)
            .exact_size(290.0)
            .frame(egui::Frame::NONE.fill(t.panel).inner_margin(egui::Margin::symmetric(12, 10)))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    heading(ui, &maps.graph.title);
                    ui.label(
                        egui::RichText::new(crate::i18n::fmt(tl!("{n} nodes"), &[("n", &maps.graph.nodes().len().to_string())]))
                            .small()
                            .color(t.text_faint),
                    );
                    ui.separator();
                    match maps.picked {
                        Some(Picked::Node(id)) if maps.graph.node(id).is_some() => focus_ui(ui, &t, maps, id, &mut actions),
                        _ => hint(ui, &t, tl!("Pick a node to see its connections and connect it to others.")),
                    }
                });
            });
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.panel).inner_margin(egui::Margin::symmetric(12, 10))).show(ui, |ui| {
            graph_view::show(ui, &t, maps, Host::Window, &here, doc.take(), &mut actions);
        });
    });
    if close {
        app.maps.window = false;
    }
    for action in actions {
        run(app, action);
    }
}
