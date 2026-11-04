//! The AI assistant in the app (M13.A8–A11): the Assistant panel, its preferences, consent before
//! the first request, the answer streaming in from a worker thread, citations that go to their
//! page, Save as comment, and the personal library.
//!
//! - **Off by default.** Nothing is sent until the user turns the assistant on in Preferences, adds
//!   a key and asks. The first request to the provider asks for consent. By default each request
//!   is shown before it goes ([`AssistantPrefs::preview`]).
//! - **No network code here.** The app binary hands over an [`AiService`] (the transport and the
//!   key store). The web build and tests without one can't send.
//! - **What is sent** comes from `engine::assistant`. A document that forbids copying gives
//!   nothing, and text under unapplied redaction marks is left out.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use pdfcraft_ai::claude::{self, Message, Options, Passage, Reply, WebSearch};
use pdfcraft_ai::claude_code::SignIn;
use pdfcraft_ai::event::{Event, StopReason};
use pdfcraft_ai::library::Library;
use pdfcraft_ai::model::{self, Effort, Usage};
use pdfcraft_ai::source::{Sha256, Source, SourceId};
use pdfcraft_ai::transport::{HttpRequest, Secret, Transport};
use pdfcraft_engine::DocId;
use pdfcraft_engine::assistant::{self, Selected};
use pdfcraft_platform::assistant::SecretStore;

use crate::theme::Tokens;
use crate::{PdfCraftApp, RightPanel, theme};

/// A library document as a map snippet's source names it: its SHA-256 (hex), paths and title.
pub(crate) type SourceDoc = (String, Vec<String>, String);

/// The provider's account name in the key store.
pub const ACCOUNT: &str = "anthropic";
/// The provider's privacy policy, shown when asking for consent.
const PRIVACY_URL: &str = "https://www.anthropic.com/legal/privacy";
/// Questions kept per document (older ones are dropped from the panel and the history).
const MAX_TURNS: usize = 50;
/// Library passages sent with a question.
const LIBRARY_PASSAGES: usize = pdfcraft_ai::library::DEFAULT_RESULTS;

/// What the app binary gives the assistant: the transport, the key store, and the key from the
/// environment, and Claude Code when it is installed. The desktop app builds it; the web build and
/// most tests have none.
#[derive(Clone)]
pub struct AiService {
    pub transport: Arc<dyn Transport>,
    pub keys: Arc<dyn SecretStore>,
    pub env_key: Arc<dyn Fn() -> Option<Secret> + Send + Sync>,
    pub claude_code: Option<ClaudeCodeLink>,
}

/// Claude Code on this computer: the user's own `claude` program, signed in with their Claude
/// subscription (personal use; `pdfcraft_ai::claude_code`).
#[derive(Clone)]
pub struct ClaudeCodeLink {
    pub transport: Arc<dyn Transport>,
    /// Where the program is, shown in Preferences.
    pub program: String,
    /// Ask the program how it is signed in. It runs the program, so not on the UI thread.
    pub sign_in: Arc<dyn Fn() -> Result<SignIn, String> + Send + Sync>,
}

/// How requests reach Claude.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// The Claude API, with the user's API key.
    #[default]
    ApiKey,
    /// The user's own Claude Code, on their Claude subscription.
    ClaudeCode,
}

/// Built into requests sent through Claude Code, which sends no key (the request needs one).
const NO_KEY: &str = "claude-code";

impl AiService {
    /// The key to send with: the stored one, else the environment's.
    pub(crate) fn key(&self) -> Option<Secret> {
        pdfcraft_platform::assistant::api_key(self.keys.as_ref(), ACCOUNT, |_| (self.env_key)().map(|k| k.expose().to_owned()))
    }

    /// The way to Claude the preferences choose: its transport, and the key to build requests
    /// with (Claude Code sends none).
    pub(crate) fn route(&self, prefs: &AssistantPrefs) -> Result<(Arc<dyn Transport>, Secret), String> {
        match prefs.provider {
            Provider::ApiKey => {
                let key = self.key().ok_or_else(|| tl!("No API key yet: add one in Preferences ▸ AI assistant.").to_owned())?;
                Ok((self.transport.clone(), key))
            }
            Provider::ClaudeCode => match &self.claude_code {
                Some(c) => Ok((c.transport.clone(), Secret::new(NO_KEY))),
                None => Err(tl!("Claude Code isn't installed on this computer.").to_owned()),
            },
        }
    }
}

/// Preferences ▸ AI assistant. Persisted with the other settings; the key never is.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AssistantPrefs {
    pub enabled: bool,
    pub model: String,
    pub effort: Effort,
    pub web_search: bool,
    /// Show each request before it is sent.
    pub preview: bool,
    /// The user allowed requests to this provider (asked once).
    pub consented: bool,
    pub provider: Provider,
}

impl Default for AssistantPrefs {
    fn default() -> Self {
        Self {
            enabled: false,
            model: model::default_model().id.to_owned(),
            effort: Effort::default(),
            web_search: false,
            preview: true,
            consented: false,
            provider: Provider::ApiKey,
        }
    }
}

