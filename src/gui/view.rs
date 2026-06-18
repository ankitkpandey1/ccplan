//! eframe/egui rendering layer. All IO boundary — never add logic here.
#![allow(dead_code, clippy::too_many_lines, clippy::too_many_arguments)]
#![cfg_attr(coverage_nightly, coverage(off))]

use eframe::egui::{self, Color32, Key, Margin, Modifiers, Stroke};

use super::model::{
    ActivityKind, ApprovalsModel, NavTab, build_activity_model, build_agents_model,
    build_approvals_model, build_automations_model, build_block_detail_model, build_nav_model,
    build_today_model, build_up_next_model, build_upcoming_model,
};

// ── Design tokens ─────────────────────────────────────────────────────────────

const BG: Color32 = Color32::from_rgb(0x0E, 0x11, 0x16);
const SURFACE: Color32 = Color32::from_rgb(0x16, 0x1B, 0x22);
const SURFACE2: Color32 = Color32::from_rgb(0x1C, 0x22, 0x30);
const BORDER: Color32 = Color32::from_rgb(0x2A, 0x31, 0x3C);
const TEXT1: Color32 = Color32::from_rgb(0xE6, 0xED, 0xF3);
const TEXT2: Color32 = Color32::from_rgb(0x9D, 0xA7, 0xB3);
const TEXT_DIM: Color32 = Color32::from_rgb(0x6E, 0x76, 0x81);
const ACCENT: Color32 = Color32::from_rgb(0x4C, 0x8D, 0xFF);
const C_GREEN: Color32 = Color32::from_rgb(0x3F, 0xB9, 0x50);
const C_RED: Color32 = Color32::from_rgb(0xF8, 0x51, 0x49);
const C_AMBER: Color32 = Color32::from_rgb(0xD2, 0x99, 0x22);

fn status_color(status: crate::model::Status) -> Color32 {
    use crate::model::Status;
    match status {
        Status::Active => C_GREEN,
        Status::Missed => C_RED,
        Status::Expired => C_AMBER,
        Status::Done | Status::Skipped | Status::Pending => TEXT_DIM,
    }
}

// ── Action queue ──────────────────────────────────────────────────────────────

#[derive(Debug)]
enum GuiAction {
    Approve(String),
    Done(String),
    Skip(String),
    Dismiss(String),
}

// ── Toast ──────────────────────────────────────────────────────────────────────

struct Toast {
    message: String,
    is_error: bool,
    created: std::time::Instant,
}

impl Toast {
    fn success(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            is_error: false,
            created: std::time::Instant::now(),
        }
    }
    fn error(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            is_error: true,
            created: std::time::Instant::now(),
        }
    }
}

// ── App state ─────────────────────────────────────────────────────────────────

pub(crate) struct CcplanApp {
    store: crate::store::Store,
    config: crate::config::Config,
    plan: Option<crate::model::Plan>,
    fire_records: Vec<crate::store::FireRecord>,
    recurring_rules: crate::model::RecurringRules,
    upcoming_plans: Vec<crate::model::Plan>,
    active_tab: NavTab,
    selected_block_id: Option<String>,
    hovered_block_id: Option<String>,
    command_input: String,
    dark_mode: bool,
    toasts: Vec<Toast>,
    last_refresh: std::time::Instant,
    cmd_bar_id: egui::Id,
}

