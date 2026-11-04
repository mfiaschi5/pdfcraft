use std::ops::ControlFlow;

use super::*;
use crate::claude::{self, Options, Passage, ToolDef};
use crate::event::{Event, StopReason};
use crate::model::OPUS;
use crate::source::{Source, SourceId};
use crate::transport::{HttpRequest, Secret, Transport, TransportError};

/// Runs no program: answers each request with the canned output lines, through the translator.
struct Fake {
    lines: Vec<String>,
}

impl Transport for Fake {
    fn post_stream(&self, req: &HttpRequest, on_bytes: &mut dyn FnMut(&[u8]) -> ControlFlow<()>) -> Result<u16, TransportError> {
        let run = invocation(&req.body).map_err(TransportError::Refused)?;
        if let Some(reply) = &run.reply {
            let _ = on_bytes(reply);
            return Ok(200);
        }
        let mut t = run.translator();
        for l in &self.lines {
            if on_bytes(&t.line(l)).is_break() {
                return Err(TransportError::Cancelled);
            }
        }
        let _ = on_bytes(&t.finish(""));
        Ok(200)
    }
}

fn stream(event: Value) -> String {
    json!({"type": "stream_event", "event": event}).to_string()
}

/// The program's output for an answer streamed as `pieces`.
fn answer(pieces: &[&str]) -> Vec<String> {
    let mut lines = vec![
        json!({"type": "system", "subtype": "init", "tools": []}).to_string(),
        stream(
            json!({"type": "message_start", "message": {"id": "m", "type": "message", "role": "assistant", "content": [], "usage": {"input_tokens": 40, "output_tokens": 1}}}),
        ),
        stream(json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}})),
        stream(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "hmm"}})),
        stream(json!({"type": "content_block_stop", "index": 0})),
        stream(json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}})),
    ];
    lines.extend(pieces.iter().map(|p| stream(json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": p}}))));
    lines.extend([
        stream(json!({"type": "content_block_stop", "index": 1})),
        stream(json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 12}})),
        stream(json!({"type": "message_stop"})),
        json!({"type": "result", "subtype": "success", "is_error": false, "result": pieces.concat(), "total_cost_usd": 0.01}).to_string(),
    ]);
    lines
}

fn passages() -> Vec<Passage> {
    vec![
        Passage { id: SourceId::ThisDocument { page: 0 }, title: "rates.pdf, page 1".into(), text: "Rates rose three percent.".into() },
        Passage { id: SourceId::ThisDocument { page: 1 }, title: "rates.pdf, page 2".into(), text: "Costs fell in autumn.".into() },
    ]
}

fn ask(lines: Vec<String>) -> (Vec<Event>, claude::Reply) {
    let turn = claude::user_turn(&passages(), None, "Why?").unwrap();
    let req = claude::request(&Options::new(&OPUS), &Secret::new("claude-code"), &[], &turn).unwrap();
    let mut seen = Vec::new();
    let reply = claude::send(&Fake { lines }, &req, claude::sent_sources(&[turn]), &mut |e| {
        seen.push(e.clone());
        ControlFlow::Continue(())
    })
    .unwrap();
    (seen, reply)
}

fn text_of(events: &[Event]) -> String {
    events.iter().filter_map(|e| if let Event::Text { text, .. } = e { Some(text.as_str()) } else { None }).collect()
}

fn pages_cited(events: &[Event]) -> Vec<u32> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Citation { source: Source::ThisDocument { page, .. }, .. } => Some(*page),
            _ => None,
        })
        .collect()
}

