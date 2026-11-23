//! Reproductor de vista previa: video decodificado por GPU (si se puede) y audio por WASAPI.
//! Dos hilos (video y audio) siguen un mismo reloj.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use eframe::egui;
use windows::core::Interface;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

use super::media::{self, d3d_manager, pack, reader, seek, AUDIO, EOS, VIDEO};
use crate::i18n::tr;
use crate::mf::{self, frames_at, Mf, FRAME_BYTES};

/// Reloj de reproducción compartido (100 ns): si avanza, desde dónde y cuándo, y a qué velocidad (1 = normal).
pub struct Clock(Mutex<(bool, Instant, i64, f64)>);

impl Clock {
    pub fn new() -> Self {
        Clock(Mutex::new((false, Instant::now(), 0, 1.0)))
    }
    fn at(g: &(bool, Instant, i64, f64)) -> i64 {
        if g.0 {
            g.2 + (g.1.elapsed().as_nanos() as f64 / 100.0 * g.3) as i64
        } else {
            g.2
        }
    }
    pub fn pos(&self) -> i64 {
        Self::at(&self.0.lock().unwrap())
    }
    pub fn playing(&self) -> bool {
        self.0.lock().unwrap().0
    }
    pub fn set(&self, pos: i64, playing: bool) {
        let mut g = self.0.lock().unwrap();
        *g = (playing, Instant::now(), pos, g.3);
    }
    /// Cambia la velocidad sin saltar.
    pub fn set_rate(&self, rate: f64) {
        let mut g = self.0.lock().unwrap();
        *g = (g.0, Instant::now(), Self::at(&g), rate);
    }
    fn stop_at(&self, pos: i64) {
        self.set(pos, false);
    }
}

#[derive(Default)]
pub struct Shared {
    /// Último cuadro, ya en el formato de egui: la textura lo comparte sin copiarlo.
    pub frame: Mutex<Option<Arc<egui::ColorImage>>>,
    pub gen: AtomicU64,
    pub error: Mutex<Option<String>>,
}

enum VCmd {
    Seek(i64),
    Play,
    Pause,
}

enum ACmd {
    Play(i64),
    Pause,
}

pub struct Player {
    pub clock: Arc<Clock>,
    pub shared: Arc<Shared>,
    pub duration: i64,
    gain: Arc<AtomicU32>,
    /// Velocidad (bits de un f64): el sonido se estira o se comprime, como una cinta.
    speed: Arc<AtomicU64>,
    vtx: Option<Sender<VCmd>>,
    atx: Option<Sender<ACmd>>,
}

impl Player {
    /// Con `video` solo se decodifica la imagen (una pieza de una pista de video); sin él, solo el sonido.
    pub fn open(path: PathBuf, probe: &media::Probe, ctx: egui::Context, video: bool) -> Player {
        let clock = Arc::new(Clock::new());
        let shared = Arc::new(Shared::default());
        // Vista previa a 960 px de ancho como máximo: alcanza y es liviana.
        let pw = probe.width.clamp(2, 960) & !1;
        let ph = ((pw as u64 * probe.height as u64 / probe.width.max(1) as u64) as u32).max(2) & !1;

        let vtx = video.then(|| {
            let (vtx, vrx) = mpsc::channel();
            let (path, clock, shared, ctx) = (path.clone(), clock.clone(), shared.clone(), ctx.clone());
            std::thread::spawn(move || video_thread(path, pw, ph, clock, vrx, shared, ctx));
            vtx
        });
        let gain = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let speed = Arc::new(AtomicU64::new(1.0f64.to_bits()));
        let atx = if !video && probe.audio.is_some() {
            let (atx, arx) = mpsc::channel();
            let (path, gain, speed) = (path.clone(), gain.clone(), speed.clone());
            std::thread::spawn(move || audio_thread(path, arx, gain, speed));
            Some(atx)
        } else {
            None
        };
        Player { clock, shared, duration: probe.duration, gain, speed, vtx, atx }
    }

