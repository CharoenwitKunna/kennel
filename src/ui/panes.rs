//! Central area: Focus view (tabs + one terminal) and Grid view.

use crate::*;

impl App {
    pub(crate) fn central(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG).inner_margin(Margin::same(4))).show(ui, |ui| {
            let area = ui.max_rect();
            if self.state.agents.is_empty() {
                empty_state(ui, &mut self.actions, &mut self.new_folder);
                return;
            }
            let key = (self.view, if self.view == View::Focus { self.selected } else { None });
            if key != self.view_key {
                self.view_key = key;
                self.view_changed = Instant::now();
            }
            match self.view {
                View::Focus => self.focus_view(ui),
                View::Grid => self.grid_view(ui),
            }
            // Fade the new content in: a background-colored veil that clears quickly.
            let t = self.view_changed.elapsed().as_secs_f32() / FADE;
            if t < 1.0 {
                let veil = (1.0 - t).powi(2);
                ui.painter().rect_filled(area.expand(4.0), 0.0, BG.gamma_multiply(veil));
                ui.ctx().request_repaint();
            }
        });
    }

    pub(crate) fn focus_view(&mut self, ui: &mut egui::Ui) {
        // One row: tabs on the left; the open agent's stats and buttons on the right.
        let current = self.selected.and_then(|id| self.state.agent(id).cloned());
        ui.horizontal(|ui| {
            ui.set_height(24.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(a) = &current {
                    let context = self.live.get(&a.id).and_then(|l| l.claude.as_ref()).and_then(|t| t.context_pct);
                    self.pane_controls(ui, a, self.status(a.id), false);
                    ui.add_space(6.0);
                    ui.spacing_mut().item_spacing.x = 3.0;
                    pane_stats(ui, a, context, true);
                    ui.add_space(8.0);
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| self.tab_strip(ui));
            });
        });
        ui.add_space(2.0);
        let Some(id) = self.selected.filter(|id| self.state.agent(*id).is_some()) else {
            ui.centered_and_justified(|ui| ui.label(RichText::new("Select an agent").color(MUTED)));
            return;
        };
        self.agent_pane(ui, id, false);
    }

    /// Browser-style tabs, one per agent (Focus view).
    pub(crate) fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let agents = self.state.agents.clone();
        egui::ScrollArea::horizontal()
            .id_salt("tabs")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for a in &agents {
                        let selected = self.selected == Some(a.id);
                        let status = self.status(a.id);
                        let unread = if selected { 0 } else { self.unread(a.id) };
                        let attention = self.attention.contains(&a.id);
                        let age = self.status_age(a.id);
                        let renaming = self.renaming.as_ref().is_some_and(|(rid, _)| *rid == a.id);
                        let inner = egui::Frame::new()
                            .corner_radius(CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 })
                            .inner_margin(Margin { left: 8, right: 8, top: 3, bottom: 3 })
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 5.0;
                                    app_icon(ui, a.provider, 12.0);
                                    if renaming {
                                        self.rename_box(ui, a.id, 12.0, 140.0);
                                        return;
                                    }
                                    let color = if selected { TEXT } else { MUTED };
                                    ui.add(
                                        egui::Label::new(RichText::new(&a.name).font(bold(12.0)).color(color))
                                            .selectable(false),
                                    );
                                    status_indicator(ui, status, false, age);
                                    unread_badge(ui, unread);
                                    if attention && !selected {
                                        let (r, _) = ui.allocate_exact_size(Vec2::splat(6.0), Sense::hover());
                                        ui.painter().circle_filled(r.center(), 3.0, AMBER);
                                    }
                                });
                            });
                        let rect = inner.response.rect;
                        if renaming {
                            continue;
                        }
                        let resp = ui.interact(rect, egui::Id::unique(("tab", a.id)), Sense::click());
                        if resp.double_clicked() {
                            self.begin_rename(a.id);
                        }
                        if selected {
                            let y = rect.bottom() - 1.0;
                            ui.painter().line_segment(
                                [egui::pos2(rect.left() + 6.0, y), egui::pos2(rect.right() - 6.0, y)],
                                Stroke::new(2.0_f32, a.provider.color()),
                            );
                        } else if resp.hovered() {
                            ui.painter().rect_filled(rect, 7.0, Color32::from_white_alpha(8));
                        }
                        if resp.clicked() {
                            self.actions.push(Action::Select(a.id));
                        }
                        if resp.middle_clicked() && status.alive() {
                            self.actions.push(Action::Stop(a.id));
                        }
                        resp.on_hover_text(format!(
                            "{}\n\nDouble-click to rename · middle-click to stop",
                            self.info_text(a)
                        ));
                    }
                });
            });
    }

    pub(crate) fn grid_view(&mut self, ui: &mut egui::Ui) {
        let ids: Vec<u64> = self.state.agents.iter().map(|a| a.id).filter(|id| self.live.contains_key(id)).collect();
        if ids.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("No agents running. Start some from the sidebar, or use “Start all”.").color(MUTED),
                );
            });
            return;
        }
        let n = ids.len();
        let cols = (n as f32).sqrt().ceil() as usize;
        let rows = n.div_ceil(cols);
        if self.grid_cols.len() != cols {
            self.grid_cols = vec![1.0 / cols as f32; cols];
        }
        if self.grid_rows.len() != rows {
            self.grid_rows = vec![1.0 / rows as f32; rows];
        }
        let gap = 4.0;
        let area = ui.available_rect_before_wrap();
        ui.allocate_rect(area, Sense::hover());
        let avail_w = area.width() - gap * (cols as f32 - 1.0);
        let avail_h = area.height() - gap * (rows as f32 - 1.0);
        let xs = offsets(&self.grid_cols, avail_w, gap, area.left());
        let ys = offsets(&self.grid_rows, avail_h, gap, area.top());

        for (i, id) in ids.into_iter().enumerate() {
            let (c, r) = (i % cols, i / cols);
            let rect = egui::Rect::from_min_size(
                egui::pos2(xs[c], ys[r]),
                Vec2::new(self.grid_cols[c] * avail_w, self.grid_rows[r] * avail_h),
            );
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Min)));
            self.agent_pane(&mut child, id, true);
        }

        // Draggable dividers between columns and between rows (double-click resets).
        for c in 0..cols.saturating_sub(1) {
            let x = xs[c + 1] - gap / 2.0;
            let hit = egui::Rect::from_x_y_ranges(x - gap / 2.0..=x + gap / 2.0, area.y_range());
            let resp = ui
                .interact(hit, egui::Id::unique(("col_div", c)), Sense::click_and_drag())
                .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            if resp.dragged() {
                resize_pair(&mut self.grid_cols, c, resp.drag_delta().x / avail_w);
            }
            if resp.double_clicked() {
                self.grid_cols = vec![1.0 / cols as f32; cols];
            }
            if resp.hovered() || resp.dragged() {
                ui.painter().vline(x, area.y_range(), Stroke::new(2.0_f32, ACCENT.gamma_multiply(0.7)));
            }
        }
        for r in 0..rows.saturating_sub(1) {
            let y = ys[r + 1] - gap / 2.0;
            let hit = egui::Rect::from_x_y_ranges(area.x_range(), y - gap / 2.0..=y + gap / 2.0);
            let resp = ui
                .interact(hit, egui::Id::unique(("row_div", r)), Sense::click_and_drag())
                .on_hover_cursor(egui::CursorIcon::ResizeVertical);
            if resp.dragged() {
                resize_pair(&mut self.grid_rows, r, resp.drag_delta().y / avail_h);
            }
            if resp.double_clicked() {
                self.grid_rows = vec![1.0 / rows as f32; rows];
            }
            if resp.hovered() || resp.dragged() {
                ui.painter().hline(area.x_range(), y, Stroke::new(2.0_f32, ACCENT.gamma_multiply(0.7)));
            }
        }
    }

    /// Stop/start, maximize/back and the ⋯ menu, laid out right to left.
    pub(crate) fn pane_controls(&mut self, ui: &mut egui::Ui, a: &Agent, status: Status, in_grid: bool) {
        let id = a.id;
        ui.spacing_mut().item_spacing.x = 2.0;
        let more = ui.add(icon_button("⋯")).on_hover_text("More");
        egui::Popup::menu(&more).close_behavior(egui::PopupCloseBehavior::CloseOnClick).show(|ui| {
            ui.set_min_width(160.0);
            if ui.button("Rename").clicked() {
                self.begin_rename(id);
                ui.close();
            }
            if status.alive() && ui.button("Restart").clicked() {
                self.actions.push(Action::Restart(id));
                ui.close();
            }
            if ui.button("Edit…").clicked() {
                self.actions.push(Action::Edit(id));
                ui.close();
            }
            if ui.button("New session").clicked() {
                self.actions.push(Action::NewSession(id));
                ui.close();
            }
            if ui.button("Copy session id").clicked() {
                if let Some(s) = &a.session_id {
                    ui.ctx().copy_text(s.clone());
                }
                ui.close();
            }
            ui.separator();
            if ui.button(RichText::new("Delete…").color(RED)).clicked() {
                self.actions.push(Action::Delete(id));
                ui.close();
            }
        });
        let (icon, tip) =
            if in_grid { ("⤢", "Maximize (or double-click the title)") } else { ("⤡", "Back to grid") };
        if ui.add(icon_button(icon)).on_hover_text(tip).clicked() {
            if in_grid {
                self.actions.push(Action::OpenFocus(id));
            } else {
                self.view = View::Grid;
            }
        }
        if status.alive() {
            if ui.add(icon_button("■")).on_hover_text("Stop").clicked() {
                self.actions.push(Action::Stop(id));
            }
        } else {
            let tip = if a.session_id.is_some() { "Resume" } else { "Start" };
            if ui.add(icon_button("▶")).on_hover_text(tip).clicked() {
                self.actions.push(Action::Start(id));
            }
        }
    }

    /// One agent: a slim title row (grid only) on top of its terminal.
    pub(crate) fn agent_pane(&mut self, ui: &mut egui::Ui, id: u64, in_grid: bool) {
        let Some(a) = self.state.agent(id).cloned() else { return };
        let status = self.status(id);
        let age = self.status_age(id);
        let context = self.live.get(&id).and_then(|l| l.claude.as_ref()).and_then(|t| t.context_pct);
        let focused = ui.memory(|m| m.has_focus(term_id(id)));
        let border = if focused {
            a.provider.color().gamma_multiply(0.85)
        } else if self.attention.contains(&id) {
            AMBER.gamma_multiply(0.7)
        } else {
            BORDER
        };
        egui::Frame::new()
            .fill(PANEL)
            .stroke(Stroke::new(1.0_f32, border))
            .corner_radius(6)
            .inner_margin(Margin::same(2))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                // In Focus view the tab strip carries the name, stats and buttons.
                if in_grid {
                    // Empty space in the title row: double-click to maximize.
                    let title_rect = egui::Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), 22.0));
                    let title_bg = ui.interact(title_rect, egui::Id::unique(("title", id)), Sense::click());
                    if title_bg.double_clicked() {
                        self.actions.push(Action::OpenFocus(id));
                    }
                    ui.horizontal(|ui| {
                        ui.set_height(22.0);
                        ui.spacing_mut().item_spacing.x = 5.0;
                        ui.add_space(3.0);
                        app_icon(ui, a.provider, 14.0);
                        self.name_widget(ui, &a, 12.5);
                        status_indicator(ui, status, true, age);
                        ui.spacing_mut().item_spacing.x = 3.0;
                        ui.label(RichText::new("·").small().color(FAINT));
                        pane_stats(ui, &a, context, false);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            self.pane_controls(ui, &a, status, true);
                        });
                    });
                }
                if let Some(p) = context {
                    // Thin context gauge under the title.
                    let (bar, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 2.0), Sense::hover());
                    let bar = bar.shrink2(Vec2::new(4.0, 0.0));
                    ui.painter().rect_filled(bar, 1.5, BORDER);
                    let fill = egui::Rect::from_min_size(
                        bar.min,
                        Vec2::new(bar.width() * (p / 100.0).clamp(0.0, 1.0), bar.height()),
                    );
                    ui.painter().rect_filled(fill, 1.5, context_color(p));
                    resp.on_hover_text(format!("Context {p:.0}% used"));
                }
                egui::Frame::new().fill(TERM_BG).corner_radius(4).inner_margin(Margin::same(1)).show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    self.terminal_or_placeholder(ui, id, status, &a);
                });
            });
    }

    pub(crate) fn begin_rename(&mut self, id: u64) {
        if let Some(a) = self.state.agent(id) {
            self.renaming = Some((id, a.name.clone()));
            self.rename_focus = true;
        }
    }

    /// Agent name; double-click turns it into a text box.
    pub(crate) fn name_widget(&mut self, ui: &mut egui::Ui, a: &Agent, size: f32) {
        if self.renaming.as_ref().is_some_and(|(id, _)| *id == a.id) {
            // As wide as the name (plus room to type), so the title doesn't shrink.
            let name_w = ui.fonts_mut(|f| f.layout_no_wrap(a.name.clone(), bold(size), TEXT).size().x);
            let width = (name_w + 40.0).clamp(140.0, 320.0).min(ui.available_width() - 120.0);
            self.rename_box(ui, a.id, size, width);
            return;
        }
        let resp = ui
            .add(egui::Label::new(RichText::new(&a.name).font(bold(size))).sense(Sense::click()).selectable(false))
            .on_hover_text(format!("{}\n\nDouble-click to rename", self.info_text(a)));
        if resp.double_clicked() {
            self.begin_rename(a.id);
        }
    }

    /// Inline rename box: Enter saves, Esc or empty cancels.
    pub(crate) fn rename_box(&mut self, ui: &mut egui::Ui, id: u64, size: f32, width: f32) {
        let Some((_, buf)) = self.renaming.as_mut() else { return };
        let resp = ui.add(
            egui::TextEdit::singleline(buf)
                .font(bold(size))
                .desired_width(width.max(60.0))
                .margin(Margin::symmetric(4, 1)),
        );
        if std::mem::take(&mut self.rename_focus) {
            resp.request_focus();
        }
        if resp.lost_focus() {
            let cancelled = ui.input(|i| i.key_pressed(Key::Escape));
            let (_, name) = self.renaming.take().unwrap();
            let name = name.trim();
            if !cancelled && !name.is_empty() {
                if let Some(a) = self.state.agent_mut(id) {
                    a.name = name.to_string();
                    self.dirty = true;
                }
            }
        }
    }

    pub(crate) fn terminal_or_placeholder(&mut self, ui: &mut egui::Ui, id: u64, status: Status, a: &Agent) {
        let font_size = self.state.settings.font_size;
        let single_pane = self.view == View::Focus;
        if let Some(l) = self.live.get_mut(&id) {
            if self.focus_term == Some(id) {
                ui.memory_mut(|m| m.request_focus(term_id(id)));
                self.focus_term = None;
            }
            let cwd = match a.cwd.trim() {
                "" => PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()),
                c => PathBuf::from(c),
            };
            let out = term_view::show(ui, &mut l.sess, term_id(id), font_size, &cwd, &mut l.search, single_pane);
            if out.typed {
                l.interacted = true;
            }
            if out.typed || out.response.clicked() {
                self.attention.remove(&id);
                if self.selected != Some(id) {
                    self.selected = Some(id);
                }
            }
            if let Status::Exited(code) = status {
                let r = out.response.rect;
                let banner = egui::Rect::from_min_size(
                    egui::pos2(r.center().x - 170.0, r.bottom() - 56.0),
                    Vec2::new(340.0, 44.0),
                );
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(banner));
                egui::Frame::new()
                    .fill(RAISED)
                    .stroke(Stroke::new(1.0_f32, BORDER))
                    .corner_radius(10)
                    .inner_margin(Margin::symmetric(12, 7))
                    .show(&mut child, |ui| {
                        ui.horizontal_centered(|ui| {
                            ui.label(RichText::new(format!("Process exited (code {code})")).color(MUTED));
                            if ui.add(primary_button("↻  Restart")).clicked() {
                                self.actions.push(Action::Restart(id));
                            }
                        });
                    });
            }
        } else {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() / 2.0 - 70.0).max(0.0));
                    ui.label(RichText::new("◇").size(34.0).color(a.provider.color()));
                    ui.label(RichText::new(format!("{} isn't running", a.name)).font(bold(16.0)));
                    let hint = if a.session_id.is_some() {
                        "Resume picks up the previous conversation."
                    } else {
                        "Start opens a fresh interactive session in this pane."
                    };
                    ui.label(RichText::new(hint).small().color(MUTED));
                    ui.add_space(10.0);
                    let label = if a.session_id.is_some() { "▶  Resume session" } else { "▶  Start" };
                    if ui.add(primary_button(label)).clicked() {
                        self.actions.push(Action::Start(id));
                    }
                });
            });
        }
    }
}