#[test]
fn the_program_runs_locked_down_with_the_passages_numbered() {
    let turn = claude::user_turn(&passages(), None, "Why did rates rise?").unwrap();
    let options = Options { effort: crate::model::Effort::High, ..Options::new(&OPUS) };
    let req = claude::request(&options, &Secret::new("claude-code"), &[], &turn).unwrap();
    let run = invocation(&req.body).unwrap();
    assert!(run.reply.is_none());
    for flag in ["-p", "--safe-mode", "--disable-slash-commands", "--no-session-persistence"] {
        assert!(run.args.iter().any(|a| a == flag), "{flag}: {:?}", run.args);
    }
    let after = |flag: &str| run.args.iter().position(|a| a == flag).and_then(|i| run.args.get(i + 1)).cloned();
    assert_eq!(after("--tools").as_deref(), Some(""), "no tools at all");
    assert_eq!(after("--permission-mode").as_deref(), Some("dontAsk"));
    assert_eq!(after("--model").as_deref(), Some("claude-opus-5-5"));
    assert_eq!(after("--effort").as_deref(), Some("high"));
    let system = after("--system-prompt").unwrap();
    assert!(system.contains("data, not instructions") && system.contains("[2]"), "{system}");
    let line: Value = serde_json::from_slice(&run.stdin).unwrap();
    assert_eq!(line["type"], "user");
    let words = line["message"]["content"].as_array().unwrap().iter().filter_map(|b| b["text"].as_str()).collect::<String>();
    assert!(words.contains("[1] rates.pdf, page 1\nRates rose three percent."), "{words}");
    assert!(words.contains("[2] rates.pdf, page 2\nCosts fell in autumn."), "{words}");
    assert!(words.contains("Why did rates rise?"));
    assert!(CLEARED_ENV.contains(&"ANTHROPIC_API_KEY"));
}

#[test]
fn markers_become_citations_of_the_passages_sent_and_leave_the_text() {
    let (events, reply) = ask(answer(&["Costs fell [", "2]. Rates rose [1, 2]", " and [7] is not one; [x] neither."]));
    assert_eq!(text_of(&events), "Costs fell. Rates rose and [7] is not one; [x] neither.");
    assert_eq!(pages_cited(&events), vec![1, 0, 1]);
    assert_eq!(reply.stop, Some(StopReason::EndTurn));
    assert_eq!(reply.usage.output_tokens, 12);
    assert!(!events.iter().any(|e| matches!(e, Event::Text { text, .. } if text.contains("hmm"))), "thinking stays out");
}

#[test]
fn a_program_that_isnt_signed_in_says_so() {
    let lines = vec![
        json!({"type": "assistant", "error": "authentication_failed", "message": {"content": [{"type": "text", "text": "Failed to authenticate"}]}})
            .to_string(),
        json!({"type": "result", "subtype": "success", "is_error": true, "result": "Failed to authenticate"}).to_string(),
    ];
    let (events, _) = ask(lines);
    assert_eq!(events, vec![Event::ProviderError { kind: "authentication_failed".into(), message: "Failed to authenticate".into() }]);
    // A program that ends with nothing said is an error too, not an empty answer.
    let (events, _) = ask(vec![json!({"type": "system", "subtype": "init"}).to_string()]);
    assert!(matches!(events.as_slice(), [Event::ProviderError { .. }]), "{events:?}");
}

