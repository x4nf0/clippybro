//! Pruebas del editor sin ventana (solo con `--features devtools`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;

use super::library;
use super::media::{self, Clip, Edit, Frame, Progress, Sizing};
use crate::mf::Mf;
use super::player::Player;
use super::timeline::{Piece, Track, TrackKind};
use super::EditorApp;

/// Pruebas sin ventana:
/// `--test-export <salida.mp4> <pista> <archivo> <ini_s> <fin_s> [<pista> <archivo> <ini_s> <fin_s> ...]` (ver `parts`)
/// `--test-frame <archivo> <t_s> <salida.ppm>`
fn secs(s: &str) -> i64 {
    (s.parse::<f64>().unwrap_or(0.0) * 1e7) as i64
}

/// Con CLIPPY_FRAME="cuartos,espejo,izq,arriba,der,abajo,encaje" (ej. "1,1,0,0,0.5,1,0"; encaje 0 entera,
/// 1 llenar, 2 estirar), la imagen de todas las piezas girada, espejada, recortada y encajada así.
fn test_frame() -> Frame {
    let Ok(v) = std::env::var("CLIPPY_FRAME") else { return Frame::default() };
    let n: Vec<f32> = v.split(',').map(|x| x.trim().parse().unwrap_or(0.0)).collect();
    let at = |i: usize, d: f32| n.get(i).copied().unwrap_or(d);
    Frame { turns: at(0, 0.0) as u8, flip: at(1, 0.0) != 0.0, crop: [at(2, 0.0), at(3, 0.0), at(4, 1.0), at(5, 1.0)], sizing: [Sizing::Whole, Sizing::Fill, Sizing::Stretch][(at(6, 0.0) as usize).min(2)] }
}

