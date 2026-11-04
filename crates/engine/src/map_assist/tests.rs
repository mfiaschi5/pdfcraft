use std::sync::Mutex;

use pdfcraft_ai::model::OPUS;
use pdfcraft_ai::source::Sha256;
use pdfcraft_ai::transport::TransportError;
use pdfcraft_graph::{Author, KindId};

use super::*;

fn passages() -> Vec<Passage> {
    vec![
        Passage {
            id: SourceId::ThisDocument { page: 2 },
            title: "rates.pdf, page 3".into(),
            text: "Rates rose three percent.\nInflation was high.".into(),
        },
        Passage {
            id: SourceId::Library { sha256: Sha256::of(b"lib"), page: 0 },
            title: "notes.pdf, page 1".into(),
            text: "Wages followed prices.".into(),
        },
    ]
}

fn anchor(p: &Passage, quote: &str) -> Option<Anchor> {
    let (sha256, page) = match p.id {
        SourceId::ThisDocument { page } => ("ab".repeat(32), page),
        SourceId::Library { sha256, page } => (sha256.to_hex(), page),
    };
    Some(Anchor { sha256, paths: vec![], title: p.title.clone(), page, rect: None, quote: quote.to_owned() })
}

fn graph() -> (Graph, NodeId) {
    let mut g = Graph::new("Economy");
    let n = g.add_node(Content::Concept { label: "Monetary policy".into() }, None, [0.0, 0.0], Author::User).unwrap();
    (g, n)
}

fn call(id: &str, name: &str, input: Value) -> ToolCall {
    ToolCall { id: id.into(), name: name.into(), input }
}

#[test]
fn calls_become_checked_proposals() {
    let (g, policy) = graph();
    let mut proposal = Proposal::default();
    let calls = [
        call(
            "1",
            "propose_node",
            json!({"key": "n1", "type": "snippet", "text": "inflation  was high", "source": "pdfcraft-doc:i2", "reason": "the cause"}),
        ),
        call("2", "propose_node", json!({"key": "n2", "type": "snippet", "text": "Rates fell", "source": "pdfcraft-doc:i2", "reason": "x"})),
        call(
            "3",
            "propose_node",
            json!({"key": "n3", "type": "snippet", "text": "Wages followed prices.", "source": "pdfcraft-doc:i9", "reason": "x"}),
        ),
        call("4", "propose_connection", json!({"from": "n1", "to": policy.0.to_string(), "kind": "causes", "strength": "strong", "reason": "r"})),
        call("5", "propose_connection", json!({"from": "n7", "to": "1", "kind": "causes", "strength": "strong", "reason": "r"})),
        call("6", "propose_connection", json!({"from": "n1", "to": "1", "kind": "loves", "strength": "strong", "reason": "r"})),
        call("7", "propose_rewording", json!({"node": policy.0, "text": "Central bank policy", "reason": "clearer"})),
        call("8", "propose_removal", json!({"connection": 99, "reason": "r"})),
        call("9", "launch_rockets", json!({})),
        call("10", "propose_node", json!({"key": "n1", "type": "concept", "text": "dup", "reason": "x"})),
    ];
    let results = take_calls(&calls, &g, &passages(), &mut proposal, &anchor);
    let errors: Vec<&str> = results.iter().filter(|r| r.2).map(|r| r.0.as_str()).collect();
    assert_eq!(errors, ["2", "3", "5", "6", "8", "9", "10"], "{results:?}");
    assert_eq!(proposal.changes.len(), 3);
    // The snippet's anchor comes from the passage it quotes.
    let Change::AddNode { anchor: Some(a), content, .. } = &proposal.changes[0].change else { panic!("{:?}", proposal.changes[0]) };
    assert_eq!((a.page, a.title.as_str()), (2, "rates.pdf, page 3"));
    assert_eq!(content.text(), "inflation  was high");
    assert_eq!(proposal.changes[0].reason, "the cause");
    // Applying it gives the map an assistant node with a source, connected to the policy node.
    let mut applied = g.clone();
    let done = proposal.apply(&mut applied, &[0, 1, 2]);
    assert!(done.skipped.is_empty(), "{:?}", done.skipped);
    assert_eq!(applied.nodes().len(), 2);
    assert_eq!(applied.edges()[0].kind, applied.kind_named("causes").unwrap().id);
    assert_eq!(applied.node(policy).unwrap().content.text(), "Central bank policy");
    assert_ne!(applied.edges()[0].kind, KindId(0));
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
    let (g, policy) = graph();
    let mut first =
        vec![r#"{"type":"message_start","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":100,"output_tokens":1}}}"#.to_owned()];
    first.extend(tool_use(0, "t1", "propose_node", r#"{"key":"n1","type":"concept","text":"Inflation","reason":"named on page 3"}"#));
    first.extend(tool_use(
        1,
        "t2",
        "propose_connection",
        &format!(r#"{{"from":"n1","to":"{}","kind":"causes","strength":"medium","reason":"r"}}"#, policy.0),
    ));
    first.push(r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":30}}"#.to_owned());
    first.push(r#"{"type":"message_stop"}"#.to_owned());
    let second = vec![
        r#"{"type":"message_start","message":{"id":"m2","model":"claude-opus-5-5","usage":{"input_tokens":150,"output_tokens":1}}}"#.to_owned(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#.to_owned(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"I linked inflation to policy."}}"#.to_owned(),
        r#"{"type":"content_block_stop","index":0}"#.to_owned(),
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}"#.to_owned(),
        r#"{"type":"message_stop"}"#.to_owned(),
    ];
    let script = Script { bodies: Mutex::new(vec![sse(&first), sse(&second)]), seen: Mutex::default() };
    let key = Secret::new("sk-ant-test");
    let req = EditRequest {
        options: Options::new(&OPUS),
        key: &key,
        graph: &g,
        scope: None,
        passages: passages(),
        request: "Connect what causes what",
        anchor: &anchor,
    };
    let edit = propose(&script, req, &mut |_| ControlFlow::Continue(())).unwrap();
    assert_eq!(edit.proposal.changes.len(), 2);
    assert_eq!(edit.summary, "I linked inflation to policy.");
    assert_eq!(edit.usage.output_tokens, 42);
    let seen = script.seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "one round for the calls, one for the answer");
    let first_body: Value = serde_json::from_slice(&seen[0].body).unwrap();
    assert_eq!(first_body["tools"].as_array().map(Vec::len), Some(4));
    assert!(first_body.to_string().contains("[1] concept: Monetary policy"), "the map is described");
    let second_body: Value = serde_json::from_slice(&seen[1].body).unwrap();
    let last = second_body["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["content"][0]["type"], "tool_result");
    assert_eq!(last["content"][0]["tool_use_id"], "t1");
    assert_eq!(last["content"][1]["tool_use_id"], "t2");
}