impl CcplanApp {
    pub(crate) fn new_with_store(
        store: crate::store::Store,
        config: crate::config::Config,
    ) -> Self {
        let mut app = Self {
            store,
            config,
            plan: None,
            fire_records: Vec::new(),
            recurring_rules: crate::model::RecurringRules::default(),
            upcoming_plans: Vec::new(),
            active_tab: NavTab::Today,
            selected_block_id: None,
            hovered_block_id: None,
            command_input: String::new(),
            dark_mode: true,
            toasts: Vec::new(),
            last_refresh: std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(10))
                .unwrap_or_else(std::time::Instant::now),
            cmd_bar_id: egui::Id::new("ccplan_cmd_bar"),
        };
        app.refresh_data();
        app
    }

    // Legacy entry point for unit-test path (no store context available).
    pub(crate) fn new() -> Self {
        let store = crate::store::Store::for_user()
            .unwrap_or_else(|_| crate::store::Store::new(std::path::Path::new("/tmp/ccplan")));
        let config = crate::config::Config::load(&store).unwrap_or_default();
        Self::new_with_store(store, config)
    }

    fn refresh_data(&mut self) {
        use jiff::tz::TimeZone;
        let now_zoned = jiff::Timestamp::now().to_zoned(TimeZone::system());
        let date_str = format!(
            "{:04}-{:02}-{:02}",
            now_zoned.year(),
            now_zoned.month(),
            now_zoned.day()
        );
        if let Ok(today) = date_str.parse::<crate::model::PlanDate>() {
            self.plan = self.store.load_plan(&today).ok().flatten();

            // Next 7 days for the Upcoming tab.
            self.upcoming_plans = (1i64..=7)
                .filter_map(|d| {
                    let future = now_zoned
                        .date()
                        .checked_add(jiff::Span::new().days(d))
                        .ok()?;
                    let ds = format!(
                        "{:04}-{:02}-{:02}",
                        future.year(),
                        future.month(),
                        future.day()
                    );
                    let pd = ds.parse::<crate::model::PlanDate>().ok()?;
                    self.store.load_plan(&pd).ok().flatten()
                })
                .collect();
        }

        self.fire_records = self.store.read_fire_log().unwrap_or_default();
        self.recurring_rules = self.store.load_recurring_rules().unwrap_or_default();
        self.last_refresh = std::time::Instant::now();
    }

    fn execute_action(&mut self, action: &GuiAction) {
        use crate::{
            cli::{ApproveArgs, BlockTarget, Commands},
            commands::dispatch,
            context::{Context, UnavailableNotifier, UnavailableScheduler},
            time::SystemClock,
        };

        let ctx = Context::new(
            self.store.clone(),
            SystemClock,
            UnavailableScheduler,
            UnavailableNotifier,
            self.config.clone(),
        );
        let refs = ctx.as_refs();
        let mut out = Vec::<u8>::new();

        let result = match action {
            GuiAction::Approve(id_str) => id_str.parse().map(|id| {
                dispatch(
                    Some(Commands::Approve(ApproveArgs { id, date: None })),
                    &mut out,
                    &refs,
                )
            }),
            GuiAction::Done(id_str) => id_str
                .parse()
                .map(|id| dispatch(Some(Commands::Done(BlockTarget { id })), &mut out, &refs)),
            GuiAction::Skip(id_str) => id_str
                .parse()
                .map(|id| dispatch(Some(Commands::Skip(BlockTarget { id })), &mut out, &refs)),
            GuiAction::Dismiss(_) => {
                // Dismiss just refreshes without any engine mutation.
                return;
            }
        };

        match result {
            Ok(Ok(())) => {
                let msg = match action {
                    GuiAction::Approve(_) => "Approved",
                    GuiAction::Done(_) => "Marked done",
                    GuiAction::Skip(_) => "Skipped",
                    GuiAction::Dismiss(_) => unreachable!(),
                };
                self.toasts.push(Toast::success(msg));
            }
            Ok(Err(e)) => {
                self.toasts.push(Toast::error(e.to_string()));
            }
            Err(e) => {
                self.toasts.push(Toast::error(e.to_string()));
            }
        }

        self.refresh_data();
    }

    #[allow(clippy::collapsible_if)]
    fn handle_keyboard(&mut self, ui: &egui::Ui) {
        let input = ui.input(Clone::clone);

        // ⌘K / Ctrl-K: focus command bar.
        if input.modifiers.matches_exact(Modifiers::COMMAND) && input.key_pressed(Key::K) {
            ui.ctx()
                .memory_mut(|mem| mem.request_focus(self.cmd_bar_id));
        }

        // Esc: clear command input or deselect block.
        if input.key_pressed(Key::Escape) {
            if self.command_input.is_empty() {
                self.selected_block_id = None;
            } else {
                self.command_input.clear();
            }
        }

        // j / k: move block selection in Today tab.
        if self.active_tab == NavTab::Today {
            if let Some(ref plan) = self.plan {
                let ids: Vec<String> =
                    plan.blocks.iter().map(|b| b.id.to_string()).collect();
                let current_idx = self
                    .selected_block_id
                    .as_ref()
                    .and_then(|id| ids.iter().position(|i| i == id));

                if !ids.is_empty() && input.key_pressed(Key::J) {
                    let next = current_idx.map_or(0, |i| (i + 1).min(ids.len() - 1));
                    self.selected_block_id = Some(ids[next].clone());
                }
                if !ids.is_empty() && input.key_pressed(Key::K) {
                    let prev = current_idx.map_or(0, |i| i.saturating_sub(1));
                    self.selected_block_id = Some(ids[prev].clone());
                }
            }
        }
    }
}

