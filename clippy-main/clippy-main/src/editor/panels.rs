//! Barra de arriba y línea de tiempo: herramientas, pistas, marcadores y el clic derecho sobre un clip.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Align2, Color32, CursorIcon, FontId, Layout, Pos2, Rangef, Rect, RichText, Sense, Shape, Stroke, Vec2};

use crate::i18n::{tr, trf};
use crate::ui_kit::theme::*;
use crate::ui_kit::window::open_folder;
use super::shortcuts::Act;
use super::shortcuts;
use super::library;
use super::inspector::{fmt_speed, SPEEDS};
use super::media::Curve;
use super::timeline::{Faded, Piece, TrackKind, Trimmed, MAX_TRACKS};
use super::{EditorApp, HelpTab};

const PAD: f32 = 16.0;

/// Columna de la izquierda, con el nombre y los botones de cada pista.
const GUTTER: f32 = 108.0;

const RULER_H: f32 = 26.0;

const LANE_GAP: f32 = 6.0;
/// Alto mínimo de una pista al arrastrar el borde entre las de video y las de audio.
const LANE_MIN: f32 = 32.0;

/// Lugar debajo de las pistas para la barra de desplazamiento.
const SCROLLBAR_H: f32 = 12.0;

/// Agarradera de un borde de clip, hacia adentro (en un clip corto, un tercio de su ancho).
const GRIP: f32 = 8.0;
/// Margen extra hacia afuera, donde el borde no toca otro clip.
const GRIP_OUT: f32 = 7.0;
/// Esquina de arriba de un clip para los fundidos: alto de la franja y ancho de la agarradera.
const FADE_GRIP: f32 = 12.0;
/// Con el imán, un borde o una esquina que se arrastra se engancha a otro a menos de esto.
const SNAP: f32 = 8.0;
/// Arrastrando a menos de esto de un costado de la vista, la línea se desplaza sola (hasta AUTO_SCROLL px/s).
const EDGE: f32 = 48.0;
const AUTO_SCROLL: f32 = 900.0;
/// Zoom de la línea de tiempo: 1 = entra entera.
const ZOOM_MAX: f32 = 40.0;
/// Mientras se arrastra, el video busca el cuadro como mucho cada tanto (buscar es caro).
pub(super) const SEEK_EVERY: Duration = Duration::from_millis(40);

#[derive(Clone)]
pub(super) enum Drag {
    None,
    Scrub,
    /// Un borde (`left`: el izquierdo) de una pieza y su pareja. `press_x` se mide desde el inicio de la línea (no
    /// de la pantalla) y `pps` queda fijo mientras dura: así el borde sigue al mouse aunque la línea se desplace o
    /// cambie de largo.
    /// `from`: dónde estaba el borde; `snaps`: dónde se puede enganchar (vacío sin el imán).
    Trim { parts: Vec<Trimmed>, left: bool, press_x: f32, pps: f32, from: i64, snaps: Vec<i64> },
    /// Una esquina de arriba (`out`: la derecha) de una pieza y su pareja: el fundido de ese lado.
    Fade { parts: Vec<Faded>, out: bool, press_x: f32, pps: f32, from: i64, snaps: Vec<i64> },
    Move { id: u64, grab_dx: f32 },
    /// Un marcador (lugar en la lista) y cómo estaban todos al empezar.
    Marker { k: usize, before: Vec<i64> },
}

/// Lo que se tocó con el clic derecho en la línea de tiempo (queda mientras el menú está abierto).
#[derive(Clone, Default)]
pub(super) enum Menu {
    #[default]
    None,
    /// Un clip y, si se tocó sobre un fundido, cuáles (pieza y si es el de salida).
    Clip { fades: Vec<(u64, bool)> },
    /// Un marcador.
    Marker(usize),
    /// La regla, en ese momento.
    Ruler(i64),
}

/// Medio ancho de un marcador en la regla (también lo que se puede errar al tocarlo).
const MARKER_W: f32 = 6.0;

/// Una pieza en la línea de tiempo: de `a` a `b` en x, dentro de la franja `row` de su pista. En una pista de
/// video se ven las miniaturas; en una de audio, la forma de onda. Devuelve las miniaturas de los cuadros
/// visibles que todavía no llegaron (esos cuadros quedan vacíos hasta que llegan), para pedirlas primero.
#[allow(clippy::too_many_arguments)]
fn draw_piece(painter: &egui::Painter, p: &Piece, info: Option<&library::Info>, (a, b): (f32, f32), row: Rect, video: bool, pps: f32, selected: bool, alpha: f32, xfade: (i64, i64)) -> Vec<usize> {
    let r = Rect::from_min_max(Pos2::new(a + 1.0, row.top()), Pos2::new(b - 1.0, row.bottom()));
    painter.rect_filled(r, 6.0, (if video { TRACK_VIDEO } else { TRACK_AUDIO }).gamma_multiply(alpha));
    let mut missing = Vec::new();
    if video {
        // Miniaturas: cada cuadro muestra siempre la misma (`thumb_for`), o nada hasta que llega.
        let clip = painter.with_clip_rect(r.shrink(1.0).intersect(painter.clip_rect()));
        let thumbs = info.map_or(&[][..], |i| &i.thumbs);
        let tile_w = (r.height() * 16.0 / 9.0).floor().max(8.0);
        let mut x = r.left();
        while x < r.right() {
            let tile = Rect::from_min_size(Pos2::new(x, r.top()), egui::vec2(tile_w, r.height()));
            x += tile_w;
            if !tile.intersects(clip.clip_rect()) {
                continue;
            }
            let at = |x: f32| p.src_at(p.start + ((x - r.left()) / pps * 1e7) as i64);
            let Some(i) = library::thumb_for(thumbs, at(tile.left()), at(tile.right())) else { continue };
            match &thumbs[i].tex {
                Some(tex) => {
                    clip.image(tex.id(), tile, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE.gamma_multiply(alpha));
                }
                None => missing.push(i),
            }
        }
    } else {
        draw_wave(painter, p, info, r, pps, alpha);
    }
    // Fundidos a negro (o a silencio): lo que todavía no se ve o no se oye, oscuro sobre la curva.
    for (out, fade, crossed) in [(false, p.fade_in, xfade.0 > 0), (true, p.fade_out, xfade.1 > 0)] {
        let curve = if out { p.curves.1 } else { p.curves.0 };
        let w = fade as f32 / 1e7 * pps;
        if crossed || w < 1.0 {
            continue;
        }
        // Del lado de afuera el nivel es 0 (abajo); hacia adentro sube hasta 1 (arriba).
        let at = |k: f32| {
            let x = if out { r.right() - w * k } else { r.left() + w * k };
            Pos2::new(x, r.bottom() - r.height() * curve.shape(k))
        };
        let corner = Pos2::new(if out { r.right() } else { r.left() }, r.top());
        let pts: Vec<Pos2> = (0..=16).map(|k| at(k as f32 / 16.0)).collect();
        let mut mesh = egui::Mesh::default();
        let shade = BLACK.gamma_multiply(0.55 * alpha);
        mesh.colored_vertex(corner, shade);
        for q in &pts {
            mesh.colored_vertex(*q, shade);
        }
        for k in 1..pts.len() as u32 {
            mesh.add_triangle(0, k, k + 1);
        }
        painter.with_clip_rect(r.intersect(painter.clip_rect())).add(Shape::mesh(mesh));
        painter.add(Shape::line(pts, Stroke::new(1.0_f32, WHITE.gamma_multiply(0.8 * alpha))));
    }
    // Etiqueta: en la imagen, y en el sonido solo si va suelto (si no, ya la tiene su pareja).
    if (video || p.link == 0) && r.width() > 70.0 && r.height() >= 22.0 {
        let how = if p.still() { tr!("  ·  congelado", "  ·  frozen", "  ·  congelado").to_string() } else if p.speed != 1.0 { format!("  ·  {}", fmt_speed(p.speed)) } else { String::new() };
        let label = format!("{}  ·  {}{how}", library::pretty_name(&p.path), fmt_len(p.len));
        let g = one_line(painter, label, FontId::proportional(11.5), r.width() - 22.0);
        let badge = Rect::from_min_size(r.min + egui::vec2(6.0, 4.0), g.size() + egui::vec2(10.0, 4.0));
        painter.rect_filled(badge, 4.0, BADGE);
        painter.galley(badge.min + egui::vec2(5.0, 2.0), g, WHITE.gamma_multiply(alpha.max(0.6)));
    }

    // Borde y agarraderas
    if selected {
        painter.rect_stroke(r, 6.0, Stroke::new(2.0_f32, WHITE));
        for x in [r.left() + 3.0, r.right() - 3.0] {
            let h = Rect::from_center_size(Pos2::new(x, r.center().y), egui::vec2(6.0, r.height()));
            painter.rect_filled(h, 3.0, WHITE);
            painter.line_segment([Pos2::new(x, r.center().y - 8.0), Pos2::new(x, r.center().y + 8.0)], Stroke::new(1.0_f32, BLACK));
        }
    } else {
        painter.rect_stroke(r, 6.0, Stroke::new(1.0_f32, LINE));
    }
    missing
}