    pub fn pos(&self) -> i64 {
        self.clock.pos().clamp(0, self.duration)
    }

    pub fn playing(&self) -> bool {
        self.clock.playing()
    }

    pub fn play(&self) {
        let p = self.pos();
        self.clock.set(p, true);
        self.send(VCmd::Play);
        if let Some(a) = &self.atx {
            let _ = a.send(ACmd::Play(p));
        }
    }

    pub fn pause(&self) {
        let p = self.pos();
        self.clock.stop_at(p);
        self.send(VCmd::Pause);
        if let Some(a) = &self.atx {
            let _ = a.send(ACmd::Pause);
        }
    }

    fn send(&self, c: VCmd) {
        if let Some(v) = &self.vtx {
            let _ = v.send(c);
        }
    }

    /// Volumen de la vista previa (0 = mudo, 1 = normal, 2 = doble).
    pub fn set_gain(&self, g: f32) {
        self.gain.store(g.to_bits(), Ordering::Relaxed);
    }

    /// Velocidad de la vista previa (1 = normal, 0 = imagen quieta).
    pub fn set_speed(&self, s: f64) {
        if self.speed.swap(s.to_bits(), Ordering::Relaxed) != s.to_bits() {
            self.clock.set_rate(s);
        }
    }

    pub fn seek(&self, t: i64) {
        let t = t.clamp(0, self.duration);
        let playing = self.playing();
        self.clock.set(t, playing);
        self.send(VCmd::Seek(t));
        if playing {
            if let Some(a) = &self.atx {
                let _ = a.send(ACmd::Play(t));
            }
        }
    }
}

// --- Video ------------------------------------------------------------------------

pub(super) struct Video {
    r: IMFSourceReader,
    w: u32,
    h: u32,
    stride: i32,
    /// Hasta dónde se leyó: el fin del último cuadro que salió del decodificador.
    read: Option<i64>,
}

impl Video {
    /// Decodificador por CPU, para un cuadro suelto (la portada): abre en unos 60 ms contra 300 por GPU.
    pub(super) fn open_cpu(path: &std::path::Path, w: u32, h: u32) -> Result<Video> {
        unsafe { Self::configure(path, w, h, None, true) }
    }

    /// Con `still` es para cuadros sueltos (portada, miniaturas): el decodificador entrega cada cuadro
    /// apenas lo tiene en vez de juntar varios, y cada búsqueda sale unos 20 ms más rápido.
    pub(super) fn open(path: &std::path::Path, w: u32, h: u32, still: bool) -> Result<Video> {
        unsafe {
            // Primero por GPU; si algo falla, decodificación por CPU.
            if let Ok(mgr) = d3d_manager() {
                if let Ok(mut v) = Self::configure(path, w, h, Some(&mgr), still) {
                    if v.next().is_ok() {
                        seek(&v.r, 0)?;
                        v.read = None;
                        return Ok(v);
                    }
                }
            }
            Self::configure(path, w, h, None, still)
        }
    }

    unsafe fn configure(path: &std::path::Path, w: u32, h: u32, mgr: Option<&IMFDXGIDeviceManager>, still: bool) -> Result<Video> {
        let r = reader(path, true, mgr, still)?;
        r.SetStreamSelection(media::ANY, false)?;
        r.SetStreamSelection(VIDEO, true)?;
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
        r.SetCurrentMediaType(VIDEO, None, &t).map_err(|_| {
            anyhow!(tr!(
                "Windows no puede decodificar este video. Si es HEVC, instalá \"Extensiones de video HEVC\" desde Microsoft Store.",
                "Windows can't decode this video. If it's HEVC, install \"HEVC Video Extensions\" from the Microsoft Store.",
                "O Windows não consegue decodificar este vídeo. Se for HEVC, instale \"Extensões de Vídeo HEVC\" pela Microsoft Store."
            ))
        })?;
        let cur = r.GetCurrentMediaType(VIDEO)?;
        let stride = cur.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or((w * 4) as i32);
        Ok(Video { r, w, h, stride, read: None })
    }

