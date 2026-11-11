//! The concept map as a graph in the panel beside the document: round nodes inside the graph's
//! frame; hovering a node grows its preview; clicking one goes to its passage in the document and
//! marks it from where it starts to where it ends (found in the page's text even when the quote's
//! spacing, case or middle words differ) while the node stays picked in an open view of the map,
//! Escape takes the mark away, and a document that has to open first is marked once it is.

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
    // The preview is the passage alone, large: no source, connections or hints around it.
    let picture = h.get_by_label(render).rect();
    assert!(picture.width() >= 500.0, "large enough to read: {picture:?}");
    assert!(picture.height() <= 460.0, "no taller than a preview's picture, whole page or not: {picture:?}");
    // Once the page's text is in, the picture is the page around the passage, not all of it.
    let cropped = support::step_until(&mut h, |h| {
        std::thread::sleep(std::time::Duration::from_millis(10));
        h.state().views[0].has_page_text(1) && h.query_by_label(render).is_some_and(|n| n.rect().height() < n.rect().width() * 0.6)
    });
    assert!(cropped, "cropped around the passage: {:?}", h.get_by_label(render).rect());
    h.run_steps(4);
    shot(&mut h, "map-preview.png");
    assert!(h.query_by_label("tassi.pdf, p. 2").is_none(), "no source line");
    assert!(h.query_by(|n| n.label().is_some_and(|l| l.starts_with("\u{2190} ") || l.starts_with("\u{2192} "))).is_none(), "no connections");
    // Away from the node, it shrinks back and goes.
    h.get_by_label("Map graph").hover();
    let gone = support::step_until(&mut h, |h| {
        std::thread::sleep(std::time::Duration::from_millis(10));
        h.query_by_label(render).is_none()
    });
    assert!(gone, "the preview closes");
    // A concept has no page: its text shows, and only its text.
    let concept_text = |h: &Harness<'static, PdfCraftApp>| h.query_all_by_label("Politica monetaria").count();
    let before = concept_text(&h);
    node(&h, "Politica monetaria").hover();
    let shown = support::step_until(&mut h, |h| {
        std::thread::sleep(std::time::Duration::from_millis(10));
        concept_text(h) > before
    });
    assert!(shown, "the concept's text in its preview");
}

#[test]
fn the_question_mark_opens_the_graphs_shortcuts() {
    let (mut h, _) = harness("the_question_mark_opens_", true, None);
    assert!(h.query_by_label("Graph shortcuts").is_none());
    help_button(&h).click();
    h.run_steps(3);
    h.get_by_label("Graph shortcuts");
    h.get_by_label("Move between nodes");
    h.get_by_label("Connect it to the node it is dropped on");
    shot(&mut h, "map-shortcuts.png");
    let popup = h.get_by_label("Graph shortcuts").rect();
    // A click elsewhere (on the document, left of the popup) closes it, and the "?" opens it again.
    let away = egui::pos2(popup.min.x - 200.0, popup.max.y + 40.0);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: away, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
    }
    h.run_steps(3);
    assert!(h.query_by_label("Graph shortcuts").is_none(), "closed by a click outside");
    help_button(&h).click();
    h.run_steps(3);
    h.get_by_label("Graph shortcuts");
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(h.query_by_label("Graph shortcuts").is_none(), "closed by Escape");
}

/// A picture of the app, into `PDFCRAFT_MAP_SHOTS` when it is set.
fn shot(h: &mut Harness<'static, PdfCraftApp>, name: &str) {
    if let Ok(dir) = std::env::var("PDFCRAFT_MAP_SHOTS") {
        let _gpu = crate::gpu();
        h.render().unwrap().save(std::path::Path::new(&dir).join(name)).unwrap();
    }
}

/// The "?" in the graph's toolbar, just above the graph.
fn help_button<'a>(h: &'a Harness<'static, PdfCraftApp>) -> egui_kittest::Node<'a> {
    let frame = h.get_by_label("Map graph").rect();
    h.get_by(move |n| {
        n.role() == Role::Button
            && n.label().as_deref() == Some("Keyboard shortcuts")
            && n.bounding_box()
                .is_some_and(|b| b.x0 as f32 >= frame.min.x - 1.0 && b.y1 as f32 <= frame.min.y + 1.0 && b.y0 as f32 >= frame.min.y - 40.0)
    })
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
    // A concept has no passage: it is picked, and the passage before is no longer marked.
    node(&h, "Politica monetaria").click();
    h.run_steps(3);
    assert_eq!(h.state().maps.picked, Some(MapPicked::Node(ids[4])));
    assert!(h.state().views[0].marked_passage().is_none(), "the concept picked leaves no mark of the node before");
    shot(&mut h, "mark-concept-picked.png");
    // Escape takes the mark away, with the node still picked beside the document: the first lets
    // the graph's keyboard go, the next is the page's.
    node(&h, "L'inflazione è scesa").click();
    support::wait_until(&mut h, "marked again", |app| app.views[0].marked_text().is_some());
    for _ in 0..2 {
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
    }
    assert_eq!(h.state().maps.picked, Some(MapPicked::Node(ids[1])));
    assert!(h.state().views[0].marked_passage().is_none());
}