impl AssistantPrefs {
    pub(crate) fn model(&self) -> &'static model::Model {
        model::find(&self.model).unwrap_or_else(model::default_model)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnState {
    Streaming,
    Done,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    index: u64,
    pub text: String,
    pub citations: Vec<Source>,
}

/// One question and its answer.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub question: String,
    /// What it was about ("selection on page 3"), for the panel.
    pub about: String,
    /// The page the question was asked on (0-based): where Save as comment puts the note.
    pub page: usize,
    pub blocks: Vec<Block>,
    pub state: TurnState,
    pub usage: Usage,
    pub stop: Option<StopReason>,
    pub errors: Vec<String>,
    pub searches: Vec<String>,
    pub model: &'static model::Model,
    /// Answered through Claude Code, on the user's plan: no cost to show.
    pub on_plan: bool,
}

impl Turn {
    pub fn answer_text(&self) -> String {
        self.blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join("")
    }
}

/// A document's conversation: what the panel shows, and the messages a follow-up replays.
#[derive(Default)]
pub struct Conversation {
    pub turns: Vec<Turn>,
    history: Vec<Message>,
}

/// A request ready to send, kept while the preview or the consent prompt is open.
pub struct Prepared {
    doc: DocId,
    page: usize,
    about: String,
    question: String,
    request: HttpRequest,
    turn: Message,
    sources: Vec<Option<SourceId>>,
    pub passages: Vec<Passage>,
    pub redacted: usize,
    pub encrypted: bool,
    model: &'static model::Model,
    provider: Provider,
}

enum WorkerMsg {
    Event(Event),
    Done(Result<Reply, String>),
}

struct Running {
    doc: DocId,
    rx: mpsc::Receiver<WorkerMsg>,
    cancel: Arc<AtomicBool>,
    turn: Message,
}

/// Whether a key is set, as last checked (the keychain isn't asked every frame).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyStatus {
    Stored,
    FromEnvironment,
    Missing,
}

#[derive(Default)]
pub struct AssistantState {
    pub prefs: AssistantPrefs,
    pub draft: String,
    /// The selection a question is about, captured when the selection bar's Ask was pressed.
    pub context: Option<(DocId, usize, Selected, String)>,
    pub conversations: HashMap<DocId, Conversation>,
    pub preview: Option<Prepared>,
    pub consent: Option<Prepared>,
    running: Option<Running>,
    pub key_draft: String,
    pub key_status: Option<KeyStatus>,
    /// What Claude Code said when last asked how it is signed in.
    pub claude_sign_in: Option<Result<SignIn, String>>,
    sign_in_rx: Option<mpsc::Receiver<Result<SignIn, String>>>,
    library: Option<Library>,
    pub library_error: Option<String>,
    /// Where the library is kept; `None` is the per-user data folder's (tests set their own).
    pub library_file: Option<std::path::PathBuf>,
    /// A library document being opened for a citation: (path, page, expected hash).
    pub(crate) jump_after_open: Option<(String, usize, Sha256)>,
}

impl AssistantState {
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }
}

/// What the panel asks the app to do.
#[derive(Clone, Debug, PartialEq)]
pub enum PanelAction {
    Ask,
    Send,
    CancelPreview,
    Allow,
    Refuse,
    Stop,
    Jump(Source),
    SaveAsComment(usize),
    OpenPreferences,
    AddToLibrary,
    RemoveFromLibrary(Sha256),
    ClearConversation,
    ClearContext,
    OpenPrivacy,
}

fn library_path() -> Option<std::path::PathBuf> {
    assistant_dir().map(|d| d.join("library.json"))
}

/// `Assistant` in the portable data folder in portable mode, otherwise the per-user data folder
/// (the same places as recovery's).
pub(crate) fn assistant_dir() -> Option<std::path::PathBuf> {
    if let Some(dir) = crate::portable::data_dir() {
        return Some(dir.join("Assistant"));
    }
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(std::path::PathBuf::from);
    if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support/PdfCraft/Assistant"))
    } else if cfg!(windows) {
        env("LOCALAPPDATA").map(|d| d.join("PdfCraft").join("Assistant"))
    } else {
        env("XDG_DATA_HOME").or_else(|| env("HOME").map(|h| h.join(".local/share"))).map(|d| d.join("pdfcraft/assistant"))
    }
}