// ── eframe::App impl ──────────────────────────────────────────────────────────

#[cfg_attr(coverage_nightly, coverage(off))]
impl eframe::App for CcplanApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Auto-refresh every 5 seconds.
        if self.last_refresh.elapsed() > std::time::Duration::from_secs(5) {
            self.refresh_data();
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));

        // Apply theme.
        if self.dark_mode {
            let mut v = egui::Visuals::dark();
            v.panel_fill = BG;
            v.window_fill = BG;
            ui.ctx().set_visuals(v);
        } else {
            ui.ctx().set_visuals(egui::Visuals::light());
        }

        let now = jiff::Timestamp::now();

        // Build models.
        let approvals_count = self
            .plan
            .as_ref()
            .map_or(0, |p| build_approvals_model(p).items.len());
        let nav = build_nav_model(self.active_tab, approvals_count);

        // Handle keyboard.
        self.handle_keyboard(ui);

        // Pending actions collected during rendering.
        let mut pending_actions: Vec<GuiAction> = Vec::new();

        // Left nav (240px).
        egui::Panel::left("nav")
            .exact_size(240.0)
            .show_inside(ui, |ui| {
                render_nav(ui, &nav, &mut self.active_tab, &mut self.dark_mode);
            });

        // Right context pane (320px).
        let selected_id = self.selected_block_id.clone();
        egui::Panel::right("context")
            .exact_size(320.0)
            .show_inside(ui, |ui| {
                render_context_pane(ui, self.plan.as_ref(), selected_id.as_deref(), now);
            });

        // Center column: command bar + main content.
        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::Panel::top("cmd_bar")
                .frame(
                    egui::Frame::new()
                        .fill(SURFACE2)
                        .inner_margin(Margin::symmetric(12, 8)),
                )
                .show_inside(ui, |ui| {
                    render_command_bar(ui, &mut self.command_input, self.cmd_bar_id);
                });

            egui::CentralPanel::default().show_inside(ui, |ui| match self.active_tab {
                NavTab::Today => {
                    let model = self.plan.as_ref().map(|p| build_today_model(p, now));
                    let prev_hovered = self.hovered_block_id.clone();
                    let (new_selected, new_hovered) = render_today(
                        ui,
                        model.as_ref(),
                        self.selected_block_id.as_deref(),
                        prev_hovered.as_deref(),
                        &mut pending_actions,
                    );
                    if let Some(id) = new_selected {
                        self.selected_block_id = Some(id);
                    }
                    self.hovered_block_id = new_hovered;
                }
                NavTab::Upcoming => {
                    let model = build_upcoming_model(&self.upcoming_plans, now);
                    render_upcoming(ui, &model);
                }
                NavTab::Automations => {
                    let model = build_automations_model(&self.recurring_rules);
                    render_automations(ui, &model);
                }
                NavTab::Agents => {
                    let model = build_agents_model(&self.fire_records);
                    render_agents(ui, &model);
                }
                NavTab::Activity => {
                    let model = build_activity_model(&self.fire_records);
                    render_activity(ui, &model);
                }
                NavTab::Approvals => {
                    let model = self
                        .plan
                        .as_ref()
                        .map(build_approvals_model)
                        .unwrap_or_default();
                    render_approvals(ui, &model, &mut pending_actions);
                }
            });
        });

        // Execute collected actions (after all borrows are released).
        let had_actions = !pending_actions.is_empty();
        for action in &pending_actions {
            self.execute_action(action);
        }
        if had_actions {
            self.refresh_data();
        }

        // Toasts at bottom-center.
        render_toasts(ui, &mut self.toasts);
    }
}

// ── Left nav ──────────────────────────────────────────────────────────────────

