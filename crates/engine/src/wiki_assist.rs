//! The wiki and the AI assistant (`pdfcraft_wiki`). The assistant reads the pages that bear on a
//! question ([`WikiIndex::passages`]), sent as passages it can cite like a document's; and it
//! proposes changes to the wiki through tools, which become a
//! [`Proposal`](pdfcraft_wiki::Proposal) the user accepts change by change. Nothing is written
//! here.
//!
//! - **Grounded.** A snippet must quote a document passage that was sent, word for word, and name
//!   its source id; its source (document, page) comes from that passage, never from the model's
//!   prose. Wiki pages can't be quoted as snippets.
//! - **Checked as it goes.** Each call is applied to a scratch copy of the wiki, so a call that
//!   wouldn't apply (a page that isn't there, words to replace that aren't) is refused with the
//!   reason, and the model can correct it; later calls see the pages proposed before them.
//! - **Bounded.** At most [`MAX_ROUNDS`] request rounds and
//!   [`MAX_CHANGES`](pdfcraft_wiki::MAX_CHANGES) changes; the list of pages sent is capped.

use std::collections::HashMap;
use std::ops::ControlFlow;

use pdfcraft_ai::claude::{self, Message, Options, Passage, ToolCall, ToolDef};
use pdfcraft_ai::event::Event;
use pdfcraft_ai::library::{Content, Library, NewDocument, PageText};
use pdfcraft_ai::model::Usage;
use pdfcraft_ai::source::{DocRef, Sha256, SourceId};
use pdfcraft_ai::transport::{HttpRequest, Secret, Transport};
use pdfcraft_wiki::{Change, Headings, Proposal, Snippet, Source, Wiki, apply};
use serde_json::{Value, json};

/// Request rounds one edit may take (each round answers the previous round's tool calls).
pub const MAX_ROUNDS: usize = 6;
/// The list of pages sent with an edit (bytes).
const MAX_PAGE_LIST: usize = 32 * 1024;

const INSTRUCTIONS: &str = "You keep the user's personal wiki: Markdown pages, one per topic, that gather what they learn from their documents. Its pages are listed below. Passages from the user's documents and from wiki pages are given as search results, with source ids such as pdfcraft-doc:i3, pdfcraft-lib:<hash>:i3 or pdfcraft-wiki:<hash>.

Make changes only by calling the tools: propose_page, propose_snippet, propose_link, propose_text and propose_replacement. Prefer adding to a page that exists over making a near-duplicate. A snippet quotes a document passage word for word and gives that passage's source id; wiki passages can't be quoted as snippets. Link related pages by their titles. Write in the language the user writes in, in plain sentences, and keep each page about one topic. Give every change a short reason. Nothing you propose changes the wiki until the user accepts it. When you are done, reply with a short summary of what you proposed and why.";

/// The tools the model proposes changes with.
pub fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "propose_page",
            description: "Propose a new page: its title and what it says (Markdown, without the title heading). Use it only for a topic no page covers yet.",
            input_schema: json!({
                "type": "object",
                "properties": {"title": {"type": "string"}, "text": {"type": "string"}, "reason": {"type": "string"}},
                "required": ["title", "text", "reason"]
            }),
        },
        ToolDef {
            name: "propose_snippet",
            description: "Propose quoting a document passage on a page: the page's title, the exact words (copied word for word from one passage), and that passage's source id. note says in a sentence why it matters.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "page": {"type": "string"},
                    "quote": {"type": "string"},
                    "source": {"type": "string"},
                    "note": {"type": "string"},
                    "reason": {"type": "string"}
                },
                "required": ["page", "quote", "source", "reason"]
            }),
        },
        ToolDef {
            name: "propose_link",
            description: "Propose linking a page to another, by their titles; note says how they relate.",
            input_schema: json!({
                "type": "object",
                "properties": {"page": {"type": "string"}, "to": {"type": "string"}, "note": {"type": "string"}, "reason": {"type": "string"}},
                "required": ["page", "to", "reason"]
            }),
        },
        ToolDef {
            name: "propose_text",
            description: "Propose a paragraph added to what a page says.",
            input_schema: json!({
                "type": "object",
                "properties": {"page": {"type": "string"}, "text": {"type": "string"}, "reason": {"type": "string"}},
                "required": ["page", "text", "reason"]
            }),
        },
        ToolDef {
            name: "propose_replacement",
            description: "Propose replacing words on a page (to correct or update it): old must appear exactly once on the page. Snippets keep the document's words.",
            input_schema: json!({
                "type": "object",
                "properties": {"page": {"type": "string"}, "old": {"type": "string"}, "new": {"type": "string"}, "reason": {"type": "string"}},
                "required": ["page", "old", "new", "reason"]
            }),
        },
    ]
}

