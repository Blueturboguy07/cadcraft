//! AI through publik API or the person's own key, desktop only. The setup flow, the credential
//! file and the HTTP calls live in the shared `craft-publik` crate. This module is CADCraft's thin
//! host: the setup dialog on first launch, Options ▸ AI, and the AI dialog (Tools ▸ AI..., or `AI`
//! typed at the command line), which answers a question about the drawing or about CADCraft's
//! commands.
//!
//! Nothing is sent, and nothing is charged, until the person clicks Ask. A request carries the
//! question, the command list (`engine.commands`: names, aliases and labels, at most
//! [`MAX_CATALOGUE_CHARS`] characters) and a short summary of the drawing read through the app's
//! own commands (`drawing.inspect`, `entities`, `getvar`; at most [`MAX_SUMMARY_CHARS`]
//! characters). It never carries the drawing file or its geometry. When the answer holds one
//! command line that names a CADCraft command, Run types that line at the command line, and only
//! on that click.

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, TryRecvError};

pub use craft_publik::AppInfo;
use craft_publik::Provider;
use craft_publik::client::{self, Fail, Reply};
use craft_publik::ui::{Action, AiState, spawn};
use egui::RichText;
use serde_json::{Value, json};

use crate::CadApp;
use crate::theme::Tokens;

/// The AI dialog (`app.ui.dialog`).
pub const DIALOG: &str = "ai";
/// The setup flow's dialog.
pub const SETUP: &str = "aiSetup";
/// Options ▸ AI.
const OPTIONS_TAB: &str = "options:ai";

const SYSTEM: &str = "You help people use CADCraft, a 2D CAD and drafting app. Its commands and its command line work like AutoCAD's. \
Answer the person's question about their drawing or about CADCraft's commands. Use only the commands in the list below. \
Be short: at most eight sentences. Use plain text without markdown.\n\n\
If one command line does what the person asks, put it on a line of its own that starts with \"Command: \". \
Write the line as the person types it at CADCraft's command line: the command name, then the answers to its prompts, separated by spaces. \
Write points as x,y, relative points as @dx,dy and polar points as @distance<angle. \
Give at most one such line. Give no command line when the task needs objects picked on screen.";

/// The most drawing summary one request carries, in characters.
pub const MAX_SUMMARY_CHARS: usize = 3_000;
/// The most command list one request carries, in characters.
pub const MAX_CATALOGUE_CHARS: usize = 9_000;
/// The longest question kept.
const MAX_QUESTION_CHARS: usize = 2_000;
/// The longest command line Run types.
const MAX_LINE_CHARS: usize = 300;
/// Layer and block names listed in the summary, and the longest name kept.
const MAX_LAYERS: usize = 60;
const MAX_BLOCKS: usize = 20;
const MAX_NAME_CHARS: usize = 40;
/// Selected objects whose type the summary reads.
const SELECTION_SAMPLE: usize = 500;

const WIDTH: f32 = 520.0;

const OFF: &str = "AI is off in this run because CADCraft has no settings folder. Set CADCRAFT_CONFIG_DIR to turn it on.";
const SENDS: &str = "When you click Ask, CADCraft sends your question, a short summary of this drawing and the list of CADCraft commands. \
The summary has the layer names, the number of objects of each type, the units and the selection. CADCraft does not send the drawing file or its geometry.";

pub struct Ai {
    pub state: AiState,
    frames: u32,
    first_run_checked: bool,
    /// The setup flow is due; [`frame`] opens it when no other dialog shows.
    open_setup: bool,
    /// The dialog the setup flow goes back to when it closes (Options ▸ AI or the AI dialog).
    /// `None` closes it.
    setup_return: Option<String>,
    question: String,
    /// The AI dialog just opened: the question field takes the keyboard.
    focus_question: bool,
    ask: Job,
}

impl Ai {
    pub fn new(app: AppInfo) -> Self {
        Ai {
            state: AiState::new(app),
            frames: 0,
            first_run_checked: false,
            open_setup: false,
            setup_return: None,
            question: String::new(),
            focus_question: false,
            ask: Job::default(),
        }
    }

    /// Do not open the setup dialog on this launch (an agent drives the app over `--control`).
    /// Options ▸ AI and the AI dialog still offer it.
    pub fn skip_first_run(&mut self) {
        self.first_run_checked = true;
    }
}

/// `PUBLIK_SELF_TEST=1`: provision for real and print one line (publikhq.com/skills/setup.md).
pub fn self_test(app: &AppInfo) -> String {
    craft_publik::self_test(app)
}

