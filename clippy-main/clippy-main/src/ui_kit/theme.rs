//! Estilo compartido (blanco y negro) y widgets propios para ajustes y editor.
//! Los botones se dibujan a mano para que el texto quede siempre centrado.

use std::sync::OnceLock;

use std::sync::Arc;

use eframe::egui::text::{LayoutJob, TextWrapping};
use eframe::egui::{self, Align, Color32, CursorIcon, FontFamily, FontId, Galley, Layout, Margin, Pos2, Rect, RichText, Sense, Shape, Stroke, TextStyle, Vec2};
use eframe::egui::emath::TSTransform;

use crate::i18n::tr;

pub const BG: Color32 = Color32::from_rgb(10, 10, 10);
pub const CARD: Color32 = Color32::from_rgb(18, 18, 18);
pub const LINE: Color32 = Color32::from_rgb(38, 38, 38);
pub const CONTROL: Color32 = Color32::from_rgb(32, 32, 32);
pub const CONTROL_HOVER: Color32 = Color32::from_rgb(48, 48, 48);
pub const TEXT: Color32 = Color32::from_rgb(245, 245, 245);
pub const MUTED: Color32 = Color32::from_rgb(140, 140, 140);
pub const WHITE: Color32 = Color32::from_rgb(250, 250, 250);
pub const BLACK: Color32 = Color32::from_rgb(10, 10, 10);
pub const DANGER: Color32 = Color32::from_rgb(235, 110, 110);
/// Borde de los controles sin fondo (botón fantasma, tuerca).
/// Fondo del texto seleccionado.
pub const SELECTION: Color32 = Color32::from_rgb(70, 70, 70);
/// Borde de los controles sin fondo (botón fantasma, tuerca).
pub const BORDER: Color32 = Color32::from_rgb(60, 60, 60);
/// Ícono en reposo (se aclara a `TEXT` con el mouse encima).
pub const ICON: Color32 = Color32::from_rgb(210, 210, 210);
/// Botón principal con el mouse encima y apretado.
pub const PRIMARY_HOVER: Color32 = Color32::from_rgb(225, 225, 225);
pub const PRIMARY_PRESSED: Color32 = Color32::from_rgb(200, 200, 200);
/// Capa oscura sobre toda la ventana (soltar archivos, detrás de un diálogo).
pub const SCRIM: Color32 = Color32::from_black_alpha(200);
/// Fondo de una etiqueta encima de una imagen (nombre del clip, duración).
pub const BADGE: Color32 = Color32::from_black_alpha(180);
/// Oscurece una miniatura con el mouse encima.
pub const HOVER_DIM: Color32 = Color32::from_black_alpha(90);
/// Pistas de la línea de tiempo y su forma de onda.
pub const TRACK_VIDEO: Color32 = Color32::from_rgb(28, 28, 28);
pub const TRACK_AUDIO: Color32 = Color32::from_rgb(22, 22, 22);
/// Fondo de una pista de la línea de tiempo (donde no hay clips).
pub const TRACK_LANE: Color32 = Color32::from_rgb(13, 13, 13);
pub const WAVE: Color32 = Color32::from_gray(200);

// --- Movimiento -------------------------------------------------------------------------
// Todo lo que se mueve usa estas duraciones (segundos) y curvas, así no se separan con el tiempo.
// Nada que se dispare con el teclado se anima: se usa decenas de veces y tiene que sentirse inmediato.

/// Respuesta al apretar con el mouse.
pub const T_PRESS: f32 = 0.12;
/// Interruptor que cambia de lado.
pub const T_TOGGLE: f32 = 0.15;
/// Diálogo que aparece.
pub const T_MODAL: f32 = 0.2;
/// Algo que aparece o se va encima de la pantalla (aviso, capa al soltar archivos).
pub const T_FADE: f32 = 0.15;
/// Piezas que se acomodan en su lugar después de soltarlas.
pub const T_SETTLE: f32 = 0.2;
/// Cuánto se achica un botón apretado a fondo (0.97) y uno chico de solo ícono (0.95).
pub const PRESS_DEPTH: f32 = 0.03;
pub const PRESS_DEPTH_ICON: f32 = 0.05;

/// Salida fuerte (parecida a cubic-bezier(0.23, 1, 0.32, 1)): para lo que aparece o responde.
pub fn ease_out(t: f32) -> f32 {
    egui::emath::easing::exponential_out(t)
}

/// Para lo que se mueve en pantalla. Es simétrica: si se invierte a mitad de camino no salta.
pub fn ease_in_out(t: f32) -> f32 {
    egui::emath::easing::cubic_in_out(t)
}

/// Windows con "Efectos de animación" apagado: nada se mueve ni se escala (los fundidos quedan).
pub fn reduced_motion() -> bool {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};
    static R: OnceLock<bool> = OnceLock::new();
    *R.get_or_init(|| {
        let mut on = BOOL(1);
        let ok = unsafe {
            SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut on as *mut BOOL as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)).is_ok()
        };
        ok && !on.as_bool()
    })
}

/// Cuánto está apretado (0 a 1) algo que se toca con el mouse: responde apenas baja el botón.
pub fn press(ui: &egui::Ui, resp: &egui::Response) -> f32 {
    let down = ui.is_enabled() && resp.is_pointer_button_down_on() && !reduced_motion();
    ui.ctx().animate_bool_with_time_and_easing(resp.id.with("press"), down, T_PRESS, ease_out)
}

/// Dibuja las formas achicadas hacia `center` según `pressed` (de `press`) y la profundidad.
pub fn paint_pressed(ui: &egui::Ui, center: Pos2, pressed: f32, depth: f32, mut shapes: Vec<Shape>) {
    let s = 1.0 - depth * pressed;
    if s < 1.0 {
        let t = scale_about(center, s);
        for sh in &mut shapes {
            sh.transform(t);
        }
    }
    ui.painter().extend(shapes);
}