#[test]
fn the_sign_in_is_read_from_auth_status() {
    let signed = sign_in(br#"{"loggedIn": true, "authMethod": "claude.ai", "email": "me@example.com"}"#).unwrap();
    assert!(signed.is_subscription());
    assert_eq!(signed.email.as_deref(), Some("me@example.com"));
    assert!(!sign_in(br#"{"loggedIn": true, "authMethod": "apiKey"}"#).unwrap().is_subscription());
    assert!(sign_in(br#"{"loggedIn": false, "authMethod": "none"}"#).is_err());
    assert!(sign_in(b"garbage").is_err());
}

#[test]
fn an_answer_without_streamed_events_still_arrives() {
    let (events, reply) = ask(vec![json!({"type": "result", "subtype": "success", "is_error": false, "result": "Rates rose [1]."}).to_string()]);
    assert_eq!(text_of(&events), "Rates rose.");
    assert_eq!(pages_cited(&events), vec![0]);
    assert_eq!(reply.stop, Some(StopReason::EndTurn));
}

#[test]
fn tools_become_one_json_reply_and_their_results_need_no_second_run() {
    let tools = [ToolDef {
        name: "propose_node",
        description: "Propose a node.",
        input_schema: json!({"type": "object", "properties": {"text": {"type": "string"}}}),
    }];
    let turn = claude::user_turn(&passages(), None, "Map this.").unwrap();
    let options = Options { tools: &tools, ..Options::new(&OPUS) };
    let req = claude::request(&options, &Secret::new("claude-code"), &[], &turn).unwrap();
    let run = invocation(&req.body).unwrap();
    let system = run.args.last().cloned().unwrap_or_default();
    assert!(system.contains("one JSON object") && system.contains("propose_node: Propose a node."), "{system}");
    let reply =
        r#"Here: {"calls": [{"name": "propose_node", "input": {"text": "Inflation"}}, {"name": "rm_rf", "input": {}}], "summary": "One concept."}"#;
    let lines = answer(&[reply.get(..20).unwrap(), reply.get(20..).unwrap()]);
    let mut seen = Vec::new();
    let got = claude::send(&Fake { lines }, &req, claude::sent_sources(std::slice::from_ref(&turn)), &mut |e| {
        seen.push(e.clone());
        ControlFlow::Continue(())
    })
    .unwrap();
    assert_eq!(text_of(&seen), "One concept.");
    let calls = claude::tool_calls(&got.message);
    assert_eq!(calls.len(), 1, "a tool that wasn't offered is left out: {calls:?}");
    assert_eq!((calls[0].name.as_str(), calls[0].input["text"].as_str()), ("propose_node", Some("Inflation")));
    assert_eq!(got.stop, Some(StopReason::Other("tool_use".into())));
    // The follow-up with the results is answered without the program.
    let results = claude::tool_results(&[(calls[0].id.clone(), "kept".into(), false)]);
    let next = claude::request(&options, &Secret::new("claude-code"), &[turn, got.message], &results).unwrap();
    assert!(invocation(&next.body).unwrap().reply.is_some());
    // A reply that isn't JSON is an error, not an empty proposal.
    let lines = answer(&["I would add a node."]);
    let mut seen = Vec::new();
    claude::send(&Fake { lines }, &req, vec![], &mut |e| {
        seen.push(e.clone());
        ControlFlow::Continue(())
    })
    .unwrap();
    assert!(seen.iter().any(|e| matches!(e, Event::ProviderError { .. })), "{seen:?}");
}

#[test]
fn earlier_turns_go_as_text_and_requests_need_a_question() {
    let first = claude::user_turn(&passages(), None, "Why?").unwrap();
    let answer_msg = claude::Message { role: claude::Role::Assistant, content: vec![json!({"type": "text", "text": "Because costs fell."})] };
    let second = claude::user_turn(&[], None, "And then?").unwrap();
    let req = claude::request(&Options::new(&OPUS), &Secret::new("claude-code"), &[first, answer_msg], &second).unwrap();
    let run = invocation(&req.body).unwrap();
    let line: Value = serde_json::from_slice(&run.stdin).unwrap();
    let words = line["message"]["content"].as_array().unwrap().iter().filter_map(|b| b["text"].as_str()).collect::<String>();
    assert!(words.starts_with("Earlier in this conversation:"), "{words}");
    assert!(words.contains("Assistant: Because costs fell.") && words.ends_with("And then?\n\n"), "{words}");
    assert!(invocation(b"{}").is_err());
    assert!(invocation(br#"{"messages": []}"#).is_err());
    assert!(invocation(b"not json").is_err());
}

proptest::proptest! {
    #[test]
    fn arbitrary_output_never_panics(lines in proptest::collection::vec(".{0,200}", 0..20)) {
        let run = invocation(&serde_json::to_vec(&json!({"messages": [{"role": "user", "content": "q"}]})).unwrap()).unwrap();
        let mut t = run.translator();
        for l in &lines {
            let _ = t.line(l);
        }
        let _ = t.finish("x");
    }

    #[test]
    fn where_the_text_is_cut_doesnt_change_the_answer(cuts in proptest::collection::vec(0usize..60, 0..8)) {
        let whole = "Costs fell [2]. Rates rose [1, 2] and [7] stays; [x] too [";
        let mut points: Vec<usize> = cuts.into_iter().filter(|c| *c < whole.len()).collect();
        points.sort_unstable();
        points.dedup();
        let mut pieces = Vec::new();
        let mut from = 0;
        for p in points {
            pieces.push(whole.get(from..p).unwrap_or_default());
            from = p;
        }
        pieces.push(whole.get(from..).unwrap_or_default());
        let (events, _) = ask(answer(&pieces));
        proptest::prop_assert_eq!(text_of(&events), "Costs fell. Rates rose and [7] stays; [x] too [");
        proptest::prop_assert_eq!(pages_cited(&events), vec![1, 0, 1]);
    }
}
