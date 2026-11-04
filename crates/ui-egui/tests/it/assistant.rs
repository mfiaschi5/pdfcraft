//! The AI assistant in the app (M13.A8–A11), with a canned transport: nothing reaches a network.
//! It covers being off by default, consent asked once, the preview before sending, an answer
//! streaming in with citations that go to their page, Save as comment, the selection bar's Ask,
//! and the key never being written with the settings.

use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ai::transport::{HttpRequest, Secret, Transport, TransportError};
use pdfcraft_platform::assistant::{SecretStore, SessionKeys};
use pdfcraft_ui_egui::{AiService, PdfCraftApp, RightPanel};

use crate::support;

/// Two pages, one line each.
pub(crate) fn fixture() -> Vec<u8> {
    let page = |text: &str| format!("BT /F1 18 Tf 30 250 Td ({text}) Tj ET");
    let (a, b) = (page("Rates rose three percent in spring."), page("Costs fell in the autumn."));
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [4 0 R 6 0 R] /Count 2 /MediaBox [0 0 300 300] >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /Contents 5 0 R /Resources << /Font << /F1 3 0 R >> >> >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{a}\nendstream", a.len()),
        "<< /Type /Page /Parent 2 0 R /Contents 7 0 R /Resources << /Font << /F1 3 0 R >> >> >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{b}\nendstream", b.len()),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

/// Answers every request with `body` and keeps the requests.
struct Canned {
    body: String,
    seen: Mutex<Vec<HttpRequest>>,
}

impl Transport for Canned {
    fn post_stream(&self, req: &HttpRequest, on_bytes: &mut dyn FnMut(&[u8]) -> std::ops::ControlFlow<()>) -> Result<u16, TransportError> {
        self.seen.lock().unwrap().push(req.clone());
        for piece in self.body.as_bytes().chunks(9) {
            if on_bytes(piece).is_break() {
                return Err(TransportError::Cancelled);
            }
        }
        Ok(200)
    }
}

/// An answer citing the passage sent first, `source` naming it, then plain text.
fn answer(source: &str) -> String {
    [
        r#"data: {"type":"message_start","message":{"id":"m","model":"claude-opus-5-5","usage":{"input_tokens":500,"output_tokens":1}}}"#.to_owned(),
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#.to_owned(),
        format!(
            r#"data: {{"type":"content_block_delta","index":0,"delta":{{"type":"citations_delta","citation":{{"type":"search_result_location","cited_text":"Costs fell","source":"{source}","title":"t","search_result_index":0,"start_block_index":0,"end_block_index":1}}}}}}"#
        ),
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Costs fell in autumn."}}"#.to_owned(),
        r#"data: {"type":"content_block_stop","index":0}"#.to_owned(),
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":20}}"#.to_owned(),
        r#"data: {"type":"message_stop"}"#.to_owned(),
    ]
    .iter()
    .map(|l| format!("{l}\n\n"))
    .collect()
}

struct Setup {
    enabled: bool,
    consented: bool,
    preview: bool,
}

fn harness(setup: Setup, transport: Arc<Canned>, keys: Arc<SessionKeys>) -> Harness<'static, PdfCraftApp> {
    let bytes = fixture();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("rates.pdf", None, bytes).unwrap();
        app.ai_service = Some(AiService { transport, keys, env_key: Arc::new(|| None), claude_code: None });
        app.assistant.prefs.enabled = setup.enabled;
        app.assistant.prefs.consented = setup.consented;
        app.assistant.prefs.preview = setup.preview;
        // Never the user's own library.
        app.assistant.library_file = Some(std::env::temp_dir().join(format!("pdfcraft-ui-assistant-{}-library.json", std::process::id())));
        app
    });
    h.run_steps(6);
    h
}

fn keyed() -> Arc<SessionKeys> {
    let keys = Arc::new(SessionKeys::default());
    keys.set("anthropic", &Secret::new("sk-ant-ui-test")).unwrap();
    keys
}

fn canned(source: &str) -> Arc<Canned> {
    Arc::new(Canned { body: answer(source), seen: Mutex::default() })
}

fn ask(h: &mut Harness<'static, PdfCraftApp>, question: &str) {
    assert!(h.state_mut().execute("ai.ask"));
    h.run_steps(3);
    h.state_mut().assistant.draft = question.into();
    h.run_steps(2);
    h.get_by_label("Ask").click();
    h.run_steps(3);
}

fn wait_for_answer(h: &mut Harness<'static, PdfCraftApp>) {
    support::wait_until(h, "the answer", |app| {
        app.assistant.conversations.values().any(|c| c.turns.last().is_some_and(|t| t.state != pdfcraft_ui_egui::AssistantTurnState::Streaming))
    });
    h.run_steps(3);
}

#[test]
fn the_assistant_is_off_until_turned_on() {
    let transport = canned("pdfcraft-doc:i0");
    let mut h = harness(Setup { enabled: false, consented: true, preview: false }, transport.clone(), keyed());
    assert!(h.state_mut().execute("ai.ask"));
    h.run_steps(3);
    assert_eq!(h.state().right, Some(RightPanel::Assistant));
    assert!(h.query_by_label_contains("The AI assistant is off").is_some());
    assert!(h.query_by_label("Ask").is_none());
    assert!(transport.seen.lock().unwrap().is_empty());
}