    /// Siguiente cuadro decodificado: (pts, duración, muestra). None al final.
    fn next(&mut self) -> Result<Option<(i64, i64, IMFSample)>> {
        unsafe {
            loop {
                let (mut flags, mut ts, mut s) = (0u32, 0i64, None);
                self.r.ReadSample(VIDEO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut s))?;
                if let Some(s) = s {
                    let d = s.GetSampleDuration().unwrap_or(166_666);
                    self.read = Some(ts + d);
                    return Ok(Some((ts, d, s)));
                }
                if flags & EOS != 0 {
                    return Ok(None);
                }
            }
        }
    }

    fn to_image(&self, s: &IMFSample) -> Result<egui::ColorImage> {
        let (w, h) = (self.w as usize, self.h as usize);
        let mut pixels = vec![egui::Color32::BLACK; w * h];
        unsafe {
            let b = s.ConvertToContiguousBuffer()?;
            let copy = |base: *const u8, pitch: isize, out: &mut [egui::Color32]| {
                for y in 0..h {
                    let src = std::slice::from_raw_parts(base.offset(y as isize * pitch), w * 4);
                    for (d, s) in out[y * w..(y + 1) * w].iter_mut().zip(src.as_chunks::<4>().0) {
                        *d = egui::Color32::from_rgb(s[2], s[1], s[0]);
                    }
                }
            };
            if let Ok(b2) = b.cast::<IMF2DBuffer>() {
                let mut p = std::ptr::null_mut();
                let mut pitch = 0i32;
                b2.Lock2D(&mut p, &mut pitch)?;
                copy(p, pitch as isize, &mut pixels);
                b2.Unlock2D()?;
            } else {
                let mut p = std::ptr::null_mut();
                b.Lock(&mut p, None, None)?;
                let pitch = self.stride as isize;
                let base = if pitch < 0 { p.offset(-pitch * (h as isize - 1)) } else { p };
                copy(base, pitch, &mut pixels);
                b.Unlock()?;
            }
        }
        Ok(egui::ColorImage { size: [w, h], pixels })
    }

    /// Busca y devuelve el cuadro visible en `t`.
    pub(super) fn frame_at(&mut self, t: i64) -> Result<Option<egui::ColorImage>> {
        unsafe { seek(&self.r, t)? };
        self.read = None;
        self.decode_to(t)
    }

    /// Como `frame_at`, pero si lo último que se leyó ya pasó `key` (el fotograma clave anterior a `t`)
    /// sigue desde ahí sin buscar: recorrer el clip hacia adelante decodifica cada cuadro una sola vez.
    pub(super) fn frame_ahead(&mut self, t: i64, key: i64) -> Result<Option<egui::ColorImage>> {
        if self.read.is_some_and(|r| r >= key && r <= t) {
            self.decode_to(t)
        } else {
            self.frame_at(t)
        }
    }

    /// El fotograma clave anterior a `t` (o en `t`): sale de un solo cuadro, sin decodificar hasta `t`.
    pub(super) fn key_frame_before(&mut self, t: i64) -> Result<Option<egui::ColorImage>> {
        unsafe { seek(&self.r, t)? };
        self.read = None;
        self.next()?.map(|(_, _, s)| self.to_image(&s)).transpose()
    }

    fn decode_to(&mut self, t: i64) -> Result<Option<egui::ColorImage>> {
        let mut last = None;
        while let Some((ts, d, s)) = self.next()? {
            let hit = ts + d > t;
            last = Some(s);
            if hit {
                break;
            }
        }
        last.map(|s| self.to_image(&s)).transpose()
    }
}

