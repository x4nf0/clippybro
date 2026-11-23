//! Vista previa: el lienzo con la proporción elegida y cada imagen recortada, girada y ubicada como se va a
//! exportar; abajo, la proporción y la pantalla completa. Desde el panel del clip se entra a recortar su imagen
//! acá mismo.

use std::time::{Duration, Instant};

use eframe::egui::text::LayoutJob;
use eframe::egui::{self, Align, Align2, Color32, CursorIcon, FontId, Layout, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2};

use crate::i18n::{tr, trf};
use crate::ui_kit::theme::*;
use super::media::{Frame, Ratio, Sizing};
use super::panels::SEEK_EVERY;
use super::shortcuts::{self, Act};
use super::EditorApp;

/// Proporciones del lienzo que se pueden elegir (None = la del primer clip).
pub(super) const RATIOS: [(Option<Ratio>, &str); 7] = [
    (None, "Original"),
    (Some((16, 9)), "16:9"),
    (Some((9, 16)), "9:16"),
    (Some((1, 1)), "1:1"),
    (Some((4, 5)), "4:5"),
    (Some((4, 3)), "4:3"),
    (Some((21, 9)), "21:9"),
];

/// Alto de la barra de abajo de la vista previa.
const BAR_H: f32 = 34.0;
/// Agarradera de un borde o una esquina del recorte.
const CROP_GRIP: f32 = 14.0;
/// Lo mínimo que puede quedar del recorte (px de pantalla).
const CROP_MIN: f32 = 32.0;
/// En pantalla completa, segundos con el mouse quieto hasta que se esconden la barra y el cursor.
const IDLE_HIDE: f64 = 2.5;
/// Alto de la barra de pantalla completa (con la sombra que la separa del video) y margen de sus controles.
const FS_BAR_H: f32 = 96.0;
const FS_PAD: f32 = 24.0;

/// La vista previa en pantalla completa: solo el video y, abajo, una barra que se esconde con el mouse quieto.
pub(super) struct Fullscreen {
    /// La ventana estaba maximizada: al salir vuelve a estarlo.
    was_maximized: bool,
    /// Cuándo se entró (cada vez la barra arranca visible, sin fundido) y cuándo se movió el mouse.
    since: f64,
    moved: f64,
    /// Arrastrando por la barra de tiempo.
    scrubbing: bool,
}

/// Recortando la imagen de una pieza.
pub(super) struct CropEdit {
    pub(super) id: u64,
    /// El recorte sobre la imagen girada y espejada, de 0 a 1 (izquierda, arriba, derecha, abajo).
    rect: [f32; 4],
    /// Proporción fija elegida (índice en `crop_shapes`), o libre.
    shape: usize,
    /// Mientras se arrastra: qué bordes se mueven (izquierda, arriba, derecha, abajo) y cómo estaba el recorte.
    drag: Option<([bool; 4], [f32; 4])>,
}

