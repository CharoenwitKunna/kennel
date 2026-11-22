//! Title bar / toolbar and the agent sidebar.

use crate::*;

impl App {
    pub(crate) fn toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("toolbar")
            .exact_height(32.0)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin { left: 6, right: 0, top: 0, bottom: 0 }).stroke(Stroke::new(1.0_f32, BORDER)))
            .show(ctx, |ui| {
                // Empty toolbar space acts as the title bar. Registered first so
                // the buttons drawn on top keep their clicks.
                let bar = ui.interact(ui.max_rect(), egui::Id::new("titlebar"), Sense::click_and_drag());
                let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                if bar.double_clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                } else if bar.drag_started_by(egui::PointerButton::Primary) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                ui.horizontal_centered(|ui| {
                    let tip = if self.state.settings.sidebar_hidden { "Show sidebar (Ctrl+Shift+B)" } else { "Hide sidebar (Ctrl+Shift+B)" };
                    if ui.add(icon_button("☰")).on_hover_text(tip).clicked() {
                        self.state.settings.sidebar_hidden = !self.state.settings.sidebar_hidden;
                        self.dirty = true;
                    }
                    logo(ui);
                    ui.label(RichText::new("Kennel").font(bold(14.0)));
                    ui.add_space(8.0);
                    segmented(ui, &mut self.view);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        if window_button(ui, WinBtn::Close).on_hover_text("Close").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        let (kind, tip) = if maximized { (WinBtn::Restore, "Restore") } else { (WinBtn::Maximize, "Maximize") };
                        if window_button(ui, kind).on_hover_text(tip).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                        }
                        if window_button(ui, WinBtn::Minimize).on_hover_text("Minimize").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                        ui.add_space(8.0);
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let resp = ui.add(primary_button("+  New agent")).on_hover_text("Ctrl+Shift+N");
                        let popup_id = egui::Id::new("new_agent_menu");
                        if resp.clicked() || std::mem::take(&mut self.open_new_menu) {
                            ui.memory_mut(|m| m.toggle_popup(popup_id));
                        }
                        egui::popup_below_widget(
                            ui,
                            popup_id,
                            &resp,
                            egui::PopupCloseBehavior::CloseOnClickOutside,
                            |ui| {
                                if let Some(p) = new_agent_menu(ui, &mut self.new_folder) {
                                    self.actions.push(Action::Create(p));
                                    ui.memory_mut(|m| m.close_popup());
                                }
                            },
                        );
                        if ui.add(icon_button("⚙")).on_hover_text("Settings").clicked() {
                            self.show_settings = !self.show_settings;
                            self.cli_cache = None;
                        }
                        match self.update.clone() {
                            UpdateState::Available(r) => {
                                if ui
                                    .add(primary_button(&format!("⬆  Update to {}", r.version)))
                                    .on_hover_text(format!("You have {}. Downloads the new version from GitHub.", update::CURRENT))
                                    .clicked()
                                {
                                    self.install_update(ctx);
                                }
                            }
                            UpdateState::Installing(v) => {
                                ui.add(egui::Spinner::new().size(14.0));
                                ui.label(RichText::new(format!("Downloading {v}…")).small().color(MUTED));
                            }
                            UpdateState::Ready(v) => {
                                if ui
                                    .add(primary_button("↻  Restart to update"))
                                    .on_hover_text(format!("Restarts into {v}; running agents come back"))
                                    .clicked()
                                {
                                    self.restart_for_update(ctx);
                                }
                            }
                            UpdateState::Idle | UpdateState::Checking => {}
                        }
                        let running = self.state.agents.iter().filter(|a| self.status(a.id).alive()).count();
                        let stopped: Vec<u64> =
                            self.state.agents.iter().map(|a| a.id).filter(|id| !self.status(*id).alive()).collect();
                        if !stopped.is_empty()
                            && ui
                                .add(ghost_button(&format!("▶  Start all ({})", stopped.len())))
                                .on_hover_text("Start every stopped agent")
                                .clicked()
                        {
                            let newly_started = stopped.len();
                            for id in stopped {
                                self.actions.push(Action::Start(id));
                            }
                            if running + newly_started > 1 {
                                self.view = View::Grid;
                            }
                        }
                    });
                });
            });
    }

    pub(crate) fn sidebar(&mut self, ctx: &egui::Context) {
        if self.state.settings.sidebar_hidden {
            return;
        }
        let saved_w = self.state.layout.sidebar_width.unwrap_or(184.0).clamp(150.0, 360.0);
        let resp = egui::SidePanel::left("sidebar")
            .resizable(true)
            .default_width(saved_w)
            .width_range(150.0..=360.0)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::same(4)).stroke(Stroke::new(1.0_f32, BORDER)))
            .show(ctx, |ui| {
                if self.state.agents.is_empty() {
                    ui.add_space(16.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("No agents yet").color(MUTED));
                        ui.add_space(6.0);
                        if ui.add(primary_button("New agent")).clicked() {
                            self.actions.push(Action::NewAgent);
                        }
                    });
                    return;
                }

                let agents = self.state.agents.clone();
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    let mut rects = Vec::with_capacity(agents.len());
                    for a in &agents {
                        let status = self.status(a.id);
                        let selected = self.selected == Some(a.id);
                        let attention = self.attention.contains(&a.id);
                        let (rect, resp) = self.agent_card(ui, a, status, selected, attention);
                        rects.push((a.id, rect));
                        let Some(resp) = resp else { continue };
                        if resp.drag_started() {
                            self.dragging = Some(a.id);
                        }
                        if resp.clicked() {
                            self.actions.push(Action::Select(a.id));
                        }
                        if resp.double_clicked() {
                            self.begin_rename(a.id);
                        }
                        resp.context_menu(|ui| {
                            ui.set_min_width(160.0);
                            if ui.button("Rename").clicked() {
                                self.begin_rename(a.id);
                                ui.close_menu();
                            }
                            if status.alive() {
                                if ui.button("Restart").clicked() {
                                    self.actions.push(Action::Restart(a.id));
                                    ui.close_menu();
                                }
                                if ui.button("Stop").clicked() {
                                    self.actions.push(Action::Stop(a.id));
                                    ui.close_menu();
                                }
                            } else if ui.button("Start").clicked() {
                                self.actions.push(Action::Start(a.id));
                                ui.close_menu();
                            }
                            if ui.button("Edit…").clicked() {
                                self.actions.push(Action::Edit(a.id));
                                ui.close_menu();
                            }
                            if ui.button("New session").clicked() {
                                self.actions.push(Action::NewSession(a.id));
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button(RichText::new("Delete…").color(RED)).clicked() {
                                self.actions.push(Action::Delete(a.id));
                                ui.close_menu();
                            }
                        });
                    }
                    self.drag_reorder(ui, &rects);
                });
            });
        let cur_w = resp.response.rect.width().round();
        if (150.0..=360.0).contains(&cur_w) && self.state.layout.sidebar_width != Some(cur_w) {
            self.state.layout.sidebar_width = Some(cur_w);
            self.dirty = true;
        }
    }

    /// One-row sidebar card: icon, name, status, cost. Returns its rect, and
    /// its response unless it is being renamed (the text box owns input then).
    pub(crate) fn agent_card(
        &mut self,
        ui: &mut egui::Ui,
        a: &Agent,
        status: Status,
        selected: bool,
        attention: bool,
    ) -> (egui::Rect, Option<egui::Response>) {
        let renaming = self.renaming.as_ref().is_some_and(|(id, _)| *id == a.id);
        let unread = self.unread(a.id);
        let age = self.status_age(a.id);
        let inner = egui::Frame::new()
            .fill(if selected { RAISED } else { PANEL })
            .stroke(Stroke::new(1.0_f32, if selected { Color32::from_rgb(0x34, 0x3B, 0x50) } else { Color32::TRANSPARENT }))
            .corner_radius(5)
            .inner_margin(Margin::symmetric(6, 3))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.horizontal(|ui| {
                    ui.set_height(18.0);
                    ui.spacing_mut().item_spacing.x = 5.0;
                    app_icon(ui, a.provider, 14.0);
                    if renaming {
                        self.rename_box(ui, a.id, 12.5, ui.available_width() - 2.0);
                        return;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        status_indicator(ui, status, false, age);
                        unread_badge(ui, unread);
                        if attention {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(6.0), Sense::hover());
                            ui.painter().circle_filled(r.center(), 3.0, AMBER);
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add(egui::Label::new(RichText::new(&a.name).font(bold(12.5))).truncate().selectable(false));
                        });
                    });
                });
                if !renaming {
                    // Running subagents, one small line each under the card.
                    let subs = self
                        .live
                        .get(&a.id)
                        .and_then(|l| l.claude.as_ref())
                        .map(|t| t.subagents.as_slice())
                        .unwrap_or(&[]);
                    const MAX_SUBS: usize = 3;
                    for s in subs.iter().take(MAX_SUBS) {
                        ui.horizontal(|ui| {
                            ui.add_space(19.0);
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let (r, _) = ui.allocate_exact_size(Vec2::new(8.0, 12.0), Sense::hover());
                            // Tree elbow: "└" in the agent's color.
                            let stroke = Stroke::new(1.0_f32, a.provider.color().gamma_multiply(0.7));
                            let (x, top, mid) = (r.left() + 2.5, r.top() + 1.0, r.center().y);
                            ui.painter().line_segment([egui::pos2(x, top), egui::pos2(x, mid)], stroke);
                            ui.painter().line_segment([egui::pos2(x, mid), egui::pos2(r.right(), mid)], stroke);
                            let label = ui.add(
                                egui::Label::new(RichText::new(&s.description).size(10.5).color(MUTED))
                                    .truncate()
                                    .selectable(false),
                            );
                            if !s.kind.is_empty() {
                                label.on_hover_text(format!("Subagent ({}): {}", s.kind, s.description));
                            }
                        });
                    }
                    if subs.len() > MAX_SUBS {
                        ui.horizontal(|ui| {
                            ui.add_space(31.0);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!("+{} more subagents", subs.len() - MAX_SUBS)).size(10.0).color(FAINT),
                                )
                                .selectable(false),
                            );
                        });
                    }
                }
            });
        let rect = inner.response.rect;
        if renaming {
            return (rect, None);
        }
        let resp = ui.interact(rect, egui::Id::new(("card", a.id)), Sense::click_and_drag());
        if self.dragging == Some(a.id) {
            ui.painter().rect_filled(rect, 6.0, Color32::from_black_alpha(110));
        }
        let resp = if self.dragging.is_none() {
            resp.on_hover_text(format!("{}\n\nDouble-click to rename · drag to reorder", self.info_text(a)))
        } else {
            resp
        };
        (rect, Some(resp))
    }

    /// While a card is dragged: show where it will land, and move it on release.
    pub(crate) fn drag_reorder(&mut self, ui: &mut egui::Ui, rects: &[(u64, egui::Rect)]) {
        let Some(drag_id) = self.dragging else { return };
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        let (pos, released) = ui.input(|i| (i.pointer.interact_pos(), !i.pointer.primary_down()));
        let (Some(pos), Some(last)) = (pos, rects.last()) else {
            self.dragging = None;
            return;
        };
        // Slot = how many cards sit above the pointer.
        let target = rects.iter().position(|(_, r)| pos.y < r.center().y).unwrap_or(rects.len());
        let y = if target < rects.len() { rects[target].1.top() - 2.0 } else { last.1.bottom() + 2.0 };
        let x = last.1.x_range();
        ui.painter().hline(x, y, Stroke::new(2.0_f32, ACCENT));
        if released {
            self.dragging = None;
            if let Some(from) = self.state.agents.iter().position(|a| a.id == drag_id) {
                let to = if from < target { target - 1 } else { target };
                if to != from {
                    let a = self.state.agents.remove(from);
                    self.state.agents.insert(to.min(self.state.agents.len()), a);
                    self.dirty = true;
                }
            }
        }
    }
}
