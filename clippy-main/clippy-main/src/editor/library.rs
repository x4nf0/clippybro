//! Biblioteca de videos: lista los clips de la carpeta (y de sus subcarpetas) + los importados, los mueve y los
//! manda a la Papelera, y en segundo plano calcula duración, miniatura de portada, tira de miniaturas y forma de
//! onda para la línea de tiempo.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::os::windows::ffi::OsStrExt;
use std::time::SystemTime;

use eframe::egui;

use super::media::{self, Probe};
use crate::i18n::{tr, trf};
use crate::mf::Mf;
use super::player::Video;
use crate::log::app_dir;

use crate::clipcache::Wave;
pub const VIDEO_EXTS: [&str; 6] = ["mp4", "mov", "m4v", "mkv", "wmv", "avi"];

/// Una carpeta dentro de la de los clips.
#[derive(Clone)]
pub struct Folder {
    pub path: PathBuf,
    pub name: String,
    /// Videos que tiene.
    pub count: usize,
}

pub struct Asset {
    pub path: PathBuf,
    pub title: String,
    pub size: u64,
    pub modified: SystemTime,
    pub imported: bool,
}

#[derive(Default)]
pub struct Info {
    pub probe: Option<Probe>,
    pub error: Option<String>,
    pub cover: Option<egui::TextureHandle>,
    /// La tira: todas las miniaturas que va a tener, ordenadas por tiempo. La imagen de cada una llega
    /// después, primero las que se ven.
    pub thumbs: Vec<Thumb>,
    /// Miniaturas que la línea de tiempo muestra y todavía no llegaron (de izquierda a derecha): van primero.
    pub wanted: Vec<usize>,
    pub wave: Option<Wave>,
    pub keyframes: Option<Vec<i64>>,
    /// Tamaño y fecha del archivo cuando se leyó: si cambian, se vuelve a leer.
    stamp: Option<(u64, SystemTime)>,
    cover_requested: bool,
    strip_requested: bool,
}

/// Una miniatura de la tira: de dónde se saca, el fotograma clave anterior y la imagen cuando ya está.
#[derive(Clone)]
pub struct Thumb {
    pub at: i64,
    key: i64,
    pub tex: Option<egui::TextureHandle>,
}

type Infos = Arc<Mutex<HashMap<PathBuf, Info>>>;
type Queue = Arc<(Mutex<VecDeque<PathBuf>>, Condvar)>;

/// Hilos que sacan portadas a la vez: con una biblioteca grande, cuatro las hacen en la mitad de tiempo que uno.
const COVER_THREADS: usize = 4;

/// Cada cosa en su fila, para que ninguna espere a otra: portadas (varios hilos), tiras (un hilo que atiende
/// primero lo que se ve de cualquier clip) y formas de onda (un hilo: decodificar audio es pesado).
pub struct Loader {
    pub info: Infos,
    covers: Queue,
    strips: Queue,
}

impl Loader {
    pub fn new(ctx: egui::Context) -> Self {
        let info = Infos::default();
        std::thread::spawn(crate::clipcache::prune);
        let (covers, strips, waves) = (Queue::default(), Queue::default(), Queue::default());
        for _ in 0..COVER_THREADS {
            let (info, queue, ctx) = (info.clone(), covers.clone(), ctx.clone());
            std::thread::spawn(move || worker(info, queue, ctx, cover));
        }
        {
            let (info, queue, ctx) = (info.clone(), waves.clone(), ctx.clone());
            std::thread::spawn(move || worker(info, queue, ctx, wave));
        }
        {
            let (info, queue, ctx) = (info.clone(), strips.clone(), ctx.clone());
            std::thread::spawn(move || strip_worker(info, queue, waves, ctx));
        }
        Self { info, covers, strips }
    }