/// An answer, and the one command line in it that CADCraft can run.
struct Answer {
    text: String,
    command: Option<String>,
}

/// What a request carried, so the dialog can show it.
struct Sent {
    summary: String,
    catalogue: Catalogue,
}

/// The question's request in flight and its last answer.
#[derive(Default)]
struct Job {
    result: Option<Answer>,
    error: Option<String>,
    sent: Option<Sent>,
    rx: Option<Receiver<Result<Reply, Fail>>>,
}

impl Job {
    fn busy(&self) -> bool {
        self.rx.is_some()
    }

    fn poll(&mut self, state: &mut AiState) {
        let Some(rx) = self.rx.take() else { return };
        match rx.try_recv() {
            Ok(Ok(r)) => {
                state.note_reply(&r);
                let text = r.text.trim().to_string();
                let command = command_line(&text, known_command);
                self.result = Some(Answer { text, command });
                self.error = None;
            }
            Ok(Err(f)) => {
                state.note_fail(&f);
                // A 402 or a retired key is the notice bar's job (one message, one link); the rest is ours.
                if state.notice.is_none() {
                    self.error = Some(f.message);
                }
            }
            Err(TryRecvError::Empty) => self.rx = Some(rx),
            Err(TryRecvError::Disconnected) => self.error = Some("The request stopped. Try again.".into()),
        }
    }

    fn start(&mut self, ctx: &egui::Context, provider: Provider, system: String, user: String, sent: Sent) {
        self.result = None;
        self.error = None;
        self.sent = Some(sent);
        self.rx = Some(spawn(ctx, move || client::chat(&provider, &system, &user)));
    }
}

/// Once per frame, before layout: finish background work, and open the setup dialog when it is
/// due and no other dialog, prompt or notice shows.
pub fn frame(app: &mut CadApp, ctx: &egui::Context) {
    let blocked = app.ui.dialog.is_some() || app.closing.is_some() || !app.recovery.items.is_empty() || app.gpu_notice.is_some();
    let Some(ai) = app.ai.as_mut() else { return };
    ai.state.poll();
    ai.ask.poll(&mut ai.state);
    // The first launch asks once the window is up (a few frames in). "Later" ends that for good.
    if !ai.first_run_checked {
        ai.frames = ai.frames.saturating_add(1);
        ctx.request_repaint();
        if ai.frames > 3 {
            ai.first_run_checked = true;
            if ai.state.wants_first_run() || ai.state.owes_first_card() {
                ai.open_setup = true;
            }
        }
    }
    if !ai.open_setup || blocked {
        return;
    }
    ai.open_setup = false;
    ai.setup_return = None;
    ai.state.begin();
    app.ui.dialog = Some(SETUP.into());
}

/// `ui.ai` (Tools ▸ AI...) and `AI` typed at the command line open the AI dialog. A JSON call
/// never opens it and never sends anything: it reports whether AI is set up.
pub fn route(app: &mut CadApp, params: &Value) -> Result<Value, String> {
    if params.is_null() {
        app.ui.dialog = Some(DIALOG.into());
        if let Some(ai) = app.ai.as_mut() {
            ai.focus_question = true;
        }
        return Ok(Value::Null);
    }
    Ok(match app.ai.as_ref() {
        Some(ai) => {
            let p = ai.state.provider();
            json!({"available": true, "setUp": p.is_set_up(), "provider": p.label()})
        }
        None => json!({"available": false}),
    })
}

fn window<'a>(ctx: &egui::Context, id: &str) -> egui::Window<'a> {
    egui::Window::new(crate::i18n::t("AI"))
        .id(egui::Id::new(id))
        .resizable(false)
        .collapsible(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
}

fn note(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(Tokens::get().text_dim));
}

fn open_url(ctx: &egui::Context, url: String) {
    ctx.open_url(egui::OpenUrl::new_tab(url));
}

// ---- dialogs ------------------------------------------------------------------------------------

/// The setup flow: on first launch, from Options ▸ AI, or from the AI dialog.
pub fn setup_dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let mut action = Action::None;
    window(ctx, "cc_ai_setup").open(open).show(ctx, |ui| match app.ai.as_mut() {
        Some(ai) => action = ai.state.setup_ui(ui),
        None => {
            ui.label(OFF);
        }
    });
    match action {
        Action::None => {}
        Action::OpenUrl(url) => open_url(ctx, url),
        // Back to Options ▸ AI or the AI dialog, if the flow came from there.
        Action::Close => app.ui.dialog = app.ai.as_mut().and_then(|ai| ai.setup_return.take()),
    }
}