fn render_nav(
    ui: &mut egui::Ui,
    nav: &super::model::NavModel,
    active_tab: &mut NavTab,
    dark_mode: &mut bool,
) {
    ui.style_mut().spacing.item_spacing = egui::Vec2::new(0.0, 2.0);

    for tab in [
        NavTab::Today,
        NavTab::Upcoming,
        NavTab::Automations,
        NavTab::Agents,
        NavTab::Activity,
        NavTab::Approvals,
    ] {
        let label = nav_label(tab, nav);
        let is_active = nav.active_tab == tab;

        let resp = egui::Frame::new()
            .fill(if is_active {
                SURFACE2
            } else {
                Color32::TRANSPARENT
            })
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                ui.set_min_size(egui::Vec2::new(208.0, 44.0));
                let color = if is_active { TEXT1 } else { TEXT2 };
                ui.colored_label(color, &label);
            });

        if resp.response.interact(egui::Sense::click()).clicked() {
            *active_tab = tab;
        }

        // 3px accent left border when active.
        if is_active {
            let rect = resp.response.rect;
            let bar = egui::Rect::from_min_size(rect.min, egui::Vec2::new(3.0, rect.height()));
            ui.painter().rect_filled(bar, 0.0, ACCENT);
        }
    }

    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(if *dark_mode { "☀" } else { "☾" }).clicked() {
                *dark_mode = !*dark_mode;
            }
        });
    });
}

fn nav_label(tab: NavTab, nav: &super::model::NavModel) -> String {
    match tab {
        NavTab::Today => "Today".to_owned(),
        NavTab::Upcoming => "Upcoming".to_owned(),
        NavTab::Automations => "Automations".to_owned(),
        NavTab::Agents => "Agents".to_owned(),
        NavTab::Activity => "Activity".to_owned(),
        NavTab::Approvals => {
            if nav.pending_approvals_count > 0 {
                format!("Approvals ({})", nav.pending_approvals_count)
            } else {
                "Approvals".to_owned()
            }
        }
    }
}

// ── Command bar ───────────────────────────────────────────────────────────────

fn render_command_bar(ui: &mut egui::Ui, input: &mut String, bar_id: egui::Id) {
    ui.horizontal(|ui| {
        let te = egui::TextEdit::singleline(input)
            .hint_text("What do you want to do?  (/add, /remind …)")
            .desired_width(f32::INFINITY)
            .id(bar_id)
            .frame(egui::Frame::NONE)
            .text_color(TEXT1);
        let resp = ui.add(te);

        ui.colored_label(TEXT_DIM, "⌘K");

        if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            // Send to agent / parse slash command — for now, clear on Enter.
            input.clear();
        }
    });
}

// ── Right context pane ────────────────────────────────────────────────────────

#[allow(clippy::collapsible_if, clippy::collapsible_match)]
fn render_context_pane(
    ui: &mut egui::Ui,
    plan: Option<&crate::model::Plan>,
    selected_id: Option<&str>,
    now: jiff::Timestamp,
) {
    ui.add_space(16.0);

    // If a block is selected, show its detail.
    if let (Some(plan), Some(id)) = (plan, selected_id) {
        if let Some(block) = plan.blocks.iter().find(|b| b.id.as_str() == id) {
            let detail = build_block_detail_model(block, plan, now);
            ui.strong(&detail.title);
            ui.colored_label(TEXT2, &detail.time_range);
            ui.colored_label(TEXT_DIM, &detail.countdown);
            ui.add_space(8.0);

            if let Some(ref rec) = detail.recurrence_label {
                ui.horizontal(|ui| {
                    ui.colored_label(TEXT_DIM, "↻");
                    ui.colored_label(TEXT2, rec);
                });
            }
            if let Some(ref argv) = detail.run_argv {
                ui.horizontal(|ui| {
                    ui.colored_label(TEXT_DIM, "▶");
                    ui.monospace(argv);
                });
                if let Some(ref appr) = detail.approval {
                    ui.colored_label(if appr == "approved" { C_GREEN } else { C_AMBER }, appr);
                }
            }
            if let Some(ref agent) = detail.agent {
                ui.horizontal(|ui| {
                    ui.colored_label(TEXT_DIM, "🤖");
                    ui.colored_label(TEXT2, agent);
                });
            }
            if !detail.after_ids.is_empty() {
                ui.colored_label(TEXT_DIM, format!("after: {}", detail.after_ids.join(", ")));
            }
            if !detail.tags.is_empty() {
                ui.colored_label(TEXT_DIM, detail.tags.join("  "));
            }
            return;
        }
    }

    // Default: "Up next" + agent status compact list.
    ui.colored_label(TEXT2, "Up next");
    ui.add_space(4.0);
    if let Some(plan) = plan {
        let up_next = build_up_next_model(plan, now);
        if up_next.items.is_empty() {
            ui.colored_label(TEXT_DIM, "Nothing coming up.");
        } else {
            for card in &up_next.items {
                ui.horizontal(|ui| {
                    ui.colored_label(TEXT_DIM, &card.time_range);
                    ui.colored_label(TEXT2, &card.title);
                });
            }
        }
    } else {
        ui.colored_label(TEXT_DIM, "No plan loaded.");
    }
}

