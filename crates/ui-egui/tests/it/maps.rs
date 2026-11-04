//! Concept maps in the app (ADR "Concept graph" G2–G3), with a canned transport: nothing reaches a
//! network. Maps are offered only where they have a folder; a selection goes on the map with its
//! document and page, and opens it again; while the map shows, Undo, Delete and Escape are its own
//! and the document behind is untouched; the assistant's proposal changes nothing until applied,
//! and then only the changes kept, as one undo step.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ai::transport::{HttpRequest, Secret, Transport, TransportError};
use pdfcraft_graph::{Author, Content, Graph};
use pdfcraft_platform::assistant::{SecretStore, SessionKeys};
use pdfcraft_ui_egui::{AiService, MapPicked, PdfCraftApp};

use crate::{assistant, support};

/// Answers each request with the next body of `queue`, and keeps the requests.
#[derive(Default)]
struct Queue {
    queue: Mutex<Vec<String>>,
    seen: Mutex<Vec<HttpRequest>>,
}

impl Transport for Queue {
    fn post_stream(&self, req: &HttpRequest, on_bytes: &mut dyn FnMut(&[u8]) -> std::ops::ControlFlow<()>) -> Result<u16, TransportError> {
        self.seen.lock().unwrap().push(req.clone());
        let mut queue = self.queue.lock().unwrap();
        let body = if queue.is_empty() { String::new() } else { queue.remove(0) };
        drop(queue);
        let _ = on_bytes(body.as_bytes());
        Ok(200)
    }
}

/// A fresh folder for this test's maps.
fn map_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-ui-maps-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn harness(dir: Option<PathBuf>, transport: Option<Arc<Queue>>) -> Harness<'static, PdfCraftApp> {
    let bytes = assistant::fixture();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("rates.pdf", None, bytes).unwrap();
        app.maps.dir = dir;
        if let Some(transport) = transport {
            let keys = Arc::new(SessionKeys::default());
            keys.set("anthropic", &Secret::new("sk-ant-ui-test")).unwrap();
            app.ai_service = Some(AiService { transport, keys, env_key: Arc::new(|| None) });
            app.assistant.prefs.enabled = true;
            app.assistant.prefs.consented = true;
            // Never the user's own library.
            app.assistant.library_file = Some(std::env::temp_dir().join(format!("pdfcraft-ui-maps-{}-library.json", std::process::id())));
        }
        app
    });
    h.run_steps(6);
    h
}

fn open_map(h: &mut Harness<'static, PdfCraftApp>) {
    assert!(h.state_mut().execute("map.open"));
    h.run_steps(3);
    assert!(h.state().maps.open);
    assert!(h.query_by_label("Back to the document").is_some());
}

fn toast(h: &Harness<'static, PdfCraftApp>) -> String {
    h.state().toast.as_ref().map(|t| t.0.clone()).unwrap_or_default()
}

#[test]
fn maps_are_offered_only_with_a_folder_for_them() {
    let mut h = harness(None, None);
    h.state_mut().views[0].select_text(0, 0, 4);
    assert!(support::step_until(&mut h, |h| h.query_by_label("Copy").is_some()), "the selection bar shows");
    assert!(h.query_by_label("Add to map").is_none());
    h.state_mut().execute("map.open");
    h.run_steps(2);
    assert!(!h.state().maps.open);
    assert!(toast(&h).contains("desktop app"), "{}", toast(&h));
}