/// The AI dialog: a question about the drawing or about CADCraft's commands, the answer, and Run
/// for the one command line the answer holds.
pub fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    // A line typed while a command or a LISP routine waits for input would answer its prompt.
    let command_busy = app.session.prompting() || app.session.lisp_busy();
    let mut ask = false;
    let mut setup = false;
    let mut close = false;
    let mut run: Option<String> = None;
    let mut action = Action::None;
    window(ctx, "cc_ai").open(open).show(ctx, |ui| {
        ui.set_width(WIDTH);
        let Some(ai) = app.ai.as_mut() else {
            ui.label(OFF);
            return;
        };
        if !ai.state.provider().is_set_up() {
            ui.label("AI is not set up. Set it up first: publik API, with no key to paste, or your own key.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                setup = ui.button("Set up AI...").clicked();
                close = ui.button(crate::tl!("Close")).clicked();
            });
            return;
        }
        ui.label("Ask a question about this drawing or about CADCraft's commands.");
        note(ui, SENDS);
        ui.add_space(6.0);
        let busy = ai.ask.busy();
        let question = ui.add_enabled(
            !busy,
            egui::TextEdit::multiline(&mut ai.question)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .char_limit(MAX_QUESTION_CHARS)
                .hint_text("How do I draw a circle that touches two lines?"),
        );
        if std::mem::take(&mut ai.focus_question) {
            question.request_focus();
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if busy {
                ui.spinner();
                ui.label("Writing the answer...");
            } else {
                ask = ui.add_enabled(!ai.question.trim().is_empty(), egui::Button::new("Ask")).clicked();
            }
        });
        if let Some(e) = &ai.ask.error {
            ui.add_space(4.0);
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        if let Some(answer) = &ai.ask.result {
            ui.add_space(6.0);
            let t = Tokens::get();
            egui::Frame::new().fill(t.field).corner_radius(egui::CornerRadius::same(4)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().id_salt("cc_ai_answer").max_height(240.0).show(ui, |ui| {
                    ui.add(egui::Label::new(answer.text.as_str()).selectable(true).wrap());
                });
            });
            ui.add_space(4.0);
            if ui.button(crate::tl!("Copy")).clicked() {
                ui.ctx().copy_text(answer.text.clone());
            }
            if let Some(line) = &answer.command {
                ui.add_space(8.0);
                ui.label("The answer has a command line that CADCraft can run:");
                ui.label(RichText::new(line).monospace());
                ui.horizontal(|ui| {
                    if ui.add_enabled(!command_busy, egui::Button::new("Run")).on_hover_text("Type this line at the command line").clicked() {
                        run = Some(line.clone());
                    }
                    if command_busy {
                        note(ui, "A command is running. Finish it or press Esc, then click Run.");
                    }
                });
                note(ui, "Run types this line at the command line. If the command asks for more input, press Enter to end it or Esc to cancel it.");
            }
        }
        if let Some(sent) = &ai.ask.sent {
            ui.add_space(6.0);
            egui::CollapsingHeader::new("What CADCraft sent").id_salt("cc_ai_sent").show(ui, |ui| {
                note(ui, &sent.describe());
                egui::ScrollArea::vertical().id_salt("cc_ai_sent_text").max_height(140.0).show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(&sent.summary).monospace().small()).selectable(true).wrap());
                });
            });
        }
        ui.add_space(4.0);
        action = ai.state.result_bar(ui);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui.button(crate::tl!("Close")).clicked();
            });
        });
    });
    if let Action::OpenUrl(url) = action {
        open_url(ctx, url);
    }
    if close {
        *open = false;
    }
    if setup && let Some(ai) = app.ai.as_mut() {
        ai.state.begin();
        ai.setup_return = Some(DIALOG.into());
        app.ui.dialog = Some(SETUP.into());
    }
    if ask {
        start_ask(app, ctx);
    }
    if let Some(line) = run {
        run_line(app, &line, open);
    }
}