/// The wiki's pages, searchable: rebuilt when the wiki changes, kept between questions.
#[derive(Default)]
pub struct WikiIndex {
    library: Library,
    /// Page id → (file, title).
    pages: HashMap<Sha256, (String, String)>,
}

impl WikiIndex {
    pub fn build(wiki: &Wiki) -> Self {
        let mut index = Self::default();
        for page in wiki.pages.iter().filter(|p| !p.skipped) {
            let doc = DocRef::of(page.file.as_bytes());
            let new = NewDocument {
                doc,
                path: page.file.clone(),
                title: page.title.clone(),
                pages: 1,
                added_at: 0,
                content: Content::Pages(vec![PageText { page: 0, text: page.text.clone() }]),
            };
            if index.library.add(new).is_ok() {
                index.pages.insert(doc.sha256, (page.file.clone(), page.title.clone()));
            }
        }
        index
    }

    /// The `k` passages of the wiki most about `query`.
    pub fn passages(&self, query: &str, k: usize) -> Vec<Passage> {
        let query = pdfcraft_ai::text::prefix(query, pdfcraft_ai::library::MAX_QUERY_BYTES);
        self.library
            .search(query, k, &[])
            .into_iter()
            .filter_map(|hit| {
                let (_, title) = self.pages.get(&hit.doc)?;
                Some(Passage { id: SourceId::Wiki { page: hit.doc }, title: format!("Wiki: {title}"), text: hit.text })
            })
            .collect()
    }

    /// The file of the page a citation names.
    pub fn file(&self, page: &Sha256) -> Option<&str> {
        self.pages.get(page).map(|(file, _)| file.as_str())
    }
}

/// The pages as the model reads them: one line each, with the start of what it says.
pub fn page_list(wiki: &Wiki) -> String {
    let mut out = String::from("Wiki pages:\n");
    if wiki.pages.is_empty() {
        out.push_str("(none yet)\n");
    }
    for p in &wiki.pages {
        let about = pdfcraft_ai::text::prefix(&p.summary().split_whitespace().collect::<Vec<_>>().join(" "), 160).to_owned();
        let links = p.links().len();
        let snippets = p.snippets().len();
        let line = format!("- {} ({snippets} snippets, {links} links): {about}\n", p.title);
        if out.len().saturating_add(line.len()) > MAX_PAGE_LIST {
            out.push_str("(more pages left out)\n");
            break;
        }
        out.push_str(&line);
    }
    out
}

/// The first user turn: the passages, the instructions, the pages, then the request.
pub fn first_turn(passages: &[Passage], wiki: &Wiki, request: &str) -> Result<Message, String> {
    let mut turn = claude::user_turn(passages, None, request).map_err(|e| e.to_string())?;
    let at = turn.content.len().saturating_sub(1);
    turn.content.insert(at, json!({"type": "text", "text": format!("{INSTRUCTIONS}\n\n{}", page_list(wiki))}));
    Ok(turn)
}

fn words(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn str_arg<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| format!("{key} is missing"))
}

fn opt_arg(input: &Value, key: &str) -> Option<String> {
    input.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

/// What one edit came back with.
#[derive(Debug, Default)]
pub struct WikiEdit {
    pub proposal: Proposal,
    /// The model's closing words.
    pub summary: String,
    pub usage: Usage,
    pub errors: Vec<String>,
}

/// Everything [`propose`] needs besides the transport.
pub struct EditRequest<'a> {
    pub options: Options<'a>,
    pub key: &'a Secret,
    pub wiki: &'a Wiki,
    pub passages: Vec<Passage>,
    pub request: &'a str,
    pub headings: Headings,
    /// A snippet's source from the document passage it quotes; `None` when it can't be cited.
    pub source: &'a dyn Fn(&Passage) -> Option<Source>,
}

/// Turn the model's tool calls into proposed changes, and the answers it gets back: (call id,
/// result, is error). `scratch` is the wiki with the changes proposed so far.
pub fn take_calls(calls: &[ToolCall], scratch: &mut Wiki, req: &EditRequest<'_>, proposal: &mut Proposal) -> Vec<(String, String, bool)> {
    calls
        .iter()
        .map(|call| match take_call(call, scratch, req, proposal) {
            Ok(text) => (call.id.clone(), text, false),
            Err(e) => (call.id.clone(), e, true),
        })
        .collect()
}

