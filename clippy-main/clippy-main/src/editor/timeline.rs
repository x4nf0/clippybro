//! Línea de tiempo: pistas de video y de audio con sus piezas, marcadores, las ediciones y deshacer.
//!
//! Un clip entra como dos piezas unidas, la imagen en una pista de video y el sonido en una de audio:
//! se eligen, mueven, recortan, dividen, aceleran y borran juntas hasta que se las separa.
//!
//! Con el imán prendido, las piezas de cada pista quedan una detrás de la otra desde el inicio
//! (si dos ya se fundían, siguen igual). Apagado, cada pieza queda donde se la suelta; si se
//! superpone con otra de su pista, lo que se superponen es un fundido cruzado.

use std::path::{Path, PathBuf};

use crate::i18n::{tr, trf};
use super::media::{self, Clip, Curve, Edit, Frame, Ratio};
use super::playback::UNKNOWN_FRAME;
use super::EditorApp;

pub(super) const MIN_LEN: i64 = 2_000_000; // 0,2 s
/// Cuánto dura un cuadro congelado recién creado.
const FREEZE_LEN: i64 = 30_000_000;
/// Pasos de deshacer que se guardan.
const UNDO_MAX: usize = 100;
/// Pistas de cada tipo como máximo: más no entran en la línea de tiempo.
pub(super) const MAX_TRACKS: usize = 4;