/// Escala `s` alrededor del punto `c`.
pub fn scale_about(c: Pos2, s: f32) -> TSTransform {
    TSTransform::from_translation(c.to_vec2()) * TSTransform::from_scaling(s) * TSTransform::from_translation(-c.to_vec2())
}

pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

pub fn setup_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let mut bold = Vec::new();
    if let Ok(b) = std::fs::read("C:\\Windows\\Fonts\\seguisb.ttf") {
        fonts.font_data.insert("segoe-sb".into(), egui::FontData::from_owned(b).tweak(segoe_tweak()));
        bold.push("segoe-sb".to_string());
    }
    if let Ok(b) = std::fs::read("C:\\Windows\\Fonts\\segoeui.ttf") {
        fonts.font_data.insert("segoe".into(), egui::FontData::from_owned(b).tweak(segoe_tweak()));
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "segoe".into());
    }
    bold.extend(fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
    fonts.families.insert(self::bold(), bold);
    ctx.set_fonts(fonts);

    let mut style = (*ctx.style()).clone();
    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.window_stroke = Stroke::new(1.0_f32, LINE);
    v.extreme_bg_color = CONTROL;
    v.faint_bg_color = CARD;
    // Texto seleccionado: fondo gris y la letra sigue clara (egui no cambia el color de la letra,
    // así que un fondo blanco la volvía invisible).
    v.selection.bg_fill = SELECTION;
    v.selection.stroke = Stroke::new(1.0_f32, TEXT);
    v.slider_trailing_fill = true;
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, LINE);
    for (w, fill) in [
        (&mut v.widgets.inactive, CONTROL),
        (&mut v.widgets.hovered, CONTROL_HOVER),
        (&mut v.widgets.active, CONTROL_HOVER),
        (&mut v.widgets.open, CONTROL_HOVER),
    ] {
        w.rounding = 6.0.into();
        w.bg_stroke = Stroke::NONE;
        w.weak_bg_fill = fill;
        w.bg_fill = fill;
        w.fg_stroke = Stroke::new(1.5_f32, TEXT);
        w.expansion = 0.0;
    }
    style.spacing.slider_width = 160.0;
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size.y = 30.0;
    style.spacing.scroll.floating = true;
    style.text_styles.insert(TextStyle::Body, FontId::proportional(14.5));
    style.text_styles.insert(TextStyle::Button, button_font());
    style.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
    ctx.set_style(style);
    // Ctrl+más/menos/0 son atajos del editor (zoom de la línea de tiempo): que egui no agrande toda la interfaz.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
}

/// Segoe UI dibuja las letras un poco bajas dentro de su línea: subirlas un poco para que el texto
/// quede centrado en botones, selectores y campos.
fn segoe_tweak() -> egui::FontTweak {
    egui::FontTweak { y_offset_factor: -0.11, ..Default::default() }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// Acción principal: blanco con texto negro.
    Primary,
    /// Acción secundaria: gris oscuro.
    Secondary,
    /// Opción elegida en un grupo.
    Selected,
    /// Sin fondo hasta pasar el mouse.
    Ghost,
    /// Acción destructiva (salir, borrar): rojo.
    Danger,
}

fn colors(kind: Kind, enabled: bool, hovered: bool, pressed: bool) -> (Color32, Color32) {
    if !enabled {
        return match kind {
            Kind::Primary | Kind::Selected => (Color32::from_rgb(58, 58, 58), Color32::from_rgb(120, 120, 120)),
            _ => (if kind == Kind::Ghost { Color32::TRANSPARENT } else { Color32::from_rgb(24, 24, 24) }, Color32::from_rgb(90, 90, 90)),
        };
    }
    match kind {
        Kind::Primary | Kind::Selected => (
            if pressed {
                PRIMARY_PRESSED
            } else if hovered {
                PRIMARY_HOVER
            } else {
                WHITE
            },
            BLACK,
        ),
        Kind::Secondary => (if pressed { Color32::from_rgb(40, 40, 40) } else if hovered { CONTROL_HOVER } else { CONTROL }, TEXT),
        Kind::Ghost => (if pressed { CONTROL } else if hovered { CONTROL_HOVER } else { Color32::TRANSPARENT }, TEXT),
        Kind::Danger => (
            if pressed {
                Color32::from_rgb(120, 30, 30)
            } else if hovered {
                Color32::from_rgb(70, 24, 24)
            } else {
                Color32::TRANSPARENT
            },
            Color32::from_rgb(255, 120, 120),
        ),
    }
}

/// Íconos de línea para poner delante del texto de un botón.
#[derive(Clone, Copy)]
pub enum Glyph {
    /// Flecha hacia abajo sobre una bandeja (bajar una versión nueva).
    Download,
    /// Flecha hacia arriba desde una bandeja (exportar el video).
    Export,
    /// Flechas curvas de deshacer y rehacer.
    Undo,
    Redo,
    /// Tijera (dividir el clip).
    Scissors,
    /// Tacho (eliminar el clip).
    Trash,
    /// Parlante (sonido del clip).
    Speaker,
    /// Carpeta (abrir una carpeta en el Explorador).
    Folder,
    /// Cuadro de video con el triángulo de reproducir (el editor).
    Film,
    /// Más y menos (agregar videos, zoom).
    Plus,
    Minus,
    /// Ojo (ver u ocultar una pista).
    Eye,
    /// Imán (los clips se pegan solos).
    Magnet,
    /// Cuadro de video sobre una onda (separar o unir el audio y el video).
    Detach,
    /// Flecha en círculo (girar la imagen).
    Rotate,
    /// Dos triángulos enfrentados (espejar la imagen).
    Flip,
    /// Dos escuadras cruzadas (recortar la imagen).
    Crop,
    /// Copo de nieve (congelar un cuadro).
    Freeze,
    /// Marcador (una cinta con punta).
    Marker,
    /// Carpeta con un más (carpeta nueva).
    NewFolder,
    /// Flecha hacia la izquierda (volver).
    Back,
    /// Cuatro esquinas hacia afuera (pantalla completa) y hacia adentro (salir de pantalla completa).
    Fullscreen,
    ExitFullscreen,
}