    /// Pone el archivo primero en la fila: lo último que se pidió es lo que se está mirando.
    fn push(queue: &Queue, path: &Path) {
        let (q, cv) = &**queue;
        q.lock().unwrap().push_front(path.to_path_buf());
        cv.notify_one();
    }

    /// Pide la portada (para una tarjeta que se está viendo: va primero en la fila).
    /// `stamp` es el tamaño y la fecha del archivo: si cambiaron desde la última vez (por ejemplo,
    /// un clip que la grabadora estaba terminando de escribir), se vuelve a leer todo.
    pub fn want_cover(&self, path: &Path, stamp: (u64, SystemTime)) {
        let mut info = self.info.lock().unwrap();
        let e = info.entry(path.to_path_buf()).or_default();
        let mut strip = false;
        if e.stamp != Some(stamp) {
            strip = e.strip_requested;
            *e = Info { stamp: Some(stamp), ..Info::default() };
        }
        let cover = !e.cover_requested;
        e.cover_requested = true;
        e.strip_requested |= strip;
        drop(info);
        if strip {
            Self::push(&self.strips, path);
        }
        if cover {
            Self::push(&self.covers, path);
        }
    }

    /// Pide la tira de miniaturas y la forma de onda (prioridad: está en la línea de tiempo).
    pub fn want_strip(&self, path: &Path) {
        let mut info = self.info.lock().unwrap();
        let e = info.entry(path.to_path_buf()).or_default();
        if !e.strip_requested {
            e.strip_requested = true;
            drop(info);
            Self::push(&self.strips, path);
        }
    }

    pub fn probe(&self, path: &Path) -> Option<Probe> {
        if let Some(p) = self.info.lock().unwrap().get(path).and_then(|i| i.probe.clone()) {
            return Some(p);
        }
        let p = media::probe(path).ok()?;
        self.info.lock().unwrap().entry(path.to_path_buf()).or_default().probe = Some(p.clone());
        Some(p)
    }
}

fn worker(info: Infos, queue: Queue, ctx: egui::Context, job: fn(&Path, &Infos, &egui::Context)) {
    let _mf = Mf::init();
    loop {
        let p = {
            let (q, cv) = &*queue;
            let mut q = q.lock().unwrap();
            while q.is_empty() {
                q = cv.wait(q).unwrap();
            }
            q.pop_front().unwrap()
        };
        job(&p, &info, &ctx);
        ctx.request_repaint();
    }
}

fn ensure_probe(p: &Path, info: &Mutex<HashMap<PathBuf, Info>>) -> Option<Probe> {
    if let Some(pr) = info.lock().unwrap().get(p).and_then(|i| i.probe.clone()) {
        return Some(pr);
    }
    match media::probe(p) {
        Ok(pr) => {
            info.lock().unwrap().entry(p.to_path_buf()).or_default().probe = Some(pr.clone());
            Some(pr)
        }
        Err(e) => {
            info.lock().unwrap().entry(p.to_path_buf()).or_default().error = Some(e.to_string());
            None
        }
    }
}

fn cover(p: &Path, info: &Infos, ctx: &egui::Context) {
    let Some(pr) = ensure_probe(p, info) else { return };
    if let Some(img) = cover_image(p, &pr) {
        let tex = ctx.load_texture(format!("cover:{}", p.display()), img, egui::TextureOptions::LINEAR);
        info.lock().unwrap().entry(p.to_path_buf()).or_default().cover = Some(tex);
    }
}

/// Portada: el fotograma clave anterior al segundo 1 (o a la mitad, si el clip es más corto), que sale de un
/// solo cuadro. Por CPU: para una imagen suelta, abrir el decodificador por CPU tarda unos 60 ms y por GPU 300.
pub(super) fn cover_image(p: &Path, pr: &Probe) -> Option<egui::ColorImage> {
    let (w, h) = fit(pr, 256);
    let mut v = Video::open_cpu(p, w, h).ok()?;
    v.key_frame_before((pr.duration / 2).min(10_000_000)).ok().flatten()
}