fn publish(shared: &Shared, ctx: &egui::Context, img: egui::ColorImage) {
    *shared.frame.lock().unwrap() = Some(Arc::new(img));
    shared.gen.fetch_add(1, Ordering::Relaxed);
    ctx.request_repaint();
}

fn video_thread(path: PathBuf, w: u32, h: u32, clock: Arc<Clock>, rx: Receiver<VCmd>, shared: Arc<Shared>, ctx: egui::Context) {
    let _mf = Mf::init();
    let mut v = match Video::open(&path, w, h, false) {
        Ok(v) => v,
        Err(e) => {
            *shared.error.lock().unwrap() = Some(e.to_string());
            ctx.request_repaint();
            return;
        }
    };
    if let Ok(Some(f)) = v.frame_at(0) {
        publish(&shared, &ctx, f);
    }
    let mut playing = false;
    let mut pending: Option<(i64, i64, IMFSample)> = None;
    loop {
        let first = if playing {
            let wait = match &pending {
                Some((ts, _, _)) => ((ts - clock.pos()) / 10_000).clamp(0, 30) as u64,
                None => 0,
            };
            match rx.recv_timeout(Duration::from_millis(wait)) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        };
        let mut seek_to = None;
        for c in first.into_iter().chain(rx.try_iter()) {
            match c {
                VCmd::Play => playing = true,
                VCmd::Pause => playing = false,
                VCmd::Seek(t) => seek_to = Some(t),
            }
        }
        if let Some(t) = seek_to {
            pending = None;
            match v.frame_at(t) {
                Ok(Some(f)) => publish(&shared, &ctx, f),
                Ok(None) => {}
                Err(e) => crate::log::log(&format!("editor: seek: {e}")),
            }
        }
        if !playing {
            continue;
        }
        if pending.is_none() {
            match v.next() {
                Ok(Some(x)) => pending = Some(x),
                Ok(None) | Err(_) => {
                    playing = false;
                    clock.stop_at(clock.pos());
                    continue;
                }
            }
        }
        let now = clock.pos();
        if let Some((ts, _, _)) = &pending {
            if *ts <= now + 20_000 {
                let (ts, _, s) = pending.take().unwrap();
                // Si vamos muy atrasados, se salta el cuadro sin convertirlo.
                if now - ts < 500_000 {
                    if let Ok(img) = v.to_image(&s) {
                        publish(&shared, &ctx, img);
                    }
                }
            }
        }
    }
}

// --- Audio ------------------------------------------------------------------------

fn audio_thread(path: PathBuf, rx: Receiver<ACmd>, gain: Arc<AtomicU32>, speed: Arc<AtomicU64>) {
    let _mf = Mf::init();
    let r = unsafe { audio_setup(&path) };
    let Ok((r, client, render, bufsize)) = r else {
        // Sin audio (dispositivo o formato): el video sigue igual.
        for _ in rx.iter() {}
        return;
    };
    let mut feed = Feed::default();
    let mut playing = false;
    loop {
        let cmd = if playing {
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => break,
            }
        };
        let mut last = cmd;
        for c in rx.try_iter() {
            last = Some(c);
        }
        unsafe {
            match last {
                Some(ACmd::Play(pos)) => {
                    let _ = client.Stop();
                    let _ = client.Reset();
                    feed = Feed { skip_until: pos, ..Feed::default() };
                    let _ = seek(&r, pos);
                    playing = true;
                    fill(&r, &render, bufsize, &client, &mut feed, f32::from_bits(gain.load(Ordering::Relaxed)), f64::from_bits(speed.load(Ordering::Relaxed)));
                    let _ = client.Start();
                    continue;
                }
                Some(ACmd::Pause) => {
                    let _ = client.Stop();
                    let _ = client.Reset();
                    playing = false;
                }
                None => {}
            }
            if playing {
                fill(&r, &render, bufsize, &client, &mut feed, f32::from_bits(gain.load(Ordering::Relaxed)), f64::from_bits(speed.load(Ordering::Relaxed)));
            }
        }
    }
    unsafe {
        let _ = client.Stop();
    }
}