/// Gather what Ask sends and start the request.
fn start_ask(app: &mut CadApp, ctx: &egui::Context) {
    let catalogue = catalogue(&crate::control::all_commands(app));
    let summary = drawing_summary(app);
    let Some(ai) = app.ai.as_mut() else { return };
    let provider = ai.state.provider();
    let question: String = ai.question.trim().chars().take(MAX_QUESTION_CHARS).collect();
    if !provider.is_set_up() || question.is_empty() || ai.ask.busy() {
        return;
    }
    let system = format!("{SYSTEM}\n\nCADCraft's commands (NAME (aliases): description):\n{}", catalogue.text);
    let user = format!("Question: {question}\n\nThe drawing now:\n{summary}");
    ai.ask.start(ctx, provider, system, user, Sent { summary, catalogue });
}

/// Run: type the answer's command line at the command line, as the person would. The dialog
/// closes when the line is taken, so the command line and the drawing are free for the prompts.
fn run_line(app: &mut CadApp, line: &str, open: &mut bool) {
    // Checked again on the click: only a line that passes the same test runs.
    if !runnable(line, known_command) || app.session.prompting() || app.session.lisp_busy() {
        return;
    }
    match app.cmdline(line) {
        None => *open = false,
        Some(e) => {
            if let Some(ai) = app.ai.as_mut() {
                ai.ask.error = Some(e);
            }
        }
    }
}

// ---- Options ▸ AI --------------------------------------------------------------------------------

pub fn options_section(app: &mut CadApp, ui: &mut egui::Ui) {
    let Some(ai) = app.ai.as_mut() else {
        note(ui, OFF);
        return;
    };
    note(
        ui,
        "Tools ▸ AI... answers questions about the drawing and about CADCraft's commands. It uses publik API or your own key, and it sends only when you click Ask.",
    );
    ui.add_space(6.0);
    let mut open_setup = false;
    let action = ai.state.options_ui(ui, &mut open_setup);
    if open_setup {
        // The setup flow takes the Options dialog's place and goes back to this tab.
        ai.setup_return = Some(OPTIONS_TAB.into());
        app.ui.dialog = Some(SETUP.into());
    }
    if let Action::OpenUrl(url) = action {
        open_url(ui.ctx(), url);
    }
}

// ---- what a request carries ----------------------------------------------------------------------

/// The command list a request carries.
struct Catalogue {
    text: String,
    listed: usize,
    total: usize,
}

impl Sent {
    fn describe(&self) -> String {
        let c = &self.catalogue;
        format!(
            "CADCraft sent your question, this summary of the drawing ({} characters) and {} of {} commands from the command list ({} characters).",
            self.summary.chars().count(),
            c.listed,
            c.total,
            c.text.chars().count()
        )
    }
}

/// The commands a person types (no `ui.*` or dotted JSON-only ids), from `engine.commands`, as
/// `NAME (ALIASES): label` lines. Commands on a menu come first; the list stops at
/// [`MAX_CATALOGUE_CHARS`].
fn catalogue(commands: &Value) -> Catalogue {
    let typed: Vec<&Value> = commands
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|c| c.get("ui").and_then(Value::as_bool) != Some(true))
                .filter(|c| c.get("id").and_then(Value::as_str).is_some_and(|id| !id.is_empty() && !id.contains('.')))
                .collect()
        })
        .unwrap_or_default();
    let on_menu = |c: &Value| c.get("menu").and_then(Value::as_array).is_some_and(|m| !m.is_empty());
    let ordered = typed.iter().filter(|c| on_menu(c)).chain(typed.iter().filter(|c| !on_menu(c)));
    let mut text = String::new();
    let mut listed = 0;
    for c in ordered {
        let id = c.get("id").and_then(Value::as_str).unwrap_or_default().to_ascii_uppercase();
        let label = c.get("label").and_then(Value::as_str).unwrap_or_default();
        let aliases: Vec<String> = c
            .get("aliases")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).take(4).map(str::to_ascii_uppercase).collect())
            .unwrap_or_default();
        let line = if aliases.is_empty() { format!("{id}: {label}") } else { format!("{id} ({}): {label}", aliases.join(", ")) };
        if text.len() + line.len() + 1 > MAX_CATALOGUE_CHARS {
            break;
        }
        text.push_str(&line);
        text.push('\n');
        listed += 1;
    }
    let total = typed.len();
    if listed < total {
        text.push_str(&format!("... and {} more commands\n", total - listed));
    }
    Catalogue { text, listed, total }
}

/// The units as the Drawing Units dialog shows them.
struct Units {
    lunits: Option<i64>,
    insunits: Option<i64>,
}