/// The panel. Reads the state; everything that changes more than the draft is returned.
pub(crate) fn panel(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut AssistantState,
    id: DocId,
    page_count: usize,
    has_service: bool,
    has_claude_code: bool,
) -> Option<PanelAction> {
    let mut action = None;
    crate::widgets::begin_dialog(ui);
    if !has_service {
        ui.label(egui::RichText::new(tl!("The AI assistant needs the desktop app.")).color(t.text_muted));
        return None;
    }
    if !state.prefs.enabled {
        ui.label(egui::RichText::new(tl!("The AI assistant is off. Nothing is sent anywhere until you turn it on.")).color(t.text_muted));
        ui.add_space(6.0);
        if ui.button(tl!("Turn on in Preferences…")).clicked() {
            action = Some(PanelAction::OpenPreferences);
        }
        return action;
    }
    ui.label(egui::RichText::new(state.prefs.model().name).font(theme::medium(12.0)).color(t.text_faint));
    let missing = match state.prefs.provider {
        Provider::ApiKey if state.key_status == Some(KeyStatus::Missing) => Some(tl!("No API key yet: add one in Preferences ▸ AI assistant.")),
        Provider::ClaudeCode if !has_claude_code => Some(tl!("Claude Code isn't installed on this computer.")),
        _ => None,
    };
    if let Some(missing) = missing {
        ui.label(egui::RichText::new(missing).color(t.text_muted));
        if ui.button(tl!("Preferences…")).clicked() {
            action = Some(PanelAction::OpenPreferences);
        }
    }
    ui.add_space(6.0);
    if let Some(conversation) = state.conversations.get(&id) {
        for (i, turn) in conversation.turns.iter().enumerate() {
            if let Some(a) = turn_ui(ui, t, turn, i, page_count) {
                action = Some(a);
            }
            ui.add_space(8.0);
        }
        if !conversation.turns.is_empty() && !state.is_running() && ui.small_button(tl!("Clear conversation")).clicked() {
            action = Some(PanelAction::ClearConversation);
        }
    }
    ui.separator();
    if let Some(prepared) = &state.preview {
        if let Some(a) = preview_ui(ui, t, prepared) {
            action = Some(a);
        }
        return action;
    }
    match state.context.as_ref().filter(|(doc, ..)| *doc == id) {
        Some((_, _, _, about)) => {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(crate::i18n::fmt(tl!("About: {what}"), &[("what", about)])).color(t.text_muted));
                if ui.small_button("×").on_hover_text(tl!("Ask about the current page instead")).clicked() {
                    action = Some(PanelAction::ClearContext);
                }
            });
        }
        None => {
            ui.label(egui::RichText::new(tl!("About: the selection, or the current page")).color(t.text_faint));
        }
    }
    ui.horizontal_wrapped(|ui| {
        for (label, prompt) in [
            (tl!("Explain"), tl!("Explain this in simple terms.")),
            (tl!("Summarize"), tl!("Summarize this.")),
            (tl!("Translate"), tl!("Translate this into English.")),
        ] {
            if ui.small_button(label).clicked() {
                state.draft = prompt.to_owned();
            }
        }
    });
    let edit = ui.add(
        crate::widgets::block(&mut state.draft)
            .hint_text(tl!("Ask a question about this document"))
            .desired_rows(3)
            .desired_width(f32::INFINITY)
            .char_limit(claude::MAX_QUESTION_BYTES / 4),
    );
    let enter = edit.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
    ui.horizontal(|ui| {
        if state.is_running() {
            if ui.button(tl!("Stop")).clicked() {
                action = Some(PanelAction::Stop);
            }
        } else if (ui.add_enabled(!state.draft.trim().is_empty(), egui::Button::new(tl!("Ask"))).clicked() || enter) && !state.draft.trim().is_empty()
        {
            action = Some(PanelAction::Ask);
        }
    });
    ui.add_space(10.0);
    egui::CollapsingHeader::new(tl!("Library")).default_open(false).show(ui, |ui| {
        if let Some(e) = &state.library_error {
            ui.label(egui::RichText::new(e).color(t.text_muted));
        }
        if ui.button(tl!("Add this document to the library")).clicked() {
            action = Some(PanelAction::AddToLibrary);
        }
        match &state.library {
            Some(lib) if !lib.entries().is_empty() => {
                for entry in lib.entries() {
                    ui.horizontal(|ui| {
                        ui.label(&entry.title).on_hover_text(entry.paths.first().map(String::as_str).unwrap_or_default());
                        if ui.small_button("×").on_hover_text(tl!("Remove from the library")).clicked() {
                            action = Some(PanelAction::RemoveFromLibrary(entry.doc.sha256));
                        }
                    });
                }
            }
            _ => {
                ui.label(egui::RichText::new(tl!("The library is empty. Answers can cite the documents you add.")).color(t.text_faint));
            }
        }
    });
    if let Some(prepared) = &state.consent
        && let Some(a) = consent_ui(ui.ctx(), prepared)
    {
        action = Some(a);
    }
    action
}

