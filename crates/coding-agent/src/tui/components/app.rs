//! App root and global key router.

use std::rc::Rc;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use iodilos::prelude::*;

use crate::core::extensions::SlashCommandScope;
use crate::tui::components::editor::input_editor;
use crate::tui::components::transcript::transcript;
use crate::tui::editor::{self, EditorAction};
use flown_agent::AgentHarness;

/// Lines scrolled per mouse-wheel notch.
const WHEEL_LINES: usize = 3;

pub fn app() -> View {
    let stack = use_context::<Rc<crate::tui::conversation::ConversationStack>>();
    let agent = use_context::<Option<Arc<AgentHarness>>>();
    let config = use_context::<crate::config::Config>();
    let overlay_stack = use_context::<Rc<crate::tui::overlay_stack::OverlayStack>>();
    let term_size = use_context::<crate::tui::state::TerminalSize>();

    let key_stack = Rc::clone(&stack);
    let key_overlay = Rc::clone(&overlay_stack);
    let key_config = config.clone();
    let resize_term_size = term_size;
    let wheel_stack = Rc::clone(&stack);
    let wheel_overlay = Rc::clone(&overlay_stack);
    View::from(
        tags::div()
            .flex_direction(FlexDirection::Column)
            .width(Size::Percent(100.0))
            .height(Size::Percent(100.0))
            .background_color(Color::Reset)
            .tabindex("0")
            .on(events::terminal_resize, move |event: Event| {
                if let Some((cols, rows)) = event.resize() {
                    resize_term_size.set(cols, rows);
                }
            })
            .on(events::raw_key, move |event: Event| {
                let Some(key) = event.key().copied() else {
                    return;
                };
                if key.kind == KeyEventKind::Release {
                    return;
                }
                handle_app_key(key, &key_stack, &key_overlay, agent.as_ref(), &key_config);
                event.stop_propagation();
            })
            .on(events::raw_mouse, move |event: Event| {
                let Some(mouse) = event.mouse().copied() else {
                    return;
                };
                // When an overlay is active, let its own key handler own the
                // mouse; otherwise the wheel scrolls the active transcript.
                if wheel_overlay.active().is_some() {
                    return;
                }
                let state = Rc::clone(&wheel_stack.active().state);
                match mouse.kind {
                    MouseEventKind::ScrollUp => state.scroll_up(WHEEL_LINES),
                    MouseEventKind::ScrollDown => state.scroll_down(WHEEL_LINES),
                    _ => {}
                }
            })
            .children((transcript(), input_editor(), overlay_layer(overlay_stack))),
    )
}

fn overlay_layer(overlay: Rc<crate::tui::overlay_stack::OverlayStack>) -> View {
    // Render every active overlay layer (or nothing) and rebuild only when the
    // layer set changes — not on every keystroke/token.
    //
    // Two load-bearing details, both prescribed by the header doc on
    // `crate::tui::overlay_stack::OverlayStack`:
    //
    // 1. Building the factories HERE, in this `from_dynamic` effect (a child of
    //    the app scope). An overlay's content factory (e.g. btw's
    //    `overlay_conversation`) calls `use_context`/`create_signal`/`on_key`,
    //    which only resolve against the app's context tree when the factory
    //    runs under the app's reactive scope — not in the `spawn_local` task
    //    that pushed the overlay. That mismatch is why overlays previously
    //    rendered blank.
    //
    // 2. `untrack` around the factory call. The content reads high-frequency
    //    signals (entries, input, term_size) that change on every keystroke and
    //    streaming token. Without `untrack`, those reads would become
    //    dependencies of this effect, rebuilding the overlays and re-stacking
    //    their `on_key` handlers on every keystroke. Each content's own nested
    //    `Dynamic` nodes already react to those signals; this outer build must
    //    not.
    //
    // Layers are emitted bottom-first: each is an absolutely-positioned box, and
    // later siblings paint on top, so the topmost (smallest) layer floats above
    // the ones beneath it — e.g. the thinking-intensity picker over the model
    // picker.
    View::from_dynamic(move || {
        // Tracking this — and only this — makes the effect re-run on any
        // push/pop.
        let layers = overlay.layers_signal().get_clone();
        if layers.is_empty() {
            return View::new();
        }
        // `untrack` keeps each factory's signal reads from widening this
        // effect's dependencies, so it re-runs only when the layer set changes.
        let boxes: Vec<View> = untrack(|| {
            layers
                .iter()
                .map(|layer| {
                    let inset = layer.geometry.inset();
                    let (border_style, border_color) = layer.geometry.border();
                    View::from(
                        tags::div()
                            .position(Position::Absolute)
                            .top(inset)
                            .right(inset)
                            .bottom(inset)
                            .left(inset)
                            .background_color(Color::Reset)
                            .border_style(border_style)
                            .border_color(border_color)
                            .children((layer.content)()),
                    )
                })
                .collect()
        });
        // Flatten the sibling overlay boxes into one view: each box is an
        // absolutely-positioned node, emitted bottom-first so later (topmost)
        // layers paint above the earlier ones.
        View::from(boxes)
    })
}