/// A short summary of the drawing, read through the app's own commands: `drawing.inspect` (title,
/// space, object counts by type, extents, layers, blocks, the selection), `getvar` (units) and
/// `entities` (the selected objects' types). Read-only: none of them records an undo step.
fn drawing_summary(app: &mut CadApp) -> String {
    let running = app.session.running.as_ref().map(|_| app.session.prompt_text());
    let Ok(inspect) = app.session.execute("drawing.inspect", &json!({"entities": false})) else {
        return "No drawing is open.".into();
    };
    let mut var = |name: &str| app.session.execute("getvar", &json!({"name": name})).ok().and_then(|v| v.get("value").and_then(Value::as_i64));
    let units = Units { lunits: var("LUNITS"), insunits: var("INSUNITS") };
    let selected = inspect.get("selection").and_then(Value::as_array).is_some_and(|s| !s.is_empty());
    let selection = if selected { app.session.execute("entities", &json!({"selected": true, "limit": SELECTION_SAMPLE})).ok() } else { None };
    summary_text(&inspect, &units, selection.as_ref(), running.as_deref())
}

fn short(name: &str) -> String {
    let name = name.replace(['\n', '\r', '\t'], " ");
    if name.chars().count() > MAX_NAME_CHARS { format!("{}...", name.chars().take(MAX_NAME_CHARS).collect::<String>()) } else { name }
}

fn number(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

fn objects(n: u64) -> String {
    if n == 1 { "1 object".into() } else { format!("{n} objects") }
}

/// `Line 12, Circle 3` from type counts, most first.
fn by_type(counts: &BTreeMap<String, u64>) -> String {
    let mut v: Vec<(&String, &u64)> = counts.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    v.iter().map(|(t, n)| format!("{t} {n}")).collect::<Vec<_>>().join(", ")
}

fn summary_text(inspect: &Value, units: &Units, selection: Option<&Value>, running: Option<&str>) -> String {
    let s = |k: &str| inspect.get(k).and_then(Value::as_str).unwrap_or_default();
    let mut lines = Vec::new();
    let dirty = if inspect.get("dirty").and_then(Value::as_bool) == Some(true) { " (unsaved changes)" } else { "" };
    lines.push(format!("Drawing: {}{dirty}", short(s("title"))));
    lines.push(format!("Space: {}", short(s("space"))));
    let lunits = units.lunits.unwrap_or(2);
    let length = crate::dialogs::LENGTH_TYPES.iter().find(|(k, _)| *k == lunits).map_or("Decimal", |(_, l)| *l);
    let insunits = units.insunits.unwrap_or(1);
    let insertion = usize::try_from(insunits).ok().and_then(|i| crate::dialogs::INSERTION_UNITS.get(i)).copied().unwrap_or("Unitless");
    lines.push(format!("Units: {length} lengths, insertion units {insertion}"));
    let counts: BTreeMap<String, u64> = inspect
        .get("counts")
        .and_then(Value::as_object)
        .map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_u64()?))).collect())
        .unwrap_or_default();
    let here: u64 = counts.values().sum();
    let all = inspect.get("entityCount").and_then(Value::as_u64).unwrap_or(here);
    if here == 0 {
        lines.push(format!("Objects in this space: none ({all} in the whole drawing)"));
    } else {
        lines.push(format!("Objects in this space: {here} ({}); {all} in the whole drawing", by_type(&counts)));
    }
    if let Some([min, max]) = inspect.get("extents").and_then(Value::as_array).map(Vec::as_slice) {
        let p = |v: &Value| {
            let c = |i: usize| v.get(i).and_then(Value::as_f64).map_or_else(|| "?".into(), number);
            format!("{},{}", c(0), c(1))
        };
        lines.push(format!("Extents: {} to {}", p(min), p(max)));
    }
    let current = s("currentLayer");
    let layers = inspect.get("layers").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut names: Vec<String> = layers
        .iter()
        .take(MAX_LAYERS)
        .map(|l| {
            let name = l.get("name").and_then(Value::as_str).unwrap_or_default();
            let mut state = Vec::new();
            if name == current {
                state.push("current");
            }
            if l.get("on").and_then(Value::as_bool) == Some(false) {
                state.push("off");
            }
            if l.get("frozen").and_then(Value::as_bool) == Some(true) {
                state.push("frozen");
            }
            if l.get("locked").and_then(Value::as_bool) == Some(true) {
                state.push("locked");
            }
            if state.is_empty() { short(name) } else { format!("{} ({})", short(name), state.join(", ")) }
        })
        .collect();
    if layers.len() > MAX_LAYERS {
        names.push(format!("... and {} more", layers.len() - MAX_LAYERS));
    }
    lines.push(format!("Layers ({}): {}", layers.len(), names.join(", ")));
    let blocks = inspect.get("blocks").and_then(Value::as_array).cloned().unwrap_or_default();
    if !blocks.is_empty() {
        let mut names: Vec<String> = blocks.iter().take(MAX_BLOCKS).filter_map(Value::as_str).map(short).collect();
        if blocks.len() > MAX_BLOCKS {
            names.push(format!("... and {} more", blocks.len() - MAX_BLOCKS));
        }
        lines.push(format!("Blocks ({}): {}", blocks.len(), names.join(", ")));
    }
    match selection {
        Some(sel) => {
            let count = sel.get("count").and_then(Value::as_u64).unwrap_or(0);
            let mut types: BTreeMap<String, u64> = BTreeMap::new();
            let entities = sel.get("entities").and_then(Value::as_array).cloned().unwrap_or_default();
            for e in &entities {
                let t = e.get("type").and_then(Value::as_str).unwrap_or("Object").to_string();
                *types.entry(t).or_default() += 1;
            }
            let sample = if (entities.len() as u64) < count { format!(", types of the first {}", entities.len()) } else { String::new() };
            lines.push(format!("Selection: {} ({}{sample})", objects(count), by_type(&types)));
        }
        None => lines.push("Selection: nothing is selected".into()),
    }
    if let Some(prompt) = running {
        lines.push(format!("A command is running: {}", short_line(prompt)));
    }
    let text = lines.join("\n");
    if text.chars().count() > MAX_SUMMARY_CHARS { format!("{}...", text.chars().take(MAX_SUMMARY_CHARS).collect::<String>()) } else { text }
}