/// Ancho del ícono y espacio hasta el texto.
const GLYPH_W: f32 = 14.0;
const GLYPH_GAP: f32 = 8.0;

fn glyph_shapes(g: Glyph, c: Pos2, col: Color32) -> Vec<Shape> {
    let s = GLYPH_W / 2.0;
    let st = Stroke::new(1.6_f32, col);
    let p = |x: f32, y: f32| c + egui::vec2(x * s, y * s);
    let tray = || Shape::line(vec![p(-1.0, 0.45), p(-1.0, 1.0), p(1.0, 1.0), p(1.0, 0.45)], st);
    // Flecha con gancho; `d` = 1 apunta a la izquierda (deshacer), -1 a la derecha (rehacer).
    let hook = |d: f32| {
        vec![
            Shape::line(vec![p(-0.35 * d, -0.95), p(-0.95 * d, -0.35), p(-0.35 * d, 0.25)], st),
            Shape::line(
                vec![p(-0.95 * d, -0.35), p(0.3 * d, -0.35), p(0.72 * d, -0.17), p(0.9 * d, 0.25), p(0.72 * d, 0.67), p(0.3 * d, 0.85), p(-0.4 * d, 0.85)],
                st,
            ),
        ]
    };
    match g {
        Glyph::Download => vec![
            Shape::line_segment([p(0.0, -1.0), p(0.0, 0.35)], st),
            Shape::line(vec![p(-0.55, -0.2), p(0.0, 0.35), p(0.55, -0.2)], st),
            tray(),
        ],
        Glyph::Export => vec![
            Shape::line_segment([p(0.0, 0.35), p(0.0, -1.0)], st),
            Shape::line(vec![p(-0.55, -0.45), p(0.0, -1.0), p(0.55, -0.45)], st),
            tray(),
        ],
        Glyph::Undo => hook(1.0),
        Glyph::Redo => hook(-1.0),
        Glyph::Scissors => vec![
            Shape::circle_stroke(p(-0.62, -0.62), 0.3 * s, st),
            Shape::circle_stroke(p(-0.62, 0.62), 0.3 * s, st),
            Shape::line_segment([p(-0.38, -0.44), p(1.0, 0.7)], st),
            Shape::line_segment([p(-0.38, 0.44), p(1.0, -0.7)], st),
        ],
        Glyph::Trash => vec![
            Shape::line_segment([p(-0.95, -0.6), p(0.95, -0.6)], st),
            Shape::line(vec![p(-0.32, -0.6), p(-0.32, -1.0), p(0.32, -1.0), p(0.32, -0.6)], st),
            Shape::line(vec![p(-0.68, -0.6), p(-0.58, 1.0), p(0.58, 1.0), p(0.68, -0.6)], st),
            Shape::line_segment([p(-0.2, -0.2), p(-0.2, 0.6)], st),
            Shape::line_segment([p(0.2, -0.2), p(0.2, 0.6)], st),
        ],
        Glyph::Speaker => vec![
            Shape::closed_line(vec![p(-1.0, -0.32), p(-0.55, -0.32), p(0.0, -0.82), p(0.0, 0.82), p(-0.55, 0.32), p(-1.0, 0.32)], st),
            Shape::line(vec![p(0.42, -0.42), p(0.62, 0.0), p(0.42, 0.42)], st),
            Shape::line(vec![p(0.75, -0.78), p(1.05, 0.0), p(0.75, 0.78)], st),
        ],
        Glyph::Folder => vec![Shape::closed_line(vec![p(-1.0, -0.75), p(-0.3, -0.75), p(-0.02, -0.42), p(1.0, -0.42), p(1.0, 0.8), p(-1.0, 0.8)], st)],
        Glyph::Film => vec![
            Shape::rect_stroke(Rect::from_min_max(p(-1.0, -0.78), p(1.0, 0.78)), 2.0, st),
            Shape::closed_line(vec![p(-0.28, -0.38), p(0.4, 0.0), p(-0.28, 0.38)], st),
        ],
        Glyph::Plus => vec![Shape::line_segment([p(-0.75, 0.0), p(0.75, 0.0)], st), Shape::line_segment([p(0.0, -0.75), p(0.0, 0.75)], st)],
        Glyph::Minus => vec![Shape::line_segment([p(-0.75, 0.0), p(0.75, 0.0)], st)],
        Glyph::Eye => vec![
            Shape::closed_line(vec![p(-1.0, 0.0), p(-0.55, -0.5), p(0.0, -0.68), p(0.55, -0.5), p(1.0, 0.0), p(0.55, 0.5), p(0.0, 0.68), p(-0.55, 0.5)], st),
            Shape::circle_stroke(p(0.0, 0.0), 0.28 * s, st),
        ],
        Glyph::Magnet => vec![
            Shape::line(vec![p(-0.75, -0.95), p(-0.75, 0.15), p(-0.6, 0.6), p(-0.25, 0.88), p(0.25, 0.88), p(0.6, 0.6), p(0.75, 0.15), p(0.75, -0.95)], st),
            Shape::line_segment([p(-1.0, -0.45), p(-0.5, -0.45)], st),
            Shape::line_segment([p(0.5, -0.45), p(1.0, -0.45)], st),
        ],
        Glyph::Detach => vec![
            Shape::rect_stroke(Rect::from_min_max(p(-1.0, -0.95), p(1.0, -0.15)), 1.5, st),
            Shape::line(vec![p(-1.0, 0.6), p(-0.6, 0.3), p(-0.2, 0.9), p(0.2, 0.3), p(0.6, 0.9), p(1.0, 0.6)], st),
        ],
        Glyph::Rotate => {
            // Arco en el sentido del reloj que termina en punta.
            let at = |a: f32| p(0.85 * a.cos(), 0.85 * a.sin());
            let (a0, a1) = (150f32.to_radians(), 400f32.to_radians());
            let arc: Vec<Pos2> = (0..=12).map(|k| at(a0 + (a1 - a0) * k as f32 / 12.0)).collect();
            let (end, dir, out) = (at(a1), egui::vec2(-a1.sin(), a1.cos()), egui::vec2(a1.cos(), a1.sin()));
            vec![Shape::line(arc, st), Shape::line(vec![end - (dir * 0.5 - out * 0.35) * s, end, end - (dir * 0.5 + out * 0.35) * s], st)]
        }
        Glyph::Flip => vec![
            Shape::closed_line(vec![p(-0.25, -0.7), p(-1.0, 0.7), p(-0.25, 0.7)], st),
            Shape::closed_line(vec![p(0.25, -0.7), p(1.0, 0.7), p(0.25, 0.7)], st),
            Shape::line_segment([p(0.0, -1.0), p(0.0, -0.6)], st),
            Shape::line_segment([p(0.0, -0.2), p(0.0, 0.2)], st),
            Shape::line_segment([p(0.0, 0.6), p(0.0, 1.0)], st),
        ],
        Glyph::Crop => vec![
            Shape::line(vec![p(-0.55, -1.0), p(-0.55, 0.55), p(1.0, 0.55)], st),
            Shape::line(vec![p(-1.0, -0.55), p(0.55, -0.55), p(0.55, 1.0)], st),
        ],
        Glyph::Freeze => (0..3)
            .flat_map(|k| {
                let a = k as f32 * std::f32::consts::PI / 3.0 + std::f32::consts::FRAC_PI_2;
                let d = egui::vec2(a.cos(), a.sin());
                let n = egui::vec2(-d.y, d.x);
                let tip = |e: f32| [c + d * e * 0.6 * s - n * 0.25 * s, c + d * e * 0.85 * s, c + d * e * 0.6 * s + n * 0.25 * s];
                [Shape::line_segment([c - d * s, c + d * s], st), Shape::line(tip(1.0).to_vec(), st), Shape::line(tip(-1.0).to_vec(), st)]
            })
            .collect(),
        Glyph::Marker => vec![Shape::closed_line(vec![p(-0.65, -0.95), p(0.65, -0.95), p(0.65, 0.35), p(0.0, 0.95), p(-0.65, 0.35)], st)],
        Glyph::NewFolder => vec![
            Shape::closed_line(vec![p(-1.0, -0.75), p(-0.3, -0.75), p(-0.02, -0.42), p(1.0, -0.42), p(1.0, 0.8), p(-1.0, 0.8)], st),
            Shape::line_segment([p(-0.35, 0.2), p(0.35, 0.2)], st),
            Shape::line_segment([p(0.0, -0.15), p(0.0, 0.55)], st),
        ],
        Glyph::Back => vec![Shape::line_segment([p(0.85, 0.0), p(-0.85, 0.0)], st), Shape::line(vec![p(-0.2, -0.65), p(-0.85, 0.0), p(-0.2, 0.65)], st)],
        Glyph::Fullscreen | Glyph::ExitFullscreen => {
            // La punta de cada esquina: en el borde (afuera) o cerca del centro (adentro).
            let k = if matches!(g, Glyph::Fullscreen) { 1.0 } else { 0.35 };
            [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
                .into_iter()
                .map(|(x, y)| Shape::line(vec![p(x, y * 0.35), p(x * k, y * k), p(x * 0.35, y)], st))
                .collect()
        }
    }
}

/// Un ícono suelto de `size` px centrado en `center` (por ejemplo, en una tarjeta).
pub fn glyph(g: Glyph, center: Pos2, size: f32, col: Color32) -> Vec<Shape> {
    let mut v = glyph_shapes(g, center, col);
    let t = scale_about(center, size / GLYPH_W);
    for s in &mut v {
        s.transform(t);
    }
    v
}

/// Botón chico de solo ícono para algo que se prende y se apaga (ver una pista, su sonido).
/// Apagado, el ícono se ve tachado y más tenue. `tip` dice qué hace.
pub fn icon_toggle(ui: &mut egui::Ui, glyph: Glyph, on: bool, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let fg = if !on { MUTED } else if resp.hovered() { TEXT } else { ICON };
        let mut shapes = Vec::new();
        if resp.hovered() {
            shapes.push(Shape::rect_filled(rect, 5.0, CONTROL_HOVER));
        }
        shapes.extend(glyph_shapes(glyph, rect.center(), fg));
        if !on {
            let d = egui::vec2(GLYPH_W / 2.0 + 1.0, GLYPH_W / 2.0 + 1.0);
            shapes.push(Shape::line_segment([rect.center() - d, rect.center() + d], Stroke::new(1.6_f32, fg)));
        }
        paint_pressed(ui, rect.center(), press(ui, &resp), PRESS_DEPTH_ICON, shapes);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip)
}

