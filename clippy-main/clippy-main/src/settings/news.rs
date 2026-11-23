//! Novedades: el texto del release de GitHub, en un modal. Aparece una vez después de actualizar
//! ("Novedades de la …") y al tocar "Actualizar", como confirmación de lo que se va a instalar.

use eframe::egui::{self, Align, Layout, RichText};

use super::SettingsApp;
use crate::app::update;
use crate::i18n::{tr, trf};
use crate::ui_kit::theme::*;

const WIDTH: f32 = 520.0;
/// Alto máximo del texto: si el release es largo, se desplaza dentro del modal.
const MAX_TEXT_H: f32 = 360.0;
const BUTTON: egui::Vec2 = egui::vec2(120.0, 38.0);

impl SettingsApp {
    pub(super) fn news_modals(&mut self, ctx: &egui::Context) {
        if let Some((v, text)) = self.news.clone() {
            let mut done = false;
            let title = trf!("Novedades de la {v}", "What's new in {v}", "Novidades da {v}");
            let sub = tr!("Clippy se actualizó. Esto es lo que cambió.", "Clippy was updated. Here's what changed.", "O Clippy foi atualizado. Veja o que mudou.");
            let x = notes_modal(ctx, "news", &title, sub, Some(&text), |ui| {
                done = button_sized(ui, tr!("Listo", "Done", "Pronto"), Kind::Primary, BUTTON).clicked();
            });
            if x || done {
                self.news = None;
                update::news_seen();
            }
        } else if let Some((v, text)) = self.confirm_update.clone() {
            let (mut install, mut later) = (false, false);
            let cur = update::current();
            let sub = trf!(
                "Tenés la {cur}. Clippy se cierra y vuelve a abrir solo.",
                "You have {cur}. Clippy closes and reopens by itself.",
                "Você tem a {cur}. O Clippy fecha e abre de novo sozinho."
            );
            let title = trf!("Actualizar a la {v}", "Update to {v}", "Atualizar para a {v}");
            let x = notes_modal(ctx, "confirm_update", &title, &sub, text.as_deref(), |ui| {
                install = button_sized(ui, tr!("Actualizar", "Update", "Atualizar"), Kind::Primary, BUTTON).clicked();
                later = button_sized(ui, tr!("Ahora no", "Not now", "Agora não"), Kind::Secondary, BUTTON).clicked();
            });
            if install {
                self.start_update();
            }
            if x || install || later {
                self.confirm_update = None;
            }
        }
    }
}

/// Modal con el texto de un release (sin texto, solo el encabezado y los botones). `buttons` se agregan
/// de derecha a izquierda. Devuelve true si se tocó la X o Esc.
fn notes_modal(ctx: &egui::Context, id: &str, title: &str, subtitle: &str, text: Option<&str>, buttons: impl FnOnce(&mut egui::Ui)) -> bool {
    modal(ctx, id, title, subtitle, WIDTH, |ui| {
        if let Some(text) = text {
            modal_body(ui, |ui| {
                egui::ScrollArea::vertical().max_height(MAX_TEXT_H).auto_shrink([false, true]).show(ui, |ui| release_notes(ui, text));
            });
        }
        modal_footer(ui, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), buttons);
        });
    })
}

/// Markdown del release con lo justo: títulos (`#`), viñetas (`-`, `*`) y párrafos, sin los adornos
/// (`**`, `` ` ``). La línea "Full Changelog" que agrega GitHub no le sirve a quien usa la app.
fn release_notes(ui: &mut egui::Ui, text: &str) {
    ui.spacing_mut().item_spacing.y = 6.0;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with("**Full Changelog**") {
            continue;
        }
        if let Some(h) = line.strip_prefix('#') {
            ui.add_space(4.0);
            ui.label(RichText::new(plain(h.trim_start_matches('#').trim())).family(bold()).size(15.0).color(TEXT));
        } else if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            ui.horizontal_top(|ui| {
                ui.label(RichText::new("•").color(MUTED));
                ui.add(egui::Label::new(RichText::new(plain(item)).color(TEXT)).wrap());
            });
        } else {
            ui.add(egui::Label::new(RichText::new(plain(line)).color(MUTED)).wrap());
        }
    }
}

fn plain(s: &str) -> String {
    s.replace("**", "").replace('`', "")
}