/// Con CLIPPY_RATIO="9:16", el lienzo con esa proporción.
fn test_ratio() -> Option<media::Ratio> {
    let v = std::env::var("CLIPPY_RATIO").ok()?;
    let (w, h) = v.split_once(':')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Piezas por pista ("v1", "a1"...), ordenadas por inicio y con sus fundidos cruzados. Cada parte:
/// `<pista>[:<inicio_s>[:<volumen 0..2>[:<aparece_s>[:<se_va_s>]]]] <archivo> <ini_s> <fin_s>`. `<pista>` es v1,
/// v2... (imagen), a1, a2... (sonido) o c1, c2... (las dos unidas, como entra un clip: en vN y aN); sin inicio
/// va después de la anterior de su pista. El `id` de cada pieza es el número de parte (las unidas lo comparten).
fn parts(args: &[String]) -> BTreeMap<String, Vec<Clip>> {
    let mut tracks: BTreeMap<String, Vec<Clip>> = BTreeMap::new();
    for (k, c) in args.chunks(4).enumerate() {
        let mut tok = c[0].split(':');
        let name = tok.next().unwrap_or("c1");
        let names = match name.strip_prefix('c') {
            Some(n) => vec![format!("v{n}"), format!("a{n}")],
            None => vec![name.to_string()],
        };
        let at = tok.next().filter(|s| !s.is_empty()).map(secs);
        let gain = tok.next().filter(|s| !s.is_empty()).and_then(|g| g.parse().ok()).unwrap_or(1.0);
        let fade = (tok.next().map_or(0, secs), tok.next().map_or(0, secs));
        let start = at.unwrap_or_else(|| tracks.get(&names[0]).and_then(|l| l.last()).map_or(0, Clip::end));
        for n in names {
            // Con CLIPPY_SPEED, todas a esa velocidad (el mismo tramo del archivo dura menos o más).
            let speed: f64 = std::env::var("CLIPPY_SPEED").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
            let src_in = secs(&c[2]);
            let len = if speed > 0.0 { ((secs(&c[3]) - src_in) as f64 / speed) as i64 } else { secs(&c[3]) - src_in };
            let frame = test_frame();
            let clip = Clip { id: k as u64 + 1, path: PathBuf::from(&c[1]), start, len, src_in, speed, fade, xfade: (0, 0), gain, curves: Default::default(), frame };
            tracks.entry(n).or_default().push(clip);
        }
    }
    for list in tracks.values_mut() {
        list.sort_by_key(|c| c.start);
        let overlaps = media::overlaps(&list.iter().map(|c| (c.start, c.end())).collect::<Vec<_>>());
        for (c, o) in list.iter_mut().zip(overlaps) {
            c.xfade = o;
        }
    }
    tracks
}

impl EditorApp {
    /// Con `CLIPPY_TEST_TIMELINE` (las mismas partes que `--test-export`, con `|` entre cada dato) el editor
    /// arranca con esa línea de tiempo: para sacar capturas sin tocar el mouse.
    pub(super) fn load_test_timeline(&mut self) {
        let Ok(spec) = std::env::var("CLIPPY_TEST_TIMELINE") else { return };
        let args: Vec<String> = spec.split('|').map(str::to_string).collect();
        let tracks = parts(&args);
        let shared = |id: u64| tracks.values().flatten().filter(|c| c.id == id).count() > 1;
        self.tracks.clear();
        for (name, list) in &tracks {
            let kind = if name.starts_with('v') { TrackKind::Video } else { TrackKind::Audio };
            let n: usize = name[1..].parse().unwrap_or(1);
            while self.count(kind) < n {
                self.next_id += 1;
                self.tracks.push(Track::new(self.next_id, kind));
            }
            let ti = (0..self.tracks.len()).filter(|&i| self.tracks[i].kind == kind).nth(n - 1).unwrap();
            for c in list {
                self.next_id += 1;
                let link = if shared(c.id) { 1000 + c.id } else { 0 };
                let (fade_in, fade_out) = c.fade;
                let p = Piece { id: self.next_id, path: c.path.clone(), start: c.start, len: c.len, src_in: c.src_in, speed: c.speed, gain: c.gain, muted: false, link, fade_in, fade_out, curves: c.curves, frame: c.frame };
                self.tracks[ti].clips.push(p);
                self.loader.want_strip(&c.path);
            }
        }
        if self.count(TrackKind::Video) == 0 {
            self.tracks.insert(0, Track::new(1, TrackKind::Video));
        }
        self.selected = self.tracks.iter().flat_map(|t| t.clips.first()).next().map(|p| p.id);
        self.ratio = test_ratio();
        self.next_id += 1000;
    }
}

pub fn test_cli(args: &[String]) -> i32 {
    let _mf = Mf::init();
    match args.first().map(|s| s.as_str()) {
        Some("--test-export") => {
            let out = PathBuf::from(&args[1]);
            let tracks = parts(&args[2..]);
            let mut edit = Edit { ratio: test_ratio(), ..Edit::default() };
            for (name, list) in tracks {
                if name.starts_with('v') {
                    edit.video.push(list);
                } else {
                    edit.audio.push(list);
                }
            }
            let cfg = crate::config::load();
            let target = std::env::var("CLIPPY_TARGET").ok().map(|v| {
                let p: Vec<&str> = v.split(',').collect();
                media::Target {
                    height: p.first().and_then(|x| x.parse().ok()).unwrap_or(0),
                    fps: p.get(1).and_then(|x| x.parse().ok()).unwrap_or(0),
                    bitrate: (p.get(2).and_then(|x| x.parse::<f64>().ok()).unwrap_or(8.0) * 1e6) as u32,
                    constant: p.get(3) == Some(&"c"),
                }
            });
            let opts = media::ExportOpts { bitrate: cfg.bitrate_mbps * 1_000_000, target };
            let t = Instant::now();
            match media::export(&edit, &out, &opts, &Progress::new()) {
                Ok(m) => {
                    println!("OK {m} en {:.2} s", t.elapsed().as_secs_f64());
                    0
                }
                Err(e) => {
                    println!("ERROR {e:?}");
                    1
                }
            }
        }
        Some("--test-strip") => {
            let path = PathBuf::from(&args[1]);
            let t = Instant::now();
            let pr = media::probe(&path).unwrap();
            println!("probe: {:?}", t.elapsed());
            let t = Instant::now();
            let w = media::waveform(&path).unwrap();
            println!("forma de onda ({} valores): {:?}", w.len(), t.elapsed());
            let t = Instant::now();
            let k = media::keyframes(&path).unwrap();
            println!("fotogramas clave ({}): {:?}", k.len(), t.elapsed());
            let t = Instant::now();
            let c = crate::clipcache::load(&path);
            println!("caché: {} en {:?}", if c.is_some() { "encontrada" } else { "no hay" }, t.elapsed());
            let t = Instant::now();
            let mut s = library::Strip::new(&path, library::strip_plan(pr.duration, &k), &pr);
            let mut n = 0;
            while let Some(i) = s.in_order() {
                n += s.frame(i).is_some() as usize;
            }
            println!("{n} miniaturas (abriendo el decodificador): {:?}", t.elapsed());
            0
        }
        Some("--test-covers") => {
            // Portadas de varios archivos, como las saca la biblioteca: una por una y en varios hilos.
            for threads in [1, 4] {
                let t = Instant::now();
                let next = std::sync::atomic::AtomicUsize::new(1);
                std::thread::scope(|s| {
                    for _ in 0..threads {
                        s.spawn(|| {
                            let _mf = Mf::init();
                            while let Some(f) = args.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) {
                                let path = PathBuf::from(f);
                                let pr = media::probe(&path).unwrap();
                                library::cover_image(&path, &pr).unwrap();
                            }
                        });
                    }
                });
                println!("{} portadas ({threads} hilos): {:?}", args.len() - 1, t.elapsed());
            }
            0
        }
        Some("--test-frame") => {
            let path = PathBuf::from(&args[1]);
            let probe = media::probe(&path).unwrap();
            let p = Player::open(path, &probe, egui::Context::default(), true);
            std::thread::sleep(Duration::from_millis(800));
            p.seek(secs(&args[2]));
            std::thread::sleep(Duration::from_millis(800));
            if let Some(e) = p.shared.error.lock().unwrap().clone() {
                println!("ERROR {e}");
                return 1;
            }
            let f = p.shared.frame.lock().unwrap().clone();
            match f {
                Some(f) => {
                    let [w, h] = f.size;
                    let mut ppm = format!("P6 {w} {h} 255\n").into_bytes();
                    for px in &f.pixels {
                        ppm.extend_from_slice(&[px.r(), px.g(), px.b()]);
                    }
                    std::fs::write(&args[3], ppm).unwrap();
                    println!("OK {w}x{h}");
                    0
                }
                None => {
                    println!("ERROR sin cuadro");
                    1
                }
            }
        }
        _ => 2,
    }
}
