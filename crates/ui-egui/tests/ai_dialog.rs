//! Tools ▸ AI... (desktop): the AI dialog opens from the menu or with AI typed at the command line,
//! a JSON call never opens it, the first launch asks once (not in a run an agent drives), and
//! Options ▸ AI holds the AI section. Nothing here sends a request.
#![cfg(not(target_arch = "wasm32"))]

use cadcraft_engine::Session;
use cadcraft_ui_egui::ai::{Ai, AppInfo};
use cadcraft_ui_egui::{CadApp, Services, menus};
use serde_json::json;

fn app() -> CadApp {
    CadApp::new(Session::new(), Services::default())
}

/// A test install: no app token, no settings folder, and a slug no real install uses, so nothing
/// on this computer is read as a key or written.
fn test_ai() -> Ai {
    Ai::new(AppInfo {
        slug: "cadcraft-test-no-such-install",
        name: "CADCraft",
        version: "0.0.0",
        app_token: "",
        dialects: &["chat_completions"],
        config_dir: None,
    })
}

/// A key in the environment sets AI up, and then nothing asks.
fn key_in_environment() -> bool {
    std::env::var_os("PUBLIK_API_KEY").is_some()
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

/// Two frames (windows fade in), returning the painted text of the last.
fn render(app: &mut CadApp, ctx: &egui::Context) -> String {
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))), ..Default::default() };
    let mut text = String::new();
    for _ in 0..2 {
        app.logic(ctx);
        let mut output = ctx.run_ui(input.clone(), |ui| app.ui(ui));
        text = painted_text(&output.shapes);
        output.textures_delta.clear();
    }
    text
}

#[test]
fn the_menu_and_typed_ai_open_the_dialog_json_never_does() {
    let mut a = app();
    let tools = menus::tree(&a).into_iter().find(|(m, _)| m == "Tools").map(|(_, e)| e).unwrap_or_default();
    assert!(tools.iter().any(|e| matches!(e, menus::Entry::Item { id, label, .. } if id == "ui.ai" && label == "AI...")));
    menus::activate(&mut a, "ui.ai");
    assert_eq!(a.ui.dialog.as_deref(), Some("ai"));
    for typed in ["AI", "ai"] {
        a.ui.dialog = None;
        assert_eq!(a.cmdline(typed), None, "{typed}");
        assert_eq!(a.ui.dialog.as_deref(), Some("ai"), "{typed}");
    }
    a.ui.dialog = None;
    assert_eq!(a.run("ui.ai", json!({})), Ok(json!({"available": false})));
    assert!(a.run("ai", json!({})).is_err(), "AI is not an engine command");
    assert!(a.ui.dialog.is_none(), "JSON calls never open dialogs");
    a.ai = Some(test_ai());
    let status = a.run("ui.ai", json!({})).unwrap();
    assert_eq!(status["available"], json!(true));
    assert_eq!(status["setUp"], json!(key_in_environment()));
    assert!(a.ui.dialog.is_none());
}

#[test]
fn without_ai_the_dialog_and_options_say_it_is_off() {
    let mut a = app();
    let ctx = egui::Context::default();
    a.ui.dialog = Some("ai".into());
    assert!(render(&mut a, &ctx).contains("AI is off in this run"));
    a.ui.dialog = Some("options:ai".into());
    let text = render(&mut a, &ctx);
    assert!(text.contains("AI is off in this run") && text.contains("User Preferences"), "{text}");
}

#[test]
fn the_first_launch_asks_once_and_an_agent_run_does_not() {
    if key_in_environment() {
        return;
    }
    let ctx = egui::Context::default();
    let mut a = app();
    a.ai = Some(test_ai());
    render(&mut a, &ctx);
    assert!(a.ui.dialog.is_none(), "not before the window is up");
    render(&mut a, &ctx);
    assert_eq!(a.ui.dialog.as_deref(), Some("aiSetup"));
    assert!(render(&mut a, &ctx).contains("Your own key"));
    // Closed: it does not come back in this run.
    a.ui.dialog = None;
    for _ in 0..3 {
        render(&mut a, &ctx);
    }
    assert!(a.ui.dialog.is_none());

    let mut a = app();
    let mut ai = test_ai();
    ai.skip_first_run();
    a.ai = Some(ai);
    for _ in 0..4 {
        render(&mut a, &ctx);
    }
    assert!(a.ui.dialog.is_none(), "an agent run does not ask unasked");
    // The dialog asks for setup before anything can be sent.
    a.ui.dialog = Some("ai".into());
    assert!(render(&mut a, &ctx).contains("AI is not set up"));
    a.ui.dialog = Some("options:ai".into());
    assert!(render(&mut a, &ctx).contains("Set up AI"));
}
