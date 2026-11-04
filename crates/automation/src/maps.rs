//! Concept map tools (ADR "Concept graph" G1, G3): maps kept as `.pdfmap.json` files inside
//! `--root`, read and rewritten atomically by every call, and `ai_map_edit`, which returns the
//! AI's proposal without applying it (`graph_apply` applies the changes the caller keeps).

use std::collections::HashSet;
use std::path::PathBuf;

use pdfcraft_ai::claude::{Options, Passage};
use pdfcraft_ai::model::{self, Effort};
use pdfcraft_ai::source::SourceId;
use pdfcraft_ai::transport::Secret;
use pdfcraft_engine::assistant::{self, Selected};
use pdfcraft_engine::map_assist::{self, EditRequest};
use pdfcraft_graph::proposal::Proposal;
use pdfcraft_graph::{Anchor, Author, Content, EdgeId, Graph, Line, NodeId, Strength, export, layout};
use serde_json::{Value, json};

use super::assistant::Use;
use super::error::{Result, ToolError, failed};
use super::json::Args;
use super::paths::{read_input, write_atomic};
use super::{Automation, file_access};

fn node_json(graph: &Graph, n: &pdfcraft_graph::Node) -> Value {
    let kind = match &n.content {
        Content::Text { .. } => "snippet",
        Content::Image { .. } => "picture",
        Content::Note { .. } => "note",
        Content::Concept { .. } => "concept",
    };
    let _ = graph;
    json!({
        "id": n.id.0,
        "type": kind,
        "text": n.content.text(),
        "source": n.anchor.as_ref().map(|a| json!({"title": a.title, "page": u64::from(a.page) + 1, "sha256": a.sha256, "paths": a.paths})),
        "position": n.position,
        "tags": n.tags,
        "author": n.author,
    })
}

fn map_json(graph: &Graph) -> Value {
    json!({
        "title": graph.title,
        "kinds": graph.kinds().iter().map(|k| json!({"id": k.id.0, "name": k.name, "directed": k.directed, "line": k.line.name()})).collect::<Vec<_>>(),
        "nodes": graph.nodes().iter().map(|n| node_json(graph, n)).collect::<Vec<_>>(),
        "connections": graph.edges().iter().map(|e| json!({
            "id": e.id.0, "from": e.from.0, "to": e.to.0,
            "kind": graph.kind(e.kind).map(|k| k.name.as_str()),
            "strength": e.strength.name(), "label": e.label, "author": e.author,
        })).collect::<Vec<_>>(),
    })
}

impl Automation {
    fn map_file(&self, a: &Args, key: &str, need: Use, may_be_missing: bool) -> Result<PathBuf> {
        let access = if need == Use::Change { file_access::Access::Write } else { file_access::Access::Read };
        let path = self.resolve_for(a.str(key)?, access, may_be_missing)?;
        if need == Use::Change && path.exists() {
            // Changing a map reads it first: that needs read access too.
            self.check_roots(&path.to_string_lossy(), &path, file_access::Access::Read)?;
        }
        Ok(path)
    }

    fn load_map(path: &std::path::Path) -> Result<Graph> {
        let bytes = read_input(path)?;
        let (graph, _warnings) = Graph::from_json(&bytes).map_err(|e| failed(format!("{}: {e}", path.display())))?;
        Ok(graph)
    }

    fn save_map(path: &std::path::Path, graph: &Graph) -> Result<()> {
        write_atomic(path, &graph.to_json().map_err(failed)?)
    }

    /// Load the map named by `map`, change it with `edit`, and write it back.
    fn edit_map<T>(&self, a: &Args, edit: impl FnOnce(&mut Graph) -> Result<T>) -> Result<(T, Graph)> {
        let path = self.map_file(a, "map", Use::Change, false)?;
        let mut graph = Self::load_map(&path)?;
        let out = edit(&mut graph)?;
        Self::save_map(&path, &graph)?;
        Ok((out, graph))
    }