fn handle_app_key(
    key: KeyEvent,
    stack: &Rc<crate::tui::conversation::ConversationStack>,
    overlay_stack: &Rc<crate::tui::overlay_stack::OverlayStack>,
    _agent: Option<&Arc<AgentHarness>>,
    config: &crate::config::Config,
) -> bool {
    if let Some(overlay) = overlay_stack.active() {
        if let Some(on_key) = &overlay.on_key
            && on_key(key)
        {
            return true;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if overlay.dismissible {
                overlay_stack.pop();
            }
            return true;
        }
        if !overlay.route_app_keys {
            return true;
        }
    }

    let state = Rc::clone(&stack.active().state);

    if key.code == KeyCode::Esc {
        if state.slash_popup.with(|p| p.is_some()) {
            state.slash_popup.set(None);
            return true;
        }
        if state.input.with(|input| !input.is_empty()) {
            state.input.update(|input| input.clear());
            return true;
        }
        if state.busy.get() {
            state.busy.set(false);
            state.status.update(|s| s.busy = false);
            return true;
        }
        return true;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
        iodilos::quit();
        return true;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        let has_input = state.input.with(|input| !input.is_empty());
        let has_overlap = stack.overlap_is_active_or_pending();
        if has_input {
            state.input.update(|input| input.clear());
            state.slash_popup.set(None);
        } else if has_overlap {
            let command_side = use_context::<Option<Rc<crate::core::extensions::CommandSide>>>();
            if let Some(cs) = command_side.as_ref() {
                cs.close_active_overlap();
            }
        } else {
            iodilos::quit();
        }
        return true;
    }

    if key.code == KeyCode::PageUp {
        state.scroll_up(10);
        return true;
    }
    if key.code == KeyCode::PageDown {
        state.scroll_down(10);
        return true;
    }

    route_editor_key(key, stack, config)
}

fn route_editor_key(
    key: KeyEvent,
    stack: &Rc<crate::tui::conversation::ConversationStack>,
    config: &crate::config::Config,
) -> bool {
    let state = Rc::clone(&stack.active().state);
    let command_side = use_context::<Option<Rc<crate::core::extensions::CommandSide>>>();
    let slash_commands_enabled = stack.active_slash_command_scope() == SlashCommandScope::Global;
    let mut commands = Vec::new();
    if slash_commands_enabled {
        commands = crate::tui::slash_commands::static_command_entries();
        if let Some(cs) = command_side.as_ref() {
            for cmd in cs.commands() {
                commands.push(crate::tui::slash_commands::CommandEntry {
                    name: cmd.name.clone(),
                    description: cmd.meta.description.clone(),
                    subcommands: cmd
                        .meta
                        .subcommands
                        .iter()
                        .map(|s| crate::tui::slash_commands::SubcommandEntry {
                            name: s.name.clone(),
                            description: s.description.clone(),
                        })
                        .collect(),
                });
            }
        }
    }

    let mut input = state.input.get_clone();
    let mut popup = state.slash_popup.get_clone();
    let action = editor::handle_key(
        &mut input,
        &mut popup,
        key,
        config,
        &commands,
        slash_commands_enabled,
    );
    state.input.set(input);
    state.slash_popup.set(popup);

    match action {
        EditorAction::Submit => submit_editor_text(
            state,
            stack,
            config,
            slash_commands_enabled,
            command_side,
            commands,
        ),
        EditorAction::None => true,
    }
}

fn submit_editor_text(
    state: Rc<crate::tui::state::UiState>,
    stack: &Rc<crate::tui::conversation::ConversationStack>,
    config: &crate::config::Config,
    slash_commands_enabled: bool,
    command_side: Option<Rc<crate::core::extensions::CommandSide>>,
    commands: Vec<crate::tui::slash_commands::CommandEntry>,
) -> bool {
    let text = state.input.with(|es| es.text()).trim().to_string();
    if text.is_empty() {
        return true;
    }
    state.input.update(|input| input.clear());
    state.slash_popup.set(None);

    let skill_invocation = slash_commands_enabled
        .then(|| crate::tui::slash_commands::parse_skill_command(&text))
        .flatten();
    if let Some(inv) = skill_invocation {
        match crate::tui::slash_commands::validate_skill_name(&inv.skill_name, config) {
            Ok(()) => {
                if state.busy.get() {
                    return true;
                }
                state.push_user(&text);
                let prompt = crate::tui::slash_commands::build_skill_prompt(&inv);
                submit_to_layer(&stack.main_layer(), prompt);
            }
            Err(available) => {
                if available.is_empty() {
                    state.push_error(format!(
                        "Skill '{}' not found. No skills are installed. Use /skills for help.",
                        inv.skill_name
                    ));
                } else {
                    state.push_error(format!(
                        "Skill '{}' not found. Available: {}",
                        inv.skill_name,
                        available.join(", ")
                    ));
                }
            }
        }
    } else if slash_commands_enabled && text.starts_with('/') {
        if let Some(cs) = command_side.as_ref()
            && cs.dispatch(&text)
        {
            return true;
        }
        let mut handle: Rc<crate::tui::state::UiState> = Rc::clone(&state);
        let should_quit =
            crate::tui::slash_commands::handle_slash_command(&text, &mut handle, config, &commands);
        if should_quit {
            iodilos::quit();
        }
    } else {
        if state.busy.get() {
            return true;
        }
        state.push_user(&text);
        submit_to_layer(&stack.active(), text);
    }
    true
}

fn submit_to_layer(layer: &Rc<crate::tui::conversation::ConversationLayer>, text: String) {
    match layer.submit_prompt(text) {
        crate::tui::conversation::SubmitOutcome::Queued => {
            layer.state.busy.set(true);
            layer.state.status.update(|s| s.busy = true);
        }
        crate::tui::conversation::SubmitOutcome::NoAgent => {
            layer
                .state
                .push_error("No LLM agent available. Check your config.");
        }
        crate::tui::conversation::SubmitOutcome::DriverGone => {
            layer
                .state
                .push_error("Layer driver exited. Cannot send prompt.");
        }
        crate::tui::conversation::SubmitOutcome::ChannelFull => {
            layer
                .state
                .push_error("Layer driver busy (command queue full).");
        }
    }
}
