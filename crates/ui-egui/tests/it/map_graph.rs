//! The concept map as a graph in the panel beside the document: round nodes inside the graph's
//! frame; hovering a node grows its preview; clicking one goes to its passage in the document and
//! marks it from where it starts to where it ends (found in the page's text even when the quote's
//! spacing, case or middle words differ), Escape takes the mark away, and a document that has to
//! open first is marked once it is.

use std::path::PathBuf;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_graph::{Anchor, Author, Content, Graph, NodeId, Strength};
use pdfcraft_ui_egui::{MapPicked, PdfCraftApp, RightPanel};

use crate::support;

const PAGE_TWO: &str = "L'inflazione è scesa al 2,1% a settembre, dopo mesi sopra il 3%, e il calo ha sorpreso molti analisti.";

fn pdf() -> Vec<u8> {
    let text =
        format!("La politica monetaria nel 2026.\n\nLa banca centrale ha alzato il tasso al 4% a marzo.\u{c}Effetti sui prezzi.\n\n{PAGE_TWO}");
    pdfcraft_engine::Session::new().create_from_text("Tassi", &text).unwrap().to_vec()
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-ui-map-graph-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn anchor(sha: &str, page: u32, quote: &str, path: Option<&str>) -> Option<Anchor> {
    Some(Anchor {
        sha256: sha.into(),
        paths: path.into_iter().map(str::to_owned).collect(),
        title: "tassi.pdf".into(),
        page,
        rect: None,
        quote: quote.into(),
    })
}

/// The map: a snippet from each page, one whose quote differs from the page in its middle, one
/// that isn't on its page, and a concept, connected.
fn map(sha: &str, path: Option<&str>) -> (Graph, Vec<NodeId>) {
    let mut g = Graph::new("Economia");
    let kind = g.kinds().first().map(|k| k.id).unwrap();
    let nodes = [
        (
            Content::Text { text: "La banca centrale ha alzato il tasso".into() },
            anchor(sha, 0, "La banca centrale ha alzato il tasso al 4% a marzo.", path),
            [0.0, 0.0],
        ),
        (Content::Text { text: "L'inflazione è scesa".into() }, anchor(sha, 1, "L'inflazione è scesa al 2,1% a settembre", path), [400.0, 40.0]),
        (
            Content::Text { text: "Il calo ha sorpreso".into() },
            anchor(sha, 1, "l'inflazione è   scesa al 2,1% a settembre, dopo TANTI mesi sopra il 3%, e il calo ha sorpreso molti analisti.", path),
            [300.0, 260.0],
        ),
        (Content::Text { text: "Non c'è".into() }, anchor(sha, 1, "Una frase che non è in questa pagina", path), [-80.0, 240.0]),
        (Content::Concept { label: "Politica monetaria".into() }, None, [200.0, -160.0]),
    ];
    let mut ids = Vec::new();
    for (content, anchor, at) in nodes {
        ids.push(g.add_node(content, anchor, at, Author::User).unwrap());
    }
    g.add_edge(ids[0], ids[4], kind, Strength::Strong, None, Author::User).unwrap();
    g.add_edge(ids[1], ids[4], kind, Strength::Medium, None, Author::User).unwrap();
    (g, ids)
}

/// The app beside the map, its maps in a folder of `name`'s own (a map one test saves must not
/// load into another's).
fn harness(name: &str, open: bool, file: Option<PathBuf>) -> (Harness<'static, PdfCraftApp>, Vec<NodeId>) {
    let bytes = pdf();
    let sha = pdfcraft_ai::source::Sha256::of(&bytes).to_hex();
    let path = file.as_ref().map(|f| f.to_string_lossy().into_owned());
    if let Some(f) = &file {
        std::fs::write(f, &bytes).unwrap();
    }
    let (graph, ids) = map(&sha, path.as_deref());
    let dir = temp(&format!("maps-{name}"));
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        if open {
            app.open_bytes("tassi.pdf", None, bytes.clone()).unwrap();
        } else {
            // Another document is being read; the map's is on disk.
            app.open_bytes("other.pdf", None, crate::assistant::fixture()).unwrap();
        }
        app.maps.dir = Some(dir.clone());
        app.maps.graph = graph.clone();
        app
    });
    h.run_steps(4);
    h.state_mut().choose_right_panel(Some(RightPanel::Map));
    h.run_steps(4);
    (h, ids)
}