fn push_capped(stack: &mut Vec<Snapshot>, v: Snapshot) {
    stack.push(v);
    if stack.len() > UNDO_MAX {
        stack.remove(0);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TrackKind {
    Video,
    Audio,
}

#[derive(Clone, PartialEq)]
pub(super) struct Piece {
    pub(super) id: u64,
    pub(super) path: PathBuf,
    /// Dónde empieza en la línea y cuánto dura ahí.
    pub(super) start: i64,
    pub(super) len: i64,
    /// Dónde empieza en el archivo.
    pub(super) src_in: i64,
    /// Cuánto del archivo pasa en cada instante de la línea (ver `media::src_at`); 0 = imagen congelada.
    pub(super) speed: f64,
    pub(super) gain: f32,
    pub(super) muted: bool,
    /// Las piezas con el mismo número (distinto de 0) están unidas: la imagen y el sonido de un clip.
    pub(super) link: u64,
    /// Cuánto tarda en aparecer desde negro (o silencio) y en irse.
    pub(super) fade_in: i64,
    pub(super) fade_out: i64,
    /// Forma del fundido de entrada y del de salida (cada uno se elige con clic derecho sobre él).
    pub(super) curves: (Curve, Curve),
    /// Cómo se ve su imagen en el lienzo (solo cuenta en las pistas de video).
    pub(super) frame: Frame,
}

impl Piece {
    pub(super) fn end(&self) -> i64 {
        self.start + self.len
    }
    pub(super) fn covers(&self, t: i64) -> bool {
        self.start <= t && t < self.end()
    }
    /// El instante del archivo que se ve o se oye en `t` de la línea.
    pub(super) fn src_at(&self, t: i64) -> i64 {
        media::src_at(self.start, self.src_in, self.speed, t)
    }
    pub(super) fn src_out(&self) -> i64 {
        self.src_at(self.end())
    }
    /// Una imagen quieta: el mismo cuadro todo el tiempo.
    pub(super) fn still(&self) -> bool {
        self.speed == 0.0
    }
    /// Cuánto dura en la línea un tramo `src` del archivo (en una imagen quieta, todo lo que se quiera).
    fn line_len(&self, src: i64) -> i64 {
        if self.still() {
            i64::MAX / 4
        } else {
            (src as f64 / self.speed).floor() as i64
        }
    }
    pub(super) fn eff_gain(&self) -> f32 {
        if self.muted {
            0.0
        } else {
            self.gain
        }
    }
    /// Que los fundidos a negro entren en la pieza (después de recortarla).
    fn fit_fades(&mut self) {
        self.fade_in = self.fade_in.min(self.len);
        self.fade_out = self.fade_out.min(self.len - self.fade_in);
    }
}

#[derive(Clone, PartialEq)]
pub(super) struct Track {
    pub(super) id: u64,
    pub(super) kind: TrackKind,
    /// Pista de video oculta: no se ve.
    pub(super) hidden: bool,
    /// Pista de audio muda: no suena.
    pub(super) muted: bool,
    /// Ordenadas por inicio.
    pub(super) clips: Vec<Piece>,
}

impl Track {
    pub(super) fn new(id: u64, kind: TrackKind) -> Self {
        Track { id, kind, hidden: false, muted: false, clips: Vec::new() }
    }

    pub(super) fn end(&self) -> i64 {
        self.clips.iter().map(Piece::end).max().unwrap_or(0)
    }

    /// Cuánto se superpone cada pieza con la anterior y con la siguiente: sus fundidos cruzados.
    pub(super) fn overlaps(&self) -> Vec<(i64, i64)> {
        media::overlaps(&self.clips.iter().map(|p| (p.start, p.end())).collect::<Vec<_>>())
    }

    /// Cuánto se superpone cada pieza con la anterior (con el imán, eso se mantiene al cambiar un largo).
    fn joins(&self) -> Vec<i64> {
        self.overlaps().iter().map(|f| f.0).collect()
    }

    /// Lo que hay en esta pista se oye.
    pub(super) fn sounds(&self) -> bool {
        self.kind == TrackKind::Audio && !self.muted
    }

    fn sort(&mut self) {
        self.clips.sort_by_key(|p| p.start);
    }

    /// Con el imán: aplica `f` a las piezas y a cuánto se superpone cada una con la anterior
    /// (0 = pegadas) y las vuelve a poner una detrás de la otra desde el inicio.
    fn reflow(&mut self, f: impl FnOnce(&mut Vec<Piece>, &mut Vec<i64>)) {
        let mut ov = self.joins();
        f(&mut self.clips, &mut ov);
        self.relayout(&ov);
    }

    /// Ubica las piezas una detrás de la otra desde el inicio, cada una superpuesta `ov` con la anterior
    /// (nunca más que la mitad de cualquiera de las dos).
    fn relayout(&mut self, ov: &[i64]) {
        let mut end = 0;
        let mut prev = 0;
        for (i, p) in self.clips.iter_mut().enumerate() {
            let o = if i == 0 { 0 } else { ov.get(i).copied().unwrap_or(0).clamp(0, prev.min(p.len) / 2) };
            p.start = end - o;
            end = p.end();
            prev = p.len;
        }
    }

    /// Saca la pieza `i`; con el imán las siguientes se corren a ocupar su lugar.
    fn take(&mut self, i: usize, magnet: bool) -> Piece {
        if !magnet {
            return self.clips.remove(i);
        }
        let mut p = None;
        self.reflow(|c, ov| {
            p = Some(c.remove(i));
            ov.remove(i);
            if let Some(o) = ov.get_mut(i) {
                *o = 0;
            }
        });
        p.unwrap()
    }

    /// Mete `p` donde empieza y corre lo que empieza ahí o después lo que dura `p` (con el imán, pegada a sus vecinas).
    fn insert(&mut self, p: Piece, magnet: bool) {
        let k = self.clips.iter().filter(|c| c.start < p.start).count();
        if magnet {
            self.reflow(|c, ov| {
                c.insert(k, p);
                ov.insert(k, 0);
                if let Some(o) = ov.get_mut(k + 1) {
                    *o = 0;
                }
            });
        } else {
            for c in self.clips.iter_mut().skip(k) {
                c.start += p.len;
            }
            self.clips.insert(k, p);
        }
    }

    /// Pone `p`: con el imán, en el lugar que le toca a su centro y pegada a sus vecinas; sin él, donde dice su inicio.
    fn put(&mut self, p: Piece, magnet: bool) {
        if !magnet {
            self.clips.push(p);
            self.sort();
            return;
        }
        let center = p.start + p.len / 2;
        let k = self.clips.iter().filter(|c| c.start + c.len / 2 < center).count();
        self.reflow(|c, ov| {
            c.insert(k, p);
            ov.insert(k, 0);
            if let Some(o) = ov.get_mut(k + 1) {
                *o = 0;
            }
        });
    }
}

/// Cómo estaba la línea de tiempo (pistas, marcadores y proporción), para deshacer.
#[derive(Clone)]
pub(super) struct Snapshot {
    tracks: Vec<Track>,
    markers: Vec<i64>,
    ratio: Option<Ratio>,
}

/// Una pieza a la que se le cambia un fundido arrastrando una esquina de arriba.
#[derive(Clone)]
pub(super) struct Faded {
    pub(super) at: (usize, usize),
    orig: Piece,
    ov: Vec<i64>,
    /// Largo de la vecina de ese lado, si la toca: la esquina cambia cuánto se funden (alargando la pieza por
    /// debajo de la otra con el material que sobra del archivo) y, pasando el borde, cuánto tarda en ir a
    /// negro. Sin vecina, solo lo segundo.
    neighbor: Option<i64>,
    max: i64,
}

/// Una pieza que se está recortando: dónde está y cómo estaban ella y su pista al empezar.
#[derive(Clone)]
pub(super) struct Trimmed {
    pub(super) at: (usize, usize),
    pub(super) orig: Piece,
    /// Cómo se unía cada pieza de su pista con la anterior (con el imán, eso no cambia al recortar).
    ov: Vec<i64>,
    /// Hasta dónde llega el archivo.
    max: i64,
}

impl EditorApp {
    pub(super) fn total(&self) -> i64 {
        self.tracks.iter().map(Track::end).max().unwrap_or(0)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.tracks.iter().all(|t| t.clips.is_empty())
    }

    /// Pista y lugar de la pieza `id`.
    pub(super) fn find(&self, id: u64) -> Option<(usize, usize)> {
        self.tracks.iter().enumerate().find_map(|(ti, t)| t.clips.iter().position(|p| p.id == id).map(|i| (ti, i)))
    }

    pub(super) fn selection(&self) -> Option<(usize, usize)> {
        self.selected.and_then(|id| self.find(id))
    }

    /// La pieza `id` y, si está unida, su pareja de la otra pista: lo que se edita junto.
    pub(super) fn group(&self, id: u64) -> Vec<(usize, usize)> {
        let Some((ti, i)) = self.find(id) else { return Vec::new() };
        let link = self.tracks[ti].clips[i].link;
        if link == 0 {
            return vec![(ti, i)];
        }
        let mut g = Vec::new();
        for (tj, t) in self.tracks.iter().enumerate() {
            g.extend(t.clips.iter().enumerate().filter(|(_, p)| p.link == link).map(|(j, _)| (tj, j)));
        }
        g
    }

    /// La pieza está elegida, o unida a la elegida.
    pub(super) fn is_selected(&self, p: &Piece) -> bool {
        let link = self.selection().map(|(ti, i)| self.tracks[ti].clips[i].link);
        self.selected == Some(p.id) || (p.link != 0 && link == Some(p.link))
    }

    /// La pieza que se ve (o se oye) en `t`: la de más arriba en la línea de tiempo.
    pub(super) fn piece_at(&self, t: i64) -> Option<(usize, usize)> {
        self.display_order().into_iter().find_map(|ti| self.tracks[ti].clips.iter().rposition(|p| p.covers(t)).map(|i| (ti, i)))
    }

    /// Pistas en el orden en que se ven, de arriba hacia abajo: las de video de la última a la primera
    /// (la de más arriba tapa a las de abajo) y después las de audio.
    pub(super) fn display_order(&self) -> Vec<usize> {
        let of = |k: TrackKind| (0..self.tracks.len()).filter(move |&i| self.tracks[i].kind == k);
        of(TrackKind::Video).rev().chain(of(TrackKind::Audio)).collect()
    }

    pub(super) fn count(&self, kind: TrackKind) -> usize {
        self.tracks.iter().filter(|t| t.kind == kind).count()
    }

    /// "Video 2", "Audio 1": el número entre las pistas de su tipo.
    pub(super) fn track_name(&self, ti: usize) -> String {
        let k = self.tracks[ti].kind;
        let n = self.tracks[..=ti].iter().filter(|t| t.kind == k).count();
        match k {
            TrackKind::Video => trf!("Video {n}", "Video {n}", "Vídeo {n}"),
            TrackKind::Audio => trf!("Audio {n}", "Audio {n}", "Áudio {n}"),
        }
    }

    pub(super) fn has_audio(&self, path: &Path) -> bool {
        self.loader.probe(path).is_some_and(|p| p.audio.is_some())
    }

    fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// La primera pista de ese tipo; si no hay, una nueva.
    fn first_track(&mut self, kind: TrackKind) -> usize {
        if let Some(ti) = self.tracks.iter().position(|t| t.kind == kind) {
            return ti;
        }
        let id = self.new_id();
        self.tracks.push(Track::new(id, kind));
        self.tracks.len() - 1
    }

    /// Cómo está la línea ahora.
    pub(super) fn snapshot(&self) -> Snapshot {
        Snapshot { tracks: self.tracks.clone(), markers: self.markers.clone(), ratio: self.ratio }
    }

    /// Guarda cómo estaba la línea (`before`) para poder deshacer el cambio que se hizo o se va a hacer.
    pub(super) fn record(&mut self, before: Snapshot) {
        self.dirty = true;
        push_capped(&mut self.undo, before);
        self.redo.clear();
    }

    /// Como `record`, para un cambio que solo tocó las pistas (`before`).
    pub(super) fn record_undo(&mut self, before: Vec<Track>) {
        let s = Snapshot { tracks: before, ..self.snapshot() };
        self.record(s);
    }

    /// Como `record`, para un cambio que solo movió marcadores (`before`).
    pub(super) fn record_markers(&mut self, before: Vec<i64>) {
        if before != self.markers {
            let s = Snapshot { markers: before, ..self.snapshot() };
            self.record(s);
        }
    }

    pub(super) fn push_undo(&mut self) {
        self.record(self.snapshot());
    }

    fn restore(&mut self, s: Snapshot) -> Snapshot {
        let now = self.snapshot();
        self.tracks = s.tracks;
        self.markers = s.markers;
        self.ratio = s.ratio;
        now
    }

    pub(super) fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            let pos = self.pos();
            self.dirty = true;
            let now = self.restore(prev);
            self.redo.push(now);
            self.after_edit(pos);
        }
    }

    pub(super) fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            let pos = self.pos();
            self.dirty = true;
            let now = self.restore(next);
            push_capped(&mut self.undo, now);
            self.after_edit(pos);
        }
    }

    /// Quita de la línea, y de lo que se puede deshacer, las piezas de archivos que ya no existen (`gone`).
    pub(super) fn forget_files(&mut self, gone: &[PathBuf]) {
        let magnet = self.prefs.magnet;
        for t in &mut self.tracks {
            for i in (0..t.clips.len()).rev() {
                if gone.contains(&t.clips[i].path) {
                    t.take(i, magnet);
                }
            }
        }
        for s in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            for t in &mut s.tracks {
                t.clips.retain(|p| !gone.contains(&p.path));
            }
        }
        let pos = self.pos();
        self.after_edit(pos);
    }

    /// Un archivo cambió de lugar (`from` -> `to`): las piezas que lo usan lo siguen.
    pub(super) fn moved_file(&mut self, from: &Path, to: &Path) {
        let fix = |t: &mut Vec<Track>| {
            for p in t.iter_mut().flat_map(|t| t.clips.iter_mut()).filter(|p| p.path == from) {
                p.path = to.to_path_buf();
            }
        };
        fix(&mut self.tracks);
        for s in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            fix(&mut s.tracks);
        }
        let pos = self.pos();
        self.after_edit(pos);
    }

    /// Agrega el clip al final de la primera pista de video, con su sonido unido en la primera de audio.
    pub(super) fn add_to_timeline(&mut self, path: &Path) {
        let Some(pr) = self.loader.probe(path) else { return };
        if pr.duration <= MIN_LEN {
            return;
        }
        self.push_undo();
        let vt = self.first_track(TrackKind::Video);
        let start = self.tracks[vt].end();
        let id = self.new_id();
        let mut p = Piece {
            id,
            path: path.to_path_buf(),
            start,
            len: pr.duration,
            src_in: 0,
            speed: 1.0,
            gain: 1.0,
            muted: false,
            link: 0,
            fade_in: 0,
            fade_out: 0,
            curves: (Curve::Linear, Curve::Linear),
            frame: Frame::default(),
        };
        if pr.audio.is_some() {
            p.link = self.new_id();
            let mut a = p.clone();
            a.id = self.new_id();
            a.curves = (Curve::Even, Curve::Even);
            let at = self.first_track(TrackKind::Audio);
            self.tracks[at].put(a, false);
        }
        self.tracks[vt].clips.push(p);
        self.selected = Some(id);
        self.loader.want_strip(path);
        self.after_edit(start);
        self.message = None;
    }

    /// La pieza (con su pareja) que se edita con el cursor: la elegida si está en el cursor; si no, la de más
    /// arriba ahí. Con `video`, solo una que tenga imagen.
    fn at_cursor(&self, video: bool) -> Option<(usize, usize)> {
        let t = self.pos();
        let ok = |&(ti, i): &(usize, usize)| self.tracks[ti].clips[i].covers(t) && (!video || self.tracks[ti].kind == TrackKind::Video);
        let chosen = self.selected.map(|id| self.group(id)).unwrap_or_default().into_iter().find(ok);
        chosen.or_else(|| self.display_order().into_iter().find_map(|ti| self.tracks[ti].clips.iter().rposition(|p| p.covers(t)).map(|i| (ti, i))).filter(ok))
    }

    /// Parte en `t` las piezas del grupo de `(ti, i)` que lo cruzan, si a todas les queda al menos MIN_LEN de
    /// cada lado. Devuelve el id de la mitad de la derecha de `(ti, i)`.
    fn split_at(&mut self, (ti, i): (usize, usize), t: i64) -> Option<u64> {
        let parts: Vec<(usize, usize)> = self.group(self.tracks[ti].clips[i].id).into_iter().filter(|&(tj, j)| self.tracks[tj].clips[j].covers(t)).collect();
        let fits = |p: &Piece| t - p.start >= MIN_LEN && p.end() - t >= MIN_LEN;
        if !parts.iter().all(|&(tj, j)| fits(&self.tracks[tj].clips[j])) {
            return None;
        }
        let link = if parts.len() > 1 { self.new_id() } else { 0 };
        let clicked = self.tracks[ti].clips[i].id;
        let mut right = None;
        for (tj, j) in parts {
            let mut b = self.tracks[tj].clips[j].clone();
            b.id = self.new_id();
            b.src_in = b.src_at(t);
            b.len = b.end() - t;
            b.start = t;
            b.link = link;
            b.fade_in = 0;
            if self.tracks[tj].clips[j].id == clicked {
                right = Some(b.id);
            }
            let track = &mut self.tracks[tj];
            let a = &mut track.clips[j];
            a.len = t - a.start;
            a.fade_out = 0;
            a.fit_fades();
            b.fit_fades();
            track.clips.insert(j + 1, b);
            track.sort();
        }
        right
    }

    /// Divide en el cursor la pieza elegida (con su pareja) si está ahí; si no, la de más arriba.
    pub(super) fn split(&mut self) {
        let t = self.pos();
        let Some(at) = self.at_cursor(false) else { return };
        let before = self.snapshot();
        if let Some(id) = self.split_at(at, t) {
            self.selected = Some(id);
            self.record(before);
            self.after_edit(t);
        }
    }

    /// Congela el cuadro que se ve en el cursor: parte ahí la pieza de video (la elegida o la de más arriba) y
    /// mete entre las dos mitades esa imagen quieta por FREEZE_LEN, con su sonido en silencio; lo que sigue en
    /// esas pistas se corre.
    pub(super) fn freeze(&mut self) {
        let t = self.pos();
        let Some((ti, i)) = self.at_cursor(true) else { return };
        let p = self.tracks[ti].clips[i].clone();
        if p.still() {
            return;
        }
        let before = self.snapshot();
        let magnet = self.prefs.magnet;
        // Cerca de un borde no se parte: la imagen va antes o después de la pieza.
        let at = if self.split_at((ti, i), t).is_some() {
            t
        } else if t - p.start < p.end() - t {
            p.start
        } else {
            p.end()
        };
        let audio = self.find(p.id).map(|(ti, i)| self.group(self.tracks[ti].clips[i].id)).unwrap_or_default().into_iter().find(|&(tj, _)| self.tracks[tj].kind == TrackKind::Audio).map(|(tj, _)| tj);
        let link = if audio.is_some() { self.new_id() } else { 0 };
        let still = Piece { id: self.new_id(), start: at, len: FREEZE_LEN, src_in: p.src_at(t.clamp(p.start, p.end() - 1)), speed: 0.0, link, fade_in: 0, fade_out: 0, ..p };
        let mut tracks = vec![(ti, still.clone())];
        if let Some(ta) = audio {
            let silence = Piece { id: self.new_id(), muted: true, curves: (Curve::Even, Curve::Even), frame: Frame::default(), ..still.clone() };
            tracks.push((ta, silence));
        }
        for (tj, piece) in tracks {
            self.tracks[tj].insert(piece, magnet);
        }
        self.selected = Some(still.id);
        self.record(before);
        self.after_edit(at);
    }

    pub(super) fn delete_selected(&mut self) {
        let Some((ti, i)) = self.selection() else { return };
        self.push_undo();
        let start = self.tracks[ti].clips[i].start;
        let magnet = self.prefs.magnet;
        // Cada pieza del grupo está en otra pista: sacar una no corre a las demás.
        for (tj, j) in self.group(self.tracks[ti].clips[i].id) {
            self.tracks[tj].take(j, magnet);
        }
        let track = &self.tracks[ti];
        self.selected = track.clips.get(i).or(track.clips.last()).map(|p| p.id);
        self.after_edit(start);
    }

    /// Copia la pieza elegida (con su pareja) justo después de ella. Sin el imán, si ahí ya hay algo,
    /// va después de lo último de esas pistas.
    pub(super) fn duplicate_selected(&mut self) {
        let Some((ti, i)) = self.selection() else { return };
        self.push_undo();
        let magnet = self.prefs.magnet;
        let group = self.group(self.tracks[ti].clips[i].id);
        let len = self.tracks[ti].clips[i].len;
        let free = group.iter().all(|&(tj, j)| {
            let at = self.tracks[tj].clips[j].end();
            self.tracks[tj].clips.iter().all(|c| c.end() <= at || c.start >= at + len)
        });
        let shift = if free || magnet {
            len
        } else {
            group.iter().map(|&(tj, j)| self.tracks[tj].end() - self.tracks[tj].clips[j].start).max().unwrap_or(len)
        };
        let link = if group.len() > 1 { self.new_id() } else { 0 };
        let copies: Vec<(usize, Piece)> = group.iter().map(|&(tj, j)| (tj, self.tracks[tj].clips[j].clone())).collect();
        let first = self.tracks[ti].clips[i].id;
        for (tj, mut p) in copies {
            let was = p.id;
            p.id = self.new_id();
            p.start += shift;
            p.link = link;
            if was == first {
                self.selected = Some(p.id);
            }
            self.tracks[tj].put(p, magnet);
        }
        let start = self.selection().map_or(0, |(ti, i)| self.tracks[ti].clips[i].start);
        self.after_edit(start);
    }

    /// La pieza de audio de lo elegido (ella misma o su pareja), si tiene.
    pub(super) fn selected_audio(&self) -> Option<(usize, usize)> {
        let id = self.selected?;
        self.group(id).into_iter().find(|&(ti, _)| self.tracks[ti].kind == TrackKind::Audio)
    }

    /// La pieza de video de lo elegido (ella misma o su pareja), si tiene.
    pub(super) fn selected_video(&self) -> Option<(usize, usize)> {
        let id = self.selected?;
        self.group(id).into_iter().find(|&(ti, _)| self.tracks[ti].kind == TrackKind::Video)
    }

    pub(super) fn toggle_mute_selected(&mut self) {
        let Some((ti, i)) = self.selected_audio() else { return };
        self.push_undo();
        let p = &mut self.tracks[ti].clips[i];
        p.muted = !p.muted;
    }

    /// Cambia la velocidad de lo elegido (con su pareja): el mismo tramo del archivo dura menos o más en la línea.
    /// Con el imán, lo que sigue se corre. El que llama guarda cómo estaba para deshacer.
    pub(super) fn set_speed(&mut self, speed: f64) {
        let Some(id) = self.selected else { return };
        let speed = speed.clamp(media::SPEED_MIN, media::SPEED_MAX);
        let magnet = self.prefs.magnet;
        for (ti, i) in self.group(id) {
            let track = &mut self.tracks[ti];
            let ov = track.joins();
            let p = &mut track.clips[i];
            if p.still() || p.speed == speed {
                continue;
            }
            let span = p.len as f64 * p.speed;
            p.speed = speed;
            p.len = ((span / speed).round() as i64).max(MIN_LEN);
            p.fit_fades();
            if magnet {
                track.relayout(&ov);
            }
        }
        let pos = self.pos();
        self.after_edit(pos);
    }

    /// Cambia cómo se ve la imagen de lo elegido (girarla, espejarla, recortarla o cómo entra en el lienzo).
    pub(super) fn edit_frame(&mut self, f: impl FnOnce(&mut Frame)) {
        let Some((ti, i)) = self.selected_video() else { return };
        let mut frame = self.tracks[ti].clips[i].frame;
        f(&mut frame);
        if frame != self.tracks[ti].clips[i].frame {
            self.push_undo();
            self.tracks[ti].clips[i].frame = frame;
        }
    }

    /// Cambia la proporción del lienzo (None = la del primer clip).
    pub(super) fn set_ratio(&mut self, ratio: Option<Ratio>) {
        if ratio != self.ratio {
            self.push_undo();
            self.ratio = ratio;
        }
    }

    /// El marcador a menos de `tol` de `t`, si hay.
    pub(super) fn marker_near(&self, t: i64, tol: i64) -> Option<usize> {
        self.markers.iter().enumerate().filter(|(_, &m)| (m - t).abs() <= tol).min_by_key(|(_, &m)| (m - t).abs()).map(|(k, _)| k)
    }

    /// Pone un marcador en el cursor o, si ya hay uno ahí, lo saca.
    pub(super) fn toggle_marker(&mut self) {
        let t = self.pos();
        self.push_undo();
        match self.marker_near(t, self.frame_at_cursor() / 2) {
            Some(k) => {
                self.markers.remove(k);
            }
            None => {
                self.markers.push(t);
                self.markers.sort_unstable();
            }
        }
    }

    /// Con qué se uniría la pieza elegida si no está unida: la pieza suelta de la otra clase de pista que
    /// salió del mismo archivo (sus tramos se tocan), la más cercana.
    fn link_partner(&self) -> Option<(usize, usize)> {
        let (ti, i) = self.selection()?;
        let p = &self.tracks[ti].clips[i];
        if p.link != 0 || p.still() {
            return None;
        }
        // Dónde caería en la línea el comienzo del archivo: dos piezas que van a la par lo tienen en el mismo lugar.
        let origin = |c: &Piece| c.start - c.line_len(c.src_in);
        let mut best: Option<((usize, usize), i64)> = None;
        for (tj, t) in self.tracks.iter().enumerate().filter(|(_, t)| t.kind != self.tracks[ti].kind) {
            for (j, c) in t.clips.iter().enumerate() {
                if c.link == 0 && !c.still() && c.path == p.path && c.src_in < p.src_out() && p.src_in < c.src_out() {
                    let gap = (origin(c) - origin(p)).abs();
                    if best.is_none_or(|b| gap < b.1) {
                        best = Some(((tj, j), gap));
                    }
                }
            }
        }
        best.map(|b| b.0)
    }

    /// La pieza elegida está unida a otra (se puede separar) o tiene con cuál volver a unirse.
    pub(super) fn can_link(&self) -> bool {
        self.group(self.selected.unwrap_or(0)).len() > 1 || self.link_partner().is_some()
    }

    /// Qué hace ahora el botón de separar o unir.
    pub(super) fn link_label(&self) -> &'static str {
        if self.group(self.selected.unwrap_or(0)).len() > 1 {
            tr!("Separar audio y video", "Unlink audio and video", "Separar áudio e vídeo")
        } else {
            tr!("Volver a unir audio y video", "Relink audio and video", "Unir áudio e vídeo de novo")
        }
    }

    /// Separa la imagen y el sonido de lo elegido o, si están separados, los vuelve a unir: el sonido
    /// vuelve a quedar a la par de la imagen (y a su velocidad).
    pub(super) fn toggle_link(&mut self) {
        let Some((ti, i)) = self.selection() else { return };
        let group = self.group(self.tracks[ti].clips[i].id);
        if group.len() > 1 {
            self.push_undo();
            for (tj, j) in group {
                self.tracks[tj].clips[j].link = 0;
            }
            return;
        }
        let Some((tj, j)) = self.link_partner() else { return };
        self.push_undo();
        let link = self.new_id();
        let p = self.tracks[ti].clips[i].clone();
        let c = &mut self.tracks[tj].clips[j];
        let span = c.len as f64 * c.speed;
        c.speed = p.speed;
        c.len = (span / p.speed).round() as i64;
        c.start = p.start + p.line_len(c.src_in - p.src_in);
        c.link = link;
        c.fit_fades();
        self.tracks[ti].clips[i].link = link;
        self.tracks[tj].sort();
        let pos = self.pos();
        self.after_edit(pos);
    }

    /// Cambia la curva de los fundidos `targets` (pieza y si es el de salida).
    pub(super) fn set_curve(&mut self, targets: &[(u64, bool)], curve: Curve) {
        self.push_undo();
        for &(id, out) in targets {
            if let Some((ti, i)) = self.find(id) {
                let c = &mut self.tracks[ti].clips[i].curves;
                if out {
                    c.1 = curve;
                } else {
                    c.0 = curve;
                }
            }
        }
    }

    pub(super) fn add_track(&mut self, kind: TrackKind) {
        if self.count(kind) >= MAX_TRACKS {
            return;
        }
        self.push_undo();
        let id = self.new_id();
        self.tracks.push(Track::new(id, kind));
    }

    /// Quita la pista y lo que tiene (lo que estaba unido a eso queda suelto). Siempre queda una de video.
    pub(super) fn remove_track(&mut self, ti: usize) {
        if self.tracks[ti].kind == TrackKind::Video && self.count(TrackKind::Video) == 1 {
            return;
        }
        self.push_undo();
        let pos = self.pos();
        let gone = self.tracks.remove(ti);
        for t in &mut self.tracks {
            for p in &mut t.clips {
                if p.link != 0 && gone.clips.iter().any(|g| g.link == p.link) {
                    p.link = 0;
                }
            }
        }
        self.after_edit(pos);
    }

    /// Oculta o muestra una pista de video; silencia o activa una de audio.
    pub(super) fn toggle_track(&mut self, ti: usize) {
        self.push_undo();
        let t = &mut self.tracks[ti];
        match t.kind {
            TrackKind::Video => t.hidden = !t.hidden,
            TrackKind::Audio => t.muted = !t.muted,
        }
    }

    /// Suelta la pieza `id` (y su pareja, corrida lo mismo) empezando en `start`, en la pista `to` si es de su
    /// tipo. Con el imán cada una va al lugar que le toca a su centro, pegada a sus vecinas. Devuelve si cambió algo.
    pub(super) fn drop_piece(&mut self, id: u64, to: usize, start: i64) -> bool {
        let Some((ti, i)) = self.find(id) else { return false };
        let magnet = self.prefs.magnet;
        let shift = start.max(0) - self.tracks[ti].clips[i].start;
        let group = self.group(id);
        // Que ninguna quede antes del inicio de la línea.
        let shift = group.iter().map(|&(tj, j)| -self.tracks[tj].clips[j].start).fold(shift, i64::max);
        let before = self.tracks.clone();
        let mut moved: Vec<(usize, Piece)> = Vec::new();
        // De la última a la primera de cada pista, así los lugares que quedan siguen valiendo.
        let mut group = group;
        group.sort_by(|a, b| b.cmp(a));
        for (tj, j) in group {
            let mut p = self.tracks[tj].take(j, magnet);
            p.start += shift;
            moved.push((if p.id == id { to } else { tj }, p));
        }
        for (tj, p) in moved {
            self.tracks[tj].put(p, magnet);
        }
        self.tracks != before
    }

    /// Lo que se recorta o se funde al arrastrar la pieza `(ti, i)`: ella y su pareja, primero la tocada.
    /// Con `alone` (Alt apretado, como en los editores), solo ella.
    pub(super) fn dragged(&self, ti: usize, i: usize, alone: bool) -> Vec<(usize, usize)> {
        if alone {
            return vec![(ti, i)];
        }
        let mut group = self.group(self.tracks[ti].clips[i].id);
        group.sort_by_key(|&at| at != (ti, i));
        group
    }

    /// Puntos donde se engancha lo que se arrastra con el imán: los bordes y los fundidos de las piezas que no
    /// se están arrastrando (`skip`) y los marcadores.
    pub(super) fn snap_points(&self, skip: &[(usize, usize)]) -> Vec<i64> {
        let mut v = self.markers.clone();
        for (ti, t) in self.tracks.iter().enumerate() {
            for (i, p) in t.clips.iter().enumerate().filter(|&(i, _)| !skip.contains(&(ti, i))) {
                let (a, b) = self.fade_handles(ti, i);
                v.extend([p.start, p.end(), a, b]);
            }
        }
        v
    }

    /// Empieza a recortar las piezas `parts` (ver `dragged`).
    pub(super) fn start_trim(&self, parts: &[(usize, usize)]) -> Vec<Trimmed> {
        parts
            .iter()
            .copied()
            .map(|(tj, j)| {
                let orig = self.tracks[tj].clips[j].clone();
                let max = self.loader.probe(&orig.path).map(|p| p.duration).unwrap_or(orig.src_out());
                Trimmed { at: (tj, j), ov: self.tracks[tj].joins(), max, orig }
            })
            .collect()
    }

    /// Mueve el borde izquierdo (`left`) o derecho de las piezas que se recortan `dt` desde donde estaban,
    /// hasta donde dejen todas. Con el imán, las que siguen en cada pista se corren y cada unión queda como
    /// antes; sin él, nada más se mueve.
    pub(super) fn trim(&mut self, parts: &[Trimmed], left: bool, dt: i64) {
        let magnet = self.prefs.magnet;
        let (mut lo, mut hi) = (i64::MIN, i64::MAX);
        for t in parts {
            let o = &t.orig;
            if left {
                // Hasta el comienzo del archivo; sin el imán, tampoco antes del inicio de la línea.
                let room = if magnet { o.line_len(o.src_in) } else { o.line_len(o.src_in).min(o.start) };
                lo = lo.max(-room);
                hi = hi.min(o.len - MIN_LEN);
            } else {
                lo = lo.max(MIN_LEN - o.len);
                hi = hi.min(o.line_len(t.max - o.src_out()));
            }
        }
        let dt = dt.clamp(lo, hi.max(lo));
        for t in parts {
            let track = &mut self.tracks[t.at.0];
            let p = &mut track.clips[t.at.1];
            let o = &t.orig;
            if left {
                p.src_in = o.src_at(o.start + dt);
                p.len = o.len - dt;
                if !magnet {
                    p.start = o.start + dt;
                }
            } else {
                p.len = o.len + dt;
            }
            p.fit_fades();
            if magnet {
                track.relayout(&t.ov);
            }
        }
    }

    /// Dónde está la esquina de arriba de cada lado de la pieza: en el borde si se funde con la vecina o no
    /// tiene fundido; si no, donde termina su fundido a negro.
    pub(super) fn fade_handles(&self, ti: usize, i: usize) -> (i64, i64) {
        let t = &self.tracks[ti];
        let (p, ov) = (&t.clips[i], t.overlaps()[i]);
        (p.start + if ov.0 > 0 { 0 } else { p.fade_in }, p.end() - if ov.1 > 0 { 0 } else { p.fade_out })
    }

    /// Lo que mide el fundido que se está arrastrando, para mostrarlo: cruzado (true) o a negro.
    pub(super) fn fade_len(&self, f: &Faded, out: bool) -> (bool, i64) {
        let (ti, i) = f.at;
        let ov = self.tracks[ti].overlaps()[i];
        let p = &self.tracks[ti].clips[i];
        match (out, if out { ov.1 } else { ov.0 }) {
            (_, o) if o > 0 => (true, o),
            (true, _) => (false, p.fade_out),
            (false, _) => (false, p.fade_in),
        }
    }

    /// Empieza a arrastrar la esquina de arriba (`out`: la derecha) de la pieza `(ti, i)`. Cada pieza tiene sus
    /// fundidos: la imagen y el sonido unidos se arrastran juntos, pero con Alt se ajusta uno solo (siguen a la
    /// par: alargar el fundido cruzado no cambia dónde cae cada momento del archivo).
    pub(super) fn start_fade(&self, (ti, i): (usize, usize), out: bool) -> Faded {
        let t = &self.tracks[ti];
        let p = &t.clips[i];
        // Toca a la vecina si la pisa o queda a menos de un cuadro.
        let near = if out { t.clips.get(i + 1).filter(|n| n.start <= p.end() + UNKNOWN_FRAME / 2) } else { i.checked_sub(1).map(|k| &t.clips[k]).filter(|n| n.end() >= p.start - UNKNOWN_FRAME / 2) };
        let max = self.loader.probe(&p.path).map(|q| q.duration).unwrap_or(p.src_out());
        Faded { at: (ti, i), orig: p.clone(), ov: t.joins(), neighbor: near.map(|n| n.len), max }
    }

    /// Mueve la esquina de arriba (`out`: la derecha) de la pieza `dt` desde donde estaba.
    pub(super) fn fade(&mut self, f: &Faded, out: bool, dt: i64) {
        let magnet = self.prefs.magnet;
        let o = &f.orig;
        // Hacia afuera de la pieza es positivo.
        let d = if out { dt } else { -dt };
        let (fade0, other) = if out { (o.fade_out, o.fade_in) } else { (o.fade_in, o.fade_out) };
        let fade_max = (o.len - other).max(0);
        let track = &mut self.tracks[f.at.0];
        let p = &mut track.clips[f.at.1];
        let Some(nb_len) = f.neighbor else {
            let v = (fade0 - d).clamp(0, fade_max);
            if out {
                p.fade_out = v;
            } else {
                p.fade_in = v;
            }
            return;
        };
        // Índice de la unión con la vecina en `ov`.
        let k = if out { f.at.1 + 1 } else { f.at.1 };
        let ov0 = f.ov.get(k).copied().unwrap_or(0);
        // Cuánto puede crecer la pieza hacia ese lado con lo que sobra del archivo.
        let room = o.line_len(if out { f.max - o.src_out() } else { o.src_in });
        // Positivo: cuánto se funde con la vecina; negativo: cuánto tarda en ir a negro.
        let s = (ov0 - fade0 + d).clamp(-fade_max, ov0.saturating_add(room).min(nb_len / 2).max(ov0));
        let grow = s.max(0) - ov0;
        p.len = o.len + grow;
        if out {
            p.fade_out = (-s).max(0);
        } else {
            p.src_in = o.src_at(o.start - grow);
            p.fade_in = (-s).max(0);
            if !magnet {
                p.start = o.start - grow;
            }
        }
        if magnet {
            let mut ov = f.ov.clone();
            if let Some(x) = ov.get_mut(k) {
                *x = s.max(0);
            }
            track.relayout(&ov);
        }
    }

    /// Tras cualquier cambio: pausar, soltar reproductores que ya no se usan y ubicar el cursor.
    /// Las piezas quedan en su lugar de una (deshacer y el teclado no se animan).
    pub(super) fn after_edit(&mut self, pos: i64) {
        self.settle = None;
        self.pause();
        let used: Vec<PathBuf> = self.tracks.iter().flat_map(|t| t.clips.iter().map(|p| p.path.clone())).collect();
        self.idle.retain(|v| used.contains(&v.path));
        for p in &used {
            self.loader.want_strip(p);
        }
        self.seek(pos);
    }

    pub(super) fn commit(&mut self, before: Option<Vec<Track>>, pos: i64) {
        for t in &mut self.tracks {
            t.sort();
        }
        if let Some(b) = before {
            if b != self.tracks {
                self.record_undo(b);
            }
        }
        self.after_edit(pos);
    }

    /// Lo que hay que exportar: las pistas de video visibles y las de audio, con sus fundidos.
    pub(super) fn edit(&self) -> Edit {
        let mut e = Edit { ratio: self.ratio, ..Edit::default() };
        for t in &self.tracks {
            let clips: Vec<Clip> = t
                .clips
                .iter()
                .zip(t.overlaps())
                .map(|(p, xfade)| {
                    let gain = if t.sounds() { p.eff_gain() } else { 0.0 };
                    Clip {
                        id: p.id,
                        path: p.path.clone(),
                        start: p.start,
                        len: p.len,
                        src_in: p.src_in,
                        speed: p.speed,
                        fade: (p.fade_in, p.fade_out),
                        xfade,
                        gain,
                        curves: p.curves,
                        frame: p.frame,
                    }
                })
                .collect();
            match t.kind {
                TrackKind::Video if !t.hidden => e.video.push(clips),
                TrackKind::Video => {}
                TrackKind::Audio => e.audio.push(clips),
            }
        }
        e
    }
}
