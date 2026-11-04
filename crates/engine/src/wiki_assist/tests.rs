use std::sync::Mutex;

use pdfcraft_ai::model::OPUS;
use pdfcraft_ai::transport::TransportError;

use super::*;

fn passages() -> Vec<Passage> {
    vec![
        Passage {
            id: SourceId::ThisDocument { page: 2 },
            title: "rates.pdf, page 3".into(),
            text: "Rates rose three percent.\nInflation was high.".into(),
        },
        Passage { id: SourceId::wiki_page("Inflation.md"), title: "Wiki: Inflation".into(), text: "Inflation is prices rising.".into() },
    ]
}

fn source(p: &Passage) -> Option<Source> {
    match p.id {
        SourceId::ThisDocument { page } => Some(Source { title: "rates.pdf".into(), sha256: "ab".repeat(32), page, path: None }),
        _ => None,
    }
}

fn wiki() -> Wiki {
    Wiki::read(vec![
        ("Inflation.md".into(), b"# Inflation\n\nInflation is prices rising.\n".to_vec()),
        ("Rates.md".into(), b"# Interest rates\n\nWhat banks charge. What banks charge.\n".to_vec()),
    ])
}

fn call(id: &str, name: &str, input: Value) -> ToolCall {
    ToolCall { id: id.into(), name: name.into(), input }
}

fn request<'a>(wiki: &'a Wiki, key: &'a Secret) -> EditRequest<'a> {
    EditRequest {
        options: Options::new(&OPUS),
        key,
        wiki,
        passages: passages(),
        request: "Add what matters on page 3",
        headings: Headings::default(),
        source: &source,
    }
}

#[test]
fn calls_become_checked_proposals_that_later_calls_build_on() {
    let w = wiki();
    let key = Secret::new("k");
    let req = request(&w, &key);
    let mut scratch = w.clone();
    let mut proposal = Proposal::default();
    let calls = [
        call("1", "propose_page", json!({"title": "Central banks", "text": "They set rates.", "reason": "a new topic"})),
        call(
            "2",
            "propose_snippet",
            json!({"page": "Central banks", "quote": "rates rose  three percent", "source": "pdfcraft-doc:i2", "note": "The rise.", "reason": "evidence"}),
        ),
        call("3", "propose_snippet", json!({"page": "Central banks", "quote": "Rates fell.", "source": "pdfcraft-doc:i2", "reason": "x"})),
        call(
            "4",
            "propose_snippet",
            json!({"page": "Inflation", "quote": "prices rising", "source": SourceId::wiki_page("Inflation.md").encode(), "reason": "x"}),
        ),
        call("5", "propose_snippet", json!({"page": "Inflation", "quote": "Inflation was high.", "source": "pdfcraft-doc:i9", "reason": "x"})),
        call("6", "propose_link", json!({"page": "Central banks", "to": "Inflation", "note": "what they fight", "reason": "r"})),
        call("7", "propose_link", json!({"page": "Central banks", "to": "Nowhere", "reason": "r"})),
        call("8", "propose_replacement", json!({"page": "Interest rates", "old": "What banks charge.", "new": "x", "reason": "r"})),
        call("9", "propose_text", json!({"page": "Ghost", "text": "x", "reason": "r"})),
        call("10", "propose_page", json!({"title": "inflation", "text": "dup", "reason": "x"})),
        call("11", "rm_rf", json!({})),
        call("12", "propose_text", json!({"page": "Interest rates", "text": "They rose in 2026.", "reason": "r"})),
    ];
    let results = take_calls(&calls, &mut scratch, &req, &mut proposal);
    let refused: Vec<(&str, &str)> = results.iter().filter(|r| r.2).map(|r| (r.0.as_str(), r.1.as_str())).collect();
    let ids: Vec<&str> = refused.iter().map(|r| r.0).collect();
    assert_eq!(ids, ["3", "4", "5", "7", "8", "9", "10", "11"], "{refused:?}");
    assert!(refused.iter().any(|(id, why)| *id == "4" && why.contains("wiki pages can't be quoted")));
    assert!(refused.iter().any(|(id, why)| *id == "8" && why.contains("more than once")), "the reason goes back to the model");
    assert_eq!(proposal.changes.len(), 4);
    // The snippet's source is the passage's, and it went on the page proposed just before.
    let Change::AddSnippet { snippet, page, note } = &proposal.changes[1].change else { panic!("{:?}", proposal.changes[1]) };
    assert_eq!(
        (page.as_str(), snippet.source.page, snippet.source.sha256.as_str(), note.as_deref()),
        ("Central banks", 2, "ab".repeat(32).as_str(), Some("The rise."))
    );
    assert_eq!(snippet.quote, "rates rose  three percent");
    let made = scratch.find("Central banks").unwrap();
    assert_eq!(made.snippets().len(), 1);
    assert_eq!(made.links(), ["Inflation"]);
    assert!(w.find("Central banks").is_none(), "the wiki itself is untouched");
}