/// Forma de onda de un clip que no la tiene en la caché (los importados): decodifica todo el audio.
fn wave(p: &Path, info: &Infos, ctx: &egui::Context) {
    let kf = info.lock().unwrap().get(p).and_then(|e| e.keyframes.clone());
    let kf = kf.unwrap_or_else(|| media::keyframes(p).unwrap_or_default());
    let wave = media::waveform(p).unwrap_or_default();
    crate::clipcache::save(p, &wave, &kf);
    info.lock().unwrap().entry(p.to_path_buf()).or_default().wave = Some(wave);
    ctx.request_repaint();
}

/// Decodificadores de tira abiertos a la vez, como mucho: abrir uno tarda unos 300 ms, pero cada uno ocupa
/// memoria de video. Si hace falta otro, se cierra el que hace más que no se usa.
const OPEN_STRIPS: usize = 3;

/// Una tira en curso: qué miniaturas faltan y su decodificador (abierto mientras hace falta).
pub(super) struct Strip {
    path: PathBuf,
    plan: Vec<Thumb>,
    size: (u32, u32),
    done: Vec<bool>,
    next: usize,
    video: Option<Video>,
    used: u64,
}

impl Strip {
    pub(super) fn new(path: &Path, plan: Vec<Thumb>, pr: &Probe) -> Strip {
        let done = vec![false; plan.len()];
        Strip { path: path.to_path_buf(), plan, size: fit(pr, 160), done, next: 0, video: None, used: 0 }
    }

    /// La primera de `wanted` (las que la pantalla muestra vacías) que falte.
    fn urgent(&self, wanted: &[usize]) -> Option<usize> {
        wanted.iter().copied().find(|&i| i < self.done.len() && !self.done[i])
    }

    /// La siguiente que falte, en orden.
    pub(super) fn in_order(&mut self) -> Option<usize> {
        while self.next < self.done.len() && self.done[self.next] {
            self.next += 1;
        }
        (self.next < self.done.len()).then_some(self.next)
    }

    /// Saca la imagen de la miniatura `i`. Si sigue en el mismo tramo que la anterior, lee hacia adelante
    /// sin volver a buscar, así que recorrer la tira en orden decodifica cada cuadro una sola vez.
    pub(super) fn frame(&mut self, i: usize) -> Option<egui::ColorImage> {
        self.done[i] = true;
        if self.video.is_none() {
            match Video::open(&self.path, self.size.0, self.size.1, true) {
                Ok(v) => self.video = Some(v),
                Err(_) => {
                    // No se puede decodificar: no hay tira.
                    self.done.fill(true);
                    return None;
                }
            }
        }
        let t = &self.plan[i];
        self.video.as_mut()?.frame_ahead(t.at, t.key).ok().flatten()
    }
}

/// Arma la tira de un clip: el plan de miniaturas (con los fotogramas clave de la caché o del índice del
/// archivo, que salen enseguida) y, si la forma de onda no está en la caché, la pide a su fila.
fn start_strip(p: &Path, info: &Infos, waves: &Queue, ctx: &egui::Context) -> Option<Strip> {
    let pr = ensure_probe(p, info)?;
    let (wave, kf) = match crate::clipcache::load(p) {
        Some((w, k)) => (Some(w), k),
        None => (None, media::keyframes(p).unwrap_or_default()),
    };
    let plan = strip_plan(pr.duration, &kf);
    {
        let mut i = info.lock().unwrap();
        let e = i.entry(p.to_path_buf()).or_default();
        e.keyframes = Some(kf);
        e.thumbs = plan.clone();
        match wave {
            Some(w) => e.wave = Some(w),
            None => Loader::push(waves, p),
        }
    }
    ctx.request_repaint();
    Some(Strip::new(p, plan, &pr))
}