fn turn_ui(ui: &mut egui::Ui, t: &Tokens, turn: &Turn, index: usize, page_count: usize) -> Option<PanelAction> {
    let mut action = None;
    egui::Frame::NONE.fill(t.card).corner_radius(t.radius).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        ui.label(egui::RichText::new(&turn.question).font(theme::semibold(13.0)));
        ui.label(egui::RichText::new(&turn.about).color(t.text_faint).small());
        for query in &turn.searches {
            ui.label(egui::RichText::new(crate::i18n::fmt(tl!("Searched the web for “{query}”"), &[("query", query)])).color(t.text_faint).small());
        }
        let mut n = 0usize;
        for block in &turn.blocks {
            ui.horizontal_wrapped(|ui| {
                ui.label(&block.text);
                for source in &block.citations {
                    n += 1;
                    let (label, hover) = citation_label(source, n, page_count);
                    if ui.small_button(label).on_hover_text(hover).clicked() {
                        action = Some(PanelAction::Jump(source.clone()));
                    }
                }
            });
        }
        match turn.state {
            TurnState::Streaming => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(egui::RichText::new(tl!("Answering…")).color(t.text_faint));
                });
            }
            TurnState::Stopped => {
                ui.label(egui::RichText::new(tl!("Stopped.")).color(t.text_faint));
            }
            TurnState::Failed | TurnState::Done => {}
        }
        if turn.stop == Some(StopReason::MaxTokens) {
            ui.label(egui::RichText::new(tl!("The answer was cut off at its length limit.")).color(t.text_muted));
        }
        if turn.stop == Some(StopReason::Refusal) {
            ui.label(egui::RichText::new(tl!("The model declined to answer.")).color(t.text_muted));
        }
        for e in &turn.errors {
            ui.label(egui::RichText::new(e).color(t.text_muted));
        }
        if turn.state == TurnState::Done {
            ui.horizontal(|ui| {
                let tokens = turn.usage.prompt_tokens().saturating_add(turn.usage.output_tokens).to_string();
                let line = if turn.on_plan {
                    crate::i18n::fmt(tl!("{tokens} tokens · on your Claude plan"), &[("tokens", &tokens)])
                } else {
                    let model_cost = model::format_dollars(turn.model.cost_nanodollars(&turn.usage));
                    crate::i18n::fmt(tl!("{tokens} tokens · about {cost}"), &[("tokens", &tokens), ("cost", &model_cost)])
                };
                ui.label(egui::RichText::new(line).color(t.text_faint).small());
                if ui.small_button(tl!("Copy")).clicked() {
                    ui.ctx().copy_text(turn.answer_text());
                }
                if ui.small_button(tl!("Save as comment")).clicked() {
                    action = Some(PanelAction::SaveAsComment(index));
                }
            });
        }
    });
    action
}

/// A citation's chip and its tooltip. Pages are shown 1-based.
fn citation_label(source: &Source, n: usize, page_count: usize) -> (String, String) {
    match source {
        Source::ThisDocument { page, quote } => {
            let page = usize::try_from(*page).unwrap_or(usize::MAX);
            let shown = if page < page_count { crate::i18n::fmt(tl!("Page {n}"), &[("n", &(page + 1).to_string())]) } else { String::new() };
            (format!("[{n}]"), format!("{shown}\n“{quote}”"))
        }
        Source::Library { page, quote, .. } => (
            format!("[{n}]"),
            format!("{}\n“{quote}”", crate::i18n::fmt(tl!("Library document, page {n}"), &[("n", &(u64::from(*page) + 1).to_string())])),
        ),
        Source::Web { url, title, quote } => (format!("[{n}]"), format!("{title}\n{url}\n“{quote}”")),
    }
}

fn preview_ui(ui: &mut egui::Ui, t: &Tokens, prepared: &Prepared) -> Option<PanelAction> {
    let mut action = None;
    ui.label(egui::RichText::new(tl!("This will be sent")).font(theme::semibold(13.0)));
    ui.label(
        egui::RichText::new(crate::i18n::fmt(tl!("To {model}, about {what}:"), &[("model", prepared.model.name), ("what", &prepared.about)]))
            .color(t.text_muted),
    );
    for p in &prepared.passages {
        ui.label(egui::RichText::new(&p.title).small().color(t.text_faint));
        ui.label(egui::RichText::new(pdfcraft_ai::text::prefix(&p.text, 400)).small());
    }
    ui.label(egui::RichText::new(crate::i18n::fmt(tl!("Question: {q}"), &[("q", &prepared.question)])).small());
    if prepared.redacted > 0 {
        ui.label(egui::RichText::new(tl!("Text under redaction marks is left out.")).small().color(t.text_muted));
    }
    if prepared.encrypted {
        ui.label(egui::RichText::new(tl!("This document is password-protected.")).small().color(t.text_muted));
    }
    let estimate = prepared.request.body.len() / 4;
    ui.label(egui::RichText::new(crate::i18n::fmt(tl!("About {n} tokens"), &[("n", &estimate.to_string())])).small().color(t.text_faint));
    ui.horizontal(|ui| {
        if ui.button(tl!("Send")).clicked() {
            action = Some(PanelAction::Send);
        }
        if ui.button(tl!("Cancel")).clicked() {
            action = Some(PanelAction::CancelPreview);
        }
    });
    action
}

fn consent_ui(ctx: &egui::Context, prepared: &Prepared) -> Option<PanelAction> {
    let mut action = None;
    let modal = egui::Modal::new(egui::Id::unique("assistant-consent")).show(ctx, |ui| {
        ui.set_max_width(420.0);
        ui.heading(tl!("Send to Anthropic?"));
        let what = match prepared.provider {
            Provider::ApiKey => tl!("Your question, and the text from this document and your library that it is about, will be sent to Anthropic to answer it with {model}. Requests are billed to your API key."),
            Provider::ClaudeCode => tl!("Your question, and the text from this document and your library that it is about, will be sent to Anthropic through Claude Code on this computer, to answer it with {model}. Requests count against your Claude plan."),
        };
        ui.label(crate::i18n::fmt(what, &[("model", prepared.model.name)]));
        if ui.link(tl!("Anthropic's privacy policy")).clicked() {
            action = Some(PanelAction::OpenPrivacy);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(tl!("Allow and send")).clicked() {
                action = Some(PanelAction::Allow);
            }
            if ui.button(tl!("Cancel")).clicked() {
                action = Some(PanelAction::Refuse);
            }
        });
    });
    if modal.should_close() && action.is_none() {
        action = Some(PanelAction::Refuse);
    }
    action
}