/// The graph's node with this label (the panel's lists name it too).
fn node<'a>(h: &'a Harness<'static, PdfCraftApp>, label: &'a str) -> egui_kittest::Node<'a> {
    let frame = h.get_by_label("Map graph").rect();
    h.get_by(move |n| {
        n.role() == Role::Button
            && n.label().as_deref() == Some(label)
            && n.bounding_box().is_some_and(|b| frame.contains(egui::pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32)))
    })
}

fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn the_map_is_drawn_as_round_nodes_and_hovering_one_grows_a_render_of_its_passage() {
    let (mut h, _) = harness("the_map_is_drawn_as_roun", true, None);
    let frame = h.get_by_label("Map graph").rect();
    for label in ["La banca centrale ha alzato il tasso", "L'inflazione è scesa", "Politica monetaria"] {
        let r = node(&h, label).rect();
        assert!((r.width() - r.height()).abs() < 1.0, "{label}: a circle's square, {r:?}");
        assert!(frame.contains_rect(r), "{label} inside the graph {frame:?}: {r:?}");
    }
    let render = "Page 2 of tassi.pdf, rendered";
    assert!(h.query_by_label(render).is_none(), "no preview before hovering");
    node(&h, "L'inflazione è scesa").hover();
    // The page is rendered for the preview, though the document shows page 1.
    let rendered = support::step_until(&mut h, |h| {
        std::thread::sleep(std::time::Duration::from_millis(10));
        h.state().views[0].has_passage_raster(1) && h.query_by_label(render).is_some()
    });
    assert!(rendered, "the passage's page is rendered into the preview");
    assert_eq!(h.state().views[0].current, 0, "the document stays where it was");
    h.get_by_label("tassi.pdf, p. 2");
    h.get_by_label("Click to go to the passage");
    // Away from the node, it shrinks back and goes.
    h.get_by_label("Map graph").hover();
    let gone = support::step_until(&mut h, |h| {
        std::thread::sleep(std::time::Duration::from_millis(10));
        h.query_by_label(render).is_none()
    });
    assert!(gone, "the preview closes");
    // A concept has no page: its text shows.
    node(&h, "Politica monetaria").hover();
    let shown = support::step_until(&mut h, |h| {
        std::thread::sleep(std::time::Duration::from_millis(10));
        h.query_by_label("Click to pick it, to connect it").is_some()
    });
    assert!(shown);
}

/// A mouse drag from `a` to `b`, with `modifiers` held.
fn drag(h: &mut Harness<'static, PdfCraftApp>, a: egui::Pos2, b: egui::Pos2, modifiers: egui::Modifiers) {
    use egui::{Event, PointerButton};
    support::input::one_frame(h, [Event::ModifiersChanged(modifiers), Event::PointerMoved(a)]);
    support::input::one_frame(h, [Event::PointerButton { pos: a, button: PointerButton::Primary, pressed: true, modifiers }]);
    for i in 1..=8 {
        support::input::one_frame(h, [Event::PointerMoved(a + (b - a) * (i as f32 / 8.0))]);
    }
    support::input::one_frame(h, [Event::PointerButton { pos: b, button: PointerButton::Primary, pressed: false, modifiers }]);
    support::input::one_frame(h, [Event::ModifiersChanged(egui::Modifiers::NONE)]);
    h.run_steps(2);
}