/// Botón con un ícono delante del texto; los dos quedan centrados juntos.
pub fn icon_button(ui: &mut egui::Ui, glyph: Glyph, text: &str, kind: Kind, size: Vec2) -> egui::Response {
    button_impl(ui, Some(glyph), text, kind, size)
}

/// Botón cuadrado de solo ícono, para acciones que se reconocen sin texto (deshacer, zoom).
/// `tip` dice qué hace (y su atajo): se ve al pasar el mouse, también con el botón apagado.
pub fn icon_only_button(ui: &mut egui::Ui, glyph: Glyph, kind: Kind, tip: &str) -> egui::Response {
    button_impl(ui, Some(glyph), "", kind, Vec2::splat(32.0)).on_hover_text(tip).on_disabled_hover_text(tip)
}

/// Botón con el texto siempre centrado. `size.x == 0` ajusta el ancho al texto.
pub fn button_sized(ui: &mut egui::Ui, text: &str, kind: Kind, size: Vec2) -> egui::Response {
    button_impl(ui, None, text, kind, size)
}

/// Letra de los botones.
fn button_font() -> FontId {
    FontId::proportional(14.0)
}

/// Ancho del texto de un botón.
fn text_width(ui: &egui::Ui, text: &str) -> f32 {
    ui.painter().layout_no_wrap(text.to_owned(), button_font(), Color32::PLACEHOLDER).size().x
}