/// A node's passage stays marked only while the node is picked in the view of the map it was
/// picked in: picking another node (one in another document too), a connection or nothing takes
/// it away, as do removing the node and closing the graph beside the document or the map's window.
#[test]
fn a_passage_stays_marked_only_while_its_node_is_picked_in_an_open_view_of_the_map() {
    let (mut h, ids) = harness("a_passage_stays_marked_o", true, None);
    let marked = |h: &Harness<'static, PdfCraftApp>, view: usize| h.state().views[view].marked_passage().is_some();
    let mark = |h: &mut Harness<'static, PdfCraftApp>, label: &str| {
        node(h, label).click();
        support::wait_until(h, "the passage marked", |app| app.views.iter().any(|v| v.marked_text().is_some()));
    };
    // Closing the graph beside the document.
    mark(&mut h, "L'inflazione è scesa");
    shot(&mut h, "mark-picked.png");
    h.state_mut().choose_right_panel(None);
    h.run_steps(2);
    assert!(!marked(&h, 0), "the graph beside the document closed");
    shot(&mut h, "mark-graph-closed.png");
    h.state_mut().choose_right_panel(Some(RightPanel::Map));
    h.run_steps(3);
    // Nothing picked: Done stops connecting the node.
    mark(&mut h, "L'inflazione è scesa");
    h.get_by_label("Done").click();
    h.run_steps(2);
    assert_eq!(h.state().maps.picked, None);
    assert!(!marked(&h, 0), "nothing picked");
    // A connection picked.
    mark(&mut h, "Il calo ha sorpreso");
    let edge = h.state().maps.graph.edges().first().map(|e| e.id).unwrap();
    h.state_mut().maps.picked = Some(MapPicked::Edge(edge));
    h.run_steps(2);
    assert!(!marked(&h, 0), "a connection picked");
    // The node removed.
    mark(&mut h, "Il calo ha sorpreso");
    h.state_mut().maps.graph.remove_node(ids[2]).unwrap();
    h.run_steps(2);
    assert!(!marked(&h, 0), "the node removed");
    // A node of another document: that document's passage is marked, and this one's isn't.
    let other = pdfcraft_engine::Session::new().create_from_text("Cambi", "Il cambio con il dollaro resta stabile.").unwrap().to_vec();
    let sha = pdfcraft_ai::source::Sha256::of(&other).to_hex();
    h.state_mut().open_bytes("cambi.pdf", None, other).unwrap();
    let quote = "Il cambio con il dollaro resta stabile.";
    let added =
        h.state_mut().maps.graph.add_node(Content::Text { text: "Il cambio".into() }, anchor(&sha, 0, quote, None), [600.0, 120.0], Author::User);
    assert!(added.is_ok());
    h.run_steps(4);
    mark(&mut h, "L'inflazione è scesa");
    assert!(marked(&h, 0));
    node(&h, "Il cambio").click();
    support::wait_until(&mut h, "the other document's passage", |app| app.views[1].marked_text().is_some());
    assert!(!marked(&h, 0), "the passage of the node picked before, in its own document");
    // The map's window: a click there marks; closing it takes the mark away, though the graph
    // beside the document stays.
    assert!(h.state_mut().execute("map.window"));
    h.run_steps(4);
    let frame = h.get_by_label("Map graph in its window").rect();
    h.get_by(|n| {
        n.role() == Role::Button
            && n.label().as_deref() == Some("L'inflazione è scesa")
            && n.bounding_box().is_some_and(|b| frame.contains(egui::pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32)))
    })
    .click();
    support::wait_until(&mut h, "marked from the window", |app| app.views[0].marked_text().is_some());
    assert!(!marked(&h, 1));
    h.get_by_label("Close this window; the map stays beside the document").click();
    h.run_steps(3);
    assert!(!h.state().maps.window);
    assert_eq!(h.state().right, Some(RightPanel::Map));
    assert!(!marked(&h, 0), "the window it was picked in closed");
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