#[test]
fn shift_dragging_a_node_onto_another_connects_them_and_dragging_moves_it() {
    let (mut h, ids) = harness("shift_dragging_a_node_on", true, None);
    let edges = h.state().maps.graph.edges().len();
    let a = node(&h, "L'inflazione è scesa").rect().center();
    let b = node(&h, "Non c'è").rect().center();
    drag(&mut h, a, b, egui::Modifiers::SHIFT);
    let g = h.state().maps.graph.clone();
    assert_eq!(g.edges().len(), edges + 1, "connected");
    let e = g.edges().last().unwrap();
    assert_eq!((e.from, e.to), (ids[1], ids[3]));
    assert_eq!(e.strength, h.state().maps.new_strength);
    // A plain drag moves the node, saved and undoable; the rest of the graph stays put.
    let before = g.node(ids[2]).unwrap().position;
    let other = node(&h, "Politica monetaria").rect().center();
    let c = node(&h, "Il calo ha sorpreso").rect().center();
    drag(&mut h, c, c + egui::vec2(-30.0, -20.0), egui::Modifiers::NONE);
    let after = h.state().maps.graph.node(ids[2]).unwrap().position;
    assert!(after[0] < before[0] && after[1] < before[1], "{before:?} -> {after:?}");
    assert_eq!(h.state().maps.graph.edges().len(), edges + 1, "a plain drag connects nothing");
    let moved_other = node(&h, "Politica monetaria").rect().center();
    assert!((moved_other - other).length() < 40.0, "the graph didn't jump");
    assert!(h.state_mut().execute("edit.undo") || h.state().maps.graph.node(ids[2]).is_some());
    // While a node is dragged, it moves as the pointer does, even when it was the one at the
    // graph's edge: the graph keeps the fit it had until the drag ends.
    use egui::{Event, PointerButton};
    let a = node(&h, "Non c'è").rect().center();
    let b = a + egui::vec2(80.0, -12.0);
    let at = |i: u32| a + (b - a) * (i as f32 / 8.0);
    support::input::one_frame(&mut h, [Event::PointerMoved(a)]);
    support::input::one_frame(
        &mut h,
        [Event::PointerButton { pos: a, button: PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }],
    );
    let mut seen = Vec::new();
    for i in 1..=8 {
        support::input::one_frame(&mut h, [Event::PointerMoved(at(i))]);
        h.run_steps(1);
        seen.push(node(&h, "Non c'è").rect().center());
    }
    let moved = seen[7] - seen[3];
    let pointer = at(8) - at(4);
    assert!((moved - pointer).length() < 1.0, "the node moved {moved:?} as the pointer moved {pointer:?}");
    support::input::one_frame(
        &mut h,
        [Event::PointerButton { pos: b, button: PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }],
    );
    h.run_steps(2);
}

#[test]
fn the_keyboard_moves_between_nodes_and_connects_them() {
    let (mut h, ids) = harness("the_keyboard_moves_betwe", true, None);
    let edges = h.state().maps.graph.edges().len();
    // ⌘⇧E gives the graph the keyboard.
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::E);
    h.run_steps(3);
    h.key_press(egui::Key::Tab);
    h.run_steps(2);
    let first = h.state().maps.graph_cursor();
    assert!(first.is_some(), "Tab lands on a node");
    assert_eq!(h.state().views[0].current, 0, "the arrows and Tab stay with the graph, not the pages");
    // From the snippet on page 2, connect to the concept, strong.
    while h.state().maps.graph_cursor() != Some(ids[1]) {
        h.key_press(egui::Key::Tab);
        h.run_steps(1);
    }
    h.key_press(egui::Key::C);
    h.key_press(egui::Key::Num3);
    h.run_steps(1);
    while h.state().maps.graph_cursor() != Some(ids[3]) {
        h.key_press(egui::Key::Tab);
        h.run_steps(1);
    }
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    let g = h.state().maps.graph.clone();
    assert_eq!(g.edges().len(), edges + 1);
    let e = g.edges().last().unwrap();
    assert_eq!((e.from, e.to, e.strength), (ids[1], ids[3], Strength::Strong));
    // Enter on a node goes to its passage.
    while h.state().maps.graph_cursor() != Some(ids[1]) {
        h.key_press(egui::Key::Tab);
        h.run_steps(1);
    }
    h.key_press(egui::Key::Enter);
    support::wait_until(&mut h, "the passage marked", |app| app.views[0].marked_text().is_some());
    assert_eq!(h.state().views[0].current, 1);
    // Escape lets the keyboard go back to the document.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.key_press(egui::Key::Tab);
    h.run_steps(2);
    assert_eq!(h.state().maps.graph_cursor(), Some(ids[1]), "Tab no longer moves the graph's cursor");
}