fn button_impl(ui: &mut egui::Ui, glyph: Option<Glyph>, text: &str, kind: Kind, size: Vec2) -> egui::Response {
    let enabled = ui.is_enabled();
    let galley = ui.painter().layout_no_wrap(text.to_owned(), button_font(), Color32::PLACEHOLDER);
    let icon_w = match glyph {
        Some(_) if text.is_empty() => GLYPH_W,
        Some(_) => GLYPH_W + GLYPH_GAP,
        None => 0.0,
    };
    let w = if size.x > 0.0 { size.x } else { galley.size().x + icon_w + 28.0 };
    let h = if size.y > 0.0 { size.y } else { 32.0 };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), if enabled { Sense::click() } else { Sense::hover() });
    if ui.is_rect_visible(rect) {
        let (fill, fg) = colors(kind, enabled, resp.hovered(), resp.is_pointer_button_down_on());
        let mut shapes = vec![Shape::rect_filled(rect, 6.0, fill)];
        if kind == Kind::Danger && enabled {
            shapes.push(Shape::rect_stroke(rect.shrink(0.5), 6.0, Stroke::new(1.0_f32, Color32::from_rgb(110, 40, 40))));
        }
        if kind == Kind::Ghost {
            // Borde fino: que se vea como un botón aunque no tenga fondo.
            let c = if enabled { BORDER } else { LINE };
            shapes.push(Shape::rect_stroke(rect.shrink(0.5), 6.0, Stroke::new(1.0_f32, c)));
        }
        // Ícono y texto se centran como un solo bloque.
        let left = rect.center().x - (icon_w + galley.size().x) / 2.0;
        if let Some(g) = glyph {
            shapes.extend(glyph_shapes(g, Pos2::new(left + GLYPH_W / 2.0, rect.center().y), fg));
        }
        let pos = Pos2::new(left + icon_w, rect.center().y - galley.size().y / 2.0);
        shapes.push(Shape::galley(ui.painter().round_pos_to_pixels(pos), galley, fg));
        paint_pressed(ui, rect.center(), press(ui, &resp), PRESS_DEPTH, shapes);
    }
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

pub fn button(ui: &mut egui::Ui, text: &str, kind: Kind) -> egui::Response {
    button_sized(ui, text, kind, Vec2::ZERO)
}

/// Botón que ocupa todo el ancho disponible.
pub fn wide_button(ui: &mut egui::Ui, text: &str, kind: Kind, h: f32) -> egui::Response {
    let w = ui.available_width();
    button_sized(ui, text, kind, egui::vec2(w, h))
}

/// Grupo de opciones (una sola elegida) que llena el ancho: del mismo ancho cada una si así entran los textos; si no,
/// cada una mide su texto y lo que sobra se reparte. Si ni así entran en una línea, siguen en otra (cada línea llena el
/// ancho) en vez de cortar el texto.
pub fn seg<T: PartialEq + Copy>(ui: &mut egui::Ui, v: &mut T, opts: &[(T, &str)]) -> bool {
    const GAP: f32 = 4.0;
    // Aire mínimo a los costados del texto (sumando los dos lados).
    const PAD: f32 = 20.0;
    let width = ui.available_width();
    let need: Vec<f32> = opts.iter().map(|(_, l)| text_width(ui, l) + PAD).collect();
    // Cuántas opciones van en cada línea: las que entran, de izquierda a derecha.
    let mut lines = vec![0usize];
    let mut used = 0.0;
    for &w in &need {
        let n = lines.last_mut().unwrap();
        if *n > 0 && used + GAP + w > width {
            lines.push(0);
            used = 0.0;
        }
        used += if used > 0.0 { GAP } else { 0.0 } + w;
        *lines.last_mut().unwrap() += 1;
    }
    let mut changed = false;
    let mut first = 0;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(GAP, GAP);
        for n in lines {
            let line = first..first + n;
            first += n;
            ui.horizontal(|ui| {
                let free = width - GAP * (n as f32 - 1.0);
                let equal = free / n as f32;
                let spare = ((free - need[line.clone()].iter().sum::<f32>()) / n as f32).max(0.0);
                let fits = need[line.clone()].iter().all(|&w| w <= equal);
                for ((val, label), need) in opts[line.clone()].iter().zip(&need[line]) {
                    let w = if fits { equal } else { need + spare }.floor();
                    let kind = if *v == *val { Kind::Selected } else { Kind::Secondary };
                    if button_sized(ui, label, kind, egui::vec2(w, 32.0)).clicked() && *v != *val {
                        *v = *val;
                        changed = true;
                    }
                }
            });
        }
    });
    changed
}