#[test]
fn an_answer_streams_in_with_citations_that_go_to_their_page() {
    let transport = canned("pdfcraft-doc:i1");
    let mut h = harness(Setup { enabled: true, consented: true, preview: false }, transport.clone(), keyed());
    // Asked from page 2, about that page.
    h.state_mut().views[0].go_to_page(1);
    h.run_steps(4);
    ask(&mut h, "What happened to costs?");
    wait_for_answer(&mut h);
    let turn = h.state().assistant.conversations.values().next().unwrap().turns[0].clone();
    assert_eq!(turn.answer_text(), "Costs fell in autumn.");
    assert_eq!(turn.blocks[0].citations.len(), 1, "{turn:?}");
    let seen = transport.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].headers.iter().any(|(n, v)| n == "x-api-key" && v.expose() == "sk-ant-ui-test"));
    let body = String::from_utf8_lossy(&seen[0].body).into_owned();
    assert!(body.contains("pdfcraft-doc:i1") && body.contains("Costs fell in the autumn."), "{body}");
    drop(seen);
    // The citation chip goes to its page.
    h.state_mut().views[0].go_to_page(0);
    h.run_steps(4);
    h.get_by_label("[1]").click();
    h.run_steps(4);
    assert_eq!(h.state().views[0].current, 1);
}

#[test]
fn consent_is_asked_once_before_the_first_request() {
    let transport = canned("pdfcraft-doc:i0");
    let mut h = harness(Setup { enabled: true, consented: false, preview: false }, transport.clone(), keyed());
    ask(&mut h, "Why did rates rise?");
    assert!(h.query_by_label("Send to Anthropic?").is_some(), "the consent prompt");
    assert!(transport.seen.lock().unwrap().is_empty(), "nothing is sent before consent");
    h.get_by_label("Allow and send").click();
    h.run_steps(3);
    wait_for_answer(&mut h);
    assert!(h.state().assistant.prefs.consented);
    assert_eq!(transport.seen.lock().unwrap().len(), 1);
    // The second question doesn't ask again.
    ask(&mut h, "And costs?");
    assert!(h.query_by_label("Send to Anthropic?").is_none());
}

#[test]
fn the_preview_shows_what_is_sent_and_sends_only_on_request() {
    let transport = canned("pdfcraft-doc:i0");
    let mut h = harness(Setup { enabled: true, consented: true, preview: true }, transport.clone(), keyed());
    ask(&mut h, "Why did rates rise?");
    assert!(h.query_by_label("This will be sent").is_some());
    assert!(h.query_by_label_contains("Rates rose three percent").is_some(), "the passage is shown");
    assert!(transport.seen.lock().unwrap().is_empty());
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(transport.seen.lock().unwrap().is_empty(), "cancelled: nothing sent");
    ask(&mut h, "Why did rates rise?");
    h.get_by_label("Send").click();
    h.run_steps(3);
    wait_for_answer(&mut h);
    assert_eq!(transport.seen.lock().unwrap().len(), 1);
}

#[test]
fn the_selection_bar_asks_about_the_selection() {
    let transport = canned("pdfcraft-doc:i0");
    let mut h = harness(Setup { enabled: true, consented: true, preview: true }, transport, keyed());
    h.state_mut().views[0].select_text(0, 0, 4);
    // The bar shows once the page's text layer has arrived.
    assert!(support::step_until(&mut h, |h| h.query_by_label("Ask AI").is_some()), "the selection bar offers Ask AI");
    h.get_by_label("Ask AI").click();
    h.run_steps(3);
    assert_eq!(h.state().right, Some(RightPanel::Assistant));
    let about = h.state().assistant.context.as_ref().map(|c| c.3.clone()).unwrap_or_default();
    assert!(about.contains("selection on page 1"), "{about}");
    h.state_mut().assistant.draft = "What does this mean?".into();
    h.run_steps(2);
    h.get_by_label("Ask").click();
    h.run_steps(3);
    // The preview sends the selection ("Rates"), and the page's text around it.
    assert!(h.query_by_label("This will be sent").is_some(), "toast: {:?}", h.state().toast);
    assert!(h.query_by_label_contains("(the selection)").is_some());
    assert!(h.query_by_label_contains("(around the selection)").is_some());
}

#[test]
fn an_answer_is_saved_as_a_comment() {
    let transport = canned("pdfcraft-doc:i0");
    let mut h = harness(Setup { enabled: true, consented: true, preview: false }, transport, keyed());
    ask(&mut h, "Why did rates rise?");
    wait_for_answer(&mut h);
    h.get_by_label("Save as comment").click();
    h.run_steps(4);
    let id = h.state().views[0].id;
    let doc = h.state().session.get(id).unwrap();
    let note = doc.info.annotations.iter().find(|a| a.subtype == "Text").expect("a note was added");
    let contents = note.contents.clone().unwrap_or_default();
    assert!(contents.contains("Why did rates rise?") && contents.contains("Costs fell in autumn.") && contents.contains("Sources:"), "{contents}");
    assert!(doc.can_undo().is_some());
}

