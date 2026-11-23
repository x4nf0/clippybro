//! Reproducción: un reloj para la línea de tiempo y un reproductor por cada pieza que se ve o se oye
//! en ese momento (o que está por empezar): uno muestra la imagen de una pieza de video, otro hace sonar una
//! de audio. Los que se sueltan se guardan un rato para la próxima pieza del mismo archivo. Varias piezas
//! suenan juntas: Windows mezcla su audio.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use eframe::egui;

use super::media::layers;
use super::player::Player;
use super::EditorApp;

/// Cuadro de 60 fps, para cuando no se sabe el fps del archivo.
pub(super) const UNKNOWN_FRAME: i64 = 166_667;
/// Mientras se reproduce, las piezas que empiezan dentro de este tiempo ya se abren y se buscan.
const PREROLL: i64 = 3_000_000;
/// Reproductores sin usar que se guardan para la próxima pieza del mismo archivo.
const IDLE_MAX: usize = 4;

/// Reproductor de una pieza (o, en `idle`, esperando otra del mismo archivo).
pub(super) struct Voice {
    piece: u64,
    pub(super) path: PathBuf,
    /// Muestra la imagen (si no, hace sonar el audio).
    video: bool,
    player: Player,
    /// Ya arrancó a la par de la línea; si no, espera quieto en el inicio de la pieza.
    live: bool,
}

/// Una imagen de la vista previa: la pieza y cuánto se ve (la de arriba de un fundido, menos de 1).
pub(super) struct Layer {
    pub(super) tex: egui::TextureHandle,
    pub(super) alpha: f32,
    src: (u64, u64),
}

impl Layer {
    /// La pieza que muestra.
    pub(super) fn piece(&self) -> u64 {
        self.src.0
    }
}

/// Lo que necesita una pieza en este momento.
struct Want {
    id: u64,
    path: PathBuf,
    video: bool,
    /// Está bajo el cursor (si no, está por empezar).
    active: bool,
    src: i64,
    gain: f32,
    speed: f64,
}

impl EditorApp {
    /// Duración de un cuadro (100 ns) de ese archivo: los clips pueden ser de 30, 60 o 120 fps.
    /// Redondeada para arriba: avanzar N cuadros cae justo pasando el borde del cuadro N, no antes.
    pub(super) fn frame_len(&self, path: &Path) -> i64 {
        let fps = self.loader.probe(path).map_or(0.0, |p| p.fps());
        if fps >= 1.0 {
            (1e7 / fps).ceil() as i64
        } else {
            UNKNOWN_FRAME
        }
    }

    /// Duración de un cuadro de la pieza que está bajo el cursor de reproducción.
    pub(super) fn frame_at_cursor(&self) -> i64 {
        self.piece_at(self.pos()).map_or(UNKNOWN_FRAME, |(ti, i)| self.frame_len(&self.tracks[ti].clips[i].path))
    }

    pub(super) fn pos(&self) -> i64 {
        // Mientras se arrastra el cursor manda el mouse: el video lo sigue con un poco de demora.
        if let Some(t) = self.scrub_t {
            return t;
        }
        self.clock.pos().clamp(0, self.total())
    }

    pub(super) fn seek(&mut self, t: i64) {
        let t = t.clamp(0, self.total());
        self.clock.set(t, self.playing);
        self.sync(true);
    }

    fn play(&mut self) {
        if self.is_empty() {
            return;
        }
        let mut t = self.pos();
        let last = self.piece_at(self.total() - 1).map_or(UNKNOWN_FRAME, |(ti, i)| self.frame_len(&self.tracks[ti].clips[i].path));
        if t >= self.total() - last {
            t = 0;
        }
        self.playing = true;
        self.clock.set(t, true);
        self.sync(true);
    }

    pub(super) fn pause(&mut self) {
        let pos = self.pos();
        self.playing = false;
        self.clock.set(pos, false);
        for v in &self.voices {
            v.player.pause();
        }
    }

    pub(super) fn toggle_play(&mut self) {
        if self.playing {
            self.pause();
        } else {
            self.play();
        }
    }

    /// En cada cuadro: frenar al final y acomodar los reproductores a lo que hay bajo el cursor.
    pub(super) fn tick(&mut self) {
        if self.playing && self.pos() >= self.total() {
            self.pause();
        }
        self.sync(false);
    }