/// Agarraderas de las esquinas de arriba de una pieza (x de cada una), dentro de `r`.
fn draw_corners(painter: &egui::Painter, (cin, cout): (f32, f32), r: Rect) {
    for c in [Pos2::new(cin.max(r.left()) + 4.0, r.top() + 4.0), Pos2::new(cout.min(r.right()) - 4.0, r.top() + 4.0)] {
        let h = Rect::from_center_size(c, egui::vec2(7.0, 7.0));
        painter.rect_filled(h, 2.0, WHITE);
        painter.rect_stroke(h, 2.0, Stroke::new(1.0_f32, BLACK));
    }
}

/// Forma de onda del audio de la pieza en `ar`.
fn draw_wave(painter: &egui::Painter, p: &Piece, info: Option<&library::Info>, ar: Rect, pps: f32, alpha: f32) {
    let clip = painter.with_clip_rect(ar.shrink(1.0).intersect(painter.clip_rect()));
    let mid = ar.center().y;
    let half = (ar.height() / 2.0 - 4.0).max(1.0);
    let note = |text: &str| {
        if ar.width() > 80.0 && ar.height() >= 18.0 {
            clip.text(Pos2::new(ar.left() + 8.0, mid), Align2::LEFT_CENTER, text, FontId::proportional(11.5), MUTED);
        }
    };
    match info.and_then(|i| i.wave.as_ref()) {
        Some(w) if !p.muted => {
            // Una barra por píxel de pantalla, alineada a la grilla (sin huecos ni rayas al arrastrar).
            // Cada barra muestra lo que cae en ese píxel: el pico, claro, y el volumen medio (RMS), más fuerte
            // por dentro. Escala en dB de -48 a 0: el audio bajo (lo normal en clips de juego) se ve, y como el
            // RMS queda varios dB debajo del pico, lo fuerte no se vuelve un bloque.
            let gain = p.gain.min(2.0);
            let peak_col = WAVE.gamma_multiply(alpha * 0.45);
            let rms_col = WAVE.gamma_multiply(alpha);
            let px = 1.0 / painter.ctx().pixels_per_point();
            let left = ar.left().max(clip.clip_rect().left());
            let right = ar.right().min(clip.clip_rect().right());
            let t_at = |x: f32| p.src_at(p.start + ((x - ar.left()) / pps * 1e7) as i64);
            let size = |v: f32| {
                let v = v * gain;
                let db = if v > 0.0 { 20.0 * v.log10() } else { -100.0 };
                (((db + 48.0) / 48.0).clamp(0.0, 1.0) * half).max(px / 2.0)
            };
            let mut mesh = egui::Mesh::default();
            let mut x = (left / px).floor() * px;
            while x < right {
                let (peak, rms) = w.range(t_at(x), t_at(x + px));
                let (hp, hr) = (size(peak), size(rms));
                mesh.add_colored_rect(Rect::from_min_max(Pos2::new(x, mid - hp), Pos2::new(x + px, mid + hp)), peak_col);
                mesh.add_colored_rect(Rect::from_min_max(Pos2::new(x, mid - hr), Pos2::new(x + px, mid + hr)), rms_col);
                x += px;
            }
            clip.add(egui::Shape::mesh(mesh));
        }
        Some(_) => {
            clip.hline(ar.x_range(), mid, Stroke::new(1.0_f32, MUTED));
            note(tr!("Silenciado", "Muted", "Silenciado"));
        }
        None => note(tr!("Cargando audio…", "Loading audio…", "Carregando áudio…")),
    }
}

/// Alto de cada pista de video y de cada pista de audio cuando todas, regla incluida, miden `h`. `split` es la
/// parte del alto que usan las de video si también hay de audio (la elige el usuario arrastrando el borde).
fn lanes(h: f32, split: f32, nv: usize, na: usize) -> (f32, f32) {
    let avail = (h - RULER_H - (nv + na) as f32 * LANE_GAP).max(0.0);
    let (fv, fa) = (nv.max(1) as f32, na as f32);
    if na == 0 {
        return (avail / fv, 0.0);
    }
    let min = LANE_MIN.min(avail / (fv + fa));
    let video = (avail * split).clamp(min * fv, avail - min * fa);
    (video / fv, (avail - video) / fa)
}

/// Regla: "0:12" en los segundos enteros y "0:12.5" en los medios.
fn fmt_ruler(s: f32) -> String {
    let ds = (s * 10.0).round() as u64; // décimas
    if ds.is_multiple_of(10) {
        fmt_clock(ds / 10)
    } else {
        format!("{}.{}", fmt_clock(ds / 10), ds % 10)
    }
}

/// Largo de un clip: "12.5 s" hasta el minuto (al recortar importa la décima) y "1:05" desde ahí.
pub(super) fn fmt_len(t: i64) -> String {
    if t < 600_000_000 {
        format!("{:.1} s", t as f64 / 1e7)
    } else {
        fmt_clock(((t + 5_000_000) / 10_000_000) as u64)
    }
}