/// Hilo de las tiras: atiende todas las que hay en la línea de tiempo a la vez. Primero las miniaturas que la
/// pantalla muestra vacías, de cualquier clip; cuando no falta ninguna, sigue en orden con la tira pedida
/// más recientemente.
fn strip_worker(info: Infos, queue: Queue, waves: Queue, ctx: egui::Context) {
    let _mf = Mf::init();
    let mut strips: Vec<Strip> = Vec::new();
    let mut tick = 0;
    loop {
        let new: Vec<PathBuf> = {
            let (q, cv) = &*queue;
            let mut q = q.lock().unwrap();
            while q.is_empty() && strips.is_empty() {
                q = cv.wait(q).unwrap();
            }
            q.drain(..).collect()
        };
        // La fila tiene lo más reciente adelante: se agregan de la más vieja a la más nueva, cada una primera.
        for p in new.into_iter().rev() {
            strips.retain(|s| s.path != p);
            if let Some(s) = start_strip(&p, &info, &waves, &ctx) {
                strips.insert(0, s);
            }
        }
        let urgent = {
            let i = info.lock().unwrap();
            strips.iter().enumerate().find_map(|(k, s)| Some((k, s.urgent(&i.get(&s.path)?.wanted)?)))
        };
        let Some((k, t)) = urgent.or_else(|| strips.iter_mut().enumerate().find_map(|(k, s)| Some((k, s.in_order()?)))) else {
            strips.clear();
            continue;
        };
        tick += 1;
        strips[k].used = tick;
        if strips[k].video.is_none() && strips.iter().filter(|s| s.video.is_some()).count() >= OPEN_STRIPS {
            if let Some(old) = strips.iter_mut().filter(|s| s.video.is_some()).min_by_key(|s| s.used) {
                old.video = None;
            }
        }
        let s = &mut strips[k];
        if let Some(img) = s.frame(t) {
            let at = s.plan[t].at;
            let tex = ctx.load_texture(format!("strip:{}:{at}", s.path.display()), img, egui::TextureOptions::LINEAR);
            // Si el archivo cambió mientras tanto, la tira es otra: esta imagen ya no va.
            if let Some(th) = info.lock().unwrap().get_mut(&s.path).and_then(|e| e.thumbs.get_mut(t)).filter(|th| th.at == at) {
                th.tex = Some(tex);
            }
            ctx.request_repaint();
        }
        // Las terminadas sueltan su decodificador.
        strips.retain(|s| s.done.contains(&false));
    }
}

/// Cuánto después de un fotograma clave se pide el cuadro: el índice del archivo da su tiempo redondeado
/// y sin el corrimiento de los cuadros B, y pedirlo justo encima podría hacer buscar desde el clave anterior.
const KEY_MARGIN: i64 = 500_000; // 50 ms

/// Las miniaturas que va a tener la tira: una por segundo (150 como mucho), cada una sobre el fotograma
/// clave más cercano si hay uno a menos de medio paso (así sale de un solo cuadro).
pub(super) fn strip_plan(duration: i64, keys: &[i64]) -> Vec<Thumb> {
    let step = (duration / 150).max(10_000_000);
    let mut plan: Vec<Thumb> = Vec::new();
    let mut t = 0;
    while t < duration {
        let prev = plan.last().map_or(i64::MIN, |p| p.at);
        let from = keys.partition_point(|&k| k <= t - step / 2);
        let near = keys[from..].iter().copied().take_while(|&k| k < t + step / 2).filter(|&k| k > prev).min_by_key(|&k| (k - t).abs());
        let at = near.map_or(t, |k| (k + KEY_MARGIN).min(duration - 1));
        // Sin índice de claves no se sabe hasta dónde conviene seguir leyendo: se busca cada vez.
        let key = keys[..keys.partition_point(|&k| k <= at)].last().copied().unwrap_or(at);
        plan.push(Thumb { at, key, tex: None });
        t += step;
    }
    plan
}