pub fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    caption(ui, title);
    card(ui, add);
    ui.add_space(14.0);
}

pub fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, LINE))
        .rounding(10.0)
        .inner_margin(Margin::symmetric(18.0, 14.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            add(ui);
        });
}

/// Tarjeta destacada (lo principal de una página): un poco más clara y con más aire que `card`.
pub fn hero_card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(Color32::from_rgb(22, 22, 22))
        .stroke(Stroke::new(1.0_f32, Color32::from_rgb(60, 60, 60)))
        .rounding(12.0)
        .inner_margin(Margin::symmetric(24.0, 20.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

pub fn caption(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title.to_uppercase()).size(11.5).color(MUTED).extra_letter_spacing(1.2));
    ui.add_space(2.0);
}

pub fn divider(ui: &mut egui::Ui) {
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(egui::vec2(w, 1.0), Sense::hover());
    ui.painter().hline(r.x_range(), r.center().y, Stroke::new(1.0_f32, LINE));
}

/// Fila "etiqueta | control" con la etiqueta en una columna fija.
pub fn row(ui: &mut egui::Ui, label: &str, label_w: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.set_min_height(34.0);
        ui.allocate_ui_with_layout(egui::vec2(label_w, 34.0), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_width(label_w);
            ui.label(RichText::new(label).color(TEXT));
        });
        add(ui);
    });
}

/// Texto secundario debajo de una fila, alineado con la columna de controles.
pub fn hint(ui: &mut egui::Ui, indent: f32, text: impl Into<String>) {
    ui.horizontal(|ui| {
        ui.add_space(indent);
        hint_label(ui, text);
    });
}

/// Texto secundario donde va (por ejemplo, al lado de un interruptor).
pub fn hint_label(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.add(egui::Label::new(RichText::new(text.into()).size(12.5).color(MUTED)).wrap());
}

pub fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(egui::vec2(42.0, 24.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let last_key = resp.id.with("last");
    let last = ui.data(|d| d.get_temp::<bool>(last_key));
    ui.data_mut(|d| d.insert_temp(last_key, *on));
    // Solo el clic del mouse anima: Espacio/Enter sobre el interruptor también cuentan como clic en egui.
    let external = last.is_some_and(|l| l != *on) && !resp.clicked_by(egui::PointerButton::Primary);
    let time = if external || reduced_motion() { 0.0 } else { T_TOGGLE };
    let t = ui.ctx().animate_bool_with_time_and_easing(resp.id, *on, time, ease_in_out);
    let r = rect.height() / 2.0;
    ui.painter().rect_filled(rect, r, mix(CONTROL_HOVER, WHITE, t));
    let x = egui::lerp((rect.left() + r)..=(rect.right() - r), t);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), r - 4.0, mix(MUTED, BLACK, t));
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// 100 ns -> "1:05.25" (o "1:02:05.25" desde una hora)
pub fn fmt_time(t: i64) -> String {
    let cs = (t.max(0) / 100_000) as u64; // centésimas
    format!("{}.{:02}", fmt_clock(cs / 100), cs % 100)
}

/// Segundos -> "1:05" (o "1:02:05" desde una hora)
pub fn fmt_clock(s: u64) -> String {
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Bytes -> "850 MB" o "2.05 GB"
pub fn fmt_bytes(b: f64) -> String {
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else {
        format!("{:.0} MB", (b / 1e6).max(1.0))
    }
}

/// 1284 -> "1.284" (en inglés, "1,284")
pub fn fmt_count(n: usize) -> String {
    let d = n.to_string();
    let sep = tr!('.', ',', '.');
    let mut out = String::new();
    for (i, c) in d.chars().enumerate() {
        if i > 0 && (d.len() - i).is_multiple_of(3) {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

/// Segundos -> "45 s", "2 min", "2 min 30 s"
pub fn fmt_secs(s: u32) -> String {
    match (s / 60, s % 60) {
        (0, s) => format!("{s} s"),
        (m, 0) => format!("{m} min"),
        (m, s) => format!("{m} min {s} s"),
    }
}

/// Contenedor centrado de ancho máximo (para ventanas maximizadas).
pub fn centered(ui: &mut egui::Ui, max_w: f32, add: impl FnOnce(&mut egui::Ui)) {
    let avail = ui.available_width();
    let w = avail.min(max_w);
    let pad = ((avail - w) / 2.0).max(0.0);
    ui.horizontal_top(|ui| {
        ui.add_space(pad);
        ui.vertical(|ui| {
            ui.set_width(w);
            add(ui);
        });
    });
}

/// Diálogo modal: encabezado (título, descripción y X), cuerpo y pie con botones.
/// Dentro de `add` usar `modal_body` y `modal_footer`. Devuelve true si se tocó la X o Esc.
pub fn modal(ctx: &egui::Context, id: &str, title: &str, subtitle: &str, width: f32, add: impl FnOnce(&mut egui::Ui)) -> bool {
    let screen = ctx.screen_rect();
    let win_id = egui::Id::new((id, "win"));
    let e = modal_entrance(ctx, win_id);
    egui::Area::new(egui::Id::new((id, "dim")))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .fade_in(false)
        .show(ctx, |ui| {
            ui.painter().rect_filled(screen, 0.0, SCRIM.gamma_multiply(e));
            ui.allocate_response(screen.size(), Sense::click());
        });
    let mut close = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    let scale = if reduced_motion() { 1.0 } else { 0.97 + 0.03 * e };
    ctx.set_transform_layer(egui::LayerId::new(egui::Order::Foreground, win_id), if scale < 1.0 { scale_about(screen.center(), scale) } else { TSTransform::IDENTITY });
    egui::Area::new(win_id)
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .fade_in(false)
        .show(ctx, |ui| {
            ui.multiply_opacity(e);
            egui::Frame::none()
                .fill(CARD)
                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(50, 50, 50)))
                .rounding(14.0)
                .shadow(egui::epaint::Shadow { offset: egui::vec2(0.0, 12.0), blur: 40.0, spread: 0.0, color: Color32::from_black_alpha(160) })
                .show(ui, |ui| {
                    ui.set_width(width);
                    ui.spacing_mut().item_spacing = egui::vec2(8.0, 0.0);
                    // Encabezado
                    egui::Frame::none().inner_margin(Margin { left: 26.0, right: 18.0, top: 20.0, bottom: 18.0 }).show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 4.0;
                                ui.label(RichText::new(title).family(bold()).size(19.0).color(TEXT));
                                if !subtitle.is_empty() {
                                    ui.label(RichText::new(subtitle).color(MUTED));
                                }
                            });
                            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                if close_button(ui).clicked() {
                                    close = true;
                                }
                            });
                        });
                    });
                    full_line(ui);
                    add(ui);
                });
        });
    close
}

