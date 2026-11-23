//! Atajos del editor: acciones, teclas por defecto, detección y la lista editable.

use std::collections::BTreeMap;

use eframe::egui::{self, Align, Layout, RichText};
use serde::{Deserialize, Serialize};

use crate::i18n::{tr, trf};
use crate::ui_kit::theme::*;

/// Combinación de teclas del editor (tecla con el nombre de egui, ej. "Space", "D", "ArrowLeft").
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Default)]
pub struct Combo {
    pub key: String,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
}

/// Atajos que el usuario cambió (id de acción -> combinación); el resto usa el de fábrica.
pub type Keys = BTreeMap<String, Combo>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Act {
    PlayPause,
    PrevFrame,
    NextFrame,
    Back,
    Forward,
    ToStart,
    ToEnd,
    Split,
    Duplicate,
    Delete,
    Freeze,
    Marker,
    MuteClip,
    Link,
    Magnet,
    Undo,
    Redo,
    ZoomIn,
    ZoomOut,
    ZoomFit,
    Fullscreen,
    Import,
    Export,
    Shortcuts,
}

pub const ALL: [Act; 24] = [
    Act::PlayPause,
    Act::PrevFrame,
    Act::NextFrame,
    Act::Back,
    Act::Forward,
    Act::ToStart,
    Act::ToEnd,
    Act::Split,
    Act::Duplicate,
    Act::Delete,
    Act::Freeze,
    Act::Marker,
    Act::MuteClip,
    Act::Link,
    Act::Magnet,
    Act::Undo,
    Act::Redo,
    Act::ZoomIn,
    Act::ZoomOut,
    Act::ZoomFit,
    Act::Fullscreen,
    Act::Import,
    Act::Export,
    Act::Shortcuts,
];

/// Grupos de la lista de atajos, en el idioma actual.
pub fn groups() -> [&'static str; 4] {
    [
        tr!("Reproducción", "Playback", "Reprodução"),
        tr!("Edición", "Editing", "Edição"),
        tr!("Vista", "View", "Visualização"),
        tr!("Archivo", "File", "Arquivo"),
    ]
}

fn k(key: &str, ctrl: bool, shift: bool) -> Combo {
    Combo { key: key.into(), ctrl, shift, alt: false }
}

impl Act {
    /// Cambia las piezas de la línea de tiempo (sus posiciones o su cantidad).
    pub fn edits_timeline(self) -> bool {
        matches!(self, Act::Split | Act::Duplicate | Act::Delete | Act::Freeze | Act::MuteClip | Act::Link | Act::Undo | Act::Redo)
    }

    /// Lo que se puede hacer en pantalla completa: moverse por el video (y salir).
    pub fn in_fullscreen(self) -> bool {
        self.group() == groups()[0] || self == Act::Fullscreen
    }