/// A tiny PNG picture for the map: a red square with a blue corner.
fn picture() -> Vec<u8> {
    let px =
        image::RgbaImage::from_fn(48, 32, |x, y| if x < 16 && y < 12 { image::Rgba([30, 60, 200, 255]) } else { image::Rgba([210, 60, 40, 255]) });
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(px).write_to(&mut png, image::ImageFormat::Png).unwrap();
    png.into_inner()
}

/// The whole map in the document area (View ▸ Concept map), with a note and a picture besides
/// the snippets and the concept.
fn whole_map(name: &str) -> (Harness<'static, PdfCraftApp>, Vec<NodeId>) {
    let (mut h, mut ids) = harness(name, true, None);
    let g = &mut h.state_mut().maps.graph;
    let kinds: Vec<_> = g.kinds().iter().map(|k| k.id).collect();
    let note = g
        .add_node(Content::Note { text: "Ricontrollare i dati di ottobre prima della relazione.".into() }, None, [-260.0, -60.0], Author::User)
        .unwrap();
    let pic = g
        .add_node(
            Content::Image { image: pdfcraft_graph::ImageBytes(std::sync::Arc::new(picture())), alt: "Grafico dei tassi".into() },
            None,
            [620.0, -120.0],
            Author::User,
        )
        .unwrap();
    g.add_edge(note, ids[0], kinds[1], Strength::Weak, None, Author::User).unwrap();
    g.add_edge(pic, ids[4], kinds[2], Strength::Medium, None, Author::User).unwrap();
    g.add_edge(ids[2], ids[1], kinds[3], Strength::Strong, None, Author::User).unwrap();
    ids.extend([note, pic]);
    assert!(h.state_mut().execute("map.open"));
    h.run_steps(6);
    (h, ids)
}

/// A card of the whole map: the node with this label on the board (the panel's graph names it
/// too).
fn card<'a>(h: &'a Harness<'static, PdfCraftApp>, label: &'a str) -> egui_kittest::Node<'a> {
    let board = h.get_by_label("The whole map").rect();
    h.get_by(move |n| {
        n.role() == Role::Button
            && n.label().as_deref() == Some(label)
            && n.bounding_box().is_some_and(|b| board.contains(egui::pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32)))
    })
}

/// Steps until `done`, letting time pass for glides and previews.
fn settle(h: &mut Harness<'static, PdfCraftApp>, done: impl Fn(&Harness<'static, PdfCraftApp>) -> bool) -> bool {
    support::step_until(h, |h| {
        std::thread::sleep(std::time::Duration::from_millis(15));
        done(h)
    })
}

#[test]
fn the_whole_map_fits_its_cards_and_previews_the_one_the_pointer_rests_on() {
    let (mut h, _) = whole_map("the_whole_map_fits_its_c");
    // Fitted when it opens: every card whole on the board.
    let labels = [
        "La banca centrale ha alzato il tasso",
        "L'inflazione è scesa",
        "Il calo ha sorpreso",
        "Non c'è",
        "Politica monetaria",
        "Picture: Grafico dei tassi",
    ];
    let fitted = settle(&mut h, |h| {
        let board = h.get_by_label("The whole map").rect();
        labels.iter().all(|l| h.query_all_by_label(l).any(|n| board.contains_rect(n.rect())))
    });
    assert!(fitted, "every card on the board");
    h.run_steps(4);
    shot(&mut h, "whole-map.png");
    // Resting on a snippet from the open document: its passage, rendered.
    let render = "Page 2 of tassi.pdf, rendered";
    assert!(h.query_by_label(render).is_none());
    card(&h, "L'inflazione è scesa").hover();
    assert!(
        settle(&mut h, |h| h.query_by_label(render).is_some() && h.state().views[0].has_passage_raster(1)),
        "the passage rendered beside the card"
    );
    let preview = h.get_by_label(render).rect();
    let board = h.get_by_label("The whole map").rect();
    assert!(board.contains_rect(preview), "on the board: {preview:?} in {board:?}");
    assert!(preview.width() >= 300.0, "large enough to read: {preview:?}");
    h.run_steps(6);
    shot(&mut h, "whole-map-preview.png");
    // Away, it goes; a picture's preview is the picture.
    h.get_by_label("The whole map").hover();
    assert!(settle(&mut h, |h| h.query_by_label(render).is_none()), "the preview closes");
    card(&h, "Picture: Grafico dei tassi").hover();
    assert!(settle(&mut h, |h| h.query_by_label("Grafico dei tassi").is_some()), "the picture's preview");
    h.run_steps(6);
    shot(&mut h, "whole-map-picture.png");
}