#[test]
fn the_pages_about_a_question_are_passages_it_can_cite() {
    let w = wiki();
    let index = WikiIndex::build(&w);
    let found = index.passages("why are prices rising", 3);
    assert_eq!(found.first().map(|p| p.title.as_str()), Some("Wiki: Inflation"));
    let SourceId::Wiki { page } = found[0].id else { panic!("{:?}", found[0].id) };
    assert_eq!(index.file(&page), Some("Inflation.md"));
    assert_eq!(SourceId::parse(&found[0].id.encode()), Some(found[0].id));
    assert!(index.passages("", 3).is_empty());
    assert!(WikiIndex::build(&Wiki::default()).passages("anything", 3).is_empty());
    let list = page_list(&w);
    assert!(list.contains("- Inflation (0 snippets, 0 links): Inflation is prices rising."), "{list}");
}

/// Answers with each body in turn, and keeps the requests.
struct Script {
    bodies: Mutex<Vec<String>>,
    seen: Mutex<Vec<HttpRequest>>,
}

impl Transport for Script {
    fn post_stream(&self, req: &HttpRequest, on_bytes: &mut dyn FnMut(&[u8]) -> ControlFlow<()>) -> Result<u16, TransportError> {
        self.seen.lock().unwrap().push(req.clone());
        let body = self.bodies.lock().unwrap().remove(0);
        for piece in body.as_bytes().chunks(11) {
            if on_bytes(piece).is_break() {
                return Err(TransportError::Cancelled);
            }
        }
        Ok(200)
    }
}

fn sse(lines: &[String]) -> String {
    lines.iter().map(|l| format!("data: {l}\n\n")).collect()
}

fn tool_use(index: u32, id: &str, name: &str, input: &str) -> Vec<String> {
    vec![
        format!(r#"{{"type":"content_block_start","index":{index},"content_block":{{"type":"tool_use","id":"{id}","name":"{name}","input":{{}}}}}}"#),
        format!(
            r#"{{"type":"content_block_delta","index":{index},"delta":{{"type":"input_json_delta","partial_json":{}}}}}"#,
            serde_json::to_string(input).unwrap()
        ),
        format!(r#"{{"type":"content_block_stop","index":{index}}}"#),
    ]
}

#[test]
fn an_edit_runs_the_tool_loop_and_returns_a_proposal() {
    let w = wiki();
    let key = Secret::new("sk-ant-test");
    let mut first =
        vec![r#"{"type":"message_start","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":100,"output_tokens":1}}}"#.to_owned()];
    first.extend(tool_use(
        0,
        "t1",
        "propose_snippet",
        r#"{"page":"Inflation","quote":"Inflation was high.","source":"pdfcraft-doc:i2","reason":"evidence"}"#,
    ));
    first.extend(tool_use(1, "t2", "propose_link", r#"{"page":"Inflation","to":"Interest rates","reason":"r"}"#));
    first.push(r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":30}}"#.to_owned());
    first.push(r#"{"type":"message_stop"}"#.to_owned());
    let second = vec![
        r#"{"type":"message_start","message":{"id":"m2","model":"claude-opus-5-5","usage":{"input_tokens":150,"output_tokens":1}}}"#.to_owned(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#.to_owned(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"I quoted page 3 on Inflation."}}"#.to_owned(),
        r#"{"type":"content_block_stop","index":0}"#.to_owned(),
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}"#.to_owned(),
        r#"{"type":"message_stop"}"#.to_owned(),
    ];
    let script = Script { bodies: Mutex::new(vec![sse(&first), sse(&second)]), seen: Mutex::default() };
    let edit = propose(&script, &request(&w, &key), &mut |_| ControlFlow::Continue(())).unwrap();
    assert_eq!(edit.proposal.changes.len(), 2);
    assert_eq!(edit.summary, "I quoted page 3 on Inflation.");
    assert_eq!(edit.usage.output_tokens, 42);
    let seen = script.seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "one round for the calls, one for the answer");
    let body: Value = serde_json::from_slice(&seen[0].body).unwrap();
    assert_eq!(body["tools"].as_array().map(Vec::len), Some(5));
    assert!(body.to_string().contains("- Interest rates (0 snippets, 0 links)"), "the pages are listed");
    assert_eq!(first_request(&request(&w, &key)).unwrap().body, seen[0].body, "a dry run shows what is sent");
}