    pub fn id(self) -> &'static str {
        match self {
            Act::PlayPause => "play_pause",
            Act::PrevFrame => "prev_frame",
            Act::NextFrame => "next_frame",
            Act::Back => "back_1s",
            Act::Forward => "forward_1s",
            Act::ToStart => "to_start",
            Act::ToEnd => "to_end",
            Act::Split => "split",
            Act::Duplicate => "duplicate",
            Act::Delete => "delete",
            Act::Freeze => "freeze",
            Act::Marker => "marker",
            Act::MuteClip => "mute_clip",
            Act::Link => "link_audio",
            Act::Magnet => "magnet",
            Act::Undo => "undo",
            Act::Redo => "redo",
            Act::ZoomIn => "zoom_in",
            Act::ZoomOut => "zoom_out",
            Act::ZoomFit => "zoom_fit",
            Act::Fullscreen => "fullscreen",
            Act::Import => "import",
            Act::Export => "export",
            Act::Shortcuts => "shortcuts",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Act::PlayPause => tr!("Reproducir / pausar", "Play / pause", "Reproduzir / pausar"),
            Act::PrevFrame => tr!("Cuadro anterior", "Previous frame", "Quadro anterior"),
            Act::NextFrame => tr!("Cuadro siguiente", "Next frame", "Próximo quadro"),
            Act::Back => tr!("Retroceder 1 segundo", "Back 1 second", "Voltar 1 segundo"),
            Act::Forward => tr!("Avanzar 1 segundo", "Forward 1 second", "Avançar 1 segundo"),
            Act::ToStart => tr!("Ir al inicio", "Go to start", "Ir para o início"),
            Act::ToEnd => tr!("Ir al final", "Go to end", "Ir para o final"),
            Act::Split => tr!("Dividir en el cursor", "Split at playhead", "Dividir no cursor"),
            Act::Duplicate => tr!("Duplicar clip", "Duplicate clip", "Duplicar clipe"),
            Act::Delete => tr!("Eliminar clip", "Delete clip", "Excluir clipe"),
            Act::Freeze => tr!("Congelar el cuadro del cursor", "Freeze frame at playhead", "Congelar o quadro do cursor"),
            Act::Marker => tr!("Poner o sacar un marcador", "Add or remove a marker", "Pôr ou tirar um marcador"),
            Act::MuteClip => tr!("Silenciar o activar el sonido del clip", "Mute or unmute the clip", "Silenciar ou ativar o som do clipe"),
            Act::Link => tr!("Separar o unir el audio y el video", "Unlink or link audio and video", "Separar ou unir áudio e vídeo"),
            Act::Magnet => tr!("Imán: pegar los clips solos", "Magnet: snap clips together", "Ímã: grudar os clipes sozinhos"),
            Act::Undo => tr!("Deshacer", "Undo", "Desfazer"),
            Act::Redo => tr!("Rehacer", "Redo", "Refazer"),
            Act::ZoomIn => tr!("Acercar línea de tiempo", "Zoom in timeline", "Aproximar linha do tempo"),
            Act::ZoomOut => tr!("Alejar línea de tiempo", "Zoom out timeline", "Afastar linha do tempo"),
            Act::ZoomFit => tr!("Ver toda la línea de tiempo", "Fit whole timeline", "Ver toda a linha do tempo"),
            Act::Fullscreen => tr!("Pantalla completa", "Fullscreen", "Tela cheia"),
            Act::Import => tr!("Importar videos", "Import videos", "Importar vídeos"),
            Act::Export => tr!("Exportar video", "Export video", "Exportar vídeo"),
            Act::Shortcuts => tr!("Ayuda y atajos", "Help and shortcuts", "Ajuda e atalhos"),
        }
    }

    pub fn group(self) -> &'static str {
        groups()[match self {
            Act::PlayPause | Act::PrevFrame | Act::NextFrame | Act::Back | Act::Forward | Act::ToStart | Act::ToEnd => 0,
            Act::Split | Act::Duplicate | Act::Delete | Act::Freeze | Act::Marker | Act::MuteClip | Act::Link | Act::Magnet | Act::Undo | Act::Redo => 1,
            Act::ZoomIn | Act::ZoomOut | Act::ZoomFit | Act::Fullscreen => 2,
            Act::Import | Act::Export | Act::Shortcuts => 3,
        }]
    }

    pub fn default_combo(self) -> Combo {
        match self {
            Act::PlayPause => k("Space", false, false),
            Act::PrevFrame => k("Left", false, false),
            Act::NextFrame => k("Right", false, false),
            Act::Back => k("Left", false, true),
            Act::Forward => k("Right", false, true),
            Act::ToStart => k("Home", false, false),
            Act::ToEnd => k("End", false, false),
            Act::Split => k("S", false, false),
            Act::Duplicate => k("D", true, false),
            Act::Delete => k("Delete", false, false),
            Act::Freeze => k("F", false, false),
            Act::Marker => k("M", false, false),
            Act::MuteClip => k("M", false, true),
            Act::Link => k("L", true, false),
            Act::Magnet => k("N", false, false),
            Act::Undo => k("Z", true, false),
            Act::Redo => k("Y", true, false),
            Act::ZoomIn => k("Plus", true, false),
            Act::ZoomOut => k("Minus", true, false),
            Act::ZoomFit => k("0", true, false),
            Act::Fullscreen => k("F11", false, false),
            Act::Import => k("I", true, false),
            Act::Export => k("E", true, false),
            Act::Shortcuts => k("F1", false, false),
        }
    }

    /// Acciones que se repiten si se mantiene la tecla apretada.
    fn repeats(self) -> bool {
        matches!(self, Act::PrevFrame | Act::NextFrame | Act::Back | Act::Forward | Act::ZoomIn | Act::ZoomOut)
    }
}

pub fn binding(keys: &Keys, a: Act) -> Combo {
    keys.get(a.id()).cloned().unwrap_or_else(|| a.default_combo())
}

/// Texto para mostrar una combinación ("Ctrl + D", "Espacio", "Sin asignar").
pub fn label(c: &Combo) -> String {
    if c.key.is_empty() {
        return tr!("Sin asignar", "Not set", "Não definido").into();
    }
    let key = match c.key.as_str() {
        "Left" => "←",
        "Right" => "→",
        "Up" => "↑",
        "Down" => "↓",
        "Space" => tr!("Espacio", "Space", "Espaço"),
        "Delete" => tr!("Supr", "Delete", "Delete"),
        "Backspace" => tr!("Retroceso", "Backspace", "Backspace"),
        "Home" => tr!("Inicio", "Home", "Home"),
        "End" => tr!("Fin", "End", "End"),
        "PageUp" => tr!("RePág", "Page Up", "Page Up"),
        "PageDown" => tr!("AvPág", "Page Down", "Page Down"),
        "Escape" => "Esc",
        "Plus" => "+",
        "Minus" => "−",
        "Equals" => "=",
        other => other,
    };
    let mut s = String::new();
    if c.ctrl {
        s += "Ctrl + ";
    }
    if c.shift {
        s += "Shift + ";
    }
    if c.alt {
        s += "Alt + ";
    }
    s + key
}