    pub(crate) fn graph_create(&mut self, a: &Args) -> Result<Value> {
        let path = self.map_file(a, "map", Use::Change, true)?;
        if path.exists() {
            return Err(failed(format!("{} exists already; graph_show reads it", path.display())));
        }
        let graph = Graph::new(a.opt_str("title")?.unwrap_or("Untitled map"));
        Self::save_map(&path, &graph)?;
        Ok(map_json(&graph))
    }

    pub(crate) fn graph_show(&mut self, a: &Args) -> Result<Value> {
        let path = self.map_file(a, "map", Use::Read, false)?;
        Ok(map_json(&Self::load_map(&path)?))
    }

    pub(crate) fn graph_add_node(&mut self, a: &Args) -> Result<Value> {
        let position = a.nums::<2>("position")?.map(|p| p.map(|v| v as f32)).unwrap_or([0.0, 0.0]);
        let (content, anchor) = match a.str("type")? {
            "concept" => (Content::Concept { label: a.str("text")?.to_owned() }, None),
            "note" => (Content::Note { text: a.str("text")?.to_owned() }, None),
            "snippet" => {
                let id = self.doc(a)?.id;
                let page = self.page(a)?;
                let selected = match a.nums::<4>("rect")? {
                    Some(r) => Selected::Region(r.map(|v| v as f32)),
                    None => Selected::Page,
                };
                let ctx = self.session.assistant_context(id, page, &selected).map_err(failed)?;
                if ctx.text.trim().is_empty() {
                    return Err(failed("there is no text there to take"));
                }
                let text = pdfcraft_ai::text::capped(&ctx.text, pdfcraft_graph::MAX_TEXT_BYTES);
                let sha = self.session.document_sha256(id).map(|h| h.to_hex()).unwrap_or_default();
                let paths = self.doc(a)?.path.clone().into_iter().collect();
                let rect = a.nums::<4>("rect")?.map(|r| r.map(|v| v as f32));
                let anchor =
                    Anchor { sha256: sha, paths, title: ctx.title.clone(), page: ctx.page, rect, quote: pdfcraft_ai::text::capped(&text, 2_000) };
                (Content::Text { text }, Some(anchor))
            }
            other => return Err(ToolError::InvalidArgs(format!("type {other:?} is concept, note or snippet"))),
        };
        let (id, _) = self.edit_map(a, |g| g.add_node(content, anchor, position, Author::User).map_err(failed))?;
        Ok(json!({ "id": id.0 }))
    }

    pub(crate) fn graph_connect(&mut self, a: &Args) -> Result<Value> {
        let from = NodeId(u64::try_from(a.int("from")?).map_err(|_| ToolError::InvalidArgs("from is a node id".into()))?);
        let to = NodeId(u64::try_from(a.int("to")?).map_err(|_| ToolError::InvalidArgs("to is a node id".into()))?);
        let kind = a.str("kind")?.to_owned();
        let strength = match a.opt_str("strength")? {
            None => Strength::Medium,
            Some(s) => Strength::parse(s).ok_or_else(|| ToolError::InvalidArgs("strength is weak, medium or strong".into()))?,
        };
        let label = a.opt_str("label")?.map(str::to_owned);
        let (id, _) = self.edit_map(a, |g| {
            let kind =
                g.kind_named(&kind).map(|k| k.id).ok_or_else(|| failed(format!("no kind of connection named {kind:?} (graph_show lists them)")))?;
            g.add_edge(from, to, kind, strength, label, Author::User).map_err(failed)
        })?;
        Ok(json!({ "id": id.0 }))
    }

    pub(crate) fn graph_add_kind(&mut self, a: &Args) -> Result<Value> {
        let name = a.str("name")?.to_owned();
        let directed = a.opt_bool("directed")?.unwrap_or(true);
        let line = match a.opt_str("line")? {
            None => Line::Solid,
            Some(s) => Line::parse(s).ok_or_else(|| ToolError::InvalidArgs("line is solid, dashed or dotted".into()))?,
        };
        let (id, _) = self.edit_map(a, |g| {
            let colour = g.next_kind_colour();
            g.add_kind(&name, directed, line, colour).map_err(failed)
        })?;
        Ok(json!({ "id": id.0, "name": name.trim() }))
    }

