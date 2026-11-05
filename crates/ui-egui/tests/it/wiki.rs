//! The wiki beside the document, over a folder of its own and canned transports: nothing reaches
//! a network. Its folder is chosen (on Google Drive by default); pages are read from it, followed
//! by their links and their snippets' sources; the selection goes on a page, and the last change
//! is undone; the assistant's proposal changes only what is kept; answers read the wiki, and a
//! citation of it opens the page.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ai::source::SourceId;
use pdfcraft_ai::transport::{HttpRequest, Secret, Transport, TransportError};
use pdfcraft_platform::assistant::{SecretStore, SessionKeys};
use pdfcraft_ui_egui::{AiService, PdfCraftApp, RightPanel};

use crate::{assistant, support};

/// Answers each request with the next of `queue`, or with `answer(request)` once it is empty; keeps
/// the requests.
#[derive(Default)]
struct Queue {
    queue: Mutex<Vec<String>>,
    seen: Mutex<Vec<HttpRequest>>,
}

impl Transport for Queue {
    fn post_stream(&self, req: &HttpRequest, on_bytes: &mut dyn FnMut(&[u8]) -> std::ops::ControlFlow<()>) -> Result<u16, TransportError> {
        self.seen.lock().unwrap().push(req.clone());
        let mut queue = self.queue.lock().unwrap();
        let body = if queue.is_empty() { citing_the_wiki(&req.body) } else { queue.remove(0) };
        drop(queue);
        let _ = on_bytes(body.as_bytes());
        Ok(200)
    }
}

/// An answer citing the first wiki passage the request sent.
fn citing_the_wiki(request: &[u8]) -> String {
    let body: serde_json::Value = serde_json::from_slice(request).unwrap_or_default();
    let blocks: Vec<serde_json::Value> = body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
        .filter(|b| b["type"] == "search_result")
        .collect();
    let (index, source) = blocks
        .iter()
        .enumerate()
        .find(|(_, b)| b["source"].as_str().is_some_and(|s| s.starts_with("pdfcraft-wiki:")))
        .map(|(i, b)| (i, b["source"].as_str().unwrap_or_default().to_owned()))
        .unwrap_or((0, String::new()));
    [
        r#"{"type":"message_start","message":{"id":"m","model":"claude-opus-5-5","usage":{"input_tokens":50,"output_tokens":1}}}"#.to_owned(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#.to_owned(),
        format!(r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"citations_delta","citation":{{"type":"search_result_location","cited_text":"slowed","source":"{source}","title":"t","search_result_index":{index},"start_block_index":0,"end_block_index":1}}}}}}"#),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Inflation slowed."}}"#.to_owned(),
        r#"{"type":"content_block_stop","index":0}"#.to_owned(),
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#.to_owned(),
        r#"{"type":"message_stop"}"#.to_owned(),
    ]
    .iter()
    .map(|l| format!("data: {l}\n\n"))
    .collect()
}

fn tool(index: u32, id: &str, name: &str, input: &str) -> String {
    format!(
        "data: {{\"type\":\"content_block_start\",\"index\":{index},\"content_block\":{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"{name}\",\"input\":{{}}}}}}\n\ndata: {{\"type\":\"content_block_delta\",\"index\":{index},\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":{}}}}}\n\ndata: {{\"type\":\"content_block_stop\",\"index\":{index}}}\n\n",
        serde_json::to_string(input).unwrap()
    )
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-ui-wiki-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn harness(folder: Option<PathBuf>, transport: Option<Arc<Queue>>) -> Harness<'static, PdfCraftApp> {
    let bytes = assistant::fixture();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("rates.pdf", None, bytes).unwrap();
        app.assistant.prefs.wiki_folder = folder.map(|f| f.to_string_lossy().into_owned());
        // Never the user's own library.
        app.assistant.library_file = Some(std::env::temp_dir().join(format!("pdfcraft-ui-wiki-{}-library.json", std::process::id())));
        if let Some(transport) = transport {
            let keys = Arc::new(SessionKeys::default());
            keys.set("anthropic", &Secret::new("sk-ant-ui-test")).unwrap();
            app.ai_service = Some(AiService { transport, keys, env_key: Arc::new(|| None), claude_code: None });
            app.assistant.prefs.enabled = true;
            app.assistant.prefs.consented = true;
            app.assistant.prefs.preview = false;
        }
        app
    });
    h.run_steps(6);
    h
}