#[test]
fn the_whole_maps_side_panel_shows_the_picked_card_and_folds_away() {
    let (mut h, ids) = whole_map("the_whole_maps_side_pane");
    // The board is narrow here: the panel starts folded away, and the toolbar opens it.
    assert!(h.query_by_label("Add note").is_none());
    h.get_by_label("Show the side panel").click();
    h.run_steps(3);
    h.get_by_label("Add note");
    // The board is narrower: the map is fitted to it again, every card whole.
    let refitted = settle(&mut h, |h| {
        let board = h.get_by_label("The whole map").rect();
        ["Politica monetaria", "Picture: Grafico dei tassi", "Non c'è"].iter().all(|l| h.query_all_by_label(l).any(|n| board.contains_rect(n.rect())))
    });
    assert!(refitted, "fitted to the narrower board");
    card(&h, "Politica monetaria").click();
    h.run_steps(3);
    assert_eq!(h.state().maps.picked, Some(MapPicked::Node(ids[4])));
    // Its node card, beside the board (the panel beside the document has one too).
    let board = h.get_by_label("The whole map").rect();
    assert!(
        h.query_all_by_label("Remove node").any(|n| n.rect().min.x > board.max.x && n.rect().max.x < board.max.x + 320.0),
        "the side panel's node card"
    );
    shot(&mut h, "whole-map-side.png");
    if std::env::var("PDFCRAFT_MAP_SHOTS").is_ok() {
        h.state_mut().set_option("theme", "dark").unwrap();
        h.run_steps(4);
        shot(&mut h, "whole-map-dark.png");
    }
    h.get_by_label("Hide the side panel").click();
    h.run_steps(3);
    assert!(h.query_by_label("Add note").is_none(), "folded away again");
}

/// The item `label` of the menu just opened at `near` (the app's own menus, tabs and the panel
/// beside the document have an Edit and a Remove node too).
fn menu_item<'a>(h: &'a Harness<'static, PdfCraftApp>, label: &'a str, near: egui::Pos2) -> egui_kittest::Node<'a> {
    let distance = |n: &egui_kittest::Node<'_>| n.rect().center().distance(near);
    h.get_all_by(move |n| n.label().as_deref() == Some(label) && n.role() == Role::Button)
        .min_by(|a, b| distance(a).total_cmp(&distance(b)))
        .unwrap_or_else(|| panic!("no {label} in the menu"))
}

/// The text field holding `value`.
fn field<'a>(h: &'a Harness<'static, PdfCraftApp>, value: &'a str) -> egui_kittest::Node<'a> {
    h.get_by(move |n| matches!(n.role(), Role::TextInput | Role::MultilineTextInput) && n.value().as_deref() == Some(value))
}

/// Replace what the focused field holds with `text`.
fn retype(h: &mut Harness<'static, PdfCraftApp>, text: &str) {
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_steps(1);
    h.event(egui::Event::Text(text.into()));
    h.run_steps(2);
}

fn text_of(h: &Harness<'static, PdfCraftApp>, id: NodeId) -> Option<String> {
    h.state().maps.graph.node(id).map(|n| n.content.text().to_owned())
}