/// Progreso (0 a 1, ya con curva) de la entrada del diálogo. Empieza de nuevo cada vez que se abre
/// (un cuadro sin dibujarlo cuenta como cerrado). Si se abrió con una tecla (F1, Ctrl+E, Alt+F4)
/// aparece de una: lo que se dispara con el teclado no se anima.
fn modal_entrance(ctx: &egui::Context, win_id: egui::Id) -> f32 {
    let key = win_id.with("entrance");
    // Leer la entrada antes de `data_mut`: dentro de esa clausura el contexto ya está bloqueado.
    let (now, by_key) = ctx.input(|i| (i.time, i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))));
    let pass = ctx.cumulative_pass_nr();
    let (t0, instant) = ctx.data_mut(|d| {
        let prev = d.get_temp::<(f64, bool, u64)>(key);
        let (t0, instant) = match prev {
            Some((t0, instant, last)) if last + 1 >= pass => (t0, instant),
            _ => (now, by_key),
        };
        d.insert_temp(key, (t0, instant, pass));
        (t0, instant)
    });
    if instant {
        return 1.0;
    }
    let p = (((now - t0) as f32) / T_MODAL).clamp(0.0, 1.0);
    if p < 1.0 {
        ctx.request_repaint();
    }
    ease_out(p)
}

/// Cuerpo del modal (con márgenes y separación entre filas).
pub fn modal_body(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none().inner_margin(Margin::symmetric(26.0, 20.0)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 10.0);
        add(ui);
    });
}

/// Pie del modal: fondo apenas distinto y botones.
pub fn modal_footer(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    full_line(ui);
    egui::Frame::none()
        .fill(Color32::from_rgb(14, 14, 14))
        .rounding(egui::Rounding { nw: 0.0, ne: 0.0, sw: 14.0, se: 14.0 })
        .inner_margin(Margin::symmetric(26.0, 16.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                add(ui);
            });
        });
}

/// Caja destacada dentro de un modal (resúmenes, avisos).
pub fn note_box(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(BG)
        .stroke(Stroke::new(1.0_f32, LINE))
        .rounding(10.0)
        .inner_margin(Margin::symmetric(16.0, 14.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            add(ui);
        });
}

fn full_line(ui: &mut egui::Ui) {
    let r = ui.max_rect();
    let (line, _) = ui.allocate_exact_size(egui::vec2(r.width(), 1.0), Sense::hover());
    ui.painter().hline(r.x_range(), line.center().y, Stroke::new(1.0_f32, LINE));
}

fn close_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(32.0, 32.0), Sense::click());
    let mut shapes = Vec::new();
    if resp.hovered() {
        shapes.push(Shape::rect_filled(rect, 6.0, CONTROL_HOVER));
    }
    let c = rect.center();
    let s = 5.5;
    let stroke = Stroke::new(1.6_f32, if resp.hovered() { TEXT } else { MUTED });
    shapes.push(Shape::line_segment([c + egui::vec2(-s, -s), c + egui::vec2(s, s)], stroke));
    shapes.push(Shape::line_segment([c + egui::vec2(-s, s), c + egui::vec2(s, -s)], stroke));
    paint_pressed(ui, c, press(ui, &resp), PRESS_DEPTH_ICON, shapes);
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tr!("Cerrar (Esc)", "Close (Esc)", "Fechar (Esc)"))
}