#[test]
fn a_selection_goes_on_the_map_with_its_page_and_opens_it_again() {
    let dir = map_dir("selection");
    let mut h = harness(Some(dir.clone()), None);
    h.state_mut().views[0].select_text(0, 0, 4);
    assert!(support::step_until(&mut h, |h| h.query_by_label("Add to map").is_some()), "the selection bar offers Add to map");
    h.get_by_label("Add to map").click();
    h.run_steps(3);
    let nodes = h.state().maps.graph.nodes().to_vec();
    assert_eq!(nodes.len(), 1);
    let Content::Text { text } = &nodes[0].content else { panic!("a snippet: {:?}", nodes[0].content) };
    assert!(text.starts_with("Rate"), "{text}");
    let anchor = nodes[0].anchor.clone().expect("a source");
    assert_eq!((anchor.page, anchor.title.as_str()), (0, "rates.pdf"));
    assert_eq!(anchor.sha256.len(), 64);
    // Saved at once, and read back the same.
    let file = dir.join("My map.pdfmap.json");
    let (saved, warnings) = Graph::from_json(&std::fs::read(&file).unwrap()).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(saved.nodes(), &nodes[..]);
    // From the map, the source opens the document at its page.
    assert_eq!(h.state().right, Some(pdfcraft_ui_egui::RightPanel::Map), "the map's panel opens beside the document");
    h.state_mut().choose_right_panel(None);
    h.state_mut().views[0].go_to_page(1);
    open_map(&mut h);
    h.state_mut().maps.picked = None;
    h.run_steps(2);
    // The node on the canvas (a button), not its text in the side panel.
    h.get_by(|n| n.role() == egui::accesskit::Role::Button && n.label().as_deref() == Some(text.as_str())).click();
    h.run_steps(2);
    assert_eq!(h.state().maps.picked, Some(MapPicked::Node(nodes[0].id)));
    h.get_by_label("Open rates.pdf, p. 1").click();
    h.run_steps(3);
    assert!(!h.state().maps.open);
    assert!(support::step_until(&mut h, |h| h.state().views[0].current == 0), "back on page 1");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn while_the_map_shows_undo_delete_and_escape_are_its_own() {
    let dir = map_dir("keys");
    let mut h = harness(Some(dir.clone()), None);
    h.state_mut().apply_edit(pdfcraft_engine::Edit::RotatePages { pages: vec![0], degrees: 90 });
    h.run_steps(2);
    open_map(&mut h);
    h.state_mut().maps.note_draft = "Photosynthesis".into();
    h.run_steps(2);
    h.get_by_label("Add note").click();
    h.run_steps(2);
    assert_eq!(h.state().maps.graph.nodes().len(), 1);
    let doc_undo = |h: &Harness<'static, PdfCraftApp>| h.state().session.get(h.state().views[0].id).and_then(|d| d.can_undo().map(str::to_owned));
    let before = doc_undo(&h);
    assert!(before.is_some(), "the document has a step to undo");
    // The document's commands wait until it is back.
    assert!(!h.state_mut().execute("page.rotate"));
    assert!(toast(&h).contains("Go back to the document"), "{}", toast(&h));
    assert_eq!(doc_undo(&h), before);
    // ⌘Z takes back the note, not the rotation behind the map; ⌘⇧Z puts it back.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(2);
    assert!(h.state().maps.graph.nodes().is_empty());
    assert_eq!(doc_undo(&h), before);
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
    h.run_steps(2);
    assert_eq!(h.state().maps.graph.nodes().len(), 1);
    // Delete removes the selected node; Escape clears the selection, then leaves the map.
    h.get_by_label("Photosynthesis").click();
    h.run_steps(2);
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert!(h.state().maps.graph.nodes().is_empty());
    assert_eq!(doc_undo(&h), before);
    assert_eq!(h.state().session.get(h.state().views[0].id).map(|d| d.info.pages.len()), Some(2), "the document's pages stay");
    h.state_mut().maps.picked = Some(MapPicked::Node(pdfcraft_graph::NodeId(99)));
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().maps.open && h.state().maps.picked.is_none());
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(!h.state().maps.open);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A streamed tool call.
fn tool(index: u32, id: &str, name: &str, input: &str) -> String {
    format!(
        "data: {{\"type\":\"content_block_start\",\"index\":{index},\"content_block\":{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"{name}\",\"input\":{{}}}}}}\n\ndata: {{\"type\":\"content_block_delta\",\"index\":{index},\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":{}}}}}\n\ndata: {{\"type\":\"content_block_stop\",\"index\":{index}}}\n\n",
        serde_json::to_string(input).unwrap()
    )
}