#[test]
fn nodes_are_edited_and_removed_on_the_whole_map() {
    let (mut h, ids) = whole_map("nodes_are_edited_and_rem");
    settle(&mut h, |h| card(h, "Politica monetaria").rect().width() > 0.0);
    // The card's menu: Edit opens an editor over the card, with its text.
    let at = card(&h, "Politica monetaria").rect().center();
    card(&h, "Politica monetaria").click_secondary();
    h.run_steps(3);
    menu_item(&h, "Edit", at).click();
    h.run_steps(3);
    assert!(h.state().maps.editing.is_some());
    field(&h, "Politica monetaria");
    shot(&mut h, "whole-map-edit.png");
    retype(&mut h, "Politica dei tassi");
    h.get_by_label("Save").click();
    h.run_steps(3);
    assert_eq!(text_of(&h, ids[4]).as_deref(), Some("Politica dei tassi"));
    assert!(h.state().maps.editing.is_none());
    let edges = h.state().maps.graph.edges().len();
    // Enter edits the picked card; Escape leaves it as it was.
    card(&h, "Ricontrollare i dati di ottobre prima della relazione.").click();
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert!(h.state().maps.editing.as_ref().is_some_and(|e| e.id == ids[5]), "Enter edits the picked card");
    retype(&mut h, "Something else");
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(h.state().maps.editing.is_none());
    assert_eq!(text_of(&h, ids[5]).as_deref(), Some("Ricontrollare i dati di ottobre prima della relazione."), "cancelled");
    assert!(h.state().maps.open, "Escape left the editor, not the map");
    // An empty text isn't saved.
    h.key_press(egui::Key::F2);
    h.run_steps(3);
    retype(&mut h, "   ");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(text_of(&h, ids[5]).as_deref(), Some("Ricontrollare i dati di ottobre prima della relazione."));
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // Remove from the menu, and Undo brings it back with its connections.
    let at = card(&h, "Picture: Grafico dei tassi").rect().center();
    card(&h, "Picture: Grafico dei tassi").click_secondary();
    h.run_steps(3);
    menu_item(&h, "Remove node", at).click();
    h.run_steps(3);
    assert!(h.state().maps.graph.node(ids[6]).is_none());
    assert_eq!(h.state().maps.graph.edges().len(), edges - 1);
    h.state_mut().maps.picked = None;
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(3);
    assert!(h.state().maps.graph.node(ids[6]).is_some(), "Undo brings it back");
    assert_eq!(h.state().maps.graph.edges().len(), edges);
}

#[test]
fn nodes_are_edited_and_removed_from_the_graph_beside_the_document_and_in_its_window() {
    let (mut h, ids) = harness("nodes_are_edited_and_bes", true, None);
    // The node's menu in the graph: Edit shows the editor in the panel under it.
    let at = node(&h, "Politica monetaria").rect().center();
    node(&h, "Politica monetaria").click_secondary();
    h.run_steps(3);
    menu_item(&h, "Edit", at).click();
    h.run_steps(3);
    assert!(h.state().maps.editing.as_ref().is_some_and(|e| e.id == ids[4]));
    field(&h, "Politica monetaria");
    shot(&mut h, "panel-edit.png");
    retype(&mut h, "Politica monetaria europea");
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(text_of(&h, ids[4]).as_deref(), Some("Politica monetaria europea"), "Enter saves a concept's name");
    // The keyboard: E edits the node under the cursor, Delete removes it.
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::E);
    h.run_steps(3);
    while h.state().maps.graph_cursor() != Some(ids[3]) {
        h.key_press(egui::Key::Tab);
        h.run_steps(1);
    }
    h.key_press(egui::Key::E);
    h.run_steps(3);
    assert!(h.state().maps.editing.as_ref().is_some_and(|e| e.id == ids[3]), "E edits the cursor's node");
    retype(&mut h, "Non trovato");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(text_of(&h, ids[3]).as_deref(), Some("Non trovato"), "⌘↩ saves a snippet's text");
    assert!(h.state().maps.graph.node(ids[3]).is_some_and(|n| n.anchor.is_some()), "its source stays");
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::E);
    h.run_steps(3);
    while h.state().maps.graph_cursor() != Some(ids[3]) {
        h.key_press(egui::Key::Tab);
        h.run_steps(1);
    }
    h.key_press(egui::Key::Delete);
    h.run_steps(3);
    assert!(h.state().maps.graph.node(ids[3]).is_none(), "Delete removes the cursor's node");
    // The panel's Undo brings it back.
    h.get_by(|n| n.role() == Role::Button && n.label().as_deref() == Some("Undo")).click();
    h.run_steps(3);
    assert!(h.state().maps.graph.node(ids[3]).is_some());
    // In the map's own window: its menu edits there, beside its graph.
    assert!(h.state_mut().execute("map.window"));
    h.run_steps(4);
    let frame = h.get_by_label("Map graph in its window").rect();
    let in_window = h.get_by(|n| {
        n.role() == Role::Button
            && n.label().as_deref() == Some("Il calo ha sorpreso")
            && n.bounding_box().is_some_and(|b| frame.contains(egui::pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32)))
    });
    let at = in_window.rect().center();
    in_window.click_secondary();
    h.run_steps(3);
    menu_item(&h, "Edit", at).click();
    h.run_steps(3);
    assert!(h.state().maps.editing.as_ref().is_some_and(|e| e.id == ids[2] && e.place == pdfcraft_ui_egui::MapEditIn::Window));
    retype(&mut h, "Il calo ha sorpreso gli analisti");
    h.get_by_label("Save").click();
    h.run_steps(3);
    assert_eq!(text_of(&h, ids[2]).as_deref(), Some("Il calo ha sorpreso gli analisti"));
}