    pub(crate) fn graph_remove(&mut self, a: &Args) -> Result<Value> {
        let node = a.opt_int("node")?.and_then(|n| u64::try_from(n).ok()).map(NodeId);
        let edge = a.opt_int("connection")?.and_then(|n| u64::try_from(n).ok()).map(EdgeId);
        if node.is_none() && edge.is_none() {
            return Err(ToolError::InvalidArgs("give node or connection".into()));
        }
        self.edit_map(a, |g| {
            if let Some(n) = node {
                g.remove_node(n).map_err(failed)?;
            }
            if let Some(e) = edge {
                g.remove_edge(e).map_err(failed)?;
            }
            Ok(())
        })?;
        Ok(json!({ "removed": true }))
    }

    pub(crate) fn graph_layout(&mut self, a: &Args) -> Result<Value> {
        let (steps, _) = self.edit_map(a, |g| Ok(layout::tidy(g, None, 300)))?;
        Ok(json!({ "steps": steps }))
    }

    pub(crate) fn graph_export(&mut self, a: &Args) -> Result<Value> {
        let path = self.map_file(a, "map", Use::Read, false)?;
        let graph = Self::load_map(&path)?;
        let bytes = match a.str("format")? {
            "json_canvas" => serde_json::to_vec_pretty(&export::to_json_canvas(&graph)).map_err(failed)?,
            "graphml" => export::to_graphml(&graph).into_bytes(),
            other => return Err(ToolError::InvalidArgs(format!("format {other:?} is json_canvas or graphml"))),
        };
        let out = self.resolve(a.str("out")?, true)?;
        write_atomic(&out, &bytes)?;
        Ok(json!({ "out": out.to_string_lossy() }))
    }