// ── Today timeline ────────────────────────────────────────────────────────────

/// Returns the newly selected block id and the newly hovered block id.
fn render_today(
    ui: &mut egui::Ui,
    model: Option<&super::model::TodayModel>,
    selected_id: Option<&str>,
    prev_hovered: Option<&str>,
    actions: &mut Vec<GuiAction>,
) -> (Option<String>, Option<String>) {
    let mut new_selected: Option<String> = None;
    let mut new_hovered: Option<String> = None;

    let Some(model) = model else {
        ui.centered_and_justified(|ui| {
            ui.colored_label(
                TEXT_DIM,
                "Nothing scheduled. Type what you want to do above.",
            );
        });
        return (None, None);
    };

    if model.cards.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.colored_label(
                TEXT_DIM,
                "Nothing scheduled. Type what you want to do above.",
            );
        });
        return (None, None);
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(8.0);
        for (idx, card) in model.cards.iter().enumerate() {
            // Draw the "now" line before the first upcoming block.
            if model.now_line_index == Some(idx) {
                render_now_line(ui, &model.now_label);
            }

            let is_selected = selected_id == Some(card.id.as_str());
            let is_hovered = prev_hovered == Some(card.id.as_str());

            let (clicked, hovered, action) =
                render_block_card(ui, card, is_selected, is_hovered, actions);

            if clicked {
                new_selected = Some(card.id.clone());
            }
            if hovered {
                new_hovered = Some(card.id.clone());
            }
            if let Some(a) = action {
                actions.push(a);
            }

            ui.add_space(8.0);
        }
    });

    (new_selected, new_hovered)
}

fn render_now_line(ui: &mut egui::Ui, label: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let available = ui.available_width();
        let (rect, _) =
            ui.allocate_exact_size(egui::Vec2::new(available, 1.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 0.0, ACCENT);
        ui.painter().text(
            egui::Pos2::new(rect.min.x + 4.0, rect.min.y - 8.0),
            egui::Align2::LEFT_BOTTOM,
            label,
            egui::FontId::monospace(11.0),
            ACCENT,
        );
    });
    ui.add_space(4.0);
}