#[test]
fn the_key_is_never_written_with_the_settings() {
    let keys = Arc::new(SessionKeys::default());
    let mut h = harness(Setup { enabled: true, consented: true, preview: false }, canned("pdfcraft-doc:i0"), keys.clone());
    h.state_mut().assistant.key_draft = "sk-ant-secret-from-prefs".into();
    h.state_mut().save_assistant_key();
    assert_eq!(keys.get("anthropic").unwrap().map(|k| k.expose().to_owned()), Some("sk-ant-secret-from-prefs".into()));
    let saved = h.state().persist();
    assert!(!saved.contains("sk-ant-secret-from-prefs"), "{saved}");
    assert!(saved.contains("\"assistant\""), "the other assistant settings are kept");
    // Restored settings keep the switch and drop an unknown model.
    let mut back = PdfCraftApp::new();
    back.restore(&saved.replace("claude-opus-5-5", "not-a-model"));
    assert!(back.assistant.prefs.enabled);
    assert_eq!(back.assistant.prefs.model, "claude-opus-5-5");
}

/// The assistant set to Claude Code: `claude_code` stands in for the program (the real transport
/// is tested in pdfcraft-platform with a fake program), and there is no API key at all.
fn through_claude_code(api: Arc<Canned>, claude_code: Option<Arc<Canned>>) -> Harness<'static, PdfCraftApp> {
    let bytes = fixture();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("rates.pdf", None, bytes).unwrap();
        let link = claude_code.map(|t| pdfcraft_ui_egui::ClaudeCodeLink {
            transport: t,
            program: "/Applications/Claude Code/claude".into(),
            sign_in: Arc::new(|| Ok(pdfcraft_ai::claude_code::SignIn { method: "claude.ai".into(), email: Some("me@example.com".into()) })),
        });
        app.ai_service = Some(AiService { transport: api, keys: Arc::new(SessionKeys::default()), env_key: Arc::new(|| None), claude_code: link });
        app.assistant.prefs.enabled = true;
        app.assistant.prefs.preview = false;
        app.assistant.prefs.provider = pdfcraft_ui_egui::AssistantProvider::ClaudeCode;
        // Asked for, but Claude Code runs with no tools: it is left out.
        app.assistant.prefs.web_search = true;
        app.assistant.library_file = Some(std::env::temp_dir().join(format!("pdfcraft-ui-assistant-{}-cc-library.json", std::process::id())));
        app
    });
    h.run_steps(6);
    h
}

#[test]
fn with_claude_code_the_assistant_answers_without_a_key_and_on_the_plan() {
    let api = canned("pdfcraft-doc:i0");
    let cc = canned("pdfcraft-doc:i0");
    let mut h = through_claude_code(api.clone(), Some(cc.clone()));
    ask(&mut h, "Why did costs fall?");
    // Consent says where it goes and that the plan pays.
    assert!(h.query_by_label_contains("through Claude Code on this computer").is_some(), "toast: {:?}", h.state().toast);
    assert!(h.query_by_label_contains("count against your Claude plan").is_some());
    h.get_by_label("Allow and send").click();
    h.run_steps(3);
    wait_for_answer(&mut h);
    assert_eq!(cc.seen.lock().unwrap().len(), 1, "sent through Claude Code");
    assert!(api.seen.lock().unwrap().is_empty(), "nothing through the API");
    let body: serde_json::Value = serde_json::from_slice(&cc.seen.lock().unwrap()[0].body).unwrap();
    assert!(body.get("tools").is_none(), "no web search: Claude Code runs without tools");
    assert!(h.query_by_label_contains("on your Claude plan").is_some(), "no dollar cost for a plan");
    assert!(h.query_by_label_contains("about $").is_none());
    // Preferences ask the program how it is signed in.
    h.state_mut().check_claude_sign_in();
    support::wait_until(&mut h, "the sign-in answer", |app| app.assistant.claude_sign_in.is_some());
    assert!(h.state().assistant.claude_sign_in.as_ref().is_some_and(|s| s.as_ref().is_ok_and(|s| s.is_subscription())));
}

#[test]
fn without_claude_code_installed_the_panel_says_so_and_sends_nothing() {
    let api = canned("pdfcraft-doc:i0");
    let mut h = through_claude_code(api.clone(), None);
    assert!(h.state_mut().execute("ai.ask"));
    h.run_steps(3);
    assert!(h.query_by_label("Claude Code isn't installed on this computer.").is_some());
    h.state_mut().assistant.draft = "Why?".into();
    h.run_steps(2);
    h.get_by_label("Ask").click();
    h.run_steps(3);
    assert!(api.seen.lock().unwrap().is_empty());
    assert!(h.state().toast.as_ref().is_some_and(|t| t.0.contains("Claude Code isn't installed")), "{:?}", h.state().toast);
}
