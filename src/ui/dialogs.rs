//! Edit / Settings / Delete windows, toasts and keyboard shortcuts.

use crate::*;

impl App {
    pub(crate) fn editor_window(&mut self, ctx: &egui::Context) {
        let Some((editing, draft)) = &mut self.editor else { return };
        let editing = *editing;
        let mut open = true;
        let (mut save, mut cancel) = (false, false);
        let running = editing.is_some_and(|id| self.live.contains_key(&id));
        egui::Window::new(if editing.is_some() { "Edit agent" } else { "New agent" })
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size([500.0, 0.0])
            .show(ctx, |ui| {
                field_label(ui, "Name");
                ui.add(
                    egui::TextEdit::singleline(&mut draft.name)
                        .hint_text("e.g. backend-refactor")
                        .desired_width(f32::INFINITY)
                        .margin(Margin::symmetric(6, 4)),
                );
                ui.add_space(8.0);
                field_label(ui, "Provider");
                ui.columns(2, |cols| {
                    for (i, p) in Provider::ALL.into_iter().enumerate() {
                        let sub = match p {
                            Provider::Claude => "Anthropic's coding agent",
                            Provider::OpenCode => "Open-source, many models",
                        };
                        if provider_card(&mut cols[i], p, sub, draft.provider == p).clicked() && draft.provider != p {
                            draft.provider = p;
                            draft.model = p.default_model().into();
                        }
                    }
                });
                ui.add_space(8.0);
                field_label(ui, "Model");
                if draft.provider == Provider::OpenCode {
                    draft.model.clear();
                    egui::Frame::new().fill(RAISED).corner_radius(8).inner_margin(Margin::symmetric(10, 7)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new(
                                "OpenCode uses the model you last picked inside it. Type /models in its terminal to switch (e.g. Hy4 Flash Free).",
                            )
                            .small()
                            .color(MUTED),
                        );
                    });
                } else {
                ui.horizontal(|ui| {
                    let hint = "default";
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.model)
                            .hint_text(hint)
                            .desired_width(ui.available_width() - 40.0)
                            .margin(Margin::symmetric(6, 4)),
                    );
                    ui.menu_button("▾", |ui| {
                        ui.set_min_width(260.0);
                        if ui.button("Default").clicked() {
                            draft.model.clear();
                            ui.close_menu();
                        }
                        for m in draft.provider.model_suggestions() {
                            if ui.button(*m).clicked() {
                                draft.model = m.to_string();
                                ui.close_menu();
                            }
                        }
                    });
                });
                }
                ui.add_space(8.0);
                field_label(ui, "Folder");
                ui.add(
                    egui::TextEdit::singleline(&mut draft.cwd)
                        .hint_text("C:\\path\\to\\project")
                        .desired_width(f32::INFINITY)
                        .margin(Margin::symmetric(6, 4)),
                );
                if !draft.cwd.trim().is_empty() && !std::path::Path::new(draft.cwd.trim()).is_dir() {
                    ui.label(RichText::new("Folder not found").small().color(RED));
                }
                ui.add_space(8.0);
                field_label(ui, "Permissions");
                for p in Permission::ALL {
                    let selected = draft.permission == p;
                    let resp = egui::Frame::new()
                        .fill(if selected { RAISED } else { Color32::TRANSPARENT })
                        .stroke(Stroke::new(1.0_f32, if selected { ACCENT.gamma_multiply(0.6) } else { BORDER }))
                        .corner_radius(8)
                        .inner_margin(Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(if selected { "◉" } else { "○" }).color(if selected { ACCENT } else { MUTED }));
                                ui.vertical(|ui| {
                                    let color = if p == Permission::FullAuto { AMBER } else { TEXT };
                                    ui.label(RichText::new(p.label()).color(color));
                                    ui.label(RichText::new(p.hint()).small().color(MUTED));
                                });
                            });
                        })
                        .response
                        .interact(Sense::click());
                    if resp.clicked() {
                        draft.permission = p;
                    }
                    ui.add_space(2.0);
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if running {
                        ui.label(RichText::new("Changes apply on the next start.").small().color(MUTED));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let ok = !draft.name.trim().is_empty();
                        let label = if editing.is_some() { "Save" } else { "Create agent" };
                        if ui.add_enabled(ok, primary_button(label)).clicked() {
                            save = true;
                        }
                        if ui.add(ghost_button("Cancel")).clicked() {
                            cancel = true;
                        }
                    });
                });
            });

        if save {
            let (editing, d) = self.editor.take().unwrap();
            let id = match editing {
                Some(id) => id,
                None => {
                    let id = self.state.alloc_id();
                    self.state.agents.push(Agent::new(id, String::new(), d.provider));
                    id
                }
            };
            if let Some(a) = self.state.agent_mut(id) {
                if a.provider != d.provider {
                    a.reset_session(); // sessions don't carry across CLIs
                }
                a.name = d.name.trim().into();
                a.provider = d.provider;
                a.model = d.model.trim().into();
                a.cwd = d.cwd.trim().into();
                a.system_prompt = d.system_prompt;
                a.permission = d.permission;
            }
            if editing.is_none() {
                self.selected = Some(id);
                self.view = View::Focus;
            }
            self.dirty = true;
        } else if cancel || !open {
            self.editor = None;
        }
    }

    pub(crate) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let claude = runner::exe_for(Provider::Claude, &self.state.settings);
        let opencode = runner::exe_for(Provider::OpenCode, &self.state.settings);
        let mut open = true;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size([520.0, 0.0])
            .show(ctx, |ui| {
                field_label(ui, "Claude Code CLI");
                self.dirty |= ui
                    .add(
                        egui::TextEdit::singleline(&mut self.state.settings.claude_path)
                            .hint_text("auto-detect (claude)")
                            .desired_width(f32::INFINITY)
                            .margin(Margin::symmetric(6, 4)),
                    )
                    .changed();
                found_label(ui, &claude);
                ui.add_space(8.0);
                field_label(ui, "OpenCode CLI");
                self.dirty |= ui
                    .add(
                        egui::TextEdit::singleline(&mut self.state.settings.opencode_path)
                            .hint_text("auto-detect (opencode)")
                            .desired_width(f32::INFINITY)
                            .margin(Margin::symmetric(6, 4)),
                    )
                    .changed();
                found_label(ui, &opencode);
                ui.add_space(8.0);
                field_label(ui, "Terminal font size");
                self.dirty |= ui.add(egui::Slider::new(&mut self.state.settings.font_size, 10.0..=20.0).step_by(0.5)).changed();
                ui.add_space(8.0);
                field_label(ui, "Scrollback lines per terminal");
                self.dirty |= ui
                    .add(egui::Slider::new(&mut self.state.settings.scrollback, 500..=20_000).step_by(500.0).logarithmic(true))
                    .changed();
                ui.label(RichText::new("Less uses less memory with many agents. Applies to agents started after the change.").small().color(MUTED));
                ui.add_space(8.0);
                field_label(ui, "Resuming");
                self.dirty |= ui
                    .checkbox(
                        &mut self.state.settings.auto_trust_on_resume,
                        "Skip Claude's folder-trust question when resuming a conversation",
                    )
                    .changed();
                ui.label(
                    RichText::new("Only for the agent's own folder, which you trusted when the conversation started.")
                        .small()
                        .color(MUTED),
                );
                ui.add_space(8.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(agent::data_dir().display().to_string()).small().color(MUTED));
                    if ui.add(ghost_button("Open folder")).clicked() {
                        let _ = std::process::Command::new("explorer").arg(agent::data_dir()).spawn();
                    }
                });
            });
        if !open {
            self.show_settings = false;
        }
    }

    pub(crate) fn delete_window(&mut self, ctx: &egui::Context) {
        let Some(id) = self.confirm_delete else { return };
        let name = self.state.agent(id).map(|a| a.name.clone()).unwrap_or_default();
        let mut decision = None;
        egui::Window::new("Delete agent")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(format!("Delete “{name}”? Its terminal is closed and its cost history removed."));
                ui.label(RichText::new("The conversation itself stays in the CLI's own history.").small().color(MUTED));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add(danger_button("Delete")).clicked() {
                        decision = Some(true);
                    }
                    if ui.add(ghost_button("Cancel")).clicked() {
                        decision = Some(false);
                    }
                });
            });
        match decision {
            Some(true) => {
                self.stop(id);
                self.state.agents.retain(|a| a.id != id);
                self.selected = self.state.agents.first().map(|a| a.id);
                self.confirm_delete = None;
                self.dirty = true;
            }
            Some(false) => self.confirm_delete = None,
            None => {}
        }
    }

    pub(crate) fn toasts(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|(t, _)| t.elapsed() < Duration::from_secs(7));
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::new(-16.0, -40.0))
            .show(ctx, |ui| {
                for (_, msg) in &self.toasts {
                    egui::Frame::new()
                        .fill(RAISED)
                        .stroke(Stroke::new(1.0_f32, RED.gamma_multiply(0.6)))
                        .corner_radius(10)
                        .inner_margin(Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.set_max_width(380.0);
                            ui.label(RichText::new(msg).color(TEXT));
                        });
                    ui.add_space(6.0);
                }
            });
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    pub(crate) fn shortcuts(&mut self, ctx: &egui::Context) {
        let cs = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;
        ctx.input_mut(|i| {
            if i.consume_key(cs, Key::G) {
                self.view = if self.view == View::Focus { View::Grid } else { View::Focus };
            }
            if i.consume_key(cs, Key::N) {
                self.actions.push(Action::NewAgent);
            }
            if i.consume_key(cs, Key::B) {
                self.state.settings.sidebar_hidden = !self.state.settings.sidebar_hidden;
                self.dirty = true;
            }
            let digits = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
            for (n, k) in digits.into_iter().enumerate() {
                if i.consume_key(cs, k) {
                    if let Some(a) = self.state.agents.get(n) {
                        self.actions.push(Action::OpenFocus(a.id));
                    }
                }
            }
        });
    }
}
