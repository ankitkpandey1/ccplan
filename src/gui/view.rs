//! eframe/egui rendering layer. All IO boundary — never add logic here.
#![allow(dead_code)]
#![cfg_attr(coverage_nightly, coverage(off))]

use eframe::egui;

use super::model::{
    ActivityKind, ApprovalsModel, NavTab, TodayModel, build_activity_model, build_approvals_model,
    build_nav_model, build_today_model,
};

#[allow(dead_code)]
pub(crate) struct CcplanApp {
    plan: Option<crate::model::Plan>,
    fire_records: Vec<crate::store::FireRecord>,
    active_tab: NavTab,
}

impl CcplanApp {
    pub(crate) fn new() -> Self {
        Self {
            plan: None,
            fire_records: Vec::new(),
            active_tab: NavTab::Today,
        }
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
impl eframe::App for CcplanApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = jiff::Timestamp::now();

        let approvals_count = self
            .plan
            .as_ref()
            .map_or(0, |p| build_approvals_model(p).items.len());
        let nav = build_nav_model(self.active_tab, approvals_count);

        egui::Panel::left("nav")
            .exact_size(240.0)
            .show_inside(ui, |ui| {
                for tab in [
                    NavTab::Today,
                    NavTab::Upcoming,
                    NavTab::Automations,
                    NavTab::Agents,
                    NavTab::Activity,
                    NavTab::Approvals,
                ] {
                    let label = nav_label(tab, &nav);
                    if ui.selectable_label(nav.active_tab == tab, label).clicked() {
                        self.active_tab = tab;
                    }
                }
            });

        egui::Panel::right("context")
            .exact_size(320.0)
            .show_inside(ui, |_ui| {});

        egui::CentralPanel::default().show_inside(ui, |ui| match self.active_tab {
            NavTab::Today => {
                render_today(ui, self.plan.as_ref().map(|p| build_today_model(p, now)));
            }
            NavTab::Approvals => {
                render_approvals(ui, self.plan.as_ref().map(build_approvals_model));
            }
            NavTab::Activity => {
                let model = build_activity_model(&self.fire_records);
                render_activity(ui, &model);
            }
            _ => {
                ui.label("Coming soon.");
            }
        });
    }
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

fn render_today(ui: &mut egui::Ui, model: Option<TodayModel>) {
    let Some(model) = model else {
        ui.centered_and_justified(|ui| {
            ui.label("Nothing scheduled. Type what you want to do above.");
        });
        return;
    };
    ui.heading(&model.now_label);
    if model.cards.is_empty() {
        ui.label("Nothing scheduled. Type what you want to do above.");
        return;
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        for card in &model.cards {
            egui::Frame::NONE
                .inner_margin(egui::Margin::same(12))
                .stroke(egui::Stroke::new(1.0, egui::Color32::DARK_GRAY))
                .corner_radius(8.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.monospace(&card.time_range);
                        ui.strong(&card.title);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.monospace(&card.countdown);
                        });
                    });
                    if !card.tags.is_empty() {
                        ui.horizontal(|ui| {
                            for tag in &card.tags {
                                ui.small(tag);
                            }
                        });
                    }
                });
        }
    });
}

fn render_approvals(ui: &mut egui::Ui, model: Option<ApprovalsModel>) {
    let Some(model) = model else {
        ui.label("All clear. Nothing waiting on you.");
        return;
    };
    if model.items.is_empty() {
        ui.label("All clear. Nothing waiting on you.");
        return;
    }
    for item in &model.items {
        ui.horizontal(|ui| {
            ui.label(&item.title);
            ui.monospace(&item.argv);
        });
    }
}

fn render_activity(ui: &mut egui::Ui, model: &super::model::ActivityModel) {
    if model.items.is_empty() {
        ui.label("No activity yet.");
        return;
    }
    for item in &model.items {
        let color = match item.kind {
            ActivityKind::Ok => egui::Color32::GREEN,
            ActivityKind::Run => egui::Color32::LIGHT_BLUE,
            ActivityKind::Error => egui::Color32::RED,
            ActivityKind::Info => egui::Color32::GRAY,
        };
        ui.colored_label(color, format!("{} {}", item.icon, item.text));
    }
}