/// Ítem de navegación lateral (texto a la izquierda, resaltado si está activo).
pub fn nav_item(ui: &mut egui::Ui, text: &str, active: bool) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 38.0), Sense::click());
    let fill = if active {
        CONTROL_HOVER
    } else if resp.hovered() {
        CONTROL
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 7.0, fill);
    if active {
        let bar = egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 9.0), egui::vec2(3.0, rect.height() - 18.0));
        ui.painter().rect_filled(bar, 1.5, WHITE);
    }
    let color = if active { TEXT } else { MUTED };
    let galley = ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(14.5), color);
    let pos = egui::pos2(rect.left() + 16.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(ui.painter().round_pos_to_pixels(pos), galley, color);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Tecla física dibujada (como en la web): muestra el atajo y se toca para cambiarlo.
/// Al apretarla la cara baja hasta el borde, como una tecla de verdad.
/// `waiting` = esperando que el usuario apriete la tecla nueva.
pub fn keycap(ui: &mut egui::Ui, text: &str, waiting: bool) -> egui::Response {
    const H: f32 = 76.0;
    const LIP: f32 = 5.0;
    // El nombre puede ser largo ("Ctrl + Shift + F12"): achicar la letra para que entre.
    let max_w = 280.0;
    let mut size = 26.0;
    let mut galley = ui.painter().layout_no_wrap(text.to_owned(), FontId::new(size, bold()), TEXT);
    while galley.size().x > max_w - 36.0 && size > 13.0 {
        size -= 1.0;
        galley = ui.painter().layout_no_wrap(text.to_owned(), FontId::new(size, bold()), TEXT);
    }
    let w = (galley.size().x + 44.0).clamp(96.0, max_w);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, H), Sense::click());
    if ui.is_rect_visible(rect) {
        let t = press(ui, &resp);
        let hover = if resp.hovered() || waiting { 1.0 } else { 0.0 };
        let p = ui.painter();
        let body = egui::Rect::from_min_max(rect.min + egui::vec2(0.0, LIP), rect.max);
        p.rect_filled(body, 12.0, Color32::from_rgb(20, 20, 20));
        let face = egui::Rect::from_min_size(rect.min + egui::vec2(0.0, LIP * t), egui::vec2(w, H - LIP));
        p.rect_filled(face, 12.0, mix(Color32::from_rgb(34, 34, 34), Color32::from_rgb(42, 42, 42), hover));
        let edge = if waiting { WHITE } else { mix(Color32::from_rgb(58, 58, 58), Color32::from_rgb(90, 90, 90), hover) };
        p.rect_stroke(face.shrink(0.5), 12.0, Stroke::new(if waiting { 1.5_f32 } else { 1.0 }, edge));
        let col = if waiting { MUTED } else { TEXT };
        // Etiqueta abajo a la izquierda, como en un teclado.
        let pos = egui::pos2(face.left() + 18.0, face.bottom() - 14.0 - galley.size().y);
        p.galley(p.round_pos_to_pixels(pos), galley, col);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Mini logo (azulejo negro con el símbolo de grabación).
pub fn app_mark(ui: &mut egui::Ui, size: f32) {
    let (r, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let p = ui.painter();
    p.rect_filled(r, size * 0.22, Color32::from_rgb(28, 28, 28));
    p.circle_stroke(r.center(), size * 0.28, Stroke::new(size * 0.07, WHITE));
    p.circle_filled(r.center(), size * 0.13, WHITE);
}

/// Una línea de texto (toma el color al dibujarla), cortada con "…" si pasa de `max_w`.
pub fn one_line(painter: &egui::Painter, text: String, font: FontId, max_w: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text, font, Color32::PLACEHOLDER);
    job.wrap = TextWrapping::truncate_at_width(max_w);
    painter.layout_job(job)
}

/// Botón de tamaño fijo: si el texto no entra se corta con "…" y el texto entero aparece al pasar el mouse.
pub fn button_fit(ui: &mut egui::Ui, glyph: Option<Glyph>, text: &str, kind: Kind, size: Vec2) -> egui::Response {
    let font = TextStyle::Button.resolve(ui.style());
    let icon_w = if glyph.is_some() { GLYPH_W + GLYPH_GAP } else { 0.0 };
    let g = one_line(ui.painter(), text.to_owned(), font, size.x - icon_w - 2.0 * ui.spacing().button_padding.x);
    if !g.elided {
        return button_impl(ui, glyph, text, kind, size);
    }
    let shown: String = g.rows.iter().flat_map(|r| r.glyphs.iter().map(|g| g.chr)).collect();
    button_impl(ui, glyph, &shown, kind, size).on_hover_text(text)
}

/// Botón con una tuerca dibujada (ajustes del editor).
pub fn gear_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(36.0), Sense::click());
    let c = rect.center();
    let col = if resp.hovered() { TEXT } else { ICON };
    let mut shapes = vec![
        Shape::rect_filled(rect, 6.0, if resp.hovered() { CONTROL_HOVER } else { Color32::TRANSPARENT }),
        Shape::rect_stroke(rect.shrink(0.5), 6.0, Stroke::new(1.0_f32, BORDER)),
    ];
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::TAU / 8.0;
        let d = egui::vec2(a.cos(), a.sin());
        shapes.push(Shape::line_segment([c + d * 6.0, c + d * 9.5], Stroke::new(3.2_f32, col)));
    }
    shapes.push(Shape::circle_stroke(c, 6.0, Stroke::new(2.4_f32, col)));
    shapes.push(Shape::circle_filled(c, 2.2, col));
    paint_pressed(ui, c, press(ui, &resp), PRESS_DEPTH_ICON, shapes);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Botón redondo de reproducir / pausar.
pub fn play_button(ui: &mut egui::Ui, playing: bool, size: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), if ui.is_enabled() { Sense::click() } else { Sense::hover() });
    let fill = if !ui.is_enabled() {
        CONTROL_HOVER
    } else if resp.hovered() {
        PRIMARY_HOVER
    } else {
        WHITE
    };
    let c = rect.center();
    let mut shapes = vec![Shape::circle_filled(c, size / 2.0, fill)];
    let k = size / 40.0;
    if playing {
        shapes.push(Shape::rect_filled(Rect::from_center_size(c - egui::vec2(5.0 * k, 0.0), egui::vec2(4.5 * k, 15.0 * k)), 1.0, BLACK));
        shapes.push(Shape::rect_filled(Rect::from_center_size(c + egui::vec2(5.0 * k, 0.0), egui::vec2(4.5 * k, 15.0 * k)), 1.0, BLACK));
    } else {
        let pts = vec![c + egui::vec2(-5.0, -8.5) * k, c + egui::vec2(-5.0, 8.5) * k, c + egui::vec2(9.0, 0.0) * k];
        shapes.push(Shape::convex_polygon(pts, BLACK, Stroke::NONE));
    }
    paint_pressed(ui, c, press(ui, &resp), PRESS_DEPTH_ICON, shapes);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}