pub fn short(keys: &Keys, a: Act) -> String {
    label(&binding(keys, a))
}

fn combo_of(key: egui::Key, m: egui::Modifiers) -> Combo {
    Combo { key: key.name().to_string(), ctrl: m.ctrl || m.command, shift: m.shift, alt: m.alt }
}

/// Acciones disparadas por las teclas apretadas en este cuadro.
pub fn pressed(ctx: &egui::Context, keys: &Keys) -> Vec<Act> {
    let mut out = Vec::new();
    ctx.input(|i| {
        for e in &i.events {
            if let egui::Event::Key { key, pressed: true, repeat, modifiers, .. } = e {
                let c = combo_of(*key, *modifiers);
                if let Some(a) = ALL.iter().copied().find(|&a| binding(keys, a) == c) {
                    if !*repeat || a.repeats() {
                        out.push(a);
                    }
                }
            }
        }
    });
    out
}

/// Mientras se elige una tecla: Some(Some(c)) = elegida, Some(None) = cancelado con Esc.
pub fn capture(ctx: &egui::Context) -> Option<Option<Combo>> {
    ctx.input(|i| {
        i.events.iter().find_map(|e| match e {
            egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => Some(None),
            egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } => Some(Some(combo_of(*key, *modifiers))),
            _ => None,
        })
    })
}

/// Estado de la lista editable.
#[derive(Default)]
pub struct Editor {
    pub capturing: Option<Act>,
    pub note: Option<String>,
}

impl Editor {
    /// Llamar antes de dibujar: aplica la tecla elegida. Devuelve true si cambiaron los atajos.
    pub fn poll(&mut self, ctx: &egui::Context, keys: &mut Keys) -> bool {
        let Some(a) = self.capturing else { return false };
        let Some(res) = capture(ctx) else { return false };
        self.capturing = None;
        let Some(c) = res else { return false };
        // Si otra acción ya usaba esa tecla, queda sin asignar.
        self.note = None;
        for other in ALL {
            if other != a && binding(keys, other) == c {
                keys.insert(other.id().into(), Combo::default());
                let (name, key) = (other.label(), label(&c));
                self.note = Some(trf!(
                    "«{name}» quedó sin tecla porque usaba {key}.",
                    "“{name}” no longer has a key because it used {key}.",
                    "“{name}” ficou sem tecla porque usava {key}."
                ));
            }
        }
        if c == a.default_combo() {
            keys.remove(a.id());
        } else {
            keys.insert(a.id().into(), c);
        }
        true
    }

    pub fn reset(&mut self, keys: &mut Keys) {
        keys.clear();
        self.capturing = None;
        self.note = Some(tr!("Se restablecieron los atajos originales.", "The original shortcuts were restored.", "Os atalhos originais foram restaurados.").into());
    }

    /// Lista de acciones con su tecla, en dos columnas por grupo. Tocar una empieza a esperar la
    /// tecla nueva, que aplica `poll`.
    pub fn ui(&mut self, ui: &mut egui::Ui, keys: &Keys) {
        let draw_group = |ui: &mut egui::Ui, g: &str, this: &mut Self| {
            ui.label(RichText::new(g).family(bold()).size(13.5).color(TEXT));
            for a in ALL.iter().copied().filter(|a| a.group() == g) {
                ui.horizontal(|ui| {
                    ui.set_min_height(32.0);
                    // Primero la tecla (a la derecha); el nombre usa lo que sobra y se corta si no entra.
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let capturing = this.capturing == Some(a);
                        let text = if capturing { tr!("Apretá una tecla…", "Press a key…", "Aperte uma tecla…").to_string() } else { short(keys, a) };
                        let kind = if capturing { Kind::Selected } else { Kind::Secondary };
                        if button_fit(ui, None, &text, kind, egui::vec2(150.0, 30.0)).on_hover_text(tr!("Clic para cambiar · Esc cancela", "Click to change · Esc cancels", "Clique para mudar · Esc cancela")).clicked() {
                            this.capturing = if capturing { None } else { Some(a) };
                            this.note = None;
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add(egui::Label::new(RichText::new(a.label()).color(MUTED)).truncate());
                        });
                    });
                });
            }
            ui.add_space(8.0);
        };
        ui.columns(2, |cols| {
            let g = groups();
            draw_group(&mut cols[0], g[0], self);
            draw_group(&mut cols[0], g[2], self);
            draw_group(&mut cols[1], g[1], self);
            draw_group(&mut cols[1], g[3], self);
        });
        if let Some(n) = &self.note {
            ui.label(RichText::new(n).size(12.5).color(MUTED));
        }
    }
}