unsafe fn audio_setup(path: &std::path::Path) -> Result<(IMFSourceReader, IAudioClient, IAudioRenderClient, u32)> {
    let r = reader(path, false, None, false)?;
    r.SetStreamSelection(media::ANY, false)?;
    r.SetStreamSelection(AUDIO, true)?;
    r.SetCurrentMediaType(AUDIO, None, &mf::pcm_type()?)?;

    let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
    let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
    client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
        1_000_000,
        0,
        &mf::WAVE_FORMAT,
        None,
    )?;
    let render: IAudioRenderClient = client.GetService()?;
    let size = client.GetBufferSize()?;
    Ok((r, client, render, size))
}

/// Audio leído del archivo que todavía no pasó a la placa de sonido.
#[derive(Default)]
struct Feed {
    queue: VecDeque<u8>,
    eos: bool,
    /// Recién buscado: lo que el archivo entrega antes de este punto se descarta.
    skip_until: i64,
    /// A otra velocidad: dónde va la lectura entre el primer cuadro de `queue` y el siguiente (0 a 1).
    frac: f64,
}

/// Llena lo que haya libre en la placa de sonido. A otra velocidad (`speed`), cada cuadro que sale mezcla los dos
/// cuadros del archivo entre los que cae.
unsafe fn fill(r: &IMFSourceReader, render: &IAudioRenderClient, bufsize: u32, client: &IAudioClient, feed: &mut Feed, gain: f32, speed: f64) {
    let Ok(pad) = client.GetCurrentPadding() else { return };
    let free = (bufsize - pad) as usize;
    let Feed { queue, eos, skip_until, frac } = feed;
    let need = (*frac + free as f64 * speed).ceil() as usize + 2;
    while queue.len() / FRAME_BYTES < need && !*eos {
        let (mut flags, mut ts, mut s) = (0u32, 0i64, None);
        if r.ReadSample(AUDIO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut s)).is_err() {
            *eos = true;
            break;
        }
        if let Some(s) = s {
            if let Ok(bytes) = mf::sample_bytes(&s) {
                // Recortar lo que quedó antes del punto pedido.
                let skip = if *skip_until > ts {
                    (frames_at(*skip_until - ts) as usize * FRAME_BYTES).min(bytes.len())
                } else {
                    0
                };
                queue.extend(&bytes[skip..]);
            }
        }
        if flags & EOS != 0 {
            *eos = true;
        }
    }
    *skip_until = 0;
    // Cuadros que se pueden armar con lo que hay: cada uno necesita también el cuadro del archivo que sigue.
    let room = (queue.len() / FRAME_BYTES) as f64 - 1.0 - *frac;
    let n = if room <= 0.0 {
        0
    } else if speed <= 0.0 {
        free
    } else {
        free.min((room / speed).ceil() as usize)
    };
    if n == 0 {
        return;
    }
    if let Ok(p) = render.GetBuffer(n as u32) {
        let dst = std::slice::from_raw_parts_mut(p as *mut i16, n * 2);
        let data = queue.make_contiguous();
        let at = |i: usize, ch: usize| i16::from_le_bytes([data[i * FRAME_BYTES + ch * 2], data[i * FRAME_BYTES + ch * 2 + 1]]) as f32;
        for j in 0..n {
            let pos = *frac + j as f64 * speed;
            let (i, f) = (pos as usize, (pos - pos.floor()) as f32);
            for ch in 0..2 {
                let v = (at(i, ch) * (1.0 - f) + at(i + 1, ch) * f) * gain;
                dst[j * 2 + ch] = v.clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            }
        }
        let _ = render.ReleaseBuffer(n as u32, 0);
        let end = *frac + n as f64 * speed;
        let used = end.floor() as usize;
        queue.drain(..used * FRAME_BYTES);
        *frac = end - used as f64;
    }
}