/// Proporciones del recorte: libre, la del lienzo y algunas fijas.
fn crop_shapes(canvas: (f32, f32)) -> [(&'static str, Option<f32>); 5] {
    [
        (tr!("Libre", "Free", "Livre"), None),
        (tr!("Lienzo", "Canvas", "Tela"), Some(canvas.0 / canvas.1)),
        ("16:9", Some(16.0 / 9.0)),
        ("9:16", Some(9.0 / 16.0)),
        ("1:1", Some(1.0)),
    ]
}

/// `r` en el rectángulo de pantalla `area` (de 0 a 1 -> px).
fn to_screen(area: Rect, r: [f32; 4]) -> Rect {
    Rect::from_min_max(area.lerp_inside(Vec2::new(r[0], r[1])), area.lerp_inside(Vec2::new(r[2], r[3])))
}

/// La textura en `dst` (pantalla) con el punto del archivo que va en cada esquina (`uv`, como en `Placed`).
fn image_mesh(tex: egui::TextureId, dst: Rect, uv: [[f32; 2]; 4], alpha: f32) -> Shape {
    let mut m = egui::Mesh::with_texture(tex);
    let col = Color32::WHITE.gamma_multiply(alpha);
    for (pos, uv) in [dst.left_top(), dst.right_top(), dst.right_bottom(), dst.left_bottom()].into_iter().zip(uv) {
        m.vertices.push(egui::epaint::Vertex { pos, uv: Pos2::new(uv[0], uv[1]), color: col });
    }
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    Shape::mesh(m)
}

/// El recorte más grande con forma `aspect` (ancho / alto, en px) dentro de una imagen de `size` px, centrado.
fn biggest(aspect: f32, size: (f32, f32)) -> [f32; 4] {
    let (w, h) = if size.0 / size.1 > aspect { (aspect * size.1 / size.0, 1.0) } else { (1.0, size.0 / aspect / size.1) };
    [(1.0 - w) / 2.0, (1.0 - h) / 2.0, (1.0 + w) / 2.0, (1.0 + h) / 2.0]
}

impl EditorApp {
    pub(super) fn preview(&mut self, ui: &mut egui::Ui) {
        let full = ui.available_rect_before_wrap();
        let area = Rect::from_min_max(full.min, Pos2::new(full.right(), full.bottom() - BAR_H - 8.0));
        let bar = Rect::from_min_max(Pos2::new(full.left(), area.bottom() + 8.0), full.max);
        if self.crop.as_ref().is_some_and(|c| self.find(c.id).is_none()) {
            self.crop = None;
        }
        if self.crop.is_some() {
            self.crop_view(ui, area, bar);
        } else {
            self.canvas_view(ui, area);
            self.ratio_bar(ui, bar);
        }
    }

    fn canvas_view(&mut self, ui: &mut egui::Ui, area: Rect) {
        let resp = ui.interact(area, ui.id().with("canvas"), Sense::click());
        let p = ui.painter_at(area);
        let (cw, ch) = self.canvas_shape();
        let scale = (area.width() / cw).min(area.height() / ch);
        let canvas = Rect::from_center_size(area.center(), Vec2::new(cw, ch) * scale);
        p.rect_filled(canvas, 6.0, Color32::BLACK);
        if self.is_empty() {
            p.text(area.center() - egui::vec2(0.0, 12.0), Align2::CENTER_CENTER, tr!("Tu video aparece acá", "Your video shows up here", "Seu vídeo aparece aqui"), FontId::proportional(20.0), TEXT);
            p.text(area.center() + egui::vec2(0.0, 16.0), Align2::CENTER_CENTER, tr!("Elegí un clip de la biblioteca para empezar", "Pick a clip from the library to start", "Escolha um clipe da biblioteca para começar"), FontId::proportional(14.0), MUTED);
            return;
        }
        if let Some(e) = self.preview_error() {
            let mut job = LayoutJob::simple(e, FontId::proportional(14.0), MUTED, (canvas.width() - 48.0).clamp(120.0, 480.0));
            job.halign = Align::Center;
            let g = p.layout_job(job);
            p.galley(Pos2::new(canvas.center().x, canvas.center().y - g.size().y / 2.0), g, MUTED);
        } else {
            // Cada imagen como se va a exportar; donde dos piezas se funden, la de arriba encima con su transparencia.
            let clip = p.with_clip_rect(canvas.intersect(p.clip_rect()));
            for l in &self.layers {
                let Some(frame) = self.find(l.piece()).map(|(ti, i)| self.tracks[ti].clips[i].frame) else { continue };
                let s = l.tex.size_vec2();
                if let Some(pl) = frame.place((s.x, s.y), (cw, ch)) {
                    let dst = Rect::from_min_max(canvas.min + Vec2::new(pl.dst[0], pl.dst[1]) * scale, canvas.min + Vec2::new(pl.dst[2], pl.dst[3]) * scale);
                    clip.add(image_mesh(l.tex.id(), dst, pl.uv, l.alpha));
                }
            }
        }
        if resp.clicked() {
            self.toggle_play();
        }
    }

    /// Barra de abajo: la proporción del lienzo y la pantalla completa.
    fn ratio_bar(&mut self, ui: &mut egui::Ui, bar: Rect) {
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(bar).layout(Layout::right_to_left(Align::Center)));
        let tip = format!("{} ({})", Act::Fullscreen.label(), shortcuts::short(&self.prefs.keys, Act::Fullscreen));
        ui.add_enabled_ui(!self.is_empty(), |ui| {
            if icon_only_button(ui, Glyph::Fullscreen, Kind::Secondary, &tip).clicked() {
                self.toggle_fullscreen();
            }
        });
        let current = RATIOS.iter().find(|r| r.0 == self.ratio).map_or("Original", |r| r.1);
        let r = button(&mut ui, current, Kind::Secondary).on_hover_text(tr!("Forma del video: horizontal, vertical, cuadrado…", "Video shape: landscape, portrait, square…", "Formato do vídeo: horizontal, vertical, quadrado…"));
        let menu = ui.id().with("ratio_menu");
        if r.clicked() {
            ui.memory_mut(|m| m.toggle_popup(menu));
        }
        let mut pick = None;
        let first = self.edit().first_video().and_then(|c| self.loader.probe(&c.path)).map(|p| (p.width, p.height));
        egui::popup_above_or_below_widget(&ui, menu, &r, egui::AboveOrBelow::Above, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(170.0);
            for (ratio, label) in RATIOS {
                let tip = match (ratio, first) {
                    (None, Some((w, h))) => trf!("Como el primer clip ({w}×{h})", "Like the first clip ({w}×{h})", "Como o primeiro clipe ({w}×{h})"),
                    (None, None) => tr!("Como el primer clip", "Like the first clip", "Como o primeiro clipe").to_string(),
                    (Some((w, h)), _) => trf!("{w} de ancho por {h} de alto", "{w} wide by {h} tall", "{w} de largura por {h} de altura"),
                };
                if ui.add(egui::Button::new(label).selected(ratio == self.ratio)).on_hover_text(tip).clicked() {
                    pick = Some(ratio);
                }
            }
        });
        if let Some(r) = pick {
            self.set_ratio(r);
        }
        ui.label(RichText::new(tr!("Proporción", "Aspect ratio", "Proporção")).size(13.0).color(MUTED));
    }

    /// Entra o sale de la pantalla completa (sin clips no hay nada que ver).
    pub(super) fn toggle_fullscreen(&mut self) {
        if self.fullscreen.is_some() {
            self.exit_fullscreen();
        } else if !self.is_empty() {
            let (now, max) = self.ctx.input(|i| (i.time, i.viewport().maximized.unwrap_or(false)));
            self.ctx.memory_mut(|m| m.close_popup());
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            self.fullscreen = Some(Fullscreen { was_maximized: max, since: now, moved: now, scrubbing: false });
        }
    }

    /// Vuelve a la ventana como estaba.
    pub(super) fn exit_fullscreen(&mut self) {
        let Some(fs) = self.fullscreen.take() else { return };
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        if fs.was_maximized {
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        }
        if fs.scrubbing {
            if let Some(t) = self.scrub_t.take() {
                self.seek(t);
            }
        }
    }

    /// Pantalla completa: el lienzo con negro alrededor y la barra de abajo mientras se mueve el mouse.
    pub(super) fn fullscreen_view(&mut self, ui: &mut egui::Ui) {
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, Color32::BLACK);
        self.canvas_view(ui, full);
        let bar = Rect::from_min_max(Pos2::new(full.left(), full.bottom() - FS_BAR_H), full.max);
        let (now, moved, hover) = ui.input(|i| (i.time, i.pointer.delta() != Vec2::ZERO || i.pointer.any_pressed(), i.pointer.hover_pos()));
        let Some(fs) = self.fullscreen.as_mut() else { return };
        if moved {
            fs.moved = now;
        }
        // Con el mouse encima de la barra (o arrastrando la de tiempo) no se esconde.
        let held = fs.scrubbing || hover.is_some_and(|h| bar.contains(h));
        let idle = now - fs.moved;
        let show = held || idle < IDLE_HIDE;
        if show && !held {
            // Un solo cuadro más, cuando toca esconderla.
            ui.ctx().request_repaint_after(Duration::from_secs_f64(IDLE_HIDE - idle));
        }
        let a = if reduced_motion() {
            f32::from(u8::from(show))
        } else {
            ui.ctx().animate_bool_with_time_and_easing(egui::Id::new("fs_bar").with(fs.since.to_bits()), show, T_FADE, ease_out)
        };
        if !show {
            ui.ctx().set_cursor_icon(CursorIcon::None);
        }
        if a > 0.0 {
            self.fullscreen_bar(ui, bar, a);
        }
    }

    /// Barra de pantalla completa (con opacidad `a`): reproducir, el tiempo, la barra para moverse y salir.
    fn fullscreen_bar(&mut self, ui: &mut egui::Ui, bar: Rect, a: f32) {
        // Una sombra de abajo hacia arriba para que se lea sobre cualquier imagen.
        let mut m = egui::Mesh::default();
        let (top, bottom) = (Color32::TRANSPARENT, BADGE.gamma_multiply(a));
        for (pos, col) in [(bar.left_top(), top), (bar.right_top(), top), (bar.right_bottom(), bottom), (bar.left_bottom(), bottom)] {
            m.colored_vertex(pos, col);
        }
        m.add_triangle(0, 1, 2);
        m.add_triangle(0, 2, 3);
        ui.painter().add(Shape::mesh(m));
        // Un clic en la barra (fuera de los controles) no pausa el video.
        ui.interact(bar, ui.id().with("fs_bar"), Sense::click());

        let row = Rect::from_min_max(Pos2::new(bar.left() + FS_PAD, bar.bottom() - FS_PAD - 40.0), Pos2::new(bar.right() - FS_PAD, bar.bottom() - FS_PAD));
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(Layout::left_to_right(Align::Center)));
        ui.multiply_opacity(a);
        ui.spacing_mut().item_spacing.x = 16.0;
        let tip = format!("{} ({})", Act::PlayPause.label(), shortcuts::short(&self.prefs.keys, Act::PlayPause));
        if play_button(&mut ui, self.playing, 40.0).on_hover_text(tip).clicked() {
            self.toggle_play();
        }
        // El ancho del tiempo más largo, así la barra no se mueve mientras cambian los números.
        let total = self.total();
        let font = FontId::proportional(15.0);
        let widest = ui.painter().layout_no_wrap(format!("{0}  /  {0}", fmt_time(total)), font, TEXT).size().x;
        let time = format!("{}  /  {}", fmt_time(self.pos()), fmt_time(total));
        ui.allocate_ui_with_layout(egui::vec2(widest, 40.0), Layout::left_to_right(Align::Center), |ui| {
            ui.label(RichText::new(time).size(15.0).color(TEXT));
        });
        let mut exit = false;
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            exit = icon_only_button(ui, Glyph::ExitFullscreen, Kind::Ghost, tr!("Salir de pantalla completa (Esc)", "Exit fullscreen (Esc)", "Sair da tela cheia (Esc)")).clicked();
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), Sense::click_and_drag());
            self.seek_bar(ui, rect, &resp);
        });
        if exit {
            self.exit_fullscreen();
        }
    }

    /// Barra para moverse por el video: el cursor sigue al mouse en cada cuadro y el video, como mucho cada
    /// `SEEK_EVERY` (como al arrastrar el cursor en la línea de tiempo).
    fn seek_bar(&mut self, ui: &egui::Ui, rect: Rect, resp: &egui::Response) {
        let total = self.total().max(1);
        let t_at = |x: f32| (((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * total as f64) as i64;
        let x_at = |t: i64| rect.left() + rect.width() * (t as f32 / total as f32).clamp(0.0, 1.0);
        let down = resp.is_pointer_button_down_on();
        if let (true, Some(h)) = (down, resp.interact_pointer_pos()) {
            let t = t_at(h.x);
            if self.scrub_t != Some(t) {
                self.scrub_t = Some(t);
                ui.ctx().request_repaint(); // el tiempo de la barra se dibujó antes que esto
            }
            if t != self.clock.pos() {
                if ui.input(|i| i.pointer.any_pressed()) || self.last_seek.elapsed() > SEEK_EVERY {
                    self.seek(t);
                    self.last_seek = Instant::now();
                } else {
                    ui.ctx().request_repaint_after(SEEK_EVERY);
                }
            }
        } else if self.fullscreen.as_ref().is_some_and(|f| f.scrubbing) {
            if let Some(t) = self.scrub_t.take() {
                if t != self.clock.pos() {
                    self.seek(t);
                }
            }
        }
        if let Some(fs) = self.fullscreen.as_mut() {
            fs.scrubbing = down;
        }

        let hot = down || resp.hovered();
        let h = if hot { 6.0 } else { 4.0 };
        let track = Rect::from_center_size(rect.center(), egui::vec2(rect.width(), h));
        let x = x_at(self.pos());
        let p = ui.painter();
        p.rect_filled(track, h / 2.0, WHITE.gamma_multiply(0.3));
        p.rect_filled(Rect::from_min_max(track.min, Pos2::new(x, track.bottom())), h / 2.0, WHITE);
        for &m in &self.markers {
            p.rect_filled(Rect::from_center_size(Pos2::new(x_at(m), track.center().y), egui::vec2(2.0, h + 6.0)), 1.0, MUTED);
        }
        if hot {
            p.circle_filled(Pos2::new(x, track.center().y), 7.0, WHITE);
        }
        // El tiempo bajo el mouse, arriba de la barra.
        if let Some(hp) = resp.hover_pos().filter(|_| !down) {
            let g = p.layout_no_wrap(fmt_time(t_at(hp.x)), FontId::proportional(13.0), TEXT);
            let r = Rect::from_center_size(Pos2::new(hp.x, rect.top() - 6.0 - g.size().y / 2.0), g.size() + egui::vec2(12.0, 6.0));
            let r = r.translate(egui::vec2((rect.left() - r.left()).max(0.0) + (rect.right() - r.right()).min(0.0), 0.0));
            p.rect_filled(r, 4.0, BADGE);
            p.galley(r.center() - g.size() / 2.0, g, TEXT);
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
    }

    /// Empieza a recortar la imagen de lo elegido, con el cursor sobre esa pieza.
    pub(super) fn start_crop(&mut self) {
        let Some((ti, i)) = self.selected_video() else { return };
        let p = &self.tracks[ti].clips[i];
        let [l, t, r, b] = p.frame.crop;
        let (a, z) = (p.frame.view(l, t), p.frame.view(r, b));
        let rect = [a[0].min(z[0]), a[1].min(z[1]), a[0].max(z[0]), a[1].max(z[1])];
        let (id, start, end) = (p.id, p.start, p.end());
        if !(start..end).contains(&self.pos()) {
            self.seek(start);
        }
        self.pause();
        self.crop = Some(CropEdit { id, rect, shape: 0, drag: None });
    }

    fn crop_view(&mut self, ui: &mut egui::Ui, area: Rect, bar: Rect) {
        let Some(mut c) = self.crop.take() else { return };
        let Some((ti, i)) = self.find(c.id) else { return };
        let frame = self.tracks[ti].clips[i].frame;
        // La imagen entera (sin recortar), girada y espejada como se va a ver.
        let whole = Frame { crop: [0.0, 0.0, 1.0, 1.0], sizing: Sizing::Whole, ..frame };
        let p = ui.painter_at(area);
        let layer = self.layers.iter().find(|l| l.piece() == c.id);
        let Some(l) = layer else {
            p.text(area.center(), Align2::CENTER_CENTER, tr!("Llevá el cursor a un lugar donde se vea este clip", "Move the playhead to where this clip is visible", "Leve o cursor a um ponto onde este clipe apareça"), FontId::proportional(14.0), MUTED);
            self.crop_bar(ui, bar, c, (16.0, 9.0));
            return;
        };
        let s = l.tex.size_vec2();
        let (vw, vh) = whole.view_size((s.x, s.y));
        let k = (area.width() / vw).min(area.height() / vh);
        let img = Rect::from_center_size(area.center(), Vec2::new(vw, vh) * k);
        p.add(image_mesh(l.tex.id(), img, [whole.source(0.0, 0.0), whole.source(1.0, 0.0), whole.source(1.0, 1.0), whole.source(0.0, 1.0)], 1.0));

        // Arrastrar un borde, una esquina o todo el recuadro.
        let resp = ui.interact(img.expand(CROP_GRIP), ui.id().with("crop"), Sense::drag());
        let shown = to_screen(img, c.rect);
        let edges_at = |h: Pos2| -> Option<[bool; 4]> {
            let near = |a: f32, b: f32| (a - b).abs() <= CROP_GRIP;
            let inside_y = (shown.top() - CROP_GRIP..=shown.bottom() + CROP_GRIP).contains(&h.y);
            let inside_x = (shown.left() - CROP_GRIP..=shown.right() + CROP_GRIP).contains(&h.x);
            let e = [near(h.x, shown.left()) && inside_y, near(h.y, shown.top()) && inside_x, near(h.x, shown.right()) && inside_y, near(h.y, shown.bottom()) && inside_x];
            if e.iter().any(|&x| x) {
                Some(e)
            } else if shown.contains(h) {
                Some([true; 4])
            } else {
                None
            }
        };
        if resp.drag_started() {
            c.drag = resp.interact_pointer_pos().and_then(edges_at).map(|e| (e, c.rect));
        }
        if let (Some((e, from)), true) = (c.drag, resp.dragged()) {
            let d = resp.interact_pointer_pos().unwrap_or_default() - ui.input(|i| i.pointer.press_origin()).unwrap_or_default();
            let (dx, dy) = (d.x / img.width(), d.y / img.height());
            let aspect = crop_shapes(self.canvas_shape())[c.shape].1;
            c.rect = drag_crop(from, e, (dx, dy), (CROP_MIN / img.width(), CROP_MIN / img.height()), aspect.map(|a| a * vh / vw));
        }
        if resp.drag_stopped() {
            c.drag = None;
        }
        let hover = resp.hover_pos().or(resp.interact_pointer_pos()).and_then(|h| c.drag.map(|d| d.0).or_else(|| edges_at(h)));
        if let Some(e) = hover {
            ui.ctx().set_cursor_icon(match e {
                [true, true, true, true] => CursorIcon::Move,
                [true, true, false, false] | [false, false, true, true] => CursorIcon::ResizeNwSe,
                [true, false, false, true] | [false, true, true, false] => CursorIcon::ResizeNeSw,
                [true, false, false, false] | [false, false, true, false] => CursorIcon::ResizeHorizontal,
                _ => CursorIcon::ResizeVertical,
            });
        }

        // Lo que queda afuera, oscuro; el recuadro con sus tercios y sus esquinas.
        let r = to_screen(img, c.rect);
        for out in [
            Rect::from_min_max(img.min, Pos2::new(img.right(), r.top())),
            Rect::from_min_max(Pos2::new(img.left(), r.bottom()), img.max),
            Rect::from_min_max(Pos2::new(img.left(), r.top()), Pos2::new(r.left(), r.bottom())),
            Rect::from_min_max(Pos2::new(r.right(), r.top()), Pos2::new(img.right(), r.bottom())),
        ] {
            p.rect_filled(out, 0.0, SCRIM);
        }
        let thin = Stroke::new(1.0_f32, WHITE.gamma_multiply(0.35));
        for k in [1.0, 2.0] {
            p.vline(r.left() + r.width() * k / 3.0, r.y_range(), thin);
            p.hline(r.x_range(), r.top() + r.height() * k / 3.0, thin);
        }
        p.rect_stroke(r, 0.0, Stroke::new(1.5_f32, WHITE));
        let arm = 14.0_f32.min(r.width() / 3.0).min(r.height() / 3.0);
        for (corner, dx, dy) in [(r.left_top(), 1.0, 1.0), (r.right_top(), -1.0, 1.0), (r.right_bottom(), -1.0, -1.0), (r.left_bottom(), 1.0, -1.0)] {
            p.add(Shape::line(vec![corner + egui::vec2(arm * dx, 0.0), corner, corner + egui::vec2(0.0, arm * dy)], Stroke::new(3.5_f32, WHITE)));
        }
        self.crop_bar(ui, bar, c, (vw, vh));
    }

    /// Barra de abajo mientras se recorta: la forma del recorte y terminar. Enter aplica y Esc cancela.
    fn crop_bar(&mut self, ui: &mut egui::Ui, bar: Rect, mut c: CropEdit, size: (f32, f32)) {
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(bar).layout(Layout::left_to_right(Align::Center)));
        ui.spacing_mut().item_spacing.x = 4.0;
        let shapes = crop_shapes(self.canvas_shape());
        for (k, (label, aspect)) in shapes.iter().enumerate() {
            let kind = if c.shape == k { Kind::Selected } else { Kind::Secondary };
            if button(&mut ui, label, kind).clicked() && c.shape != k {
                c.shape = k;
                if let Some(a) = aspect {
                    c.rect = biggest(*a, size);
                }
            }
        }
        let (mut done, mut cancel) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            done |= button(ui, tr!("Listo", "Done", "Pronto"), Kind::Secondary).on_hover_text("Enter").clicked();
            cancel |= button(ui, tr!("Cancelar", "Cancel", "Cancelar"), Kind::Ghost).on_hover_text("Esc").clicked();
            if button(ui, tr!("Ver todo", "Show all", "Ver tudo"), Kind::Ghost)
                .on_hover_text(tr!("Volver a la imagen entera", "Back to the whole picture", "Voltar à imagem inteira"))
                .clicked()
            {
                c.rect = [0.0, 0.0, 1.0, 1.0];
                c.shape = 0;
            }
        });
        if done {
            let id = c.id;
            if let Some((ti, i)) = self.find(id) {
                let f = self.tracks[ti].clips[i].frame;
                let whole = Frame { crop: [0.0, 0.0, 1.0, 1.0], ..f };
                let (a, z) = (whole.source(c.rect[0], c.rect[1]), whole.source(c.rect[2], c.rect[3]));
                let crop = [a[0].min(z[0]), a[1].min(z[1]), a[0].max(z[0]), a[1].max(z[1])];
                self.selected = Some(id);
                self.edit_frame(|f| f.crop = crop);
            }
        } else if !cancel {
            self.crop = Some(c);
        }
    }
}