fn short_line(text: &str) -> String {
    let text = text.replace(['\n', '\r'], " ");
    if text.chars().count() > 200 { format!("{}...", text.chars().take(200).collect::<String>()) } else { text }
}

// ---- the command line in an answer ---------------------------------------------------------------

/// Whether `name` (the first word of a typed line) starts a CADCraft command, as the command line
/// reads it: a leading `'`, `_`, `.` or `-` and aliases (`C`, `L`) included.
fn known_command(name: &str) -> bool {
    use cadcraft_engine::cmd::{find_command, resolve_alias};
    let name = name.trim().trim_start_matches('\'');
    let dashed = resolve_alias(&name.trim_start_matches(['_', '.']).to_ascii_lowercase());
    let plain = resolve_alias(&name.trim_start_matches(['_', '.', '-']).to_ascii_lowercase());
    find_command(&dashed).or_else(|| find_command(&plain)).is_some()
}

/// The one command line in an answer: the first line that starts with `Command:`, or else the only
/// line of a code block. `None` when there is none, or when it isn't a line Run may type.
fn command_line(text: &str, known: impl Fn(&str) -> bool) -> Option<String> {
    let marked = text.lines().find_map(|l| {
        let l = l.trim().trim_matches('`').trim();
        let head = l.get(..8)?;
        head.eq_ignore_ascii_case("command:").then(|| l.get(8..).unwrap_or_default().trim().trim_matches('`').trim().to_string())
    });
    let line = marked.or_else(|| {
        let mut blocks = text.split("```").skip(1).step_by(2);
        let block = blocks.next()?;
        // The fence's language tag (```text) is not part of the line.
        let body: Vec<&str> = block.lines().skip(1).map(str::trim).filter(|l| !l.is_empty()).collect();
        match body.as_slice() {
            [one] => Some(one.to_string()),
            _ => None,
        }
    })?;
    runnable(&line, known).then_some(line)
}