#[test]
fn the_assistants_proposal_applies_only_the_changes_kept_as_one_step() {
    let dir = map_dir("proposal");
    let queue = Arc::new(Queue::default());
    let mut first = String::from(
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\n",
    );
    first += &tool(
        0,
        "t1",
        "propose_node",
        r#"{"key":"n1","type":"snippet","text":"Rates rose three percent in spring","source":"pdfcraft-doc:i0","reason":"the fact"}"#,
    );
    first += &tool(1, "t2", "propose_node", r#"{"key":"n2","type":"concept","text":"Inflation","reason":"the idea"}"#);
    first += &tool(2, "t3", "propose_connection", r#"{"from":"n1","to":"n2","kind":"is an example of","strength":"strong","reason":"prices"}"#);
    first += "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":50}}\n\ndata: {\"type\":\"message_stop\"}\n\n";
    let second = "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Mapped the rise.\"}}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\ndata: {\"type\":\"message_stop\"}\n\n";
    *queue.queue.lock().unwrap() = vec![first, second.to_owned()];
    let mut h = harness(Some(dir.clone()), Some(queue.clone()));
    open_map(&mut h);
    h.state_mut().maps.request = "Map this page".into();
    h.run_steps(2);
    h.get_by_label("Propose changes").click();
    support::wait_until(&mut h, "the proposal", |app| app.maps.review.is_some() || !app.maps.is_running() && app.toast.is_some());
    h.run_steps(2);
    assert!(h.state().maps.review.is_some(), "toast: {}", toast(&h));
    assert!(h.state().maps.graph.nodes().is_empty(), "nothing changes before Apply");
    assert!(!dir.join("My map.pdfmap.json").exists(), "nor is anything written");
    // What was sent: the page being read, the map tools, the key in its header only.
    let sent = queue.seen.lock().unwrap()[0].clone();
    let body = String::from_utf8_lossy(&sent.body).into_owned();
    assert!(body.contains("Rates rose three percent in spring") && body.contains("(the page)"), "{body}");
    assert!(body.contains("\"propose_node\""));
    assert!(!body.contains("sk-ant-ui-test"));
    assert!(h.query_by_label("Mapped the rise.").is_some());
    // Drop the concept: the connection to it is skipped and said so.
    h.get_by_label("2. Add n2: Inflation").click();
    h.run_steps(2);
    h.get_by_label("Apply 2 changes").click();
    h.run_steps(3);
    let graph = h.state().maps.graph.clone();
    assert_eq!(graph.nodes().len(), 1, "toast: {}", toast(&h));
    assert!(graph.edges().is_empty());
    assert_eq!(graph.nodes()[0].author, Author::Assistant);
    assert_eq!(graph.nodes()[0].anchor.as_ref().map(|a| a.page), Some(0));
    assert!(toast(&h).contains("1 of the kept changes"), "{}", toast(&h));
    assert!(h.state().maps.review.is_none());
    // One step takes it all back.
    assert!(h.state_mut().execute("edit.undo"));
    h.run_steps(2);
    assert!(h.state().maps.graph.nodes().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nothing_is_sent_for_the_map_before_consent() {
    let dir = map_dir("consent");
    let queue = Arc::new(Queue::default());
    let mut h = harness(Some(dir.clone()), Some(queue.clone()));
    h.state_mut().assistant.prefs.consented = false;
    open_map(&mut h);
    h.state_mut().maps.request = "Map this page".into();
    h.run_steps(2);
    h.get_by_label("Propose changes").click();
    h.run_steps(3);
    assert!(!h.state().maps.is_running());
    assert!(queue.seen.lock().unwrap().is_empty());
    assert!(toast(&h).contains("allow requests"), "{}", toast(&h));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_kind_of_ones_own_joins_the_map_and_is_chosen_for_new_connections() {
    let dir = map_dir("kinds");
    let mut h = harness(Some(dir.clone()), None);
    open_map(&mut h);
    h.get_by_label("Kinds of connection").click();
    h.run_steps(2);
    h.state_mut().maps.kind_draft = "  hunts ".into();
    h.run_steps(2);
    h.get_by_label("Add kind").click();
    h.run_steps(2);
    let kind = h.state().maps.graph.kind_named("hunts").map(|k| (k.id, k.directed)).expect("the new kind");
    assert_eq!(kind, (h.state().maps.new_kind, true));
    assert!(h.state().maps.kind_draft.is_empty());
    let (saved, _) = Graph::from_json(&std::fs::read(dir.join("My map.pdfmap.json")).unwrap()).unwrap();
    assert!(saved.kind_named("hunts").is_some(), "saved with the map");
    assert!(h.state_mut().execute("edit.undo"));
    h.run_steps(2);
    assert!(h.state().maps.graph.kind_named("hunts").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_map_saved_earlier_takes_the_next_snippet_and_an_unreadable_one_is_never_written_over() {
    let add_selection = |h: &mut Harness<'static, PdfCraftApp>| {
        h.state_mut().views[0].select_text(0, 0, 4);
        assert!(support::step_until(h, |h| h.query_by_label("Add to map").is_some()), "the selection bar offers Add to map");
        h.get_by_label("Add to map").click();
        h.run_steps(3);
    };
    // A map from an earlier session, never opened in this one, gets the snippet too.
    let dir = map_dir("earlier");
    std::fs::create_dir_all(&dir).unwrap();
    let mut earlier = Graph::new("My map");
    earlier.add_node(Content::Note { text: "From yesterday".into() }, None, [0.0, 0.0], Author::User).unwrap();
    let file = dir.join("My map.pdfmap.json");
    std::fs::write(&file, earlier.to_json().unwrap()).unwrap();
    let mut h = harness(Some(dir.clone()), None);
    add_selection(&mut h);
    let (saved, _) = Graph::from_json(&std::fs::read(&file).unwrap()).unwrap();
    let texts: Vec<&str> = saved.nodes().iter().map(|n| n.content.text()).collect();
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert_eq!(texts[0], "From yesterday");
    let _ = std::fs::remove_dir_all(&dir);
    // One that can't be read is left as it is, and nothing is added.
    let dir = map_dir("unreadable");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("My map.pdfmap.json");
    std::fs::write(&file, b"{ not a map").unwrap();
    let mut h = harness(Some(dir.clone()), None);
    add_selection(&mut h);
    assert_eq!(std::fs::read(&file).unwrap(), b"{ not a map");
    assert!(h.state().maps.graph.nodes().is_empty());
    assert!(toast(&h).contains("Couldn't open the map"), "{}", toast(&h));
    open_map(&mut h);
    h.state_mut().maps.note_draft = "Try again".into();
    h.run_steps(2);
    h.get_by_label("Add note").click();
    h.run_steps(2);
    assert_eq!(std::fs::read(&file).unwrap(), b"{ not a map");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn while_reading_a_snippet_is_added_and_connected_from_the_panel_without_leaving_the_page() {
    let dir = map_dir("reading");
    std::fs::create_dir_all(&dir).unwrap();
    let mut earlier = Graph::new("My map");
    let inflation = earlier.add_node(Content::Concept { label: "Inflation".into() }, None, [0.0, 0.0], Author::User).unwrap();
    earlier.add_node(Content::Note { text: "Unrelated note".into() }, None, [300.0, 0.0], Author::User).unwrap();
    std::fs::write(dir.join("My map.pdfmap.json"), earlier.to_json().unwrap()).unwrap();
    let mut h = harness(Some(dir.clone()), None);
    // The rail offers the map beside the document.
    h.state_mut().choose_right_panel(None);
    h.run_steps(2);
    h.get_by_label("Concept map").click();
    h.run_steps(2);
    assert_eq!(h.state().right, Some(pdfcraft_ui_egui::RightPanel::Map));
    // ⌘⇧M adds the selection; the panel shows it, ready to connect.
    h.state_mut().views[0].select_text(0, 0, 4);
    assert!(support::step_until(&mut h, |h| h.query_by_label("Add to map").is_some()), "the page's text has arrived");
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::M);
    h.run_steps(3);
    let graph = h.state().maps.graph.clone();
    assert_eq!(graph.nodes().len(), 3, "toast: {}", toast(&h));
    let snippet = graph.nodes()[2].id;
    assert_eq!(h.state().maps.picked, Some(MapPicked::Node(snippet)));
    assert!(!h.state().maps.open, "the document stays in view");
    assert!(h.query_by_label_contains("On the map from this page").is_some());
    // Find the concept by a word and connect to it with one click.
    h.state_mut().maps.filter = "infl".into();
    h.run_steps(2);
    assert!(h.query_by_label("Connect to: Unrelated note").is_none(), "the search keeps to its words");
    h.get_by_label("Connect to: Inflation").click();
    h.run_steps(2);
    let edges = h.state().maps.graph.edges().to_vec();
    assert_eq!(edges.len(), 1);
    assert_eq!((edges[0].from, edges[0].to), (snippet, inflation));
    assert_eq!(h.state().maps.graph.kind(edges[0].kind).map(|k| k.name.clone()).as_deref(), Some("is related to"));
    let (saved, _) = Graph::from_json(&std::fs::read(dir.join("My map.pdfmap.json")).unwrap()).unwrap();
    assert_eq!(saved.edges().len(), 1, "saved at once");
    assert_eq!(h.state().views[0].current, 0);
    assert!(!h.state().maps.open);
    // The connection leads on to the other node.
    h.get_by_label_contains("is related to (Medium) · Inflation").click();
    h.run_steps(2);
    assert_eq!(h.state().maps.picked, Some(MapPicked::Node(inflation)));
    let _ = std::fs::remove_dir_all(&dir);
}