/// Returns whether the card was clicked, whether it is hovered, and an optional kebab action.
fn render_block_card(
    ui: &mut egui::Ui,
    card: &super::model::BlockCardModel,
    is_selected: bool,
    is_hovered: bool,
    _actions: &mut Vec<GuiAction>,
) -> (bool, bool, Option<GuiAction>) {
    let mut clicked = false;
    let mut kebab_action: Option<GuiAction> = None;

    let fill = if is_selected { SURFACE2 } else { SURFACE };
    let stroke_color = if is_selected { ACCENT } else { BORDER };
    let status_color = status_color(card.status);

    let frame_resp = egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0_f32,stroke_color))
        .corner_radius(8.0)
        .inner_margin(Margin::symmetric(12, 12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width() - 4.0);

            ui.horizontal(|ui| {
                // Row 1: time range · title · countdown (right-aligned).
                ui.colored_label(TEXT2, &card.time_range);
                ui.add_space(8.0);

                let title_color = if card.status == crate::model::Status::Active {
                    C_GREEN
                } else {
                    TEXT1
                };
                ui.colored_label(title_color, &card.title);

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Hover kebab: show Done / Skip buttons on hover.
                    if is_hovered {
                        if ui.small_button("Skip").clicked() {
                            kebab_action = Some(GuiAction::Skip(card.id.clone()));
                        }
                        if ui.small_button("Done").clicked() {
                            kebab_action = Some(GuiAction::Done(card.id.clone()));
                        }
                    }
                    ui.monospace(&card.countdown);
                });
            });

            // Row 2: tags + badges (only when non-empty).
            let has_row2 = !card.tags.is_empty()
                || card.has_recurrence
                || card.has_run
                || card.awaiting_approval
                || card.has_agent
                || card.has_expect_by_breach;

            if has_row2 {
                ui.horizontal(|ui| {
                    for tag in &card.tags {
                        ui.colored_label(TEXT_DIM, format!("  {tag}"));
                    }
                    if card.has_recurrence {
                        ui.colored_label(TEXT2, " ↻");
                    }
                    if card.has_run {
                        ui.colored_label(TEXT2, " ▶");
                    }
                    if card.awaiting_approval {
                        ui.colored_label(C_AMBER, " ⏳");
                    }
                    if card.has_agent {
                        ui.colored_label(TEXT2, " 🤖");
                    }
                    if card.has_expect_by_breach {
                        ui.colored_label(C_RED, " !");
                    }
                });
            }
        });

    // 4px status color bar on the left edge of the card.
    let card_rect = frame_resp.response.rect;
    let bar_rect =
        egui::Rect::from_min_size(card_rect.min, egui::Vec2::new(4.0, card_rect.height()));
    ui.painter().rect_filled(bar_rect, 4.0, status_color);

    let interact = frame_resp.response.interact(egui::Sense::click());
    if interact.clicked() {
        clicked = true;
    }
    let hovered = interact.hovered();

    (clicked, hovered, kebab_action)
}

// ── Upcoming ──────────────────────────────────────────────────────────────────

fn render_upcoming(ui: &mut egui::Ui, model: &super::model::UpcomingModel) {
    if model.days.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.colored_label(
                TEXT_DIM,
                "Nothing scheduled for the coming days. Run `ccplan materialize` to expand recurring rules.",
            );
        });
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        for day in &model.days {
            ui.add_space(8.0);
            ui.colored_label(TEXT2, &day.date_label);
            ui.separator();
            if day.cards.is_empty() {
                ui.colored_label(TEXT_DIM, "  Nothing scheduled.");
            } else {
                for card in &day.cards {
                    ui.horizontal(|ui| {
                        ui.colored_label(
                            status_color(card.status),
                            egui::RichText::new("▌").small(),
                        );
                        ui.colored_label(TEXT_DIM, &card.time_range);
                        ui.colored_label(TEXT1, &card.title);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.colored_label(TEXT_DIM, &card.countdown);
                        });
                    });
                }
            }
        }
    });
}

// ── Automations ───────────────────────────────────────────────────────────────

fn render_automations(ui: &mut egui::Ui, model: &super::model::AutomationsModel) {
    if model.rules.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.colored_label(
                TEXT_DIM,
                "No automation rules. Add blocks with --every to create recurring automations.",
            );
        });
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        for rule in &model.rules {
            ui.add_space(8.0);
            egui::Frame::new()
                .fill(SURFACE)
                .stroke(Stroke::new(1.0_f32,BORDER))
                .corner_radius(8.0)
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(TEXT1, &rule.title);
                        ui.colored_label(TEXT_DIM, format!("  #{}", rule.id));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if rule.has_run {
                                ui.colored_label(TEXT2, " ▶");
                            }
                            if rule.has_agent {
                                ui.colored_label(TEXT2, " 🤖");
                            }
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.colored_label(ACCENT, &rule.schedule);
                        if let Some(ref end) = rule.end_label {
                            ui.colored_label(TEXT_DIM, format!("  {end}"));
                        }
                    });
                });
        }
    });
}

// ── Agents ────────────────────────────────────────────────────────────────────