/// A line Run may type: one line of plain command-line input that starts with a known command.
/// Not the JSON form (`cmd {…}`), not a `ui.*` command, and no LISP (`(…)`, `!var`), so a click
/// on Run does no more than the line shows.
fn runnable(line: &str, known: impl Fn(&str) -> bool) -> bool {
    if line.is_empty() || line.chars().count() > MAX_LINE_CHARS || line.chars().any(char::is_control) {
        return false;
    }
    let mut words = line.split_whitespace();
    let Some(first) = words.next() else { return false };
    let lower = first.to_ascii_lowercase();
    if lower.starts_with("ui.") || lower.trim_start_matches(['\'', '_', '.', '-']).starts_with("ui.") {
        return false;
    }
    if line.split_whitespace().any(|w| w.starts_with(['(', '!', '{'])) {
        return false;
    }
    known(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(name: &str) -> bool {
        ["circle", "c", "line", "-layer", "erase", "zoom"].contains(&name.trim_start_matches(['\'', '_', '.']).to_ascii_lowercase().as_str())
    }

    #[test]
    fn the_marked_command_line_is_found() {
        let answer = "Use CIRCLE with the center and the radius.\nCommand: CIRCLE 0,0 5\nThen press Enter.";
        assert_eq!(command_line(answer, known).as_deref(), Some("CIRCLE 0,0 5"));
        assert_eq!(command_line("command: `zoom e`", known).as_deref(), Some("zoom e"));
        assert_eq!(command_line("Type this:\n```\n_LINE 0,0 10,0\n```", known).as_deref(), Some("_LINE 0,0 10,0"));
        assert_eq!(command_line("```text\n-LAYER M WALLS\n```", known).as_deref(), Some("-LAYER M WALLS"));
        // Only the first marked line counts.
        assert_eq!(command_line("Command: ERASE L\nCommand: ZOOM E", known).as_deref(), Some("ERASE L"));
    }

    #[test]
    fn lines_that_are_not_plain_known_commands_get_no_run() {
        for answer in [
            "Draw it with the CIRCLE command.",
            "Command: FLY 0,0",
            "Command: circle {\"center\": [0, 0], \"radius\": 5}",
            "Command: (command \"erase\" \"all\")",
            "Command: line !p1 10,10",
            "Command: ui.quit",
            "Command:",
            "```\nLINE 0,0 1,1\nLINE 1,1 2,2\n```",
        ] {
            assert_eq!(command_line(answer, known), None, "{answer}");
        }
        assert!(!runnable(&format!("line {}", "1,1 ".repeat(100)), known));
        assert!(!runnable("line 0,0\t1,1", known));
    }

    #[test]
    fn engine_commands_and_aliases_are_known() {
        assert!(known_command("CIRCLE"));
        assert!(known_command("c"));
        assert!(known_command("_line"));
        assert!(known_command("'zoom"));
        assert!(!known_command("nosuchcommand"));
        assert!(!known_command(""));
    }

    #[test]
    fn the_catalogue_lists_typed_commands_menu_first_and_is_capped() {
        let commands = json!([
            {"id": "zoom", "label": "Zoom", "menu": [], "aliases": ["z"]},
            {"id": "circle", "label": "Circle", "menu": ["Draw", "Circle"], "aliases": ["c"]},
            {"id": "layer.new", "label": "New Layer", "menu": []},
            {"id": "ui.quit", "label": "Quit CADCraft", "menu": [], "ui": true},
        ]);
        let c = catalogue(&commands);
        assert_eq!(c.text, "CIRCLE (C): Circle\nZOOM (Z): Zoom\n");
        assert_eq!((c.listed, c.total), (2, 2));
        let many: Vec<Value> = (0..2000).map(|i| json!({"id": format!("cmd{i}"), "label": "A command with a label", "menu": []})).collect();
        let c = catalogue(&Value::Array(many));
        assert!(c.text.len() <= MAX_CATALOGUE_CHARS + 40, "{}", c.text.len());
        assert!(c.listed < c.total && c.text.ends_with(&format!("... and {} more commands\n", c.total - c.listed)));
    }

    #[test]
    fn the_summary_names_layers_counts_units_and_the_selection() {
        let inspect = json!({
            "title": "Bracket.dxf", "dirty": true, "space": "Model", "entityCount": 7,
            "counts": {"Line": 4, "Circle": 2, "Text": 1},
            "extents": [[0.0, -0.00001], [100.5, 50.0]],
            "layers": [{"name": "0", "on": true}, {"name": "WALLS", "on": true}, {"name": "DIMS", "on": false, "frozen": true}],
            "currentLayer": "WALLS", "blocks": ["Door"], "selection": ["1A", "1B"],
        });
        let selection = json!({"count": 2, "entities": [{"type": "Line"}, {"type": "Circle"}]});
        let text = summary_text(&inspect, &Units { lunits: Some(2), insunits: Some(4) }, Some(&selection), Some("LINE Specify next point:"));
        assert_eq!(
            text,
            "Drawing: Bracket.dxf (unsaved changes)\nSpace: Model\nUnits: Decimal lengths, insertion units Millimeters\n\
             Objects in this space: 7 (Line 4, Circle 2, Text 1); 7 in the whole drawing\nExtents: 0,0 to 100.5,50\n\
             Layers (3): 0, WALLS (current), DIMS (off, frozen)\nBlocks (1): Door\nSelection: 2 objects (Circle 1, Line 1)\n\
             A command is running: LINE Specify next point:"
        );
        let empty = summary_text(&json!({"title": "Drawing1.dxf", "space": "Model"}), &Units { lunits: None, insunits: None }, None, None);
        assert!(empty.contains("Objects in this space: none") && empty.contains("Selection: nothing is selected"), "{empty}");
    }

    #[test]
    fn a_big_drawing_summary_stays_within_the_cap() {
        let layers: Vec<Value> = (0..5000).map(|i| json!({"name": format!("{}{i}", "L".repeat(80))})).collect();
        let inspect = json!({"title": "Big.dwg", "space": "Model", "layers": layers});
        let text = summary_text(&inspect, &Units { lunits: None, insunits: None }, None, None);
        assert!(text.chars().count() <= MAX_SUMMARY_CHARS + 3);
        assert!(text.contains("... and 4940 more") || text.ends_with("..."));
    }

    fn painted_text(shapes: &[egui::epaint::ClippedShape]) -> String {
        fn collect(shape: &egui::Shape, text: &mut String) {
            match shape {
                egui::Shape::Text(s) => {
                    text.push_str(s.galley.text());
                    text.push('\n');
                }
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, text)),
                _ => {}
            }
        }
        let mut text = String::new();
        shapes.iter().for_each(|s| collect(&s.shape, &mut text));
        text
    }

    #[test]
    fn run_types_the_answers_line_at_the_command_line_only_when_no_command_runs() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        // An own key that is never used: nothing here sends a request.
        let mut ai = Ai::new(AppInfo {
            slug: "cadcraft-test-no-such-install",
            name: "CADCraft",
            version: "0.0.0",
            app_token: "",
            dialects: &["chat_completions"],
            config_dir: None,
        });
        ai.state.settings.provider = craft_publik::settings::PROVIDER_OWN.into();
        ai.state.settings.own_key = "sk-test-not-a-key".into();
        let text = "Use CIRCLE.\nCommand: CIRCLE 0,0 5";
        ai.ask.result = Some(Answer { text: text.into(), command: command_line(text, known_command) });
        app.ai = Some(ai);
        app.ui.dialog = Some(DIALOG.into());
        let ctx = egui::Context::default();
        let input =
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))), ..Default::default() };
        let mut shown = String::new();
        for _ in 0..2 {
            app.logic(&ctx);
            let mut output = ctx.run_ui(input.clone(), |ui| app.ui(ui));
            shown = painted_text(&output.shapes);
            output.textures_delta.clear();
        }
        assert!(shown.contains("CIRCLE 0,0 5") && shown.contains("Run") && shown.contains("Copy"), "{shown}");

        let circles = |app: &CadApp| {
            app.session.doc().map_or(0, |d| {
                d.model.iter().filter(|e| matches!(&e.kind, cadcraft_engine::doc::EntityKind::Circle(c) if (c.radius - 5.0).abs() < 1e-9)).count()
            })
        };
        let mut open = true;
        app.start("line");
        run_line(&mut app, "CIRCLE 0,0 5", &mut open);
        assert!(open && circles(&app) == 0, "a running command's prompt never gets the line");
        app.session.cancel();
        run_line(&mut app, "(command \"circle\" \"0,0\" 5)", &mut open);
        assert!(open && circles(&app) == 0, "only a line that passes the test runs");
        run_line(&mut app, "CIRCLE 0,0 5", &mut open);
        assert!(!open, "the dialog closes when the line is taken");
        assert_eq!(circles(&app), 1);
    }

    #[test]
    fn the_summary_reads_the_drawing_through_its_commands_without_an_undo_step() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.session.execute("circle", &json!({"center": [0, 0], "radius": 1})).unwrap();
        app.session.execute("selectall", &json!({})).unwrap();
        let undo = app.session.state().unwrap().undo.len();
        let text = drawing_summary(&mut app);
        assert!(text.contains("Circle 1") && text.contains("Selection: 1 object (Circle 1)"), "{text}");
        assert_eq!(app.session.state().unwrap().undo.len(), undo);
        let c = catalogue(&crate::control::all_commands(&app));
        assert!(c.text.contains("CIRCLE (C") && c.text.contains("LINE (L)"), "{}", c.text);
    }
}