/// La miniatura que muestra un cuadro de la línea de tiempo que abarca `lo..=hi` del archivo: de las que caen
/// adentro, la más barata de sacar (y entre esas, la más cercana al centro); si el cuadro abarca menos que la
/// distancia entre miniaturas, la más cercana. Es siempre la misma llegue cuando llegue, así el cuadro queda
/// vacío hasta que está y después no cambia de imagen.
pub fn thumb_for(thumbs: &[Thumb], lo: i64, hi: i64) -> Option<usize> {
    let (a, b) = (thumbs.partition_point(|t| t.at < lo), thumbs.partition_point(|t| t.at <= hi));
    let c = (lo + hi) / 2;
    let dist = |i: usize| (thumbs[i].at - c).abs();
    if a < b {
        return (a..b).min_by_key(|&i| (thumbs[i].at - thumbs[i].key, dist(i)));
    }
    [a.checked_sub(1), Some(a)].into_iter().flatten().filter(|&i| i < thumbs.len()).min_by_key(|&i| dist(i))
}

fn fit(p: &Probe, w: u32) -> (u32, u32) {
    let w = w.min(p.width.max(2)) & !1;
    let h = ((w as u64 * p.height as u64 / p.width.max(1) as u64) as u32).max(2) & !1;
    (w, h)
}

// --- Lista de archivos ---------------------------------------------------------------

fn library_file() -> PathBuf {
    app_dir().join("library.json")
}

pub fn load_imported() -> Vec<PathBuf> {
    std::fs::read_to_string(library_file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_imported(v: &[PathBuf]) {
    let _ = std::fs::create_dir_all(app_dir());
    let _ = std::fs::write(library_file(), serde_json::to_string_pretty(v).unwrap_or_default());
}

pub fn is_video(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO_EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// Videos de `dir` que se muestran (el archivo temporal de una exportación en curso, no).
fn videos_in(dir: &Path) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| is_video(p) && !p.to_string_lossy().contains("clippy-tmp"))
}

/// Las carpetas de adentro de la de los clips (`root`) y los videos que se ven: en la de los clips (`dir` None), los
/// suyos y los importados; dentro de una carpeta, los de ella. Los más nuevos primero.
pub fn scan(root: &Path, dir: Option<&Path>, imported: &[PathBuf]) -> (Vec<Folder>, Vec<Asset>) {
    let mut v: Vec<Asset> = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |p: PathBuf, imported: bool| {
        if !seen.insert(p.clone()) {
            return;
        }
        if let Ok(m) = std::fs::metadata(&p) {
            v.push(Asset {
                title: pretty_name(&p),
                size: m.len(),
                modified: m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                path: p,
                imported,
            });
        }
    };
    for p in videos_in(dir.unwrap_or(root)) {
        add(p, false);
    }
    if dir.is_none() {
        for p in imported {
            add(p.clone(), true);
        }
    }
    let rd = std::fs::read_dir(root).into_iter().flatten().flatten();
    let mut folders: Vec<Folder> = rd
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| Folder { name: e.file_name().to_string_lossy().into_owned(), count: videos_in(&e.path()).count(), path: e.path() })
        .collect();
    folders.sort_by_key(|f| f.name.to_lowercase());
    v.sort_by_key(|a| std::cmp::Reverse(a.modified));
    (folders, v)
}