fn render_agents(ui: &mut egui::Ui, model: &super::model::AgentsModel) {
    if model.agents.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.colored_label(
                TEXT_DIM,
                "No agents active. Assign blocks with --agent to coordinate agents.",
            );
        });
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        for agent in &model.agents {
            ui.add_space(8.0);
            egui::Frame::new()
                .fill(SURFACE)
                .stroke(Stroke::new(1.0_f32,BORDER))
                .corner_radius(8.0)
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let status_col = if agent.is_ok { C_GREEN } else { C_RED };
                        ui.colored_label(status_col, "●");
                        ui.colored_label(TEXT1, &agent.name);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.colored_label(TEXT_DIM, &agent.last_action);
                        });
                    });
                });
        }
    });
}

// ── Activity ──────────────────────────────────────────────────────────────────

fn render_activity(ui: &mut egui::Ui, model: &super::model::ActivityModel) {
    if model.items.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.colored_label(TEXT_DIM, "No activity yet.");
        });
        return;
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        for item in &model.items {
            let color = match item.kind {
                ActivityKind::Ok => C_GREEN,
                ActivityKind::Run => ACCENT,
                ActivityKind::Error => C_RED,
                ActivityKind::Info => TEXT_DIM,
            };
            ui.horizontal(|ui| {
                ui.colored_label(color, item.icon);
                ui.colored_label(TEXT_DIM, &item.ts_label);
                ui.colored_label(color, &item.text);
            });
            ui.add_space(2.0);
        }
    });
}

// ── Approvals ─────────────────────────────────────────────────────────────────

fn render_approvals(ui: &mut egui::Ui, model: &ApprovalsModel, actions: &mut Vec<GuiAction>) {
    if model.items.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.colored_label(TEXT_DIM, "All clear. Nothing waiting on you.");
        });
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        for item in &model.items {
            ui.add_space(8.0);
            egui::Frame::new()
                .fill(SURFACE)
                .stroke(Stroke::new(1.0_f32,BORDER))
                .corner_radius(8.0)
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    // Title + when
                    ui.horizontal(|ui| {
                        ui.colored_label(TEXT1, &item.title);
                        ui.colored_label(TEXT_DIM, format!("  {}", item.when));
                    });
                    // argv (monospace)
                    ui.monospace(&item.argv);
                    // Agent reason (if any)
                    if let Some(ref reason) = item.reason {
                        ui.colored_label(TEXT_DIM, reason);
                    }
                    // Approve / Dismiss buttons
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Approve").color(Color32::WHITE),
                                )
                                .fill(ACCENT)
                                .corner_radius(4.0),
                            )
                            .clicked()
                        {
                            actions.push(GuiAction::Approve(item.id.clone()));
                        }
                        ui.add_space(8.0);
                        if ui
                            .add(
                                egui::Button::new(egui::RichText::new("Dismiss").color(TEXT2))
                                    .fill(Color32::TRANSPARENT)
                                    .stroke(Stroke::new(1.0_f32,BORDER))
                                    .corner_radius(4.0),
                            )
                            .clicked()
                        {
                            actions.push(GuiAction::Dismiss(item.id.clone()));
                        }
                    });
                });
        }
    });
}

// ── Toasts ────────────────────────────────────────────────────────────────────

fn render_toasts(ui: &mut egui::Ui, toasts: &mut Vec<Toast>) {
    const TOAST_LIFETIME_SECS: u64 = 4;

    // Expire old toasts.
    toasts.retain(|t| t.created.elapsed().as_secs() < TOAST_LIFETIME_SECS);

    if toasts.is_empty() {
        return;
    }

    // Position toast window at bottom-center.
    let screen_rect = ui.ctx().viewport_rect();
    let x = screen_rect.center().x;
    let y = screen_rect.max.y - 60.0;

    for (i, toast) in toasts.iter().enumerate() {
        let bg = if toast.is_error { C_RED } else { SURFACE2 };
        let border = if toast.is_error { C_RED } else { ACCENT };

        egui::Area::new(egui::Id::new("toast").with(i))
            .fixed_pos(egui::Pos2::new(x - 150.0, y - f32::from(u8::try_from(i.min(20)).unwrap_or(20)) * 44.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(bg)
                    .stroke(Stroke::new(1.0_f32,border))
                    .corner_radius(8.0)
                    .inner_margin(Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.set_min_width(300.0);
                        ui.colored_label(TEXT1, &toast.message);
                    });
            });
    }
}