/// El recorte `from` con los bordes `e` (izquierda, arriba, derecha, abajo) corridos (`dx`, `dy`), sin salirse de la
/// imagen ni quedar más chico que `min`. Con `aspect` (ancho / alto del recorte en fracciones de la imagen) la forma
/// se mantiene.
fn drag_crop(from: [f32; 4], e: [bool; 4], (dx, dy): (f32, f32), min: (f32, f32), aspect: Option<f32>) -> [f32; 4] {
    let [l, t, r, b] = from;
    if e == [true; 4] {
        // Todo el recuadro, sin salirse.
        let dx = dx.clamp(-l, 1.0 - r);
        let dy = dy.clamp(-t, 1.0 - b);
        return [l + dx, t + dy, r + dx, b + dy];
    }
    let mut n = from;
    if e[0] {
        n[0] = (l + dx).clamp(0.0, r - min.0);
    }
    if e[2] {
        n[2] = (r + dx).clamp(l + min.0, 1.0);
    }
    if e[1] {
        n[1] = (t + dy).clamp(0.0, b - min.1);
    }
    if e[3] {
        n[3] = (b + dy).clamp(t + min.1, 1.0);
    }
    let Some(a) = aspect else { return n };
    // Con la forma fija manda el ancho (o el alto, si solo se mueve un borde de arriba o de abajo); el otro lado
    // crece desde el borde quieto (o desde el centro) y, si no entra, se achican los dos.
    let horizontal = e[0] || e[2];
    let (mut w, mut h) = (n[2] - n[0], n[3] - n[1]);
    if horizontal {
        h = w / a;
    } else {
        w = h * a;
    }
    let k = (1.0f32).min(1.0 / w).min(1.0 / h);
    let (w, h) = (w * k, h * k);
    let x0 = match (e[0], e[2]) {
        (true, false) => n[2] - w,
        (false, true) => n[0],
        _ => (n[0] + n[2] - w) / 2.0,
    };
    let y0 = match (e[1], e[3]) {
        (true, false) => n[3] - h,
        (false, true) => n[1],
        _ => (n[1] + n[3] - h) / 2.0,
    };
    let x0 = x0.clamp(0.0, 1.0 - w);
    let y0 = y0.clamp(0.0, 1.0 - h);
    [x0, y0, x0 + w, y0 + h]
}