    pub(crate) fn graph_import(&mut self, a: &Args) -> Result<Value> {
        let canvas = self.resolve(a.str("canvas")?, false)?;
        let path = self.map_file(a, "map", Use::Change, true)?;
        if path.exists() {
            return Err(failed(format!("{} exists already", path.display())));
        }
        let bytes = read_input(&canvas)?;
        let title =
            a.opt_str("title")?.map_or_else(|| canvas.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), str::to_owned);
        let (graph, warnings) = export::from_json_canvas(&bytes, &title).map_err(failed)?;
        Self::save_map(&path, &graph)?;
        Ok(json!({ "map": map_json(&graph), "warnings": warnings }))
    }

    pub(crate) fn graph_apply(&mut self, a: &Args) -> Result<Value> {
        let proposal: Proposal = serde_json::from_value(a.get("proposal").cloned().unwrap_or(Value::Null))
            .map_err(|e| ToolError::InvalidArgs(format!("proposal isn't one ai_map_edit returned: {e}")))?;
        let accept: Vec<usize> = a.ints("accept")?.into_iter().filter_map(|i| usize::try_from(i).ok()?.checked_sub(1)).collect();
        let (applied, _) = self.edit_map(a, |g| Ok(proposal.apply(g, &accept)))?;
        Ok(json!({
            "nodes": applied.nodes.iter().map(|n| n.0).collect::<Vec<_>>(),
            "connections": applied.edges.iter().map(|e| e.0).collect::<Vec<_>>(),
            "skipped": applied.skipped.iter().map(|(i, why)| json!({"change": i + 1, "why": why})).collect::<Vec<_>>(),
        }))
    }

    pub(crate) fn ai_map_edit(&mut self, a: &Args) -> Result<Value> {
        if a.opt_str("library")?.is_some() {
            self.library(a, Use::Read)?;
        }
        let path = self.map_file(a, "map", Use::Read, false)?;
        let dry_run = a.opt_bool("dry_run")?.unwrap_or(false);
        if !dry_run && self.ai_network.is_none() {
            return Err(failed(
                "sending is off: the network is allowed only when pdfcraft-cli is started with --allow-ai-network. Pass dry_run: true to see the request.",
            ));
        }
        let graph = Self::load_map(&path)?;
        let request = a.str("request")?.to_owned();
        let model = match a.opt_str("model")? {
            Some(m) => model::find(m).ok_or_else(|| ToolError::InvalidArgs(format!("unknown model {m:?}")))?,
            None => model::default_model(),
        };
        let effort = match a.opt_str("effort")? {
            None | Some("medium") => Effort::Medium,
            Some("low") => Effort::Low,
            Some("high") => Effort::High,
            Some("xhigh") => Effort::Xhigh,
            Some("max") => Effort::Max,
            Some(other) => return Err(ToolError::InvalidArgs(format!("unknown effort {other:?}"))),
        };
        // The passages it may quote: a page of an open document, and the library's best matches.
        let mut passages: Vec<Passage> = Vec::new();
        let mut doc_anchor: Option<(String, Vec<String>, String)> = None;
        if a.get("doc").is_some() {
            let id = self.doc(a)?.id;
            let page = self.page(a)?;
            let ctx = self.session.assistant_context(id, page, &Selected::Page).map_err(failed)?;
            passages.extend(assistant::context_passages(&ctx));
            let sha = self.session.document_sha256(id).map(|h| h.to_hex()).unwrap_or_default();
            doc_anchor = Some((sha, self.doc(a)?.path.clone().into_iter().collect(), ctx.title));
        }
        let k = a.opt_int("k")?.and_then(|k| usize::try_from(k).ok()).unwrap_or(pdfcraft_ai::library::DEFAULT_RESULTS);
        if k > 0 && (a.opt_str("library")?.is_some() || self.ai.is_some()) {
            let library = self.library(a, Use::Read)?;
            passages.extend(assistant::library_passages(library, &request, None, &[], k));
        }
        let entries: Vec<(String, Vec<String>, String)> = self
            .ai
            .as_ref()
            .map(|l| l.library_entries().iter().map(|e| (e.doc.sha256.to_hex(), e.paths.clone(), e.title.clone())).collect())
            .unwrap_or_default();
        let anchor = move |p: &Passage, quote: &str| -> Option<Anchor> {
            let (sha256, paths, title, page) = match p.id {
                SourceId::ThisDocument { page } => {
                    let (sha, paths, title) = doc_anchor.clone()?;
                    (sha, paths, title, page)
                }
                SourceId::Library { sha256, page } => {
                    let hex = sha256.to_hex();
                    let (_, paths, title) = entries.iter().find(|(h, ..)| *h == hex)?.clone();
                    (hex, paths, title, page)
                }
            };
            Some(Anchor { sha256, paths, title, page, rect: None, quote: pdfcraft_ai::text::capped(quote, 2_000) })
        };
        let scope: Option<HashSet<NodeId>> =
            a.opt_ints("nodes")?.map(|ids| ids.into_iter().filter_map(|i| u64::try_from(i).ok()).map(NodeId).collect());
        let key_value = if dry_run {
            Secret::new("")
        } else {
            self.ai_network
                .as_ref()
                .and_then(|n| (n.key)())
                .ok_or_else(|| failed("no API key: store one in the keychain, or set ANTHROPIC_API_KEY"))?
        };
        let req = EditRequest {
            options: Options { effort, ..Options::new(model) },
            key: &key_value,
            graph: &graph,
            scope: scope.as_ref(),
            passages,
            request: &request,
            anchor: &anchor,
        };
        if dry_run {
            let tools = map_assist::tools(&graph);
            let request = map_assist::first_request(&req, &tools).map_err(failed)?;
            return Ok(json!({ "sent": false, "request": request.preview() }));
        }
        let network = self.ai_network.clone().ok_or_else(|| failed("sending is off"))?;
        let edit = map_assist::propose(network.transport.as_ref(), req, &mut |_| std::ops::ControlFlow::Continue(())).map_err(failed)?;
        let numbered: Vec<Value> =
            edit.proposal.changes.iter().enumerate().map(|(i, p)| json!({"change": i + 1, "what": p.change, "reason": p.reason})).collect();
        Ok(json!({
            "sent": true,
            "summary": edit.summary,
            "changes": numbered,
            "proposal": edit.proposal,
            "errors": edit.errors,
            "usage": edit.usage,
            "cost_estimate": model::format_dollars(model.cost_nanodollars(&edit.usage)),
            "note": "Nothing was applied. graph_apply {map, proposal, accept: [change numbers]} applies the changes you keep.",
        }))
    }
}