impl EditorApp {
    pub(super) fn top_bar(&mut self, ui: &mut egui::Ui) {
        // El aviso ("Listo: …", un error) aparece con un fundido corto; al irse, se va de una.
        let shown = self.job.is_none() && self.message.is_some();
        let fade = ui.ctx().animate_bool_with_time_and_easing(egui::Id::new("message"), shown, if shown { T_FADE } else { 0.0 }, ease_out);
        ui.horizontal_centered(|ui| {
            app_mark(ui, 26.0);
            ui.label(RichText::new("Clippy").family(bold()).size(18.0).color(TEXT));
            ui.label(RichText::new("Editor").size(18.0).color(MUTED));
            ui.add_space(24.0);
            ui.add_enabled_ui(!self.undo.is_empty(), |ui| {
                let tip = format!("{} ({})", tr!("Deshacer", "Undo", "Desfazer"), shortcuts::short(&self.prefs.keys, Act::Undo));
                if icon_only_button(ui, Glyph::Undo, Kind::Ghost, &tip).clicked() {
                    self.undo();
                }
            });
            ui.add_enabled_ui(!self.redo.is_empty(), |ui| {
                let tip = format!("{} ({})", tr!("Rehacer", "Redo", "Refazer"), shortcuts::short(&self.prefs.keys, Act::Redo));
                if icon_only_button(ui, Glyph::Redo, Kind::Ghost, &tip).clicked() {
                    self.redo();
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let tip = format!("{} ({})", tr!("Atajos y ayuda", "Shortcuts and help", "Atalhos e ajuda"), shortcuts::short(&self.prefs.keys, Act::Shortcuts));
                if gear_button(ui).on_hover_text(tip).clicked() {
                    self.open_help(HelpTab::Keys);
                }
                ui.add_space(6.0);
                if let Some(job) = &self.job {
                    let p = job.progress.get();
                    if button(ui, tr!("Cancelar", "Cancel", "Cancelar"), Kind::Secondary).clicked() {
                        job.progress.cancel.store(true, Ordering::Relaxed);
                    }
                    ui.add(
                        egui::ProgressBar::new(p)
                            .desired_width(240.0)
                            .desired_height(32.0)
                            .rounding(6.0)
                            .text(RichText::new(trf!("Exportando… {:.0}%", "Exporting… {:.0}%", "Exportando… {:.0}%", p * 100.0)).color(BLACK)),
                    );
                } else {
                    let enabled = !self.is_empty();
                    let r = ui.add_enabled_ui(enabled, |ui| icon_button(ui, Glyph::Export, tr!("Exportar video", "Export video", "Exportar vídeo"), Kind::Primary, egui::vec2(164.0, 36.0))).inner;
                    if r.clicked() {
                        self.export();
                    }
                    if !enabled {
                        r.on_disabled_hover_text(tr!("Agregá al menos un clip a la línea de tiempo", "Add at least one clip to the timeline", "Adicione pelo menos um clipe à linha do tempo"));
                    }
                    if let Some((ok, msg, out)) = self.message.clone() {
                        ui.add_space(8.0);
                        ui.scope(|ui| {
                            ui.multiply_opacity(fade);
                            if let Some(out) = out {
                                if icon_button(ui, Glyph::Folder, tr!("Abrir carpeta", "Open folder", "Abrir pasta"), Kind::Secondary, Vec2::ZERO).clicked() {
                                    if let Some(d) = out.parent() {
                                        open_folder(d);
                                    }
                                }
                                if button(ui, tr!("Ver video", "Watch video", "Ver vídeo"), Kind::Secondary).clicked() {
                                    let _ = std::process::Command::new("explorer").arg(&out).spawn();
                                }
                            }
                            ui.add(egui::Label::new(RichText::new(msg).color(if ok { TEXT } else { DANGER })).truncate());
                        });
                    }
                }
            });
        });
    }

    pub(super) fn timeline_panel(&mut self, ui: &mut egui::Ui) {
        // Ocupar todo el panel: su fondo y su tamaño (el que se guarda al arrastrar el borde) son los de esto.
        ui.set_min_height(ui.available_height());
        // Barra de herramientas
        let pos = self.pos();
        let total = self.total();
        let has_sel = self.selection().is_some();
        let empty = self.is_empty();
        ui.horizontal(|ui| {
            ui.set_height(40.0);
            let third = ui.available_width() / 3.0;
            ui.allocate_ui_with_layout(egui::vec2(third, 40.0), Layout::left_to_right(Align::Center), |ui| {
                ui.add_enabled_ui(!empty, |ui| {
                    if icon_button(ui, Glyph::Scissors, tr!("Dividir", "Split", "Dividir"), Kind::Secondary, Vec2::ZERO).on_hover_text(format!("{} ({})", tr!("Cortar en el cursor", "Split at playhead", "Cortar no cursor"), shortcuts::short(&self.prefs.keys, Act::Split))).clicked() {
                        self.split();
                    }
                });
                ui.add_enabled_ui(has_sel, |ui| {
                    if icon_button(ui, Glyph::Trash, tr!("Eliminar", "Delete", "Excluir"), Kind::Secondary, Vec2::ZERO).on_hover_text(format!("{} ({})", tr!("Eliminar el clip elegido", "Delete the selected clip", "Excluir o clipe escolhido"), shortcuts::short(&self.prefs.keys, Act::Delete))).clicked() {
                        self.delete_selected();
                    }
                });
                ui.add_enabled_ui(self.can_link(), |ui| {
                    let tip = format!("{} ({})", self.link_label(), shortcuts::short(&self.prefs.keys, Act::Link));
                    if icon_only_button(ui, Glyph::Detach, Kind::Secondary, &tip).clicked() {
                        self.toggle_link();
                    }
                });
                ui.add_enabled_ui(!empty, |ui| {
                    let tip = format!("{} ({})", tr!("Congelar el cuadro del cursor 3 segundos", "Freeze the frame at the playhead for 3 seconds", "Congelar o quadro do cursor por 3 segundos"), shortcuts::short(&self.prefs.keys, Act::Freeze));
                    if icon_only_button(ui, Glyph::Freeze, Kind::Secondary, &tip).clicked() {
                        self.freeze();
                    }
                    let tip = format!("{} ({})", tr!("Poner o sacar un marcador en el cursor", "Add or remove a marker at the playhead", "Pôr ou tirar um marcador no cursor"), shortcuts::short(&self.prefs.keys, Act::Marker));
                    if icon_only_button(ui, Glyph::Marker, Kind::Secondary, &tip).clicked() {
                        self.toggle_marker();
                    }
                });
            });
            ui.allocate_ui_with_layout(egui::vec2(third, 40.0), Layout::left_to_right(Align::Center), |ui| {
                let time = format!("{}  /  {}", fmt_time(pos), fmt_time(total));
                let tw = ui.painter().layout_no_wrap(time.clone(), FontId::proportional(15.0), TEXT).size().x;
                let group = 36.0 + 12.0 + tw;
                ui.add_space(((third - group) / 2.0).max(0.0));
                ui.add_enabled_ui(!empty, |ui| {
                    if play_button(ui, self.playing, 36.0).on_hover_text(format!("{} ({})", tr!("Reproducir / pausar", "Play / pause", "Reproduzir / pausar"), shortcuts::short(&self.prefs.keys, Act::PlayPause))).clicked() {
                        self.toggle_play();
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new(time).size(15.0).color(TEXT));
            });
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 40.0), Layout::right_to_left(Align::Center), |ui| {
                if button(ui, tr!("Ajustar", "Fit", "Ajustar"), Kind::Ghost).on_hover_text(Act::ZoomFit.label()).clicked() {
                    self.run_action(Act::ZoomFit);
                }
                let tip = format!("{} ({})", tr!("Acercar", "Zoom in", "Aproximar"), shortcuts::short(&self.prefs.keys, Act::ZoomIn));
                if icon_only_button(ui, Glyph::Plus, Kind::Ghost, &tip).clicked() {
                    self.run_action(Act::ZoomIn);
                }
                ui.spacing_mut().slider_width = 90.0;
                let mut zoom = self.zoom;
                if ui.add(egui::Slider::new(&mut zoom, 1.0..=ZOOM_MAX).logarithmic(true).show_value(false)).changed() {
                    self.zoom_to = Some((zoom, None));
                }
                let tip = format!("{} ({})", tr!("Alejar", "Zoom out", "Afastar"), shortcuts::short(&self.prefs.keys, Act::ZoomOut));
                if icon_only_button(ui, Glyph::Minus, Kind::Ghost, &tip).clicked() {
                    self.run_action(Act::ZoomOut);
                }
                ui.label(RichText::new("Zoom").size(13.0).color(MUTED));
                ui.add_space(8.0);
                let magnet = self.prefs.magnet;
                let key = shortcuts::short(&self.prefs.keys, Act::Magnet);
                let tip = if magnet {
                    trf!("Imán prendido: los clips se pegan solos ({key})", "Magnet on: clips snap together ({key})", "Ímã ligado: os clipes grudam sozinhos ({key})")
                } else {
                    trf!(
                        "Imán apagado: los clips quedan donde los soltás y, si se superponen, se funden ({key})",
                        "Magnet off: clips stay where you drop them and, if they overlap, they crossfade ({key})",
                        "Ímã desligado: os clipes ficam onde você os solta e, se se sobrepõem, se fundem ({key})"
                    )
                };
                if icon_only_button(ui, Glyph::Magnet, if magnet { Kind::Secondary } else { Kind::Ghost }, &tip).clicked() {
                    self.run_action(Act::Magnet);
                }
            });
        });
        ui.add_space(6.0);

        // Pistas: ocupan el alto que le dé el usuario al panel.
        let tracks_h = (ui.available_height() - SCROLLBAR_H).max(RULER_H + 2.0 * LANE_GAP);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let (g, _) = ui.allocate_exact_size(egui::vec2(GUTTER, tracks_h), Sense::hover());
            self.track_headers(ui, g);
            self.tracks(ui, tracks_h);
        });
    }

    /// Franja vertical de cada pista, de arriba hacia abajo, si todo (regla incluida) empieza en `top` y mide `h`.
    fn rows(&self, top: f32, h: f32) -> Vec<(usize, Rangef)> {
        let (vh, ah) = lanes(h, self.prefs.lane_split, self.count(TrackKind::Video), self.count(TrackKind::Audio));
        let mut y = top + RULER_H;
        self.display_order()
            .into_iter()
            .map(|ti| {
                let rh = if self.tracks[ti].kind == TrackKind::Video { vh } else { ah };
                y += LANE_GAP + rh;
                (ti, Rangef::new(y - rh, y))
            })
            .collect()
    }

    /// Clic derecho sobre un clip (ya elegido): lo que se le puede hacer, con sus atajos. `fades`: los fundidos
    /// que había donde se tocó.
    fn clip_menu(&mut self, ui: &mut egui::Ui, fades: &[(u64, bool)]) {
        let Some((ti, i)) = self.selection() else { return };
        let p = self.tracks[ti].clips[i].clone();
        let at_cursor = self.group(p.id).iter().any(|&(tj, j)| self.tracks[tj].clips[j].covers(self.pos()));
        let keys = self.prefs.keys.clone();
        let mut run = None;
        let mut item = |ui: &mut egui::Ui, act: Act, label: &str, enabled: bool, why: &str| {
            let b = ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(shortcuts::short(&keys, act))).on_disabled_hover_text(why);
            if b.clicked() {
                run = Some(act);
                ui.close_menu();
            }
        };
        if !fades.is_empty() {
            let current = self.find(fades[0].0).map(|(ti, i)| {
                let c = self.tracks[ti].clips[i].curves;
                if fades[0].1 { c.1 } else { c.0 }
            });
            ui.menu_button(tr!("Curva del fundido", "Fade curve", "Curva da transição"), |ui| {
                for (c, label, tip) in [
                    (
                        Curve::Even,
                        tr!("Pareja", "Equal power", "Uniforme"),
                        tr!("Al cruzarse dos sonidos el volumen no baja", "The volume doesn't dip when two sounds cross", "O volume não cai quando dois sons se cruzam"),
                    ),
                    (Curve::Linear, tr!("Lineal", "Linear", "Linear"), tr!("En línea recta", "In a straight line", "Em linha reta")),
                    (Curve::Smooth, tr!("Suave", "Smooth", "Suave"), tr!("Arranca y termina despacio", "Starts and ends slowly", "Começa e termina devagar")),
                ] {
                    if ui.add(egui::Button::new(label).selected(current == Some(c))).on_hover_text(tip).clicked() {
                        self.set_curve(fades, c);
                        ui.close_menu();
                    }
                }
            });
            ui.separator();
        }
        let away = tr!("Llevá el cursor sobre el clip", "Move the playhead over the clip", "Leve o cursor até o clipe");
        item(ui, Act::Split, tr!("Dividir", "Split", "Dividir"), at_cursor, away);
        item(ui, Act::Duplicate, tr!("Duplicar", "Duplicate", "Duplicar"), true, "");
        let video = self.selected_video().is_some();
        if video && !p.still() {
            item(ui, Act::Freeze, tr!("Congelar cuadro", "Freeze frame", "Congelar quadro"), at_cursor, away);
        }
        if !p.still() {
            ui.menu_button(tr!("Velocidad", "Speed", "Velocidade"), |ui| {
                for s in SPEEDS {
                    if ui.add(egui::Button::new(fmt_speed(s)).selected(p.speed == s)).clicked() {
                        self.push_undo();
                        self.set_speed(s);
                        ui.close_menu();
                    }
                }
            });
        }
        if video {
            ui.menu_button(tr!("Imagen", "Picture", "Imagem"), |ui| {
                if ui.button(tr!("Girar a la derecha", "Rotate right", "Girar para a direita")).clicked() {
                    self.edit_frame(|f| f.turns = (f.turns + 1) % 4);
                    ui.close_menu();
                }
                if ui.button(tr!("Espejo", "Mirror", "Espelhar")).clicked() {
                    self.edit_frame(|f| f.flip = !f.flip);
                    ui.close_menu();
                }
                if ui.button(tr!("Recortar…", "Crop…", "Recortar…")).clicked() {
                    self.start_crop();
                    ui.close_menu();
                }
            });
        }
        if let Some((ta, ia)) = self.selected_audio().filter(|_| !p.still()) {
            let muted = self.tracks[ta].clips[ia].muted;
            let label = if muted { tr!("Activar el sonido", "Unmute", "Ativar o som") } else { tr!("Silenciar", "Mute", "Silenciar") };
            item(ui, Act::MuteClip, label, true, "");
        }
        if self.can_link() {
            item(ui, Act::Link, self.link_label(), true, "");
        }
        ui.separator();
        item(ui, Act::Delete, tr!("Eliminar", "Delete", "Excluir"), true, "");
        if let Some(act) = run {
            self.run_action(act);
        }
    }

    /// Columna de la izquierda: el botón para agregar pistas y, en cada una, su nombre y los botones para verla
    /// y oírla. Con el botón derecho sobre una pista se la quita.
    fn track_headers(&mut self, ui: &mut egui::Ui, g: Rect) {
        let size = egui::vec2(GUTTER - 10.0, RULER_H - 2.0);
        let r = ui.put(Rect::from_min_size(g.min + egui::vec2(0.0, 1.0), size), |ui: &mut egui::Ui| icon_button(ui, Glyph::Plus, tr!("Pista", "Track", "Faixa"), Kind::Ghost, size));
        let menu = ui.id().with("add_track");
        if r.clicked() {
            ui.memory_mut(|m| m.toggle_popup(menu));
        }
        let mut add = None;
        egui::popup_below_widget(ui, menu, &r, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(150.0);
            for (kind, label) in [
                (TrackKind::Video, tr!("Pista de video", "Video track", "Faixa de vídeo")),
                (TrackKind::Audio, tr!("Pista de audio", "Audio track", "Faixa de áudio")),
            ] {
                let b = ui.add_enabled(self.count(kind) < MAX_TRACKS, egui::Button::new(label));
                if b.clicked() {
                    add = Some(kind);
                }
                b.on_disabled_hover_text(trf!("Hasta {MAX_TRACKS} pistas de cada tipo", "Up to {MAX_TRACKS} tracks of each kind", "Até {MAX_TRACKS} faixas de cada tipo"));
            }
        });
        // Qué tocaron: (pista, false = ocultar o silenciar, true = quitar).
        let mut act = None;
        let one_video = self.count(TrackKind::Video) == 1;
        for (ti, yr) in self.rows(g.top(), g.height()) {
            let t = &self.tracks[ti];
            let row = Rect::from_x_y_ranges(g.x_range(), yr);
            let removable = t.kind == TrackKind::Audio || !one_video;
            ui.interact(row, ui.id().with(("track", t.id)), Sense::click()).context_menu(|ui| {
                let b = ui
                    .add_enabled(removable, egui::Button::new(tr!("Quitar pista", "Remove track", "Remover faixa")))
                    .on_disabled_hover_text(tr!("Siempre queda al menos una pista de video", "There's always at least one video track", "Sempre fica pelo menos uma faixa de vídeo"));
                if b.clicked() {
                    act = Some((ti, true));
                    ui.close_menu();
                }
            });
            let x = row.right() - 22.0;
            let (glyph, on, tip) = match t.kind {
                TrackKind::Video => (
                    Glyph::Eye,
                    !t.hidden,
                    if t.hidden { tr!("Mostrar la pista", "Show the track", "Mostrar a faixa") } else { tr!("Ocultar la pista", "Hide the track", "Ocultar a faixa") },
                ),
                TrackKind::Audio => (
                    Glyph::Speaker,
                    !t.muted,
                    if t.muted {
                        tr!("Activar el sonido de la pista", "Unmute the track", "Ativar o som da faixa")
                    } else {
                        tr!("Silenciar la pista", "Mute the track", "Silenciar a faixa")
                    },
                ),
            };
            let at = Rect::from_center_size(Pos2::new(x, row.center().y), Vec2::splat(24.0));
            if ui.put(at, |ui: &mut egui::Ui| icon_toggle(ui, glyph, on, tip)).clicked() {
                act = Some((ti, false));
            }
            let name = one_line(ui.painter(), self.track_name(ti), FontId::proportional(12.5), x - 16.0 - row.left());
            ui.painter().galley(Pos2::new(row.left() + 4.0, row.center().y - name.size().y / 2.0), name, MUTED);
        }
        match act {
            Some((ti, false)) => self.toggle_track(ti),
            Some((ti, true)) => self.remove_track(ti),
            None => {}
        }
        if let Some(k) = add {
            self.add_track(k);
        }
    }

    /// Con el imán, `dt` corrido para que `from + dt` caiga justo en el punto de `snaps` más cercano, si está a
    /// menos de SNAP px; anota dónde se enganchó para marcarlo.
    fn snapped(&mut self, from: i64, dt: i64, snaps: &[i64], pps: f32) -> i64 {
        let tol = (SNAP / pps * 1e7) as i64;
        self.snap = snaps.iter().copied().filter(|s| (s - from - dt).abs() <= tol).min_by_key(|s| (s - from - dt).abs());
        self.snap.map_or(dt, |s| s - from)
    }

    /// Los fundidos que hay en (x, y) en el tiempo `t`: el cruzado (los dos lados) o el de entrada o salida de
    /// una pieza. Vacío si ahí no hay ninguno.
    fn fade_at(&self, rows: &[(usize, Rangef)], xs: &[Vec<(f32, f32)>], x: f32, y: f32, t: i64) -> Vec<(u64, bool)> {
        let Some(&(ti, _)) = rows.iter().find(|(_, yr)| yr.contains(y)) else { return Vec::new() };
        let track = &self.tracks[ti];
        let ov = track.overlaps();
        for i in (0..track.clips.len()).rev() {
            let p = &track.clips[i];
            if x < xs[ti][i].0 || x > xs[ti][i].1 {
                continue;
            }
            if ov[i].0 > 0 && t < p.start + ov[i].0 {
                return vec![(track.clips[i - 1].id, true), (p.id, false)];
            }
            if t < p.start + p.fade_in.max(ov[i].0) {
                return vec![(p.id, false)];
            }
            if t >= p.end() - p.fade_out {
                return vec![(p.id, true)];
            }
            return Vec::new();
        }
        Vec::new()
    }

    /// Acerca (k > 1) o aleja la línea de tiempo. Se aplica al dibujarla, con el cursor de reproducción quieto.
    pub(super) fn zoom_by(&mut self, k: f32) {
        let z = self.zoom_to.map_or(self.zoom, |(z, _)| z);
        self.zoom_to = Some((z * k, None));
    }

    fn tracks(&mut self, ui: &mut egui::Ui, tracks_h: f32) {
        let view_w = ui.available_width();
        let left = ui.cursor().min.x;
        let total = self.total();
        let total_s = (total as f32 / 1e7).max(1.0);
        let fit = ((view_w - 2.0 * PAD) / total_s).max(1.0);
        let frozen = match self.drag {
            Drag::Trim { pps, .. } | Drag::Fade { pps, .. } => Some(pps),
            _ => None,
        };

        // Zoom con Ctrl + rueda (alrededor del puntero), desplazamiento con la rueda.
        let hovered = ui.rect_contains_pointer(Rect::from_min_size(ui.cursor().min, egui::vec2(view_w, tracks_h + SCROLLBAR_H)));
        if hovered {
            let (zoom_delta, scroll_y, at) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta.y, i.pointer.hover_pos().map(|p| p.x)));
            if zoom_delta != 1.0 {
                self.zoom_to = Some((self.zoom * zoom_delta, at));
            } else if scroll_y != 0.0 {
                self.set_scroll = Some((self.scroll_x - scroll_y).max(0.0));
            }
        }
        // El zoom cambia la escala dejando quieto un punto: el puntero, o el cursor de reproducción si se ve,
        // o el centro. Mientras se recorta espera: la escala está fija hasta soltar.
        if frozen.is_none() {
            if let Some((z, at)) = self.zoom_to.take() {
                let z = z.clamp(1.0, ZOOM_MAX);
                let (old, new) = (fit * self.zoom, fit * z);
                let ph = PAD + self.pos() as f32 / 1e7 * old - self.scroll_x;
                let vx = at.map_or(if (0.0..=view_w).contains(&ph) { ph } else { view_w / 2.0 }, |x| x - left);
                let s = (self.scroll_x + vx - PAD) / old;
                self.zoom = z;
                self.set_scroll = Some((PAD + s * new - vx).max(0.0));
            }
        }
        let pps = frozen.unwrap_or(fit * self.zoom);
        let mut content_w = (total_s * pps + 2.0 * PAD).max(view_w);
        if frozen.is_some() {
            // Si la línea se achicara al recortar, el desplazamiento se correría y el borde saltaría bajo el mouse.
            content_w = content_w.max(self.scroll_x + view_w);
        }
        // Seguir el cursor mientras se reproduce.
        let ph_x = PAD + self.pos() as f32 / 1e7 * pps;
        if self.playing && (ph_x > self.scroll_x + view_w - 60.0 || ph_x < self.scroll_x) {
            self.set_scroll = Some((ph_x - 80.0).max(0.0));
        }
        // Arrastrando cerca de un costado, la línea se desplaza sola: más rápido cuanto más cerca (o más afuera).
        if !matches!(self.drag, Drag::None) && ui.input(|i| i.pointer.is_decidedly_dragging()) {
            if let Some(x) = ui.input(|i| i.pointer.interact_pos()).map(|p| p.x) {
                let push = ((x - (left + view_w - EDGE)).max(0.0) - (left + EDGE - x).max(0.0)) / EDGE;
                let from = self.set_scroll.unwrap_or(self.scroll_x);
                let dt = ui.input(|i| i.stable_dt).min(0.05);
                let to = (from + push.clamp(-1.0, 1.0) * AUTO_SCROLL * dt).clamp(0.0, content_w - view_w);
                if to != from {
                    self.set_scroll = Some(to);
                    ui.ctx().request_repaint();
                }
            }
        }

        let mut sa = egui::ScrollArea::horizontal().id_salt("tracks").auto_shrink([false, true]);
        if let Some(x) = self.set_scroll.take() {
            sa = sa.horizontal_scroll_offset(x);
        }
        let out = sa.show(ui, |ui| {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(content_w, tracks_h), Sense::click_and_drag());
            self.draw_tracks(ui, rect, &resp, pps);
        });
        self.scroll_x = out.state.offset.x;
    }

    /// Dónde se ve cada pieza (x de inicio y fin, por pista), con `tx` de tiempo a x. Con el imán, durante un
    /// recorte por la izquierda el borde derecho queda quieto; recién soltado un clip, cada pieza se desliza
    /// desde donde se veía.
    fn piece_xs(&mut self, ui: &egui::Ui, tx: impl Fn(i64) -> f32) -> Vec<Vec<(f32, f32)>> {
        let mut xs: Vec<Vec<(f32, f32)>> = self.tracks.iter().map(|t| t.clips.iter().map(|p| (tx(p.start), tx(p.end()))).collect()).collect();
        if let Drag::Trim { parts, left: true, .. } = &self.drag {
            if self.prefs.magnet {
                for t in parts {
                    let (ti, i) = t.at;
                    let shift = tx(self.tracks[ti].clips[i].src_in) - tx(t.orig.src_in);
                    for x in xs[ti].iter_mut().skip(i) {
                        x.0 += shift;
                        x.1 += shift;
                    }
                }
            }
        }
        if let Some((t0, from)) = self.settle.take() {
            let p = ((ui.input(|i| i.time) - t0) as f32 / T_SETTLE).min(1.0);
            if p < 1.0 {
                let k = 1.0 - ease_out(p);
                for (t, row) in self.tracks.iter().zip(xs.iter_mut()) {
                    for (pc, x) in t.clips.iter().zip(row.iter_mut()) {
                        if let Some(&(_, d)) = from.iter().find(|(id, _)| *id == pc.id) {
                            x.0 += d * k;
                            x.1 += d * k;
                        }
                    }
                }
                self.settle = Some((t0, from));
                ui.ctx().request_repaint();
            }
        }
        xs
    }

    fn draw_tracks(&mut self, ui: &mut egui::Ui, rect: Rect, resp: &egui::Response, pps: f32) {
        let x0 = rect.left() + PAD;
        let tx = |t: i64| x0 + t as f32 / 1e7 * pps;
        let xt = |x: f32| ((x - x0) / pps * 1e7) as i64;
        let ruler = Rect::from_min_size(rect.min, egui::vec2(rect.width(), RULER_H));
        let (nv, na) = (self.count(TrackKind::Video), self.count(TrackKind::Audio));
        let mut rows = self.rows(rect.top(), rect.height());
        // Borde entre las pistas de video y las de audio: se arrastra (como el de los paneles) y el alto se reparte
        // al instante. Toma el clic y el arrastre de esa franja, así no se mueve el cursor ni un clip debajo.
        let split = (na > 0).then(|| {
            let y = rows[nv - 1].1.max;
            let band = Rect::from_min_max(Pos2::new(rect.left(), y - 2.0), Pos2::new(rect.right(), y + LANE_GAP + 2.0));
            let r = ui.interact(band, ui.id().with("lane_split"), Sense::click_and_drag());
            if let Some(p) = r.interact_pointer_pos().filter(|_| r.dragged()) {
                let avail = (rect.height() - RULER_H - (nv + na) as f32 * LANE_GAP).max(1.0);
                let video = p.y - ruler.bottom() - nv as f32 * LANE_GAP - LANE_GAP / 2.0;
                let (v, a) = lanes(rect.height(), video / avail, nv, na);
                self.prefs.lane_split = v * nv as f32 / (v * nv as f32 + a * na as f32).max(1.0);
                rows = self.rows(rect.top(), rect.height());
            }
            if r.drag_stopped() {
                self.prefs.save();
            }
            r
        });
        let on_split = split.as_ref().is_some_and(|r| r.hovered() || r.dragged());
        let lanes_bottom = rows.last().map_or(ruler.bottom(), |r| r.1.max);
        let row_of = |ti: usize| rows.iter().find(|r| r.0 == ti).map(|r| r.1);
        let xs = self.piece_xs(ui, tx);

        // --- Interacción ---
        let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
        // Esquinas de arriba de cada pieza, en x.
        let corners: Vec<Vec<(f32, f32)>> = (0..self.tracks.len())
            .map(|ti| {
                (0..self.tracks[ti].clips.len())
                    .map(|i| {
                        let (a, b) = self.fade_handles(ti, i);
                        (tx(a), tx(b))
                    })
                    .collect()
            })
            .collect();
        // Qué hay en (x, y): pista, pieza y -1 = borde izquierdo, 0 = medio, 1 = borde derecho, 2 y 3 = esquinas
        // de arriba (fundidos). Donde dos se superponen manda la de arriba (la que empieza después).
        let hit = |x: f32, y: f32| -> Option<(usize, usize, i32)> {
            let &(ti, yr) = rows.iter().find(|(_, yr)| yr.contains(y))?;
            let row = &xs[ti];
            if y <= yr.min + FADE_GRIP {
                for i in (0..row.len()).rev() {
                    let (cin, cout) = corners[ti][i];
                    if (x - cout).abs() <= FADE_GRIP / 2.0 && x <= row[i].1 + 2.0 {
                        return Some((ti, i, 3));
                    }
                    if (x - cin).abs() <= FADE_GRIP / 2.0 && x >= row[i].0 - 2.0 {
                        return Some((ti, i, 2));
                    }
                }
            }
            (0..row.len()).rev().find_map(|i| {
                let (a, b) = row[i];
                // Entre dos clips pegados, cada lado del corte es del clip que está de ese lado.
                let lo = if i == 0 || row[i - 1].1 < a - 1.0 { a - GRIP_OUT } else { a };
                let hi = if i + 1 == row.len() || row[i + 1].0 > b + 1.0 { b + GRIP_OUT } else { b };
                if x < lo || x > hi {
                    return None;
                }
                let g = ((b - a) / 3.0).min(GRIP);
                Some((ti, i, if x <= a + g { -1 } else if x >= b - g { 1 } else { 0 }))
            })
        };
        // El marcador que hay en (x, y) de la regla.
        let markers = self.markers.clone();
        let marker_at = |h: Pos2| -> Option<usize> {
            ruler.contains(h).then(|| markers.iter().enumerate().filter(|(_, &m)| (tx(m) - h.x).abs() <= MARKER_W).min_by_key(|(_, &m)| (tx(m) - h.x).abs() as i64).map(|(k, _)| k)).flatten()
        };
        // Apretar en la regla o fuera de los clips lleva el cursor ahí en el acto, y desde ahí se arrastra.
        let pressed = resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_pressed());
        if pressed && pointer.is_some_and(|h| hit(h.x, h.y).is_none() && marker_at(h).is_none()) {
            self.drag = Drag::Scrub;
        }
        // Un clic lleva el cursor y sigue reproduciendo; recién al arrastrar se pausa (como antes).
        if resp.drag_started() && matches!(self.drag, Drag::Scrub) {
            self.pause();
        }
        if resp.drag_started() && matches!(self.drag, Drag::None) {
            let press = ui.input(|i| i.pointer.press_origin()).or(pointer).unwrap_or_default();
            let press_x = press.x - rect.left();
            self.drag_before = Some(self.tracks.clone());
            self.pause();
            self.drag = match hit(press.x, press.y) {
                _ if marker_at(press).is_some() => Drag::Marker { k: marker_at(press).unwrap_or(0), before: self.markers.clone() },
                Some((ti, i, kind)) if kind != 0 => {
                    let at = self.dragged(ti, i, ui.input(|i| i.modifiers.alt));
                    let snaps = if self.prefs.magnet { self.snap_points(&at) } else { Vec::new() };
                    let p = &self.tracks[ti].clips[i];
                    let corners = self.fade_handles(ti, i);
                    match kind {
                        2 | 3 => Drag::Fade {
                            parts: at.iter().map(|&a| self.start_fade(a, kind == 3)).collect(),
                            out: kind == 3,
                            press_x,
                            pps,
                            from: if kind == 3 { corners.1 } else { corners.0 },
                            snaps,
                        },
                        _ => Drag::Trim { parts: self.start_trim(&at), left: kind < 0, press_x, pps, from: if kind < 0 { p.start } else { p.end() }, snaps },
                    }
                }
                Some((ti, i, _)) => Drag::Move { id: self.tracks[ti].clips[i].id, grab_dx: press.x - xs[ti][i].0 },
                None => Drag::Scrub,
            };
            if let Some((ti, i, _)) = hit(press.x, press.y) {
                self.selected = Some(self.tracks[ti].clips[i].id);
            }
        }
        if resp.is_pointer_button_down_on() {
            if let Some(h) = pointer {
                let throttle = self.last_seek.elapsed() > SEEK_EVERY;
                match self.drag.clone() {
                    Drag::Scrub => {
                        // El cursor sigue al mouse en cada cuadro; el video, como mucho cada SEEK_EVERY.
                        let t = xt(h.x).clamp(0, self.total());
                        if self.scrub_t != Some(t) {
                            self.scrub_t = Some(t);
                            ui.ctx().request_repaint(); // la hora de la barra se dibujó antes que esto
                        }
                        if t != self.clock.pos() {
                            if pressed || throttle {
                                self.seek(t);
                                self.last_seek = Instant::now();
                            } else {
                                ui.ctx().request_repaint_after(SEEK_EVERY);
                            }
                        }
                    }
                    Drag::Trim { parts, left, press_x, from, snaps, .. } => {
                        let dt = self.snapped(from, ((h.x - rect.left() - press_x) / pps * 1e7) as i64, &snaps, pps);
                        self.trim(&parts, left, dt);
                        if throttle {
                            let p = &self.tracks[parts[0].at.0].clips[parts[0].at.1];
                            let t = if left { p.start } else { p.end() - self.frame_len(&p.path) };
                            self.seek(t);
                            self.last_seek = Instant::now();
                        }
                    }
                    Drag::Fade { parts, out, press_x, from, snaps, .. } => {
                        let dt = self.snapped(from, ((h.x - rect.left() - press_x) / pps * 1e7) as i64, &snaps, pps);
                        for f in &parts {
                            self.fade(f, out, dt);
                        }
                        if throttle {
                            let (a, b) = self.fade_handles(parts[0].at.0, parts[0].at.1);
                            self.seek(if out { b - self.frame_at_cursor() } else { a });
                            self.last_seek = Instant::now();
                        }
                    }
                    Drag::Marker { k, .. } => {
                        // Con el imán se engancha a los bordes de los clips y a los otros marcadores.
                        let mut snaps = if self.prefs.magnet { self.snap_points(&[]) } else { Vec::new() };
                        if let Some(m) = self.markers.get(k) {
                            snaps.retain(|s| s != m);
                        }
                        let t = xt(h.x).clamp(0, self.total());
                        let t = t + self.snapped(t, 0, &snaps, pps);
                        if let Some(m) = self.markers.get_mut(k) {
                            *m = t;
                        }
                        if throttle {
                            self.seek(t);
                            self.last_seek = Instant::now();
                        }
                    }
                    Drag::Move { .. } | Drag::None => {}
                }
            }
        } else if !matches!(self.drag, Drag::None) {
            // Se soltó (o Esc cortó el arrastre).
            let before = self.drag_before.take();
            self.snap = None;
            match std::mem::replace(&mut self.drag, Drag::None) {
                Drag::Trim { parts, left, .. } => {
                    let p = &self.tracks[parts[0].at.0].clips[parts[0].at.1];
                    let s = if left { p.start } else { p.end() - self.frame_len(&p.path) };
                    self.commit(before, s);
                }
                Drag::Fade { parts, out, .. } => {
                    let (a, b) = self.fade_handles(parts[0].at.0, parts[0].at.1);
                    self.commit(before, if out { b - self.frame_at_cursor() } else { a });
                }
                Drag::Move { id, grab_dx } => {
                    if let (Some(h), Some((ti, _))) = (pointer, self.find(id)) {
                        let a = h.x - grab_dx;
                        // Va a la pista del mismo tipo que esté bajo el mouse (si no, se queda en la suya).
                        let kind = self.tracks[ti].kind;
                        let to = rows.iter().find(|(k, yr)| yr.contains(h.y) && self.tracks[*k].kind == kind).map_or(ti, |r| r.0);
                        // Dónde se ve cada pieza al soltar (la arrastrada, bajo el mouse).
                        let mut was: Vec<(u64, f32)> = Vec::new();
                        for (t, row) in self.tracks.iter().zip(&xs) {
                            was.extend(t.clips.iter().zip(row).map(|(p, x)| (p.id, x.0)));
                        }
                        if let Some(w) = was.iter_mut().find(|w| w.0 == id) {
                            w.1 = a;
                        }
                        if self.drop_piece(id, to, xt(a)) {
                            let s = self.find(id).map_or(0, |(ti, i)| self.tracks[ti].clips[i].start);
                            self.commit(before, s);
                        }
                        if !reduced_motion() {
                            let mut from = Vec::new();
                            for t in &self.tracks {
                                from.extend(t.clips.iter().filter_map(|p| was.iter().find(|w| w.0 == p.id).map(|w| (p.id, w.1 - tx(p.start)))));
                            }
                            self.settle = Some((ui.input(|i| i.time), from));
                        }
                    }
                }
                Drag::Marker { before, .. } => {
                    self.markers.sort_unstable();
                    self.record_markers(before);
                }
                Drag::Scrub | Drag::None => {
                    let t = pointer.map_or(self.pos(), |h| xt(h.x)).clamp(0, self.total());
                    self.scrub_t = None;
                    if t != self.clock.pos() {
                        self.seek(t);
                    }
                }
            }
        }
        let cursor = match &self.drag {
            _ if on_split => Some(CursorIcon::ResizeVertical),
            Drag::Move { .. } | Drag::Marker { .. } => Some(CursorIcon::Grabbing),
            Drag::Trim { .. } | Drag::Fade { .. } => Some(CursorIcon::ResizeHorizontal),
            Drag::Scrub | Drag::None => resp.hover_pos().and_then(|h| match hit(h.x, h.y) {
                _ if marker_at(h).is_some() => Some(CursorIcon::Grab),
                Some((_, _, 0)) => Some(CursorIcon::Grab),
                Some(_) => Some(CursorIcon::ResizeHorizontal),
                None => ruler.contains(h).then_some(CursorIcon::PointingHand),
            }),
        };
        if let Some(c) = cursor {
            ui.ctx().set_cursor_icon(c);
        }
        // Clic derecho: qué se tocó queda guardado mientras el menú está abierto.
        if resp.secondary_clicked() {
            self.menu = match pointer {
                Some(h) if marker_at(h).is_some() => Menu::Marker(marker_at(h).unwrap_or(0)),
                Some(h) if ruler.contains(h) => Menu::Ruler(xt(h.x).clamp(0, self.total())),
                Some(h) => match hit(h.x, h.y) {
                    Some((ti, i, _)) => {
                        self.selected = Some(self.tracks[ti].clips[i].id);
                        Menu::Clip { fades: self.fade_at(&rows, &xs, h.x, h.y, xt(h.x)) }
                    }
                    None => Menu::None,
                },
                None => Menu::None,
            };
        }
        if !matches!(self.menu, Menu::None) {
            resp.context_menu(|ui| match self.menu.clone() {
                Menu::Clip { fades } => self.clip_menu(ui, &fades),
                Menu::Marker(k) => {
                    if ui.button(tr!("Ir al marcador", "Go to marker", "Ir para o marcador")).clicked() {
                        if let Some(&m) = self.markers.get(k) {
                            self.seek(m);
                        }
                        ui.close_menu();
                    }
                    if ui.button(tr!("Quitar marcador", "Remove marker", "Remover marcador")).clicked() {
                        if k < self.markers.len() {
                            self.push_undo();
                            self.markers.remove(k);
                        }
                        ui.close_menu();
                    }
                }
                Menu::Ruler(t) => {
                    if ui.button(tr!("Poner un marcador acá", "Add a marker here", "Pôr um marcador aqui")).clicked() {
                        self.push_undo();
                        self.markers.push(t);
                        self.markers.sort_unstable();
                        ui.close_menu();
                    }
                }
                Menu::None => {}
            });
        }
        if resp.clicked() {
            if let Some(h) = pointer {
                match hit(h.x, h.y) {
                    Some((ti, i, _)) => self.selected = Some(self.tracks[ti].clips[i].id),
                    None if !ruler.contains(h) => self.selected = None,
                    None => {}
                }
                // Tocar un marcador lleva el cursor justo ahí.
                let t = marker_at(h).and_then(|k| self.markers.get(k).copied()).unwrap_or(xt(h.x)).clamp(0, self.total());
                if t != self.clock.pos() {
                    self.seek(t);
                }
            }
        }

        // --- Dibujo ---
        // Soltar pudo cambiar la línea (orden, largos, pistas): se dibuja como quedó.
        let xs = self.piece_xs(ui, tx);
        let painter = ui.painter_at(rect);
        // Regla
        let step = [0.5f32, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0]
            .into_iter()
            .find(|s| s * pps >= 70.0)
            .unwrap_or(600.0);
        let mut s = 0.0f32;
        let end_s = (rect.width() - PAD) / pps;
        while s <= end_s {
            let x = x0 + s * pps;
            painter.line_segment([Pos2::new(x, ruler.bottom() - 8.0), Pos2::new(x, ruler.bottom())], Stroke::new(1.0_f32, MUTED));
            painter.text(Pos2::new(x + 4.0, ruler.top() + 2.0), Align2::LEFT_TOP, fmt_ruler(s), FontId::proportional(11.0), MUTED);
            let half = x + step * pps / 2.0;
            painter.line_segment([Pos2::new(half, ruler.bottom() - 4.0), Pos2::new(half, ruler.bottom())], Stroke::new(1.0_f32, LINE));
            s += step;
        }
        painter.hline(rect.x_range(), ruler.bottom(), Stroke::new(1.0_f32, LINE));
        for (_, yr) in &rows {
            painter.rect_filled(Rect::from_x_y_ranges(rect.x_range(), *yr), 4.0, TRACK_LANE);
        }

        if self.is_empty() {
            let r = Rect::from_min_max(Pos2::new(x0, ruler.bottom() + LANE_GAP), Pos2::new(rect.right() - PAD, lanes_bottom));
            painter.rect_stroke(r, 8.0, Stroke::new(1.0_f32, CONTROL_HOVER));
            painter.text(r.center(), Align2::CENTER_CENTER, tr!("Hacé clic en un video de la biblioteca o arrastralo acá", "Click a video in the library or drag it here", "Clique em um vídeo da biblioteca ou arraste-o para cá"), FontId::proportional(14.0), MUTED);
            return;
        }

        let hovered = resp.hover_pos().and_then(|h| hit(h.x, h.y)).map(|(ti, i, _)| self.tracks[ti].clips[i].id);
        let info = self.loader.info.clone();
        let mut info = info.lock().unwrap();
        // Miniaturas de la tira que se ven y todavía no llegaron, por archivo.
        let mut missing: HashMap<PathBuf, Vec<usize>> = HashMap::new();
        let moving = if let Drag::Move { id, .. } = self.drag { self.group(id) } else { Vec::new() };
        for (ti, t) in self.tracks.iter().enumerate() {
            let Some(yr) = row_of(ti) else { continue };
            let row = Rect::from_x_y_ranges(rect.x_range(), yr);
            let video = t.kind == TrackKind::Video;
            // Lo que no se ve (pista oculta) o no se oye (pista muda) queda tenue.
            let alpha = if t.hidden || t.muted { 0.4 } else { 1.0 };
            let overlaps = t.overlaps();
            for (i, (p, &x)) in t.clips.iter().zip(&xs[ti]).enumerate() {
                if !moving.contains(&(ti, i)) {
                    let m = draw_piece(&painter, p, info.get(&p.path), x, row, video, pps, self.is_selected(p), alpha, overlaps[i]);
                    missing.entry(p.path.clone()).or_default().extend(m);
                }
            }
            // Fundidos cruzados: una cruz sobre lo que se superponen.
            for (i, &(fin, _)) in overlaps.iter().enumerate() {
                if fin <= 0 || moving.contains(&(ti, i)) {
                    continue;
                }
                let a = xs[ti][i].0;
                let r = Rect::from_min_max(Pos2::new(a, row.top() + 1.0), Pos2::new(a + fin as f32 / 1e7 * pps, row.bottom() - 1.0));
                painter.rect_filled(r, 0.0, WHITE.gamma_multiply(0.12));
                // La que entra sube con su curva y la que sale baja con la suya.
                let st = Stroke::new(1.0_f32, WHITE.gamma_multiply(0.7));
                let (cin, cout) = (t.clips[i].curves.0, t.clips[i - 1].curves.1);
                let line = |c: Curve, up: bool| -> Vec<Pos2> {
                    (0..=16)
                        .map(|k| {
                            let x = k as f32 / 16.0;
                            let v = c.shape(if up { x } else { 1.0 - x });
                            Pos2::new(r.left() + r.width() * x, r.bottom() - r.height() * v)
                        })
                        .collect()
                };
                painter.add(Shape::line(line(cin, true), st));
                painter.add(Shape::line(line(cout, false), st));
            }
            // Agarraderas de los fundidos, encima de todo: en lo elegido y bajo el mouse.
            for (i, p) in t.clips.iter().enumerate() {
                if !moving.contains(&(ti, i)) && (self.is_selected(p) || hovered == Some(p.id)) {
                    draw_corners(&painter, corners[ti][i], Rect::from_x_y_ranges(Rangef::new(xs[ti][i].0 + 1.0, xs[ti][i].1 - 1.0), yr));
                }
            }
        }
        // Pieza que se está moviendo (con su pareja): sigue al mouse, en la pista donde caería. Con el imán, una
        // barra marca dónde va a quedar.
        let moved = if let Drag::Move { id, grab_dx } = self.drag { self.find(id).map(|at| (id, grab_dx, at)) } else { None };
        if let (Some((id, grab_dx, (ti, i))), Some(h)) = (moved, pointer) {
            let kind = self.tracks[ti].kind;
            let to = rows.iter().find(|(k, yr)| yr.contains(h.y) && self.tracks[*k].kind == kind).map_or(ti, |r| r.0);
            let dx = h.x - grab_dx - xs[ti][i].0;
            for &(tj, j) in &moving {
                let p = &self.tracks[tj].clips[j];
                let dest = if p.id == id { to } else { tj };
                let (a, b) = (xs[tj][j].0 + dx, xs[tj][j].1 + dx);
                let row = Rect::from_x_y_ranges(rect.x_range(), row_of(dest).unwrap_or(rows[0].1));
                if self.prefs.magnet {
                    let center = xt((a + b) / 2.0);
                    let others: Vec<usize> = (0..self.tracks[dest].clips.len()).filter(|&k| !moving.contains(&(dest, k))).collect();
                    let k = others.iter().filter(|&&k| self.tracks[dest].clips[k].start + self.tracks[dest].clips[k].len / 2 < center).count();
                    let bar_x = match others.get(k) {
                        Some(&k) => xs[dest][k].0,
                        None => others.last().map_or(x0, |&k| xs[dest][k].1),
                    };
                    painter.rect_filled(Rect::from_center_size(Pos2::new(bar_x - 1.0, row.center().y), egui::vec2(3.0, row.height() + 6.0)), 1.5, WHITE);
                }
                let video = self.tracks[dest].kind == TrackKind::Video;
                let m = draw_piece(&painter, p, info.get(&p.path), (a, b), row.translate(egui::vec2(0.0, -4.0)), video, pps, true, 0.75, self.tracks[tj].overlaps()[j]);
                missing.entry(p.path.clone()).or_default().extend(m);
            }
        }
        for (path, m) in missing {
            if let Some(e) = info.get_mut(&path) {
                e.wanted = m;
            }
        }

        // Dónde quedó enganchado lo que se arrastra.
        if let Some(t) = self.snap {
            painter.vline(tx(t), Rangef::new(ruler.bottom(), lanes_bottom), Stroke::new(1.0_f32, WHITE.gamma_multiply(0.7)));
        }
        // Mientras se arrastra una esquina, cuánto dura ese fundido.
        if let (Drag::Fade { parts, out, .. }, Some(h)) = (&self.drag, pointer) {
            let (crossed, len) = self.fade_len(&parts[0], *out);
            let len = fmt_len(len);
            let text = if crossed {
                trf!("Fundido {len}", "Crossfade {len}", "Transição {len}")
            } else if *out {
                trf!("Se va en {len}", "Fades out over {len}", "Desaparece em {len}")
            } else {
                trf!("Aparece en {len}", "Fades in over {len}", "Surge em {len}")
            };
            let g = painter.layout_no_wrap(text, FontId::proportional(12.0), WHITE);
            let r = Rect::from_min_size(Pos2::new(h.x + 12.0, h.y - g.size().y - 12.0), g.size() + egui::vec2(12.0, 6.0));
            painter.rect_filled(r, 4.0, BADGE);
            painter.galley(r.min + egui::vec2(6.0, 3.0), g, WHITE);
        }

        // El borde entre las pistas de video y las de audio se marca con el mouse encima (sin fundido) y más
        // claro mientras se arrastra.
        if let Some(r) = split.filter(|_| on_split) {
            let y = rows[nv - 1].1.max + LANE_GAP / 2.0;
            painter.hline(rect.x_range(), y, Stroke::new(2.0_f32, if r.dragged() { TEXT } else { BORDER }));
        }

        // Marcadores: una cinta en la regla y una línea tenue por las pistas.
        for &m in &self.markers {
            let x = tx(m);
            painter.vline(x, Rangef::new(ruler.bottom(), lanes_bottom), Stroke::new(1.0_f32, WHITE.gamma_multiply(0.18)));
            let (top, w) = (ruler.bottom() - 15.0, MARKER_W);
            let pts = vec![Pos2::new(x - w, top), Pos2::new(x + w, top), Pos2::new(x + w, top + 9.0), Pos2::new(x, top + 14.0), Pos2::new(x - w, top + 9.0)];
            painter.add(Shape::convex_polygon(pts, ICON, Stroke::new(1.0_f32, BLACK)));
        }

        // Cursor de reproducción
        let px = tx(self.pos());
        painter.line_segment([Pos2::new(px, ruler.top() + 6.0), Pos2::new(px, lanes_bottom)], Stroke::new(2.0_f32, WHITE));
        painter.rect_filled(Rect::from_center_size(Pos2::new(px, ruler.top() + 8.0), egui::vec2(12.0, 14.0)), 3.0, WHITE);
    }
}