fn take_call(call: &ToolCall, scratch: &mut Wiki, req: &EditRequest<'_>, proposal: &mut Proposal) -> Result<String, String> {
    if proposal.changes.len() >= pdfcraft_wiki::MAX_CHANGES {
        return Err("no more changes can be proposed in one go".into());
    }
    let input = &call.input;
    let reason = input.get("reason").and_then(Value::as_str).unwrap_or_default();
    let change = match call.name.as_str() {
        "propose_page" => Change::NewPage { title: str_arg(input, "title")?.to_owned(), text: opt_arg(input, "text").unwrap_or_default() },
        "propose_snippet" => {
            let quote = str_arg(input, "quote")?;
            let source = str_arg(input, "source")?;
            let id = SourceId::parse(source).ok_or_else(|| format!("{source} isn't a source id"))?;
            if matches!(id, SourceId::Wiki { .. }) {
                return Err("a snippet quotes a document; wiki pages can't be quoted".into());
            }
            let passage = req
                .passages
                .iter()
                .filter(|p| p.id == id)
                .find(|p| words(&p.text).contains(&words(quote)))
                .ok_or_else(|| format!("the quote isn't word for word in a passage with the source id {source}"))?;
            let source = (req.source)(passage).ok_or_else(|| "that passage's document can't be cited".to_owned())?;
            Change::AddSnippet {
                page: str_arg(input, "page")?.to_owned(),
                snippet: Snippet { quote: quote.to_owned(), source },
                note: opt_arg(input, "note"),
            }
        }
        "propose_link" => {
            let to = str_arg(input, "to")?;
            if scratch.find(to).is_none() {
                return Err(format!("there is no page {to}; propose it first"));
            }
            Change::AddLink { page: str_arg(input, "page")?.to_owned(), to: to.to_owned(), note: opt_arg(input, "note") }
        }
        "propose_text" => Change::AddText { page: str_arg(input, "page")?.to_owned(), text: str_arg(input, "text")?.to_owned() },
        "propose_replacement" => Change::Replace {
            page: str_arg(input, "page")?.to_owned(),
            old: input.get("old").and_then(Value::as_str).unwrap_or_default().to_owned(),
            new: input.get("new").and_then(Value::as_str).unwrap_or_default().to_owned(),
        },
        other => return Err(format!("there is no tool {other}")),
    };
    let applied = apply(scratch, &change, &req.headings).map_err(|e| e.to_string())?;
    scratch.set(&applied.file, applied.after);
    proposal.push(change, reason);
    Ok(format!("proposed as change {}", proposal.changes.len()))
}

/// The first request of an edit, as a dry run shows it.
pub fn first_request(req: &EditRequest<'_>) -> Result<HttpRequest, String> {
    let tools = tools();
    let turn = first_turn(&req.passages, req.wiki, req.request)?;
    let options = Options { tools: &tools, ..req.options.clone() };
    claude::request(&options, req.key, &[], &turn).map_err(|e| e.to_string())
}

/// Ask the model for changes, answering its tool calls until it is done (or [`MAX_ROUNDS`]).
/// `on_event` sees every event (the answer streaming in); `Break` cancels.
pub fn propose(transport: &dyn Transport, req: &EditRequest<'_>, on_event: &mut dyn FnMut(&Event) -> ControlFlow<()>) -> Result<WikiEdit, String> {
    let tools = tools();
    let options = Options { tools: &tools, ..req.options.clone() };
    let mut out = WikiEdit::default();
    let mut scratch = req.wiki.clone();
    let mut history: Vec<Message> = Vec::new();
    let mut turn = first_turn(&req.passages, req.wiki, req.request)?;
    for _ in 0..MAX_ROUNDS {
        let request = claude::request(&options, req.key, &history, &turn).map_err(|e| e.to_string())?;
        let mut all = history.clone();
        all.push(turn.clone());
        let mut text = String::new();
        let reply = claude::send(transport, &request, claude::sent_sources(&all), &mut |e| {
            match e {
                Event::Text { text: t, .. } => text.push_str(t),
                Event::ProviderError { kind, message } => out.errors.push(format!("{kind}: {message}")),
                _ => {}
            }
            on_event(e)
        })
        .map_err(|e| e.to_string())?;
        out.usage = out.usage.plus(&reply.usage);
        if !text.trim().is_empty() {
            out.summary = text;
        }
        if reply.status != 200 {
            break;
        }
        let calls = claude::tool_calls(&reply.message);
        history.push(turn);
        history.push(reply.message);
        if calls.is_empty() {
            break;
        }
        let results = take_calls(&calls, &mut scratch, req, &mut out.proposal);
        turn = claude::tool_results(&results);
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