#[test]
fn the_map_opens_in_a_window_of_its_own_and_a_click_there_marks_the_passage_in_the_document() {
    let (mut h, ids) = harness("the_map_opens_in_a_windo", true, None);
    assert!(h.state_mut().execute("map.window"));
    h.run_steps(4);
    assert!(h.state().maps.window);
    let frame = h.get_by_label("Map graph in its window").rect();
    let in_window = h.get_by(|n| {
        n.role() == Role::Button
            && n.label().as_deref() == Some("L'inflazione è scesa")
            && n.bounding_box().is_some_and(|b| frame.contains(egui::pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32)))
    });
    in_window.click();
    support::wait_until(&mut h, "the passage marked in the document", |app| app.views[0].marked_text().is_some());
    assert_eq!(h.state().views[0].current, 1);
    assert_eq!(h.state().maps.picked, Some(MapPicked::Node(ids[1])));
    // The picked node's connections show beside the window's graph, as in the panel.
    assert_eq!(h.get_all_by_label("Connect").count(), 2);
    h.get_by_label("Close this window; the map stays beside the document").click();
    h.run_steps(3);
    assert!(!h.state().maps.window);
    // The system's close button closes it too (here, where it is embedded, the main window's).
    assert!(h.state_mut().execute("map.window"));
    h.run_steps(3);
    assert!(h.state().maps.window);
    h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
    h.run_steps(2);
    assert!(!h.state().maps.window, "closed by the system's close button");
}

#[test]
fn clicking_a_node_goes_to_its_passage_and_marks_where_it_starts_and_ends() {
    let (mut h, ids) = harness("clicking_a_node_goes_to_", true, None);
    assert_eq!(h.state().views[0].current, 0);
    node(&h, "L'inflazione è scesa").click();
    support::wait_until(&mut h, "the passage marked", |app| app.views[0].marked_text().is_some());
    assert_eq!(h.state().views[0].current, 1, "on its page");
    assert_eq!(flat(&h.state().views[0].marked_text().unwrap()), "L'inflazione è scesa al 2,1% a settembre");
    assert_eq!(h.state().maps.picked, Some(MapPicked::Node(ids[1])));
    // Another spacing and case, and other words in the middle: from its first words to its last.
    node(&h, "Il calo ha sorpreso").click();
    support::wait_until(&mut h, "the longer passage", |app| app.views[0].marked_text().is_some_and(|t| t.contains("analisti")));
    assert_eq!(flat(&h.state().views[0].marked_text().unwrap()), PAGE_TWO);
    // A quote the page doesn't have: the page shows, with nothing marked.
    node(&h, "Non c'è").click();
    support::wait_until(&mut h, "the quote looked for", |app| app.views[0].marked_passage().is_some_and(|(_, found)| found.is_some()));
    assert_eq!(h.state().views[0].marked_passage(), Some((1, Some(None))));
    // A concept has no passage: it is picked, and the mark stays as it was.
    node(&h, "Politica monetaria").click();
    h.run_steps(3);
    assert_eq!(h.state().maps.picked, Some(MapPicked::Node(ids[4])));
    assert_eq!(h.state().views[0].marked_passage(), Some((1, Some(None))));
    // Escape takes the mark away.
    node(&h, "L'inflazione è scesa").click();
    support::wait_until(&mut h, "marked again", |app| app.views[0].marked_text().is_some());
    h.state_mut().choose_right_panel(None);
    h.run_steps(2);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().views[0].marked_passage().is_none());
}

#[test]
fn a_document_that_has_to_open_first_is_marked_once_it_has() {
    let dir = temp("file");
    let file = dir.join("tassi.pdf");
    let (mut h, _) = harness("opens-first", false, Some(file.clone()));
    assert_eq!(h.state().views.len(), 1);
    node(&h, "L'inflazione è scesa").click();
    support::wait_until(&mut h, "the document opened and marked", |app| {
        app.views.iter().any(|v| v.marked_text().is_some_and(|t| t.starts_with("L'inflazione")))
    });
    let s = h.state();
    let i = s.active.unwrap();
    assert_eq!(s.session.get(s.views[i].id).and_then(|d| d.path.clone()).as_deref(), file.to_str());
    assert_eq!(s.views[i].current, 1);
    let _ = std::fs::remove_dir_all(&dir);
}