    /// Abre, ubica y suelta reproductores según lo que se ve y se oye en el cursor (y lo que está por
    /// empezar); pone el volumen de cada uno con sus fundidos y anota qué muestra la vista previa.
    /// `reseek`: el cursor saltó, todos se vuelven a buscar.
    fn sync(&mut self, reseek: bool) {
        let t = self.pos();
        let playing = self.playing;
        // Lo mismo que se exporta: las pistas de video visibles y lo que suena, con sus fundidos.
        let edit = self.edit();
        let mut want = Vec::new();
        let lists = edit.video.iter().map(|l| (l, true)).chain(edit.audio.iter().map(|l| (l, false)));
        for (list, video) in lists {
            for c in list {
                let active = c.covers(t);
                if !active && !(playing && c.start > t && c.start - t < PREROLL) {
                    continue;
                }
                if !video && (c.gain <= 0.0 || !self.has_audio(&c.path)) {
                    continue;
                }
                let gain = if !video && active { c.gain_at(t) } else { 0.0 };
                let src = if active { c.src_at(t) } else { c.src_in };
                want.push(Want { id: c.id, path: c.path.clone(), video, active, src, gain, speed: c.speed });
            }
        }

        // Soltar lo que ya no hace falta.
        let mut i = 0;
        while i < self.voices.len() {
            if want.iter().any(|w| w.id == self.voices[i].piece) {
                i += 1;
                continue;
            }
            let v = self.voices.remove(i);
            v.player.pause();
            self.idle.push(v);
        }
        if self.idle.len() > IDLE_MAX {
            self.idle.drain(..self.idle.len() - IDLE_MAX);
        }

        for w in want {
            let (vi, fresh) = match self.voices.iter().position(|v| v.piece == w.id) {
                Some(i) => (i, false),
                None => {
                    let reuse = self.idle.iter().position(|v| v.path == w.path && v.video == w.video);
                    let mut v = match reuse {
                        Some(k) => self.idle.remove(k),
                        None => {
                            let Some(pr) = self.loader.probe(&w.path) else { continue };
                            let player = Player::open(w.path.clone(), &pr, self.ctx.clone(), w.video);
                            Voice { piece: 0, path: w.path.clone(), video: w.video, player, live: false }
                        }
                    };
                    v.piece = w.id;
                    self.voices.push(v);
                    (self.voices.len() - 1, true)
                }
            };
            let v = &mut self.voices[vi];
            v.player.set_speed(w.speed);
            if fresh || reseek || (v.live && !w.active) {
                v.player.seek(w.src);
                if w.active && playing {
                    if !v.player.playing() {
                        v.player.play();
                    }
                } else {
                    v.player.pause();
                }
                v.live = w.active;
            } else if w.active && !v.live {
                // Llegó su momento: ya está buscada en el inicio de la pieza.
                if playing {
                    v.player.play();
                }
                v.live = true;
            }
            v.player.set_gain(w.gain);
        }

        // Vista previa: las capas que se ven en el cursor, de abajo hacia arriba sobre negro.
        let shown: Vec<(u64, f32)> = layers(&edit.video, t).into_iter().map(|(ti, i, a)| (edit.video[ti][i].id, a)).collect();
        // Al llegar al final queda la última imagen.
        if !shown.is_empty() || t < self.total() {
            self.shown = shown;
        }
    }

    pub(super) fn update_texture(&mut self) {
        if self.is_empty() {
            self.layers.clear();
            return;
        }
        self.layers.truncate(self.shown.len());
        for (k, &(id, alpha)) in self.shown.iter().enumerate() {
            let Some(v) = self.voices.iter().find(|v| v.piece == id) else { continue };
            let src = (id, v.player.shared.gen.load(Ordering::Relaxed));
            if let Some(l) = self.layers.get_mut(k) {
                l.alpha = alpha;
                if l.src == src {
                    continue;
                }
            }
            let Some(img) = v.player.shared.frame.lock().unwrap().clone() else { continue };
            let n = self.layers.len();
            match self.layers.get_mut(k) {
                Some(l) => {
                    l.tex.set(img, egui::TextureOptions::LINEAR);
                    l.src = src;
                }
                None if k == n => {
                    let tex = self.ctx.load_texture(format!("preview{k}"), img, egui::TextureOptions::LINEAR);
                    self.layers.push(Layer { tex, alpha, src });
                }
                None => {}
            }
        }
    }

    /// El error del reproductor de lo que se está mostrando, si no pudo abrir el archivo.
    pub(super) fn preview_error(&self) -> Option<String> {
        let &(id, _) = self.shown.last()?;
        self.voices.iter().find(|v| v.piece == id).and_then(|v| v.player.shared.error.lock().unwrap().clone())
    }

    /// Suelta los reproductores de esos archivos (que los tienen abiertos), para poder moverlos o borrarlos.
    pub(super) fn release_files(&mut self, paths: &[PathBuf]) {
        self.voices.retain(|v| !paths.contains(&v.path));
        self.idle.retain(|v| !paths.contains(&v.path));
    }

    pub(super) fn step(&mut self, dt: i64) {
        if self.playing {
            self.pause();
        }
        let t = self.pos() + dt;
        self.seek(t);
    }
}