impl PdfCraftApp {
    /// The selection bar's Ask: keep the selection as what the question is about, and open the
    /// panel.
    pub(crate) fn ask_about_selection(&mut self, index: usize) {
        let Some(view) = self.views.get(index) else { return };
        let id = view.id;
        if let Some((page, selected)) = view.assistant_selection() {
            let words = view.selected_text().map(|s| s.split_whitespace().count()).unwrap_or(0);
            let about =
                crate::i18n::fmt(tl!("the selection on page {n} ({words} words)"), &[("n", &(page + 1).to_string()), ("words", &words.to_string())]);
            self.assistant.context = Some((id, page, selected, about));
        }
        self.choose_right_panel(Some(RightPanel::Assistant));
        self.refresh_key_status();
    }

    /// Ask Claude Code how it is signed in, off the UI thread (Preferences shows the answer).
    pub fn check_claude_sign_in(&mut self) {
        let Some(link) = self.ai_service.as_ref().and_then(|s| s.claude_code.clone()) else { return };
        if self.assistant.sign_in_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        let work = move || {
            let _ = tx.send(pdfcraft_engine::guard(|| (link.sign_in)()).unwrap_or_else(Err));
            if let Some(c) = &ctx {
                c.request_repaint();
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        if let Err(e) = std::thread::Builder::new().name("claude-code-sign-in".into()).spawn(work) {
            log::warn!("couldn't ask Claude Code how it is signed in: {e}");
            return;
        }
        #[cfg(target_arch = "wasm32")]
        work();
        self.assistant.sign_in_rx = Some(rx);
    }

    pub(crate) fn checking_claude_sign_in(&self) -> bool {
        self.assistant.sign_in_rx.is_some()
    }

    fn poll_sign_in(&mut self) {
        let Some(rx) = &self.assistant.sign_in_rx else { return };
        match rx.try_recv() {
            Ok(answer) => {
                self.assistant.claude_sign_in = Some(answer);
                self.assistant.sign_in_rx = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.assistant.claude_sign_in = Some(Err(tl!("Claude Code didn't answer.").to_owned()));
                self.assistant.sign_in_rx = None;
            }
        }
    }

    /// Check whether a key is set (on opening the panel or Preferences, and after a change).
    pub(crate) fn refresh_key_status(&mut self) {
        self.assistant.key_status = self.ai_service.as_ref().map(|s| match s.keys.get(ACCOUNT) {
            Ok(Some(_)) => KeyStatus::Stored,
            _ if (s.env_key)().is_some() => KeyStatus::FromEnvironment,
            _ => KeyStatus::Missing,
        });
    }

    /// Store the key typed in Preferences (in the keychain, or for this session).
    pub fn save_assistant_key(&mut self) {
        let key = std::mem::take(&mut self.assistant.key_draft);
        let Some(service) = self.ai_service.clone() else { return };
        match service.keys.set(ACCOUNT, &Secret::new(key.trim())) {
            Ok(()) => self.notify_tr("API key saved"),
            Err(e) => self.notify_fmt("Couldn't save the key: {e}", &[("e", &e.to_string())]),
        }
        self.refresh_key_status();
    }

    pub(crate) fn remove_assistant_key(&mut self) {
        if let Some(service) = self.ai_service.clone()
            && let Err(e) = service.keys.delete(ACCOUNT)
        {
            self.notify_fmt("Couldn't remove the key: {e}", &[("e", &e.to_string())]);
        }
        self.refresh_key_status();
    }

    fn library_file(&self) -> Option<std::path::PathBuf> {
        self.assistant.library_file.clone().or_else(library_path)
    }

    fn library_mut(&mut self) -> &mut Library {
        if self.assistant.library.is_none() {
            let loaded = self
                .library_file()
                .filter(|p| p.is_file())
                .map(|p| std::fs::read(&p).map_err(|e| e.to_string()).and_then(|b| Library::from_json(&b).map_err(|e| e.to_string())));
            let library = match loaded {
                Some(Ok((library, _warnings))) => library,
                Some(Err(e)) => {
                    // Kept aside, never written over: adding a document then starts a new file.
                    self.assistant.library_error = Some(crate::i18n::fmt(tl!("The library file couldn't be read: {e}"), &[("e", &e)]));
                    Library::new()
                }
                None => Library::new(),
            };
            self.assistant.library = Some(library);
        }
        self.assistant.library.get_or_insert_with(Library::new)
    }

    fn save_library(&mut self) {
        if self.assistant.library_error.is_some() {
            self.notify_tr("The library file couldn't be read, so it was left as it is. Move it away to start a new one.");
            return;
        }
        let (Some(path), Some(library)) = (self.library_file(), self.assistant.library.as_ref()) else { return };
        let written = library.to_json().map_err(|e| e.to_string()).and_then(|bytes| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            crate::editing::write_atomically(&path.to_string_lossy(), &bytes).map_err(|e| e.to_string())
        });
        if let Err(e) = written {
            self.notify_fmt("Couldn't save the library: {e}", &[("e", &e)]);
        }
    }

    /// Build the request for the draft question, from the captured selection, the view's
    /// selection, or the current page.
    fn prepare_question(&mut self, index: usize) -> Result<Prepared, String> {
        let service = self.ai_service.clone().ok_or_else(|| tl!("The AI assistant needs the desktop app.").to_owned())?;
        let (_, key) = service.route(&self.assistant.prefs)?;
        let view = self.views.get(index).ok_or_else(|| "no document".to_owned())?;
        let id = view.id;
        let (page, selected, about) = match self.assistant.context.clone().filter(|(doc, ..)| *doc == id) {
            Some((_, page, selected, about)) => (page, selected, about),
            None => match view.assistant_selection() {
                Some((page, selected)) => (page, selected, crate::i18n::fmt(tl!("the selection on page {n}"), &[("n", &(page + 1).to_string())])),
                None => (view.current, Selected::Page, crate::i18n::fmt(tl!("page {n}"), &[("n", &(view.current + 1).to_string())])),
            },
        };
        let question = self.assistant.draft.trim().to_owned();
        let ctx = self.session.assistant_context(id, page, &selected).map_err(|e| e.to_string())?;
        let mut passages = assistant::context_passages(&ctx);
        // The library is taken out while the session reads it, and put back.
        self.library_mut();
        let library = self.assistant.library.take().unwrap_or_default();
        let exclude = self.session.library_exclusions(id, &library);
        passages.extend(assistant::library_passages(&library, &question, Some(&ctx), &exclude, LIBRARY_PASSAGES));
        self.assistant.library = Some(library);
        passages.truncate(claude::MAX_PASSAGES);
        let turn = claude::user_turn(&passages, None, &question).map_err(|e| e.to_string())?;
        let model = self.assistant.prefs.model();
        let search = WebSearch { max_uses: 3, ..WebSearch::default() };
        // Claude Code runs with no tools, so it can't search the web.
        let provider = self.assistant.prefs.provider;
        let searching = self.assistant.prefs.web_search && provider == Provider::ApiKey;
        let opts = Options { effort: self.assistant.prefs.effort, web_search: searching.then_some(&search), ..Options::new(model) };
        let history = self.assistant.conversations.get(&id).map(|c| c.history.clone()).unwrap_or_default();
        let request = claude::request(&opts, &key, &history, &turn).map_err(|e| e.to_string())?;
        let mut all = history;
        all.push(turn.clone());
        let sources = claude::sent_sources(&all);
        Ok(Prepared {
            doc: id,
            page,
            about,
            question,
            request,
            turn,
            sources,
            passages,
            redacted: ctx.redacted_glyphs,
            encrypted: ctx.encrypted,
            model,
            provider,
        })
    }

    pub(crate) fn assistant_action(&mut self, index: usize, action: PanelAction) {
        match action {
            PanelAction::Ask => match self.prepare_question(index) {
                Ok(prepared) if self.assistant.prefs.preview => self.assistant.preview = Some(prepared),
                Ok(prepared) => self.send_prepared(prepared),
                Err(e) => self.notify(e),
            },
            PanelAction::Send => {
                if let Some(prepared) = self.assistant.preview.take() {
                    self.send_prepared(prepared);
                }
            }
            PanelAction::CancelPreview => self.assistant.preview = None,
            PanelAction::Allow => {
                self.assistant.prefs.consented = true;
                if let Some(prepared) = self.assistant.consent.take() {
                    self.send_prepared(prepared);
                }
            }
            PanelAction::Refuse => self.assistant.consent = None,
            PanelAction::Stop => {
                if let Some(running) = &self.assistant.running {
                    running.cancel.store(true, Ordering::Relaxed);
                }
            }
            PanelAction::Jump(source) => self.jump_to_source(index, source),
            PanelAction::SaveAsComment(turn) => self.save_answer_as_comment(index, turn),
            PanelAction::OpenPreferences => {
                self.refresh_key_status();
                self.run_command("app.preferences");
            }
            PanelAction::AddToLibrary => self.add_to_library(index),
            PanelAction::RemoveFromLibrary(sha) => {
                if self.library_mut().remove(&sha) {
                    self.save_library();
                }
            }
            PanelAction::ClearConversation => {
                if let Some(view) = self.views.get(index) {
                    self.assistant.conversations.remove(&view.id);
                }
            }
            PanelAction::ClearContext => self.assistant.context = None,
            PanelAction::OpenPrivacy => self.open_url(PRIVACY_URL),
        }
    }

    fn send_prepared(&mut self, prepared: Prepared) {
        if !self.assistant.prefs.consented {
            self.assistant.consent = Some(prepared);
            return;
        }
        let Some(service) = self.ai_service.clone() else { return };
        if self.assistant.running.is_some() {
            self.notify_tr("Wait for the current answer, or stop it.");
            return;
        }
        let conversation = self.assistant.conversations.entry(prepared.doc).or_default();
        conversation.turns.push(Turn {
            question: prepared.question.clone(),
            about: prepared.about.clone(),
            page: prepared.page,
            blocks: Vec::new(),
            state: TurnState::Streaming,
            usage: Usage::default(),
            stop: None,
            errors: Vec::new(),
            searches: Vec::new(),
            model: prepared.model,
            on_plan: prepared.provider == Provider::ClaudeCode,
        });
        if conversation.turns.len() > MAX_TURNS {
            conversation.turns.remove(0);
        }
        self.assistant.draft.clear();
        let cancel = Arc::new(AtomicBool::new(false));
        let transport = match prepared.provider {
            Provider::ClaudeCode => service.claude_code.as_ref().map_or_else(|| service.transport.clone(), |c| c.transport.clone()),
            Provider::ApiKey => service.transport.clone(),
        };
        let rx = start_worker(transport, prepared.request, prepared.sources, cancel.clone(), self.ctx.clone());
        self.assistant.running = Some(Running { doc: prepared.doc, rx, cancel, turn: prepared.turn });
    }

    /// Take what the worker sent since the last frame.
    pub(crate) fn poll_assistant(&mut self) {
        self.finish_library_jump();
        self.poll_sign_in();
        let Some(running) = &self.assistant.running else { return };
        let doc = running.doc;
        let mut finished: Option<Result<Reply, String>> = None;
        let mut events = Vec::new();
        loop {
            match running.rx.try_recv() {
                Ok(WorkerMsg::Event(e)) => events.push(e),
                Ok(WorkerMsg::Done(result)) => {
                    finished = Some(result);
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    finished = Some(Err(tl!("The request stopped unexpectedly.").to_owned()));
                    break;
                }
            }
        }
        let Some(turn) = self.assistant.conversations.get_mut(&doc).and_then(|c| c.turns.last_mut()) else {
            if finished.is_some() {
                self.assistant.running = None;
            }
            return;
        };
        for event in events {
            apply_event(turn, event);
        }
        let Some(result) = finished else { return };
        let Some(running) = self.assistant.running.take() else { return };
        let cancelled = running.cancel.load(Ordering::Relaxed);
        match result {
            Ok(reply) => {
                turn.usage = reply.usage;
                turn.stop = reply.stop.clone();
                turn.state = if turn.errors.is_empty() { TurnState::Done } else { TurnState::Failed };
                // A complete answer continues the conversation; a failed one isn't replayed.
                if reply.status == 200
                    && matches!(reply.stop, Some(StopReason::EndTurn | StopReason::MaxTokens))
                    && let Some(conversation) = self.assistant.conversations.get_mut(&doc)
                {
                    conversation.history.push(running.turn);
                    conversation.history.push(reply.message);
                }
            }
            Err(e) => {
                turn.state = if cancelled { TurnState::Stopped } else { TurnState::Failed };
                if !cancelled {
                    turn.errors.push(e);
                }
            }
        }
    }

    fn jump_to_source(&mut self, index: usize, source: Source) {
        match source {
            Source::ThisDocument { page, .. } => {
                let Some(view) = self.views.get_mut(index) else { return };
                let page = usize::try_from(page).unwrap_or(usize::MAX);
                if page < view.page_count() {
                    view.go_to_page(page);
                }
            }
            Source::Library { sha256, page, .. } => {
                let page = usize::try_from(page).unwrap_or(usize::MAX);
                if let Some(i) = self.views.iter().position(|v| self.session.document_sha256(v.id) == Some(sha256)) {
                    self.active = Some(i);
                    if let Some(view) = self.views.get_mut(i)
                        && page < view.page_count()
                    {
                        view.go_to_page(page);
                    }
                    return;
                }
                let path = self.library_mut().entry(&sha256).and_then(|e| e.paths.first().cloned());
                match path {
                    Some(path) => self.open_library_document(path, page, sha256),
                    None => self.notify_tr("That document is no longer in the library."),
                }
            }
            Source::Web { url, .. } => self.request_document_url(&url, crate::LinkOrigin::Assistant),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn open_library_document(&mut self, path: String, page: usize, sha256: Sha256) {
        if !std::path::Path::new(&path).is_file() {
            self.notify_fmt("{path} isn't there any more. Add the file to the library again from where it is now.", &[("path", &path)]);
            return;
        }
        self.assistant.jump_after_open = Some((path.clone(), page, sha256));
        let _ = self.open_path(&path);
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn open_library_document(&mut self, _path: String, _page: usize, _sha256: Sha256) {
        self.notify_tr("Opening library documents needs the desktop app");
    }

    /// Once a library document opened for a citation is in a tab: check it is still the same
    /// file, and go to the cited page.
    fn finish_library_jump(&mut self) {
        let Some((path, page, sha256)) = self.assistant.jump_after_open.clone() else { return };
        let Some(i) = self.tab_showing(&path) else { return };
        self.assistant.jump_after_open = None;
        let Some(id) = self.views.get(i).map(|v| v.id) else { return };
        if self.session.document_sha256(id) != Some(sha256) {
            self.notify_tr("The file changed since it was added to the library; the cited page may be different. Add it again to update it.");
        }
        if let Some(view) = self.views.get_mut(i)
            && page < view.page_count()
        {
            view.go_to_page(page);
        }
    }

    fn save_answer_as_comment(&mut self, index: usize, turn: usize) {
        let Some(view) = self.views.get(index) else { return };
        let Some(turn) = self.assistant.conversations.get(&view.id).and_then(|c| c.turns.get(turn)) else { return };
        let Some(info_page) = self.session.get(view.id).and_then(|d| d.info.pages.get(turn.page).cloned()) else { return };
        let mut text = format!("{}\n\n{}", turn.question, turn.answer_text());
        let cited: Vec<String> = turn
            .blocks
            .iter()
            .flat_map(|b| &b.citations)
            .map(|s| match s {
                Source::ThisDocument { page, .. } => crate::i18n::fmt(tl!("page {n}"), &[("n", &(u64::from(*page) + 1).to_string())]),
                Source::Library { page, .. } => crate::i18n::fmt(tl!("library document, page {n}"), &[("n", &(u64::from(*page) + 1).to_string())]),
                Source::Web { url, .. } => url.clone(),
            })
            .collect();
        if !cited.is_empty() {
            text.push_str(&format!("\n\n{} {}", tl!("Sources:"), cited.join("; ")));
        }
        let at = info_page.view_to_user(info_page.width - 44.0, 24.0);
        let shape = pdfcraft_engine::Shape::Note { at: [f64::from(at[0]), f64::from(at[1])], icon: pdfcraft_engine::NoteIcon::Comment };
        let edit = pdfcraft_engine::Edit::AddAnnotation(pdfcraft_engine::NewAnnotation {
            page: turn.page,
            style: self.comment_prefs.style(crate::comments::CommentTool::Note),
            shape,
            contents: text,
            author: self.comment_prefs.author.clone(),
        });
        self.apply_edit(edit);
    }

    /// For a map request: the library passages matching `query` (leaving out copies of `doc`),
    /// and each library document's hash, paths and title, to anchor the snippets quoted from them.
    pub(crate) fn library_for_map(&mut self, doc: Option<DocId>, query: &str) -> (Vec<Passage>, Vec<SourceDoc>) {
        self.library_mut();
        let library = self.assistant.library.take().unwrap_or_default();
        let exclude = doc.map(|id| self.session.library_exclusions(id, &library)).unwrap_or_default();
        let passages = assistant::library_passages(&library, query, None, &exclude, LIBRARY_PASSAGES);
        let entries = library.entries().iter().map(|e| (e.doc.sha256.to_hex(), e.paths.clone(), e.title.clone())).collect();
        self.assistant.library = Some(library);
        (passages, entries)
    }

    fn add_to_library(&mut self, index: usize) {
        let Some(id) = self.views.get(index).map(|v| v.id) else { return };
        let now = self.session.now_secs();
        let new = match self.session.library_document(id, now) {
            Ok(new) => new,
            Err(e) => {
                self.notify_fmt("Couldn't add it to the library: {e}", &[("e", &e.to_string())]);
                return;
            }
        };
        match self.library_mut().add(new) {
            Ok(_) => {
                self.save_library();
                self.notify_tr("Added to the library");
            }
            Err(e) => self.notify_fmt("Couldn't add it to the library: {e}", &[("e", &e.to_string())]),
        }
    }
}

fn apply_event(turn: &mut Turn, event: Event) {
    let block = |turn: &mut Turn, index: u64| -> usize {
        match turn.blocks.iter().position(|b| b.index == index) {
            Some(pos) => pos,
            None => {
                turn.blocks.push(Block { index, text: String::new(), citations: Vec::new() });
                turn.blocks.len().saturating_sub(1)
            }
        }
    };
    match event {
        Event::Text { block: index, text } => {
            let pos = block(turn, index);
            if let Some(b) = turn.blocks.get_mut(pos) {
                b.text.push_str(&text);
            }
        }
        Event::Citation { block: index, source } => {
            let pos = block(turn, index);
            if let Some(b) = turn.blocks.get_mut(pos) {
                b.citations.push(source);
            }
        }
        Event::WebSearch { query } => turn.searches.push(query),
        Event::Usage { usage } => turn.usage = usage,
        Event::Stop { reason } => turn.stop = Some(reason),
        Event::ProviderError { kind, message } => turn.errors.push(format!("{kind}: {message}")),
        Event::Started { .. } | Event::WebResults { .. } | Event::Skipped { .. } => {}
    }
}

/// Run the request on a worker thread (in the caller on the web, where nothing can send anyway).
fn start_worker(
    transport: Arc<dyn Transport>,
    request: HttpRequest,
    sources: Vec<Option<SourceId>>,
    cancel: Arc<AtomicBool>,
    ctx: Option<egui::Context>,
) -> mpsc::Receiver<WorkerMsg> {
    let (tx, rx) = mpsc::channel();
    let work = move || {
        let send = |msg: WorkerMsg| {
            let _ = tx.send(msg);
            if let Some(c) = &ctx {
                c.request_repaint();
            }
        };
        let result = pdfcraft_engine::guard(|| {
            claude::send(transport.as_ref(), &request, sources, &mut |e| {
                send(WorkerMsg::Event(e.clone()));
                if cancel.load(Ordering::Relaxed) { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
            })
        });
        let done = match result {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(e)) => Err(e.to_string()),
            Err(panic) => Err(format!("the request failed unexpectedly ({panic})")),
        };
        send(WorkerMsg::Done(done));
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        // If the thread can't start, the dropped sender reads as "stopped unexpectedly".
        if let Err(e) = std::thread::Builder::new().name("ai-assistant".into()).spawn(work) {
            log::warn!("couldn't start the assistant's worker: {e}");
        }
    }
    #[cfg(target_arch = "wasm32")]
    work();
    rx
}