fn open_wiki(h: &mut Harness<'static, PdfCraftApp>, pages: usize) {
    h.state_mut().choose_right_panel(Some(RightPanel::Wiki));
    h.run_steps(2);
    support::wait_until(h, "the wiki read", |app| !app.wiki.loading() && app.wiki.pages() == pages);
    h.run_steps(2);
}

fn toast(h: &Harness<'static, PdfCraftApp>) -> String {
    h.state().toast.as_ref().map(|t| t.0.clone()).unwrap_or_default()
}

fn sha(h: &Harness<'static, PdfCraftApp>) -> String {
    let s = h.state();
    s.session.document_sha256(s.views[0].id).unwrap().to_hex()
}

fn read(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file)).unwrap_or_default()
}

#[test]
fn the_wiki_goes_on_google_drive_and_its_pages_lead_to_each_other_and_their_sources() {
    let home = temp("home");
    std::fs::create_dir_all(home.join("Library/CloudStorage/GoogleDrive-a@example.com/Il mio Drive")).unwrap();
    let mut h = harness(None, None);
    h.state_mut().choose_right_panel(Some(RightPanel::Wiki));
    h.run_steps(3);
    h.get_by_label("Choose a folder…");
    h.get_by_label("Keep it on this computer");
    assert!(h.query_by_label("Keep it on Google Drive").is_none(), "no Drive here");
    h.state_mut().drive.home = Some(home.clone());
    h.run_steps(2);
    h.get_by_label("Keep it on Google Drive").click();
    h.run_steps(3);
    let folder = home.join("Library/CloudStorage/GoogleDrive-a@example.com/Il mio Drive/PdfCraft Wiki");
    assert!(folder.is_dir());
    assert_eq!(h.state().assistant.prefs.wiki_folder.as_deref(), folder.to_str());
    support::wait_until(&mut h, "the empty wiki", |app| !app.wiki.loading());
    h.run_steps(2);
    assert!(h.query_by_label_contains("No pages yet").is_some());

    // Pages written in another editor are read back.
    let sha = sha(&h);
    std::fs::write(
        folder.join("Rates.md"),
        format!("# Interest rates\n\nSee [[Inflation]].\n\n## Snippets\n\n> Costs fell in the autumn.\n> — [rates.pdf, p. 2](pdfcraft://open?sha256={sha}&page=2)\n"),
    )
    .unwrap();
    std::fs::write(folder.join("Inflation.md"), "# Inflation\n\nPrices rising.\n").unwrap();
    h.get_by_label("Reload").click();
    open_wiki(&mut h, 2);
    h.get_by_label("Interest rates  ·  1").click();
    h.run_steps(3);
    assert_eq!(h.state().wiki.page_shown(), Some("Rates.md"));
    h.get_by_label("Costs fell in the autumn.");
    // The snippet's source opens the document at its page.
    assert_eq!(h.state().views[0].current, 0);
    h.get_by_label("rates.pdf, page 2").click();
    h.run_steps(3);
    assert!(support::step_until(&mut h, |h| h.state().views[0].current == 1), "on page 2");
    // A link leads to its page, which lists where it is linked from.
    h.get_by(|n| n.role() == Role::Link && n.label().as_deref() == Some("Inflation")).click();
    h.run_steps(3);
    assert_eq!(h.state().wiki.page_shown(), Some("Inflation.md"));
    h.get_by_label("Linked from");
    h.get_by(|n| n.role() == Role::Link && n.label().as_deref() == Some("Interest rates"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_selection_goes_on_a_page_and_the_last_change_is_undone() {
    let dir = temp("selection");
    std::fs::write(dir.join("Rates.md"), "# Rates\n\nWhat banks charge.\n\n## Notes\n\nMine.\n").unwrap();
    let mut h = harness(Some(dir.clone()), None);
    open_wiki(&mut h, 1);
    h.state_mut().views[0].select_text(0, 0, 4);
    h.run_steps(2);
    assert!(h.state_mut().execute("wiki.add_selection"));
    h.run_steps(3);
    h.get_by_label("Add the selection to a page");
    // A new page, named in the field.
    let field = h.get_by_role_and_label(Role::TextInput, "Page");
    field.focus();
    field.type_text("Prices");
    h.run_steps(2);
    h.get_by_label("New page “Prices”").click();
    h.run_steps(3);
    let prices = read(&dir, "Prices.md");
    let sha = sha(&h);
    assert!(prices.starts_with("# Prices\n\n## Snippets\n\n> Rate"), "{prices}");
    assert!(prices.contains(&format!("](pdfcraft://open?sha256={sha}&page=1)")), "{prices}");
    assert!(toast(&h).contains("Prices"), "{}", toast(&h));
    assert!(read(&dir, "index.md").contains("- [[Prices]]"));
    assert!(read(&dir, "log.md").contains("- [[Prices]] (new page)"));
    open_wiki(&mut h, 2);
    assert_eq!(h.state().wiki.page_shown(), Some("Prices.md"), "the page it went on shows");
    // Undo takes the new page away again.
    h.get_by_label("< All pages").click();
    h.run_steps(2);
    h.get_by_label("Undo last changes").click();
    h.run_steps(3);
    assert!(!dir.join("Prices.md").exists());
    assert!(!read(&dir, "index.md").contains("Prices"));
    open_wiki(&mut h, 1);
    // On a page that is there, into its snippets section, the rest kept.
    assert!(h.state_mut().execute("wiki.add_selection"));
    h.run_steps(3);
    h.get_by_label("Rates").click();
    h.run_steps(3);
    let rates = read(&dir, "Rates.md");
    assert!(rates.starts_with("# Rates\n\nWhat banks charge.\n\n## Notes\n\nMine.\n\n## Snippets\n\n> Rate"), "{rates}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_assistants_proposal_changes_only_what_is_kept() {
    let dir = temp("proposal");
    std::fs::write(dir.join("Rates.md"), "# Rates\n\nWhat banks charge.\n").unwrap();
    let queue = Arc::new(Queue::default());
    let mut first = String::from(
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\n",
    );
    first += &tool(0, "t1", "propose_page", r#"{"title":"Spring costs","text":"Costs in spring.","reason":"a topic"}"#);
    first += &tool(
        1,
        "t2",
        "propose_snippet",
        r#"{"page":"Rates","quote":"Rates rose three percent in spring","source":"pdfcraft-doc:i0","reason":"the fact"}"#,
    );
    first += &tool(2, "t3", "propose_link", r#"{"page":"Rates","to":"Ghost","reason":"r"}"#);
    first += &tool(3, "t4", "propose_text", r#"{"page":"Rates","text":"They rose in spring.","reason":"r"}"#);
    first += "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":50}}\n\ndata: {\"type\":\"message_stop\"}\n\n";
    let second = "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Quoted the rise.\"}}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\ndata: {\"type\":\"message_stop\"}\n\n";
    *queue.queue.lock().unwrap() = vec![first, second.to_owned()];
    let mut h = harness(Some(dir.clone()), Some(queue.clone()));
    open_wiki(&mut h, 1);
    h.get_by_label("From this page").click();
    support::wait_until(&mut h, "the proposal", |app| app.wiki.has_review() || !app.wiki.running() && app.toast.is_some());
    h.run_steps(2);
    assert!(h.state().wiki.has_review(), "toast: {}", toast(&h));
    assert_eq!(read(&dir, "Rates.md"), "# Rates\n\nWhat banks charge.\n", "nothing is written before Apply");
    // What was sent: the page being read, the wiki's pages and tools, the key in its header only.
    let sent = queue.seen.lock().unwrap()[0].clone();
    let body = String::from_utf8_lossy(&sent.body).into_owned();
    assert!(body.contains("Rates rose three percent in spring") && body.contains("- Rates (0 snippets, 0 links)"), "{body}");
    assert!(body.contains("\"propose_snippet\"") && !body.contains("sk-ant-ui-test"));
    // The link to a page that isn't there went back to the model, not into the proposal.
    let second_body = String::from_utf8_lossy(&queue.seen.lock().unwrap()[1].body).into_owned();
    assert!(second_body.contains("there is no page Ghost"), "{second_body}");
    h.get_by_label("Quoted the rise.");
    h.get_by_label("New page “Spring costs”: Costs in spring.").click();
    h.run_steps(2);
    h.get_by_label("Apply 2").click();
    h.run_steps(3);
    let rates = read(&dir, "Rates.md");
    assert!(
        rates.starts_with(
            "# Rates\n\nWhat banks charge.\n\nThey rose in spring.\n\n## Snippets\n\n> Rates rose three percent in spring\n> — [rates.pdf, p. 1]"
        ),
        "{rates}"
    );
    assert!(!dir.join("Spring costs.md").exists(), "the change not kept isn't made");
    assert!(read(&dir, "log.md").contains("assistant's changes"));
    assert!(!h.state().wiki.has_review());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn answers_read_the_wiki_and_a_citation_of_it_opens_the_page() {
    let dir = temp("answers");
    std::fs::write(dir.join("Inflation.md"), "# Inflation\n\nCosts fell because inflation slowed in the autumn.\n").unwrap();
    let queue = Arc::new(Queue::default());
    let mut h = harness(Some(dir.clone()), Some(queue.clone()));
    open_wiki(&mut h, 1);
    h.state_mut().choose_right_panel(None);
    h.run_steps(2);
    assert!(h.state_mut().execute("ai.ask"));
    h.run_steps(3);
    h.state_mut().assistant.draft = "Why did costs fall in the autumn?".into();
    h.run_steps(2);
    h.get_by_label("Ask").click();
    support::wait_until(&mut h, "the answer", |app| {
        app.assistant.conversations.values().any(|c| c.turns.last().is_some_and(|t| t.state != pdfcraft_ui_egui::AssistantTurnState::Streaming))
    });
    h.run_steps(3);
    let body = String::from_utf8_lossy(&queue.seen.lock().unwrap()[0].body).into_owned();
    let id = SourceId::wiki_page("Inflation.md").encode();
    assert!(body.contains(&id) && body.contains("Wiki: Inflation") && body.contains("inflation slowed"), "{body}");
    // The citation chip opens the wiki page beside the document.
    h.get_by_label("[1]").click();
    h.run_steps(3);
    assert_eq!(h.state().right, Some(RightPanel::Wiki));
    assert_eq!(h.state().wiki.page_shown(), Some("Inflation.md"));
    // Turned off, answers leave the wiki out.
    h.state_mut().assistant.prefs.use_wiki = false;
    h.state_mut().choose_right_panel(Some(RightPanel::Assistant));
    h.run_steps(2);
    h.state_mut().assistant.draft = "And in spring?".into();
    h.run_steps(2);
    h.get_by_label("Ask").click();
    // The request is sent from the assistant's worker: wait until it has been.
    let sent = support::step_until(&mut h, |_| queue.seen.lock().unwrap().len() >= 2);
    assert!(sent, "the second question is sent");
    let body = String::from_utf8_lossy(&queue.seen.lock().unwrap()[1].body).into_owned();
    // The first question's wiki passage is replayed with the conversation; none is added.
    assert_eq!(body.matches("Wiki: Inflation").count(), 1, "{body}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn undo_leaves_a_page_that_changed_since_as_it_is() {
    let dir = temp("undo");
    std::fs::write(dir.join("Rates.md"), "# Rates\n").unwrap();
    let mut h = harness(Some(dir.clone()), None);
    open_wiki(&mut h, 1);
    h.state_mut().views[0].select_text(0, 0, 4);
    h.run_steps(2);
    assert!(h.state_mut().execute("wiki.add_selection"));
    h.run_steps(3);
    h.get_by_label("Rates").click();
    h.run_steps(3);
    assert!(read(&dir, "Rates.md").contains("> Rate"));
    // Edited in another editor after the change.
    let edited = format!("{}\nMy own line.\n", read(&dir, "Rates.md"));
    std::fs::write(dir.join("Rates.md"), &edited).unwrap();
    open_wiki(&mut h, 1);
    h.state_mut().wiki.page = None;
    h.run_steps(2);
    h.get_by_label("Undo last changes").click();
    h.run_steps(3);
    assert_eq!(read(&dir, "Rates.md"), edited, "left as it is");
    assert!(toast(&h).contains("changed since"), "{}", toast(&h));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_index_page_of_ones_own_is_never_written_over() {
    let dir = temp("own-index");
    std::fs::write(dir.join("index.md"), "# My index\n\nHand-made.\n").unwrap();
    std::fs::write(dir.join("Rates.md"), "# Rates\n").unwrap();
    let mut h = harness(Some(dir.clone()), None);
    open_wiki(&mut h, 1);
    h.state_mut().views[0].select_text(0, 0, 4);
    h.run_steps(2);
    assert!(h.state_mut().execute("wiki.add_selection"));
    h.run_steps(3);
    h.get_by_label("Rates").click();
    h.run_steps(3);
    assert!(read(&dir, "Rates.md").contains("> Rate"), "the change is made");
    assert_eq!(read(&dir, "index.md"), "# My index\n\nHand-made.\n", "the user's own index is left alone");
    // One PdfCraft wrote is kept up to date. (The page shown grows once the wiki is read again:
    // wait for that, so the button is where it is clicked.)
    open_wiki(&mut h, 1);
    std::fs::remove_file(dir.join("index.md")).unwrap();
    h.get_by_label("Undo last changes").click();
    h.run_steps(3);
    assert!(read(&dir, "index.md").contains("pdfcraft:index") && read(&dir, "index.md").contains("- [[Rates]]"));
    let _ = std::fs::remove_dir_all(&dir);
}