/// Repite `op` un rato si falla: el editor (o el que lee las miniaturas) puede tener el archivo abierto un
/// instante más.
fn retry<T>(mut op: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut left = 20;
    loop {
        match op() {
            Err(_) if left > 0 => {
                left -= 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            r => return r,
        }
    }
}

/// Manda un archivo o una carpeta a la Papelera de reciclaje. Si ese disco no tiene Papelera, Windows avisa
/// antes de borrar para siempre.
pub fn recycle(p: &Path) -> std::io::Result<()> {
    use windows::Win32::UI::Shell::*;
    let from: Vec<u16> = p.as_os_str().encode_wide().chain([0, 0]).collect();
    retry(|| {
        let mut op = SHFILEOPSTRUCTW {
            wFunc: FO_DELETE,
            pFrom: windows::core::PCWSTR(from.as_ptr()),
            fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOF_WANTNUKEWARNING).0 as u16,
            ..Default::default()
        };
        let r = unsafe { SHFileOperationW(&mut op) };
        if r == 0 && !op.fAnyOperationsAborted.as_bool() && !p.exists() {
            Ok(())
        } else {
            Err(std::io::Error::other(trf!(
                "no se pudo mandar a la Papelera ({r})",
                "couldn't send it to the Recycle Bin ({r})",
                "não foi possível mandar para a Lixeira ({r})"
            )))
        }
    })
}

/// Mueve un video a la carpeta `dir` (si ahí hay uno que se llama igual, le agrega un número). Su caché se va con
/// él. Devuelve dónde quedó.
pub fn move_into(p: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut to = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while to.exists() {
        to = dir.join(format!("{stem} ({n}).{ext}"));
        n += 1;
    }
    let cache = crate::clipcache::cache_file(p);
    retry(|| std::fs::rename(p, &to))?;
    let _ = std::fs::rename(cache, crate::clipcache::cache_file(&to));
    Ok(to)
}

/// Le cambia el nombre a una carpeta (`from` -> `to`); las cachés de sus videos se van con ellos. Devuelve dónde
/// quedó cada video.
pub fn rename_folder(from: &Path, to: &Path) -> std::io::Result<Vec<(PathBuf, PathBuf)>> {
    let videos: Vec<(PathBuf, PathBuf)> = videos_in(from).map(|p| {
        let c = crate::clipcache::cache_file(&p);
        (p, c)
    }).collect();
    retry(|| std::fs::rename(from, to))?;
    Ok(videos
        .into_iter()
        .map(|(p, cache)| {
            let new = to.join(p.file_name().unwrap_or_default());
            let _ = std::fs::rename(cache, crate::clipcache::cache_file(&new));
            (p, new)
        })
        .collect())
}

/// Un nombre de carpeta que Windows acepta: sin caracteres prohibidos ni puntos o espacios al final.
pub fn folder_name_ok(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty() && !n.ends_with('.') && !n.chars().any(|c| BAD_NAME.contains(c) || c.is_control())
}

/// `name` como nombre de archivo o carpeta que Windows acepta: los caracteres prohibidos pasan a "_" y se
/// sacan los espacios y puntos de los bordes. Puede quedar vacío.
pub fn clean_name(name: &str) -> String {
    let s: String = name.chars().map(|c| if BAD_NAME.contains(c) || c.is_control() { '_' } else { c }).collect();
    s.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string()
}

/// Caracteres que Windows no deja usar en un nombre de archivo o carpeta.
const BAD_NAME: &str = "<>:\"/\\|?*";

/// "Clippy_2026-11-15_23-38-30" -> "15/11 · 23:38:30" (en inglés, "11/15 · 23:38:30")
pub fn pretty_name(p: &Path) -> String {
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    if let Some(rest) = stem.strip_prefix("Clippy_") {
        let parts: Vec<&str> = rest.split(['_', '-']).collect();
        if parts.len() >= 6 && parts[..6].iter().all(|x| x.chars().all(|c| c.is_ascii_digit())) {
            let extra = if parts.len() > 6 { format!(" ({})", parts[6..].join(" ")) } else { String::new() };
            // Día y mes en el orden de cada idioma (en inglés, mes/día).
            let (a, b) = tr!((parts[2], parts[1]), (parts[1], parts[2]), (parts[2], parts[1]));
            return format!("{a}/{b} · {}:{}:{}{extra}", parts[3], parts[4], parts[5]);
        }
    }
    stem
}
