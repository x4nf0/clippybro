//! Lectura de clips y exportación (cortar/unir) con Media Foundation. Sin FFmpeg.
//!
//! - Modo copia: los paquetes H.264/HEVC y AAC se copian tal cual (instantáneo, sin pérdida).
//!   El inicio de cada parte cae en el fotograma clave anterior.
//! - Modo recodificar: arma el video cuadro a cuadro (por GPU): corte exacto, varias pistas,
//!   fundidos entre piezas que se superponen y clips de distinta resolución o formato.

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use anyhow::{anyhow, bail, Result};
use windows::core::{Interface, HSTRING, GUID, PROPVARIANT};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Media::MediaFoundation::*;

use crate::clipcache::{Wave, WaveBuilder};
use crate::i18n::{tr, trf};
use crate::mf::{aac_type, frames_at, pcm_sample, pcm_type, sample_bytes, time_of, FRAME_BYTES};

pub const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
pub const AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
pub const ANY: u32 = MF_SOURCE_READER_ANY_STREAM.0 as u32;
pub const SOURCE: u32 = MF_SOURCE_READER_MEDIASOURCE.0 as u32;
pub const EOS: u32 = MF_SOURCE_READERF_ENDOFSTREAM.0 as u32;

/// `low_latency`: el decodificador entrega cada cuadro apenas lo tiene (para cuadros sueltos).
pub unsafe fn reader(path: &Path, video_processing: bool, d3d: Option<&IMFDXGIDeviceManager>, low_latency: bool) -> Result<IMFSourceReader> {
    let mut a = None;
    MFCreateAttributes(&mut a, 4)?;
    let a = a.unwrap();
    if video_processing {
        a.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
    }
    if low_latency {
        a.SetUINT32(&MF_LOW_LATENCY, 1)?;
    }
    if let Some(m) = d3d {
        a.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, m)?;
        a.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
    }
    Ok(MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &a)?)
}

/// Dispositivo de la GPU para Media Foundation: decodificar, escalar y codificar sin pasar por la CPU.
pub unsafe fn d3d_manager() -> Result<IMFDXGIDeviceManager> {
    let mut device = None;
    D3D11CreateDevice(
        None,
        D3D_DRIVER_TYPE_HARDWARE,
        HMODULE::default(),
        D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        None,
        D3D11_SDK_VERSION,
        Some(&mut device),
        None,
        None,
    )?;
    let device = device.ok_or_else(|| anyhow!("sin D3D11"))?;
    if let Ok(mt) = device.cast::<ID3D11Multithread>() {
        let _ = mt.SetMultithreadProtected(true);
    }
    let mut token = 0u32;
    let mut mgr = None;
    MFCreateDXGIDeviceManager(&mut token, &mut mgr)?;
    let mgr = mgr.unwrap();
    mgr.ResetDevice(&device, token)?;
    Ok(mgr)
}

pub unsafe fn seek(r: &IMFSourceReader, t: i64) -> Result<()> {
    r.SetCurrentPosition(&GUID::zeroed(), &PROPVARIANT::from(t.max(0)))?;
    Ok(())
}

fn unpack(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

pub fn pack(a: u32, b: u32) -> u64 {
    ((a as u64) << 32) | b as u64
}

/// Lo que se sabe de un archivo sin decodificarlo.
#[derive(Clone, Debug, Default)]
pub struct Probe {
    pub duration: i64,
    pub width: u32,
    pub height: u32,
    /// Cuadros por segundo como fracción (60/1, 30000/1001...). 0/0 si el archivo no lo dice.
    pub frame_rate: (u32, u32),
    /// Formato del video (H.264, HEVC...).
    pub subtype: GUID,
    /// Encabezado del video (SPS/PPS): si cambia entre dos clips no se pueden unir sin recodificar.
    pub seq: Vec<u8>,
    /// Formato del audio (tipo, muestras por segundo, canales), si tiene.
    pub audio: Option<(GUID, u32, u32)>,
}

impl Probe {
    pub fn fps(&self) -> f64 {
        let (n, d) = self.frame_rate;
        if d > 0 {
            n as f64 / d as f64
        } else {
            0.0
        }
    }

    pub fn codec(&self) -> &'static str {
        if self.subtype == MFVideoFormat_H264 {
            "H.264"
        } else if self.subtype == MFVideoFormat_HEVC {
            "HEVC"
        } else {
            tr!("otro", "other", "outro")
        }
    }

    /// Mismo formato de video y de audio: los clips se pueden unir copiando los paquetes.
    pub fn same_format(&self, o: &Probe) -> bool {
        self.subtype == o.subtype && (self.width, self.height) == (o.width, o.height) && self.seq == o.seq && self.audio == o.audio
    }
}

pub fn probe(path: &Path) -> Result<Probe> {
    unsafe {
        let r = reader(path, false, None, false)?;
        let duration = r
            .GetPresentationAttribute(SOURCE, &MF_PD_DURATION)
            .ok()
            .and_then(|p| u64::try_from(&p).ok())
            .unwrap_or(0) as i64;
        let vt = r.GetNativeMediaType(VIDEO, 0).map_err(|_| anyhow!(tr!("el archivo no tiene video", "the file has no video", "o arquivo não tem vídeo")))?;
        let (width, height) = unpack(vt.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0));
        let mut seq = Vec::new();
        if let Ok(n) = vt.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) {
            seq = vec![0u8; n as usize];
            let _ = vt.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut seq, None);
        }
        let audio = r.GetNativeMediaType(AUDIO, 0).ok().map(|at| {
            (
                at.GetGUID(&MF_MT_SUBTYPE).unwrap_or_default(),
                at.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(0),
                at.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(0),
            )
        });
        Ok(Probe {
            duration,
            width,
            height,
            frame_rate: unpack(vt.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(0)),
            subtype: vt.GetGUID(&MF_MT_SUBTYPE).unwrap_or_default(),
            seq,
            audio,
        })
    }
}

/// Marcas de tiempo de los fotogramas clave. En MP4 se leen del índice del archivo
/// (milisegundos); si no se puede, se recorren los cuadros sin decodificarlos.
pub fn keyframes(path: &Path) -> Result<Vec<i64>> {
    if let Some(k) = mp4_keyframes(path) {
        return Ok(k);
    }
    unsafe {
        let r = reader(path, false, None, false)?;
        r.SetStreamSelection(ANY, false)?;
        r.SetStreamSelection(VIDEO, true)?;
        let mut v = Vec::new();
        loop {
            let (mut flags, mut ts, mut s) = (0u32, 0i64, None);
            r.ReadSample(VIDEO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut s))?;
            if let Some(s) = s {
                if s.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0 {
                    v.push(ts);
                }
            }
            if flags & EOS != 0 {
                break;
            }
        }
        if v.is_empty() {
            v.push(0);
        }
        Ok(v)
    }
}

// --- Exportación -------------------------------------------------------------------

/// Una pieza tal como se exporta (y como la muestra la vista previa).
#[derive(Clone, Debug)]
pub struct Clip {
    /// La pieza de la línea de tiempo de la que sale (0 si no sale de una).
    pub id: u64,
    pub path: PathBuf,
    /// Dónde empieza en la línea de tiempo y cuánto dura ahí.
    pub start: i64,
    pub len: i64,
    /// Dónde empieza en el archivo.
    pub src_in: i64,
    /// Cuánto del archivo pasa en cada instante de la línea (ver `src_at`).
    pub speed: f64,
    /// Cuánto tarda en aparecer desde negro (o silencio) y en irse.
    pub fade: (i64, i64),
    /// Fundido cruzado con la pieza anterior y con la siguiente de su pista: lo que se superponen.
    pub xfade: (i64, i64),
    /// Volumen de su audio (0 = mudo).
    pub gain: f32,
    /// Forma del fundido de entrada y del de salida.
    pub curves: (Curve, Curve),
    /// Cómo se ve su imagen en el lienzo.
    pub frame: Frame,
}

/// Velocidades que se pueden elegir. 0 no está: es la de una imagen congelada.
pub const SPEED_MIN: f64 = 0.1;
pub const SPEED_MAX: f64 = 10.0;

/// El instante del archivo que se ve (o se oye) en `t` de la línea, para una pieza que empieza en `start` y en el
/// archivo en `src_in`: 1 = velocidad normal, 2 = el doble de rápido, 0 = una imagen congelada.
pub fn src_at(start: i64, src_in: i64, speed: f64, t: i64) -> i64 {
    src_in + ((t - start) as f64 * speed).round() as i64
}

/// Proporción del lienzo (ancho, alto).
pub type Ratio = (u32, u32);

/// Tamaño (px, pares) del video que sale: con la proporción `ratio` (o la del primer clip, `first`) y como lado
/// corto el del primer clip, o `short` si es más chico (0 = como el original).
pub fn canvas_size(first: (u32, u32), ratio: Option<Ratio>, short: u32) -> (u32, u32) {
    let (sw, sh) = (first.0.max(2) as u64, first.1.max(2) as u64);
    let (rw, rh) = ratio.map_or((sw, sh), |(w, h)| (w.max(1) as u64, h.max(1) as u64));
    let base = sw.min(sh);
    let s = if short == 0 || short as u64 >= base { base } else { short as u64 };
    let (w, h) = if rw >= rh { (s * rw / rh, s) } else { (s, s * rh / rw) };
    ((w as u32).max(2) & !1, (h as u32).max(2) & !1)
}

/// Misma forma (proporción) a menos de un píxel.
pub fn same_shape((aw, ah): (u32, u32), (bw, bh): (u32, u32)) -> bool {
    (aw as f64 * bh as f64 / ah.max(1) as f64 - bw as f64).abs() < 1.0
}

/// Cómo ocupa el lienzo una imagen que no tiene su forma.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Sizing {
    /// Entra entera, con bordes negros.
    #[default]
    Whole,
    /// Llena el lienzo; lo que sobra queda afuera.
    Fill,
    /// Se deforma hasta tener la forma del lienzo (clips grabados con resolución estirada).
    Stretch,
}

/// Cómo se ve la imagen de una pieza: girada, espejada, recortada y cómo ocupa el lienzo.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Frame {
    /// Cuartos de vuelta en el sentido del reloj (0 a 3).
    pub turns: u8,
    /// Espejada de izquierda a derecha (ya girada).
    pub flip: bool,
    /// Lo que se ve del cuadro del archivo, de 0 a 1: izquierda, arriba, derecha, abajo.
    pub crop: [f32; 4],
    pub sizing: Sizing,
}

impl Default for Frame {
    fn default() -> Self {
        Frame { turns: 0, flip: false, crop: [0.0, 0.0, 1.0, 1.0], sizing: Sizing::Whole }
    }
}

/// Dónde cae una imagen en el lienzo: el rectángulo (px; izquierda, arriba, derecha, abajo) y el punto del cuadro
/// del archivo (0 a 1) que se ve en cada esquina (arriba a la izquierda, arriba a la derecha, abajo a la derecha,
/// abajo a la izquierda).
#[derive(Clone, Copy, Debug)]
pub struct Placed {
    pub dst: [f32; 4],
    pub uv: [[f32; 2]; 4],
}

impl Placed {
    /// La parte del cuadro del archivo que se ve (0 a 1; izquierda, arriba, derecha, abajo).
    pub fn src_rect(&self) -> [f32; 4] {
        let xs = self.uv.map(|p| p[0]);
        let ys = self.uv.map(|p| p[1]);
        let (min, max) = (|v: [f32; 4]| v.into_iter().fold(f32::MAX, f32::min), |v: [f32; 4]| v.into_iter().fold(f32::MIN, f32::max));
        [min(xs), min(ys), max(xs), max(ys)]
    }

    /// El punto del archivo (0 a 1) que se ve en (fx, fy) del rectángulo (0 a 1). Como los giros son de a un cuarto
    /// de vuelta, mezclar las esquinas da el punto exacto.
    pub fn uv_at(&self, fx: f32, fy: f32) -> [f32; 2] {
        let lerp = |a: [f32; 2], b: [f32; 2], k: f32| [a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k];
        lerp(lerp(self.uv[0], self.uv[1], fx), lerp(self.uv[3], self.uv[2], fx), fy)
    }
}

impl Frame {
    /// Sin girar, espejar ni recortar: si el archivo tiene la forma del lienzo, se ve tal cual.
    pub fn is_plain(&self) -> bool {
        Frame { sizing: Sizing::Whole, ..*self } == Frame::default()
    }

    /// El punto del cuadro del archivo (0 a 1) que se ve en (u, v) de la imagen ya recortada, girada y espejada.
    pub fn source(&self, u: f32, v: f32) -> [f32; 2] {
        let u = if self.flip { 1.0 - u } else { u };
        let (x, y) = match self.turns % 4 {
            0 => (u, v),
            1 => (v, 1.0 - u),
            2 => (1.0 - u, 1.0 - v),
            _ => (1.0 - v, u),
        };
        let [l, t, r, b] = self.crop;
        [l + x * (r - l), t + y * (b - t)]
    }

    /// Al revés que `source` sin recortar: dónde queda (0 a 1) en la imagen girada y espejada el punto (x, y) del
    /// cuadro del archivo.
    pub fn view(&self, x: f32, y: f32) -> [f32; 2] {
        let (u, v) = match self.turns % 4 {
            0 => (x, y),
            1 => (1.0 - y, x),
            2 => (1.0 - x, 1.0 - y),
            _ => (y, 1.0 - x),
        };
        [if self.flip { 1.0 - u } else { u }, v]
    }

    /// Tamaño (px) de la imagen recortada y girada de un archivo de `w`×`h`.
    pub fn view_size(&self, (w, h): (f32, f32)) -> (f32, f32) {
        let [l, t, r, b] = self.crop;
        let (cw, ch) = ((r - l) * w, (b - t) * h);
        if self.turns % 2 == 1 {
            (ch, cw)
        } else {
            (cw, ch)
        }
    }

    /// Dónde va, en un lienzo de `canvas` px, la imagen de un archivo de `src` px: centrada, entera, llenándolo o estirada.
    /// Solo la parte que queda adentro del lienzo; None si no se ve nada.
    pub fn place(&self, src: (f32, f32), canvas: (f32, f32)) -> Option<Placed> {
        let (vw, vh) = self.view_size(src);
        if vw < 1.0 || vh < 1.0 {
            return None;
        }
        let (sx, sy) = (canvas.0 / vw, canvas.1 / vh);
        let (dw, dh) = match self.sizing {
            Sizing::Whole => (vw * sx.min(sy), vh * sx.min(sy)),
            Sizing::Fill => (vw * sx.max(sy), vh * sx.max(sy)),
            Sizing::Stretch => canvas,
        };
        let (x0, y0) = ((canvas.0 - dw) / 2.0, (canvas.1 - dh) / 2.0);
        let (l, t, r, b) = (x0.max(0.0), y0.max(0.0), (x0 + dw).min(canvas.0), (y0 + dh).min(canvas.1));
        if r - l < 0.5 || b - t < 0.5 {
            return None;
        }
        let (u0, v0, u1, v1) = ((l - x0) / dw, (t - y0) / dh, (r - x0) / dw, (b - y0) / dh);
        Some(Placed { dst: [l, t, r, b], uv: [self.source(u0, v0), self.source(u1, v0), self.source(u1, v1), self.source(u0, v1)] })
    }
}

/// Forma de los fundidos del audio.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Curve {
    /// Potencia constante: al cruzarse dos sonidos distintos el volumen no baja.
    #[default]
    Even,
    /// En línea recta.
    Linear,
    /// Arranca y termina despacio.
    Smooth,
}

impl Curve {
    /// Volumen (0 a 1) a un avance `x` (0 a 1) del fundido.
    pub fn shape(self, x: f32) -> f32 {
        match self {
            Curve::Even => (x * std::f32::consts::FRAC_PI_2).sin(),
            Curve::Linear => x,
            Curve::Smooth => x * x * (3.0 - 2.0 * x),
        }
    }
}

/// Avance (0 a 1) de un fundido de largo `f` a `d` de su comienzo.
pub fn ramp(d: i64, f: i64) -> f32 {
    if f > 0 {
        (d as f32 / f as f32).clamp(0.0, 1.0)
    } else {
        1.0
    }
}

impl Clip {
    pub fn end(&self) -> i64 {
        self.start + self.len
    }
    pub fn src_at(&self, t: i64) -> i64 {
        src_at(self.start, self.src_in, self.speed, t)
    }
    pub fn src_out(&self) -> i64 {
        self.src_at(self.end())
    }
    pub fn covers(&self, t: i64) -> bool {
        self.start <= t && t < self.end()
    }
    /// Cuánto se ve (0 a 1) en `t`: aparece de a poco (desde negro o sobre la anterior) y se va a negro. Al
    /// fundirse con la siguiente no se apaga: la siguiente le aparece encima.
    pub fn alpha(&self, t: i64) -> f32 {
        let (cin, cout) = self.curves;
        cin.shape(ramp(t - self.start, self.fade.0.max(self.xfade.0))) * cout.shape(ramp(self.end() - t, self.fade.1))
    }
    /// Volumen en `t` con sus fundidos (de entrada, de salida y cruzados), cada uno con la forma de su curva.
    pub fn gain_at(&self, t: i64) -> f32 {
        let (cin, cout) = self.curves;
        self.gain * cin.shape(ramp(t - self.start, self.fade.0.max(self.xfade.0))) * cout.shape(ramp(self.end() - t, self.fade.1.max(self.xfade.1)))
    }
    fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// Lo que se exporta.
#[derive(Clone, Default)]
pub struct Edit {
    /// Pistas de video visibles, de abajo hacia arriba: la de más arriba tapa a las otras.
    pub video: Vec<Vec<Clip>>,
    /// Lo que suena, por pista.
    pub audio: Vec<Vec<Clip>>,
    /// Proporción del lienzo; None = la del primer clip.
    pub ratio: Option<Ratio>,
}

impl Edit {
    pub fn len(&self) -> i64 {
        self.video.iter().chain(&self.audio).flatten().map(Clip::end).max().unwrap_or(0)
    }

    /// La primera pieza de video: de ella salen el tamaño y los cuadros por segundo.
    pub fn first_video(&self) -> Option<&Clip> {
        self.video.iter().flatten().min_by_key(|c| c.start)
    }

    /// Si el video es una sola pista de piezas pegadas desde el inicio, a velocidad normal, sin fundidos, sin
    /// tocar su imagen, en un lienzo con la forma del primer clip (`first`, su tamaño) y sin nada que suene
    /// después del final, esas piezas: se pueden copiar sin recodificar.
    pub fn copy_parts(&self, first: (u32, u32)) -> Option<&[Clip]> {
        if self.ratio.is_some_and(|r| !same_shape(first, r)) {
            return None;
        }
        let mut used = self.video.iter().filter(|t| !t.is_empty());
        let parts = used.next()?;
        if used.next().is_some() {
            return None;
        }
        let mut t = 0;
        for c in parts {
            if (c.start - t).abs() > 2_000 || c.fade != (0, 0) || c.xfade != (0, 0) || c.speed != 1.0 || !c.frame.is_plain() {
                return None;
            }
            t = c.end();
        }
        (self.len() <= t + 2_000).then_some(parts.as_slice())
    }

    /// Lo que suena es justo el audio de `parts`, sin cambios: se puede copiar tal cual.
    fn plain_audio(&self, parts: &[Clip]) -> bool {
        let same = |a: &Clip, v: &Clip| {
            a.path == v.path
                && (a.start, a.src_in, a.len, a.fade, a.xfade) == (v.start, v.src_in, v.len, (0, 0), (0, 0))
                && a.speed == 1.0
                && (a.gain - 1.0).abs() < 0.001
        };
        let mut used = self.audio.iter().filter(|t| !t.is_empty());
        used.next().is_some_and(|a| a.len() == parts.len() && a.iter().zip(parts).all(|(a, v)| same(a, v))) && used.next().is_none()
    }
}

/// Cuánto se superpone cada pieza de una pista (`spans`: inicio y fin, en orden de inicio) con la anterior
/// y con la siguiente: el largo de sus fundidos cruzados.
pub fn overlaps(spans: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut f = vec![(0, 0); spans.len()];
    for i in 1..spans.len() {
        let ((_, a_end), (b_start, b_end)) = (spans[i - 1], spans[i]);
        let o = (a_end.min(b_end) - b_start).max(0);
        f[i].0 = o;
        f[i - 1].1 = o;
    }
    f
}

/// Lo que se ve en `t`, de abajo hacia arriba: (pista, pieza, opacidad). Arriba va la pista más alta y, en
/// cada pista, la pieza que empieza después. Lo que queda debajo de algo opaco no se ve; debajo de todo, negro.
pub fn layers(video: &[Vec<Clip>], t: i64) -> Vec<(usize, usize, f32)> {
    let mut out = Vec::new();
    'tracks: for (ti, track) in video.iter().enumerate().rev() {
        for (i, c) in track.iter().enumerate().rev().filter(|(_, c)| c.covers(t)) {
            let a = c.alpha(t);
            if a > 0.0 {
                out.push((ti, i, a));
            }
            if a >= 1.0 {
                break 'tracks;
            }
        }
    }
    out.reverse();
    out
}

pub struct Progress {
    bits: AtomicU32,
    pub cancel: AtomicBool,
    pub result: Mutex<Option<Result<String, String>>>,
}

impl Progress {
    pub fn new() -> Self {
        Self { bits: AtomicU32::new(0), cancel: AtomicBool::new(false), result: Mutex::new(None) }
    }
    pub fn get(&self) -> f32 {
        f32::from_bits(self.bits.load(Ordering::Relaxed))
    }
    fn set(&self, v: f32) {
        self.bits.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
    fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            bail!("cancelado");
        }
        Ok(())
    }
}

pub struct ExportOpts {
    /// Bitrate de video si hay que renderizar sin un destino elegido.
    pub bitrate: u32,
    /// Formato elegido por el usuario. Si está, siempre se renderiza con estos valores.
    pub target: Option<Target>,
}

#[derive(Clone, Copy, Debug)]
pub struct Target {
    /// Alto en píxeles (el ancho se calcula manteniendo la proporción). 0 = original.
    pub height: u32,
    /// Cuadros por segundo. 0 = original.
    pub fps: u32,
    /// Bitrate de video (bits/s).
    pub bitrate: u32,
    /// Bitrate constante (para no pasarse de un tamaño máximo).
    pub constant: bool,
}

/// ¿`t` cae justo en uno de los fotogramas clave `keys`? (entonces se puede cortar ahí sin recodificar)
pub fn on_keyframe(keys: &[i64], t: i64) -> bool {
    keys.iter().any(|&x| (x - t).abs() <= 2_000)
}

fn no_video() -> anyhow::Error {
    anyhow!(tr!("no hay video para exportar", "there's no video to export", "não há vídeo para exportar"))
}

/// Exporta la edición a `out`. Si el video es una sola pista de piezas pegadas, del mismo formato y con
/// cortes en fotogramas clave, lo copia sin recodificar (instantáneo, sin pérdida); si no, lo arma cuadro
/// a cuadro con la GPU. Devuelve una descripción del modo usado.
pub fn export(edit: &Edit, out: &Path, opts: &ExportOpts, prog: &Progress) -> Result<String> {
    let first = edit.first_video().ok_or_else(no_video)?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = out.with_extension("clippy-tmp.mp4");
    let _ = std::fs::remove_file(&tmp);
    let r = (|| unsafe {
        let mut probes = HashMap::new();
        for c in edit.video.iter().chain(&edit.audio).flatten() {
            if !probes.contains_key(&c.path) {
                probes.insert(c.path.clone(), probe(&c.path)?);
            }
        }
        // Solo suena lo que tiene audio.
        let edit = Edit {
            video: edit.video.clone(),
            audio: edit.audio.iter().map(|t| t.iter().filter(|c| probes[&c.path].audio.is_some()).cloned().collect()).collect(),
            ratio: edit.ratio,
        };
        let info = &probes[&first.path];
        let size = (info.width, info.height);
        let mut keys = HashMap::new();
        let copy = edit.copy_parts(size).filter(|parts| {
            opts.target.is_none()
                && parts.iter().all(|c| probes[&c.path].same_format(info))
                && parts.iter().all(|c| on_keyframe(keys.entry(&c.path).or_insert_with(|| keyframes(&c.path).unwrap_or_default()), c.src_in))
        });
        if let Some(parts) = copy {
            let plain = edit.plain_audio(parts);
            export_copy(&edit, parts, info, &tmp, prog, plain)?;
            return Ok(if plain {
                tr!("sin recodificar: instantáneo y sin pérdida", "no re-encoding: instant and lossless", "sem recodificar: instantâneo e sem perda")
            } else {
                tr!("video sin recodificar, audio ajustado", "video not re-encoded, audio adjusted", "vídeo sem recodificar, áudio ajustado")
            }
            .to_string());
        }
        let why = if opts.target.is_none() && edit.copy_parts(size).is_some() {
            tr!("renderizado con la GPU para cortar exacto", "rendered on the GPU for exact cuts", "renderizado na GPU para cortar exato")
        } else {
            tr!("renderizado con la GPU", "rendered on the GPU", "renderizado na GPU")
        };
        // Todo en la GPU; si esa placa o ese archivo no se dejan, se decodifica por CPU (con devtools,
        // CLIPPY_CPU=1 fuerza ese camino para probarlo).
        let gpu = d3d_manager().ok().filter(|_| !cfg!(feature = "devtools") || std::env::var_os("CLIPPY_CPU").is_none());
        match gpu {
            Some(m) => export_reencode(&edit, &probes, &tmp, opts, prog, Some(&m)).or_else(|e| {
                prog.check()?;
                crate::log::log(&format!("exportar: por GPU no se pudo ({e:?}), se decodifica por CPU"));
                let _ = std::fs::remove_file(&tmp);
                export_reencode(&edit, &probes, &tmp, opts, prog, None)
            }),
            None => export_reencode(&edit, &probes, &tmp, opts, prog, None),
        }?;
        Ok(why.to_string())
    })();
    match r {
        Ok(m) => {
            let _ = std::fs::remove_file(out);
            std::fs::rename(&tmp, out)?;
            prog.set(1.0);
            Ok(m)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// `hardware`: encoder por GPU. `d3d`: las imágenes llegan ya en la GPU; ahí Media Foundation frena
/// la lectura si el encoder va atrasado (si no, se acumularían cuadros en la memoria de video).
unsafe fn sink(out: &Path, hardware: bool, d3d: Option<&IMFDXGIDeviceManager>) -> Result<IMFSinkWriter> {
    let mut a = None;
    MFCreateAttributes(&mut a, 3)?;
    let a = a.unwrap();
    a.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
    match d3d {
        Some(m) => a.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, m)?,
        None => a.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?,
    }
    if hardware {
        a.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
    }
    Ok(MFCreateSinkWriterFromURL(&HSTRING::from(out.as_os_str()), None, &a)?)
}

unsafe fn clone_type(t: &IMFMediaType) -> Result<IMFMediaType> {
    let n = MFCreateMediaType()?;
    t.CopyAllItems(&n)?;
    Ok(n)
}

/// Agrega a `w` un stream de audio AAC que recibe PCM.
unsafe fn pcm_stream(w: &IMFSinkWriter) -> Result<u32> {
    let s = w.AddStream(&aac_type()?)?;
    w.SetInputMediaType(s, &pcm_type()?, None)?;
    Ok(s)
}

/// Índice real (0, 1, ...) del primer stream de video del lector.
unsafe fn video_index(r: &IMFSourceReader) -> u32 {
    for i in 0..8 {
        if let Ok(t) = r.GetNativeMediaType(i, 0) {
            if t.GetGUID(&MF_MT_MAJOR_TYPE).ok() == Some(MFMediaType_Video) {
                return i;
            }
        }
    }
    0
}

/// Copia el video de `parts` tal cual. El audio también se copia si es justo el de esas piezas
/// (`plain_audio`); si no, se mezcla lo que suena y se vuelve a codificar.
unsafe fn export_copy(edit: &Edit, parts: &[Clip], info: &Probe, out: &Path, prog: &Progress, plain_audio: bool) -> Result<()> {
    let first = reader(&parts[0].path, false, None, false)?;
    let vt = clone_type(&first.GetNativeMediaType(VIDEO, 0)?)?;
    let at = match (info.audio, plain_audio) {
        (Some(_), true) => Some(clone_type(&first.GetNativeMediaType(AUDIO, 0)?)?),
        _ => None,
    };
    drop(first);

    let w = sink(out, false, None)?;
    let vs = w.AddStream(&vt)?;
    w.SetInputMediaType(vs, &vt, None)?;
    let a_s = match &at {
        Some(t) => {
            let s = w.AddStream(t)?;
            w.SetInputMediaType(s, t, None)?;
            Some(s)
        }
        None => None,
    };
    let mut mix = if plain_audio { None } else { Mixer::new(edit) };
    let mix_stream = if mix.is_some() { Some(pcm_stream(&w)?) } else { None };
    w.BeginWriting()?;

    let total: i64 = parts.iter().map(|s| (s.end() - s.start).max(1)).sum();
    let mut offset = 0i64;
    let mut done = 0i64;
    for seg in parts {
        let r = reader(&seg.path, false, None, false)?;
        let vidx = video_index(&r);
        if a_s.is_none() {
            let _ = r.SetStreamSelection(AUDIO, false);
        }
        // Ubicar el fotograma clave donde arranca la parte y volver a posicionarse ahí,
        // así el audio también empieza en ese punto (si no, arrancaría unos ms más tarde).
        seek(&r, seg.src_in)?;
        loop {
            let (mut flags, mut ts, mut s) = (0u32, 0i64, None);
            r.ReadSample(VIDEO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut s))?;
            if let Some(s) = s {
                if s.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0 {
                    seek(&r, ts)?;
                    break;
                }
            }
            if flags & EOS != 0 {
                seek(&r, seg.src_in)?;
                break;
            }
        }
        let mut base: Option<i64> = None;
        let mut pending_audio: Vec<(IMFSample, i64)> = Vec::new();
        let (mut vdone, mut adone) = (false, a_s.is_none());
        let mut seg_end = seg.src_in;
        while !(vdone && adone) {
            prog.check()?;
            let (mut idx, mut flags, mut ts, mut s) = (0u32, 0u32, 0i64, None);
            r.ReadSample(ANY, 0, Some(&mut idx), Some(&mut flags), Some(&mut ts), Some(&mut s))?;
            let is_video = idx == vidx;
            if flags & EOS != 0 {
                if is_video {
                    vdone = true;
                } else {
                    adone = true;
                }
            }
            let Some(s) = s else { continue };
            if is_video {
                if vdone {
                    continue;
                }
                if base.is_none() {
                    if s.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) == 0 {
                        continue;
                    }
                    base = Some(ts);
                    if let Some(a_s) = a_s {
                        for (asmp, ats) in pending_audio.drain(..) {
                            if ats >= ts && ats < seg.src_out() {
                                asmp.SetSampleTime(ats - ts + offset)?;
                                w.WriteSample(a_s, &asmp)?;
                            }
                        }
                    }
                }
                if ts >= seg.src_out() {
                    vdone = true;
                    continue;
                }
                let b = base.unwrap();
                let dur = s.GetSampleDuration().unwrap_or(0);
                s.SetSampleTime(ts - b + offset)?;
                w.WriteSample(vs, &s)?;
                seg_end = seg_end.max(ts + dur);
                prog.set((done + (ts - seg.src_in).max(0)) as f32 / total as f32);
            } else if let Some(a_s) = a_s {
                if adone {
                    continue;
                }
                match base {
                    None => pending_audio.push((s, ts)),
                    Some(b) => {
                        if ts >= seg.src_out() {
                            adone = true;
                        } else if ts >= b {
                            s.SetSampleTime(ts - b + offset)?;
                            w.WriteSample(a_s, &s)?;
                        }
                    }
                }
            }
        }
        let b = base.ok_or_else(|| {
            let name = seg.name();
            anyhow!(trf!("no se encontró video en {name}", "no video found in {name}", "nenhum vídeo encontrado em {name}"))
        })?;
        if let (Some(m), Some(ms)) = (&mut mix, mix_stream) {
            m.write(&w, ms, seg.start, seg.end(), offset)?;
        }
        offset += (seg_end - b).max(0);
        done += (seg.end() - seg.start).max(1);
    }
    w.Finalize()?;
    Ok(())
}

/// Audio de un archivo decodificado en orden (PCM 48 kHz estéreo).
struct Pcm {
    r: IMFSourceReader,
    /// Muestras (izquierda, derecha, ...) desde el cuadro de audio `at` del archivo.
    buf: Vec<i16>,
    at: i64,
    eos: bool,
}

impl Pcm {
    unsafe fn open(path: &Path, from: i64) -> Result<Pcm> {
        let r = reader(path, false, None, false)?;
        r.SetStreamSelection(ANY, false)?;
        r.SetStreamSelection(AUDIO, true)?;
        r.SetCurrentMediaType(AUDIO, None, &pcm_type()?)?;
        seek(&r, from)?;
        Ok(Pcm { r, buf: Vec::new(), at: frames_at(from), eos: false })
    }

    /// Suma a `out` (estéreo) el audio del archivo desde el cuadro `from` (puede caer entre dos), avanzando
    /// `speed` cuadros del archivo por cada uno de la salida (más rápido suena más agudo, como una cinta), con el
    /// volumen `gain(k)` en el cuadro `k` de la salida. Donde el archivo no tiene audio queda silencio. `from`
    /// nunca retrocede.
    unsafe fn mix(&mut self, from: f64, speed: f64, out: &mut [f32], gain: impl Fn(i64) -> f32) -> Result<()> {
        let n = (out.len() / 2) as i64;
        let last = (from + n as f64 * speed).floor() as i64;
        while !self.eos && self.at + self.buf.len() as i64 / 2 < last + 2 {
            let (mut flags, mut ts, mut s) = (0u32, 0i64, None);
            if self.r.ReadSample(AUDIO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut s)).is_err() {
                self.eos = true;
                break;
            }
            if let Some(s) = s {
                let f = frames_at(ts);
                let end = self.at + self.buf.len() as i64 / 2;
                if self.buf.is_empty() {
                    self.at = f;
                } else if f > end {
                    self.buf.resize(self.buf.len() + 2 * (f - end) as usize, 0);
                }
                self.buf.extend(sample_bytes(&s)?.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c)));
            }
            if flags & EOS != 0 {
                self.eos = true;
            }
        }
        let have = self.buf.len() as i64 / 2;
        for k in 0..n {
            let pos = from + k as f64 * speed - self.at as f64;
            if pos < 0.0 {
                continue;
            }
            let i = pos.floor() as i64;
            if i >= have {
                break;
            }
            // Entre dos cuadros del archivo, la mezcla de los dos.
            let (a, b, f) = (i as usize * 2, (i + 1).min(have - 1) as usize * 2, (pos - i as f64) as f32);
            let g = gain(k);
            for ch in 0..2 {
                let v = self.buf[a + ch] as f32 * (1.0 - f) + self.buf[b + ch] as f32 * f;
                out[k as usize * 2 + ch] += v * g;
            }
        }
        let used = (last - self.at).clamp(0, have);
        self.buf.drain(..used as usize * 2);
        self.at += used;
        Ok(())
    }
}

/// Mezcla de todo lo que suena, con el volumen y los fundidos de cada pieza.
struct Mixer {
    items: Vec<(Clip, Option<Pcm>)>,
}

impl Mixer {
    /// None si no suena nada (la salida no lleva audio).
    fn new(edit: &Edit) -> Option<Mixer> {
        let items: Vec<_> = edit.audio.iter().flatten().map(|c| (c.clone(), None)).collect();
        (!items.is_empty()).then_some(Mixer { items })
    }

    /// Mezcla `[from, to)` de la línea de tiempo y lo escribe en `stream` a partir de `at` (tiempo de la salida).
    unsafe fn write(&mut self, w: &IMFSinkWriter, stream: u32, from: i64, to: i64, at: i64) -> Result<()> {
        let (f0, f1, out0) = (frames_at(from), frames_at(to), frames_at(at));
        let mut f = f0;
        while f < f1 {
            let n = (f1 - f).min(4800);
            let mut acc = vec![0f32; n as usize * 2];
            for (c, pcm) in &mut self.items {
                let (a, b) = (frames_at(c.start), frames_at(c.end()));
                if b <= f {
                    *pcm = None;
                    continue;
                }
                if a >= f + n || c.gain <= 0.0 {
                    continue;
                }
                let (lo, hi) = (f.max(a), (f + n).min(b));
                if pcm.is_none() {
                    match Pcm::open(&c.path, c.src_at(c.start + time_of(lo - a))) {
                        Ok(p) => *pcm = Some(p),
                        Err(e) => {
                            crate::log::log(&format!("exportar: sin audio de {} ({e:?})", c.name()));
                            c.gain = 0.0;
                            continue;
                        }
                    }
                }
                let c = &*c;
                let gain = |k: i64| c.gain_at(c.start + time_of(lo + k - a));
                if let Some(p) = pcm {
                    let from = frames_at(c.src_in) as f64 + (lo - a) as f64 * c.speed;
                    p.mix(from, c.speed, &mut acc[((lo - f) * 2) as usize..((hi - f) * 2) as usize], gain)?;
                }
            }
            let bytes: Vec<u8> = acc.iter().flat_map(|v| (v.clamp(i16::MIN as f32, i16::MAX as f32) as i16).to_le_bytes()).collect();
            w.WriteSample(stream, &pcm_sample(&bytes, time_of(out0 + f - f0))?)?;
            f += n;
        }
        Ok(())
    }
}

/// Forma de onda del audio del archivo (decodifica solo el audio, una vez; después va a la caché).
pub fn waveform(path: &Path) -> Result<Wave> {
    unsafe {
        let r = reader(path, false, None, false)?;
        r.SetStreamSelection(ANY, false)?;
        r.SetStreamSelection(AUDIO, true)?;
        r.SetCurrentMediaType(AUDIO, None, &pcm_type()?)?;
        let mut w = WaveBuilder::new();
        loop {
            let (mut flags, mut ts, mut s) = (0u32, 0i64, None);
            r.ReadSample(AUDIO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut s))?;
            if let Some(s) = s {
                // Rellenar huecos (por ejemplo si el audio empieza más tarde).
                w.pad_to_time(ts);
                for c in sample_bytes(&s)?.as_chunks::<FRAME_BYTES>().0 {
                    w.push(i16::from_le_bytes([c[0], c[1]]), i16::from_le_bytes([c[2], c[3]]));
                }
            }
            if flags & EOS != 0 {
                break;
            }
        }
        Ok(w.finish())
    }
}

/// Video de una pieza decodificado en orden, en NV12: del tamaño de la salida si se ve tal cual; si no, del
/// tamaño del archivo, para recortarlo, girarlo y ubicarlo en el lienzo.
struct Frames {
    r: IMFSourceReader,
    /// El cuadro que se ve ahora y el siguiente (tiempo del archivo y muestra).
    cur: Option<(i64, IMFSample)>,
    next: Option<(i64, IMFSample)>,
    eos: bool,
    /// Tamaño de los cuadros que entrega.
    size: (u32, u32),
    alloc_h: u32,
    stride: i32,
    /// El último cuadro que se escribió tal cual (tiempo del archivo) y cuándo (tiempo de la salida).
    written: Option<(i64, i64)>,
}

impl Frames {
    unsafe fn open(c: &Clip, from: i64, (tw, th): (u32, u32), gpu: Option<&IMFDXGIDeviceManager>) -> Result<Frames> {
        let r = reader(&c.path, true, gpu, false)?;
        r.SetStreamSelection(ANY, false)?;
        r.SetStreamSelection(VIDEO, true)?;
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(tw, th))?;
        if r.SetCurrentMediaType(VIDEO, None, &t).is_err() {
            if r.GetNativeMediaType(VIDEO, 0).and_then(|t| t.GetGUID(&MF_MT_SUBTYPE)).ok() == Some(MFVideoFormat_HEVC) {
                bail!(tr!(
                    "para recodificar clips HEVC hace falta instalar \"Extensiones de video HEVC\" (Microsoft Store)",
                    "re-encoding HEVC clips needs \"HEVC Video Extensions\" (Microsoft Store)",
                    "para recodificar clipes HEVC é preciso instalar \"Extensões de Vídeo HEVC\" (Microsoft Store)"
                ));
            }
            let name = c.name();
            bail!(trf!("no se pudo decodificar {name}", "couldn't decode {name}", "não foi possível decodificar {name}"));
        }
        // Alto real de los cuadros que entrega el decodificador (puede ser 1088 en vez de 1080).
        let cur = r.GetCurrentMediaType(VIDEO)?;
        let alloc_h = unpack(cur.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(pack(tw, th))).1.max(th);
        let stride = cur.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|v| v as i32).unwrap_or(tw as i32);
        seek(&r, from)?;
        Ok(Frames { r, cur: None, next: None, eos: false, size: (tw, th), alloc_h, stride, written: None })
    }

    /// Avanza hasta el cuadro que se ve en `src` (tiempo del archivo, con `tol` de margen).
    unsafe fn seek_to(&mut self, src: i64, tol: i64) -> Result<()> {
        loop {
            if self.next.is_none() && !self.eos {
                let (mut flags, mut ts, mut s) = (0u32, 0i64, None);
                self.r.ReadSample(VIDEO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut s))?;
                if flags & EOS != 0 {
                    self.eos = true;
                }
                if let Some(s) = s {
                    self.next = Some((ts, s));
                }
            }
            match &self.next {
                Some((ts, _)) if *ts <= src + tol || self.cur.is_none() => self.cur = self.next.take(),
                None if !self.eos => {}
                _ => return Ok(()),
            }
        }
    }

    fn sample(&self) -> Option<&IMFSample> {
        self.cur.as_ref().map(|(_, s)| s)
    }

    /// El cuadro actual en NV12 compacto, para fundirlo o escribirlo desde la CPU.
    unsafe fn bytes(&self) -> Result<Vec<u8>> {
        let s = self.sample().ok_or_else(|| anyhow!("sin cuadro"))?;
        nv12_bytes(s, self.size.0, self.size.1, self.alloc_h, self.stride)
    }
}

/// `D3D11_VIDEO_COLOR` del negro en YCbCr (Y = 16, U = V = 128).
const VP_BLACK: D3D11_VIDEO_COLOR = D3D11_VIDEO_COLOR { Anonymous: D3D11_VIDEO_COLOR_0 { YCbCr: D3D11_VIDEO_COLOR_YCbCrA { Y: 16.0 / 255.0, Cb: 0.5, Cr: 0.5, A: 1.0 } } };

/// Procesador de video de la GPU (sin shaders) para los cuadros que no salen tal cual del decodificador: ubicar
/// uno en el lienzo (recortado, girado, espejado y escalado, con negro alrededor), fundir dos (el de arriba con
/// transparencia) y el negro de los huecos. Recibe cuadros de un solo tamaño (`in_size`).
struct Vp {
    dev: ID3D11Device,
    ctx: ID3D11DeviceContext,
    vdev: ID3D11VideoDevice,
    vctx: ID3D11VideoContext,
    en: ID3D11VideoProcessorEnumerator,
    vp: ID3D11VideoProcessor,
    inputs: Vec<(ID3D11Texture2D, ID3D11VideoProcessorInputView)>,
    black: ID3D11Texture2D,
    /// Lo que sabe hacer (`D3D11_VIDEO_PROCESSOR_FEATURE_CAPS`).
    caps: u32,
    in_size: (u32, u32),
    out_size: (u32, u32),
}

impl Vp {
    unsafe fn new(mgr: &IMFDXGIDeviceManager, in_size: (u32, u32), out_size: (u32, u32), streams: u32) -> Result<Vp> {
        let hd = mgr.OpenDeviceHandle()?;
        let mut dev: Option<ID3D11Device> = None;
        let got = mgr.GetVideoService(hd, &ID3D11Device::IID, &mut dev as *mut _ as *mut *mut c_void);
        let _ = mgr.CloseDeviceHandle(hd);
        got?;
        let dev = dev.ok_or_else(|| anyhow!("sin D3D11"))?;
        let ctx = dev.GetImmediateContext()?;
        let vdev: ID3D11VideoDevice = dev.cast()?;
        let vctx: ID3D11VideoContext = ctx.cast()?;
        let rate = DXGI_RATIONAL { Numerator: 60, Denominator: 1 };
        let cd = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: in_size.0,
            InputHeight: in_size.1,
            OutputFrameRate: rate,
            OutputWidth: out_size.0,
            OutputHeight: out_size.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        let en = vdev.CreateVideoProcessorEnumerator(&cd)?;
        let mut caps = D3D11_VIDEO_PROCESSOR_CAPS::default();
        en.GetVideoProcessorCaps(&mut caps)?;
        if streams > 1 && (caps.MaxInputStreams < streams || caps.FeatureCaps & D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_ALPHA_STREAM.0 as u32 == 0) {
            bail!(tr!(
                "el procesador de video de esta GPU no funde dos imágenes",
                "this GPU's video processor can't blend two images",
                "o processador de vídeo desta GPU não funde duas imagens"
            ));
        }
        let vp = vdev.CreateVideoProcessor(&en, 0)?;
        let ivd = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
        };
        let mut inputs = Vec::new();
        for i in 0..streams {
            let t = nv12_texture(&dev, in_size.0, in_size.1, None)?;
            let mut v = None;
            vdev.CreateVideoProcessorInputView(&t, &en, &ivd, Some(&mut v))?;
            inputs.push((t, v.ok_or_else(|| anyhow!("sin vista de entrada"))?));
            vctx.VideoProcessorSetStreamFrameFormat(&vp, i, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            vctx.VideoProcessorSetStreamAutoProcessingMode(&vp, i, false);
        }
        // Entradas y salida en el mismo espacio de color: el procesador no convierte nada.
        if let Ok(v1) = vctx.cast::<ID3D11VideoContext1>() {
            for i in 0..streams {
                v1.VideoProcessorSetStreamColorSpace1(&vp, i, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
            }
            v1.VideoProcessorSetOutputColorSpace1(&vp, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
        }
        // Lo que no tapa ninguna imagen queda negro.
        vctx.VideoProcessorSetOutputBackgroundColor(&vp, true, &VP_BLACK);
        let black = nv12_texture(&dev, out_size.0, out_size.1, Some(&black_nv12(out_size.0, out_size.1)))?;
        Ok(Vp { dev, ctx, vdev, vctx, en, vp, inputs, black, caps: caps.FeatureCaps, in_size, out_size })
    }

    /// Copia el cuadro de `s` (una textura del decodificador o de otro procesador) a la entrada `i`.
    unsafe fn load(&self, i: usize, s: &IMFSample) -> Result<()> {
        let b: IMFDXGIBuffer = s.GetBufferByIndex(0)?.cast()?;
        let mut tex: Option<ID3D11Texture2D> = None;
        b.GetResource(&ID3D11Texture2D::IID, &mut tex as *mut _ as *mut *mut c_void)?;
        let tex = tex.ok_or_else(|| anyhow!("cuadro sin textura"))?;
        // Solo el área de la imagen: el decodificador puede entregar cuadros más altos (1088).
        let (w, h) = self.in_size;
        let area = D3D11_BOX { left: 0, top: 0, front: 0, right: w, bottom: h, back: 1 };
        self.ctx.CopySubresourceRegion(&self.inputs[i].0, 0, 0, 0, 0, &tex, b.GetSubresourceIndex()?, Some(&area));
        Ok(())
    }

    /// Pasa las entradas por el procesador a un cuadro nuevo de la salida.
    unsafe fn run(&self) -> Result<IMFSample> {
        let out = nv12_texture(&self.dev, self.out_size.0, self.out_size.1, None)?;
        let ovd = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
        };
        let mut view = None;
        self.vdev.CreateVideoProcessorOutputView(&out, &self.en, &ovd, Some(&mut view))?;
        let view = view.ok_or_else(|| anyhow!("sin vista de salida"))?;
        let mut streams: Vec<D3D11_VIDEO_PROCESSOR_STREAM> = self
            .inputs
            .iter()
            .map(|(_, v)| D3D11_VIDEO_PROCESSOR_STREAM { Enable: true.into(), pInputSurface: ManuallyDrop::new(Some(v.clone())), ..Default::default() })
            .collect();
        let r = self.vctx.VideoProcessorBlt(&self.vp, &view, 0, &streams);
        for s in &mut streams {
            ManuallyDrop::drop(&mut s.pInputSurface);
        }
        r?;
        surface_sample(&out)
    }

    /// `b` encima de `a` con opacidad `alpha`, en un cuadro nuevo.
    unsafe fn blend(&self, a: &IMFSample, b: &IMFSample, alpha: f32) -> Result<IMFSample> {
        self.load(0, a)?;
        self.load(1, b)?;
        self.vctx.VideoProcessorSetStreamAlpha(&self.vp, 1, true, alpha);
        self.run()
    }

    /// El cuadro `s` en el lienzo como dice `p`, girado `turns` cuartos de vuelta y espejado si `flip`; negro
    /// alrededor.
    unsafe fn place(&self, s: &IMFSample, p: &Placed, f: &Frame) -> Result<IMFSample> {
        let turns = f.turns % 4;
        if turns != 0 && self.caps & D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_ROTATION.0 as u32 == 0 {
            bail!(tr!("la GPU no puede girar la imagen", "the GPU can't rotate the picture", "a GPU não consegue girar a imagem"));
        }
        self.load(0, s)?;
        let (w, h) = (self.in_size.0 as f32, self.in_size.1 as f32);
        let px = |r: [f32; 4], (w, h): (f32, f32)| windows::Win32::Foundation::RECT {
            left: (r[0] * w).round() as i32,
            top: (r[1] * h).round() as i32,
            right: (r[2] * w).round() as i32,
            bottom: (r[3] * h).round() as i32,
        };
        let src = px(p.src_rect(), (w, h));
        let dst = px(p.dst, (1.0, 1.0));
        self.vctx.VideoProcessorSetStreamSourceRect(&self.vp, 0, true, Some(&src));
        self.vctx.VideoProcessorSetStreamDestRect(&self.vp, 0, true, Some(&dst));
        // El procesador espeja antes de girar: girar y después espejar es lo mismo que espejar y girar al revés.
        let turns = if f.flip { (4 - turns) % 4 } else { turns };
        let rot = [D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY, D3D11_VIDEO_PROCESSOR_ROTATION_90, D3D11_VIDEO_PROCESSOR_ROTATION_180, D3D11_VIDEO_PROCESSOR_ROTATION_270];
        self.vctx.VideoProcessorSetStreamRotation(&self.vp, 0, turns != 0, rot[turns as usize]);
        if let Ok(v1) = self.vctx.cast::<ID3D11VideoContext1>() {
            v1.VideoProcessorSetStreamMirror(&self.vp, 0, f.flip, f.flip, false);
        } else if f.flip {
            bail!(tr!("la GPU no puede espejar la imagen", "the GPU can't mirror the picture", "a GPU não consegue espelhar a imagem"));
        }
        self.run()
    }

    unsafe fn black(&self) -> Result<IMFSample> {
        surface_sample(&self.black)
    }
}

/// El procesador de video que recibe cuadros de `in_size` y entrega del tamaño de la salida, creado la primera vez
/// que hace falta.
unsafe fn vp<'a>(all: &'a mut HashMap<(u32, u32), Vp>, mgr: &IMFDXGIDeviceManager, in_size: (u32, u32), out_size: (u32, u32), streams: u32) -> Result<&'a Vp> {
    Ok(match all.entry(in_size) {
        std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
        std::collections::hash_map::Entry::Vacant(e) => e.insert(Vp::new(mgr, in_size, out_size, streams)?),
    })
}

/// Textura NV12 que el procesador de video puede escribir y el encoder leer.
unsafe fn nv12_texture(dev: &ID3D11Device, w: u32, h: u32, init: Option<&[u8]>) -> Result<ID3D11Texture2D> {
    let mut td = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_VIDEO_ENCODER.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let data = init.map(|px| D3D11_SUBRESOURCE_DATA { pSysMem: px.as_ptr() as *const _, SysMemPitch: w, SysMemSlicePitch: 0 });
    let data = data.as_ref().map(|d| d as *const _);
    let mut t = None;
    if dev.CreateTexture2D(&td, data, Some(&mut t)).is_err() {
        td.BindFlags = D3D11_BIND_RENDER_TARGET.0 as u32;
        dev.CreateTexture2D(&td, data, Some(&mut t))?;
    }
    t.ok_or_else(|| anyhow!("sin textura"))
}

unsafe fn surface_sample(t: &ID3D11Texture2D) -> Result<IMFSample> {
    let buf = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, t, 0, false)?;
    if let Ok(len) = buf.cast::<IMF2DBuffer>().and_then(|b| b.GetContiguousLength()) {
        let _ = buf.SetCurrentLength(len);
    }
    let s = MFCreateSample()?;
    s.AddBuffer(&buf)?;
    Ok(s)
}

/// Otra muestra con la misma imagen que `s`, para escribirla con otro tiempo.
unsafe fn same_image(s: &IMFSample) -> Result<IMFSample> {
    let o = MFCreateSample()?;
    o.AddBuffer(&s.GetBufferByIndex(0)?)?;
    Ok(o)
}

/// Cuadro NV12 negro (Y = 16, UV = 128).
fn black_nv12(w: u32, h: u32) -> Vec<u8> {
    let n = (w * h) as usize;
    let mut v = vec![16u8; n];
    v.resize(n * 3 / 2, 128);
    v
}

/// `b` encima de `a` con opacidad `alpha` (los dos NV12 compactos del mismo tamaño).
fn mix_nv12(a: &[u8], b: &[u8], alpha: f32) -> Vec<u8> {
    let k = (alpha.clamp(0.0, 1.0) * 256.0) as u32;
    a.iter().zip(b).map(|(&x, &y)| ((x as u32 * (256 - k) + y as u32 * k) >> 8) as u8).collect()
}

/// La imagen NV12 compacta `src` (`sw`×`sh`) en un lienzo negro de `tw`×`th` como dice `p` (lo mismo que hace
/// `Vp::place` por GPU, con el píxel más cercano).
fn place_nv12(src: &[u8], (sw, sh): (u32, u32), p: &Placed, (tw, th): (u32, u32)) -> Vec<u8> {
    let (sw, sh, tw, th) = (sw as usize, sh as usize, tw as usize, th as usize);
    let mut out = black_nv12(tw as u32, th as u32);
    let [l, t, r, b] = p.dst;
    let (x0, y0, x1, y1) = (l.round() as usize, t.round() as usize, (r.round() as usize).min(tw), (b.round() as usize).min(th));
    // El píxel del archivo que cae en el punto (x, y) del lienzo.
    let at = |x: f32, y: f32| {
        let [u, v] = p.uv_at((x - l) / (r - l), (y - t) / (b - t));
        (((u * sw as f32) as usize).min(sw - 1), ((v * sh as f32) as usize).min(sh - 1))
    };
    for y in y0..y1 {
        for x in x0..x1 {
            let (sx, sy) = at(x as f32 + 0.5, y as f32 + 0.5);
            out[y * tw + x] = src[sy * sw + sx];
        }
    }
    // Color: U y V juntos, uno cada 2×2 píxeles.
    for y in y0 / 2..y1.div_ceil(2).min(th / 2) {
        for x in x0 / 2..x1.div_ceil(2).min(tw / 2) {
            let (sx, sy) = at(x as f32 * 2.0 + 1.0, y as f32 * 2.0 + 1.0);
            let (si, di) = (sw * sh + sy / 2 * sw + sx / 2 * 2, tw * th + y * tw + x * 2);
            out[di] = src[si];
            out[di + 1] = src[si + 1];
        }
    }
    out
}

/// Un cuadro que no cambió no se vuelve a escribir (un clip de 30 fps sale a 60 sin cuadros de más), salvo cada
/// tanto: una imagen congelada también tiene que seguir teniendo cuadros.
const REPEAT_EVERY: i64 = 5_000_000;

/// Cómo entra cada pieza de video en el lienzo: tal cual (del tamaño de la salida) o ubicada (`Placed`).
struct Fit {
    direct: bool,
    placed: Option<Placed>,
    /// Tamaño al que se decodifica.
    size: (u32, u32),
}

impl Fit {
    fn shows(&self) -> bool {
        self.direct || self.placed.is_some()
    }
}

/// El cuadro actual de `f` ya en el lienzo (por GPU).
unsafe fn canvas_sample(f: &Frames, fit: &Fit, frame: &Frame, mgr: &IMFDXGIDeviceManager, placers: &mut HashMap<(u32, u32), Vp>, size: (u32, u32)) -> Result<IMFSample> {
    let s = f.sample().ok_or_else(|| anyhow!("sin cuadro"))?;
    match &fit.placed {
        Some(p) if !fit.direct => vp(placers, mgr, f.size, size, 1)?.place(s, p, frame),
        _ => same_image(s),
    }
}

/// El cuadro actual de `f` ya en el lienzo (por CPU).
unsafe fn canvas_bytes(f: &Frames, fit: &Fit, size: (u32, u32)) -> Result<Vec<u8>> {
    match &fit.placed {
        Some(p) if !fit.direct => Ok(place_nv12(&f.bytes()?, f.size, p, size)),
        _ => f.bytes(),
    }
}

/// Arma el video cuadro a cuadro y lo vuelve a codificar: en cada cuadro se ve la pista visible más alta
/// que tenga algo (donde dos piezas se superponen, fundidas; donde no hay nada, negro), cada pieza recortada,
/// girada y ubicada en el lienzo, y suena la mezcla de todo. Con `gpu` los cuadros no salen de la placa de
/// video (la GPU decodifica, escala, ubica, funde y codifica); sin ella se decodifica y se arma por CPU y se
/// codifica por GPU.
unsafe fn export_reencode(edit: &Edit, probes: &HashMap<PathBuf, Probe>, out: &Path, opts: &ExportOpts, prog: &Progress, gpu: Option<&IMFDXGIDeviceManager>) -> Result<()> {
    let info = edit.first_video().map(|c| &probes[&c.path]).ok_or_else(no_video)?;
    let src_rate = if info.fps() > 0.0 { info.frame_rate } else { (60, 1) };
    let target = opts.target.unwrap_or(Target { height: 0, fps: 0, bitrate: opts.bitrate, constant: false });
    let (tw, th) = canvas_size((info.width, info.height), edit.ratio, target.height);
    let fps = if target.fps == 0 || (target.fps as f64) >= src_rate.0 as f64 / src_rate.1 as f64 - 0.5 {
        src_rate
    } else {
        (target.fps, 1)
    };

    let w = sink(out, true, gpu)?;
    let vo = MFCreateMediaType()?;
    vo.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    vo.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
    vo.SetUINT32(&MF_MT_AVG_BITRATE, target.bitrate)?;
    vo.SetUINT64(&MF_MT_FRAME_SIZE, pack(tw, th))?;
    vo.SetUINT64(&MF_MT_FRAME_RATE, pack(fps.0, fps.1.max(1)))?;
    vo.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
    vo.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    vo.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
    let vs = w.AddStream(&vo)?;
    let vi = MFCreateMediaType()?;
    vi.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    vi.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
    vi.SetUINT64(&MF_MT_FRAME_SIZE, pack(tw, th))?;
    vi.SetUINT64(&MF_MT_FRAME_RATE, pack(fps.0, fps.1.max(1)))?;
    vi.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
    vi.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    // Control de bitrate del encoder: constante para respetar un tamaño máximo, variable para calidad.
    let mut enc = None;
    MFCreateAttributes(&mut enc, 2)?;
    let enc = enc.unwrap();
    let mode = if target.constant { eAVEncCommonRateControlMode_CBR } else { eAVEncCommonRateControlMode_UnconstrainedVBR };
    enc.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, mode.0 as u32)?;
    enc.SetUINT32(&CODECAPI_AVEncCommonMeanBitRate, target.bitrate)?;
    if w.SetInputMediaType(vs, &vi, &enc).is_err() {
        w.SetInputMediaType(vs, &vi, None)?;
    }

    let mut mix = Mixer::new(edit);
    let mix_stream = if mix.is_some() { Some(pcm_stream(&w)?) } else { None };
    w.BeginWriting()?;

    // El video sale a cuadros fijos.
    let fdur = 10_000_000 * fps.1.max(1) as i64 / fps.0.max(1) as i64;
    let size = (tw, th);
    let len = edit.len();
    // Cómo entra cada pieza en el lienzo: tal cual si tiene su forma y no se tocó su imagen.
    let fits: Vec<Vec<Fit>> = edit
        .video
        .iter()
        .map(|t| {
            t.iter()
                .map(|c| {
                    let p = &probes[&c.path];
                    let src = (p.width.max(2) & !1, p.height.max(2) & !1);
                    let direct = c.frame.is_plain() && same_shape(src, size);
                    let placed = c.frame.place((src.0 as f32, src.1 as f32), (tw as f32, th as f32));
                    Fit { direct, placed, size: if direct { size } else { src } }
                })
                .collect()
        })
        .collect();
    let mut blend_gpu: HashMap<(u32, u32), Vp> = HashMap::new();
    let mut placers: HashMap<(u32, u32), Vp> = HashMap::new();
    // Lo que se está leyendo de cada pieza de video (se abre al llegar a ella y se cierra al pasarla).
    let mut open: Vec<Vec<Option<Frames>>> = edit.video.iter().map(|t| t.iter().map(|_| None).collect()).collect();
    let mut t = 0i64;
    while t < len {
        prog.check()?;
        for (track, frames) in edit.video.iter().zip(&mut open) {
            for (c, f) in track.iter().zip(frames.iter_mut()) {
                if c.end() <= t {
                    *f = None;
                }
            }
        }
        // Lo que se ve en `t`, de abajo hacia arriba (con lo que todavía no tiene cuadro, afuera).
        let shown: Vec<(usize, usize, f32)> = layers(&edit.video, t).into_iter().filter(|&(ti, i, _)| fits[ti][i].shows()).collect();
        for &(ti, i, _) in &shown {
            let c = &edit.video[ti][i];
            let src = c.src_at(t);
            if open[ti][i].is_none() {
                open[ti][i] = Some(Frames::open(c, src, fits[ti][i].size, gpu)?);
            }
            if let Some(f) = &mut open[ti][i] {
                f.seek_to(src, fdur / 2)?;
            }
        }
        let ready: Vec<(usize, usize, f32)> = shown.iter().copied().filter(|&(ti, i, _)| open[ti][i].as_ref().is_some_and(|f| f.cur.is_some())).collect();
        let sample = match ready[..] {
            [] if !shown.is_empty() => None,
            // Una sola imagen que tapa todo: su cuadro, si es nuevo.
            [(ti, i, a)] if a >= 1.0 => {
                let f = open[ti][i].as_mut().unwrap();
                let ts = f.cur.as_ref().map_or(0, |c| c.0);
                // El último cuadro siempre va: si no, el video terminaría donde se escribió el anterior.
                if f.written.is_some_and(|(w, at)| w == ts && t - at < REPEAT_EVERY) && t + fdur < len {
                    None
                } else {
                    f.written = Some((ts, t));
                    let (fit, frame) = (&fits[ti][i], &edit.video[ti][i].frame);
                    Some(match gpu {
                        Some(m) => canvas_sample(f, fit, frame, m, &mut placers, size)?,
                        None => mem_sample(&canvas_bytes(f, fit, size)?)?,
                    })
                }
            }
            // Cada capa encima de lo de abajo con su opacidad, empezando por negro si lo de abajo no tapa todo.
            _ => Some(match gpu {
                Some(m) => {
                    let mut acc: Option<IMFSample> = None;
                    for &(ti, i, a) in &ready {
                        let f = open[ti][i].as_ref().unwrap();
                        let s = canvas_sample(f, &fits[ti][i], &edit.video[ti][i].frame, m, &mut placers, size)?;
                        let b = vp(&mut blend_gpu, m, size, size, 2)?;
                        acc = Some(match acc {
                            None if a >= 1.0 => s,
                            None => b.blend(&b.black()?, &s, a)?,
                            Some(under) => b.blend(&under, &s, a)?,
                        });
                    }
                    match acc {
                        Some(s) => s,
                        None => vp(&mut blend_gpu, m, size, size, 2)?.black()?,
                    }
                }
                None => {
                    let mut acc = black_nv12(tw, th);
                    for &(ti, i, a) in &ready {
                        acc = mix_nv12(&acc, &canvas_bytes(open[ti][i].as_ref().unwrap(), &fits[ti][i], size)?, a);
                    }
                    mem_sample(&acc)?
                }
            }),
        };
        if let Some(s) = sample {
            s.SetSampleTime(t)?;
            s.SetSampleDuration(fdur)?;
            w.WriteSample(vs, &s)?;
        }
        if let (Some(m), Some(ms)) = (&mut mix, mix_stream) {
            m.write(&w, ms, t, (t + fdur).min(len), t)?;
        }
        prog.set(t as f32 / len as f32);
        t += fdur;
    }
    w.Finalize()?;
    Ok(())
}

// --- Índice MP4 (moov/trak/.../stss + stts) -------------------------------------------

fn mp4_keyframes(path: &Path) -> Option<Vec<i64>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut pos = 0u64;
    // Buscar la caja "moov" entre las de primer nivel sin leer los datos de video.
    let moov = loop {
        if pos + 8 > len {
            return None;
        }
        f.seek(SeekFrom::Start(pos)).ok()?;
        let mut h = [0u8; 16];
        f.read_exact(&mut h[..8]).ok()?;
        let mut size = u32::from_be_bytes(h[..4].try_into().ok()?) as u64;
        let mut header = 8u64;
        if size == 1 {
            f.read_exact(&mut h[8..16]).ok()?;
            size = u64::from_be_bytes(h[8..16].try_into().ok()?);
            header = 16;
        } else if size == 0 {
            size = len - pos;
        }
        if size < header {
            return None;
        }
        if &h[4..8] == b"moov" {
            let mut buf = vec![0u8; (size - header) as usize];
            f.read_exact(&mut buf).ok()?;
            break buf;
        }
        pos += size;
    };
    for trak in boxes(&moov).filter(|(t, _)| t == b"trak").map(|(_, b)| b) {
        let mdia = child(trak, b"mdia")?;
        let hdlr = child(mdia, b"hdlr")?;
        if hdlr.get(8..12)? != b"vide" {
            continue;
        }
        let mdhd = child(mdia, b"mdhd")?;
        let timescale = if mdhd[0] == 1 { be32(mdhd, 20)? } else { be32(mdhd, 12)? } as i64;
        let stbl = child(child(mdia, b"minf")?, b"stbl")?;
        let stts = child(stbl, b"stts")?;
        // Tiempo (en unidades del track) de cada cuadro.
        let mut times = Vec::new();
        let mut t = 0i64;
        for i in 0..be32(stts, 4)? as usize {
            let count = be32(stts, 8 + i * 8)?;
            let delta = be32(stts, 12 + i * 8)? as i64;
            for _ in 0..count {
                times.push(t);
                t += delta;
            }
        }
        let to_100ns = |x: i64| x * 10_000_000 / timescale.max(1);
        let keys: Vec<i64> = match child(stbl, b"stss") {
            Some(stss) => (0..be32(stss, 4)? as usize)
                .filter_map(|i| be32(stss, 8 + i * 4))
                .filter_map(|n| times.get((n as usize).checked_sub(1)?).copied())
                .map(to_100ns)
                .collect(),
            None => times.iter().copied().map(to_100ns).collect(), // sin tabla: todos son clave
        };
        return (!keys.is_empty()).then_some(keys);
    }
    None
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn boxes(mut b: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    std::iter::from_fn(move || {
        if b.len() < 8 {
            return None;
        }
        let size = u32::from_be_bytes(b[..4].try_into().ok()?) as usize;
        let typ: [u8; 4] = b[4..8].try_into().ok()?;
        let (hdr, size) = if size == 1 {
            (16, u64::from_be_bytes(b.get(8..16)?.try_into().ok()?) as usize)
        } else if size == 0 {
            (8, b.len())
        } else {
            (8, size)
        };
        if size < hdr || size > b.len() {
            return None;
        }
        let body = &b[hdr..size];
        b = &b[size..];
        Some((typ, body))
    })
}

fn child<'a>(b: &'a [u8], typ: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(b).find(|(t, _)| t == typ).map(|(_, body)| body)
}

/// Copia un cuadro NV12 a un búfer compacto de exactamente `w`×`h`.
/// El decodificador puede entregar cuadros con más alto (alineado a 16) y otro paso de fila;
/// si se pasaban tal cual, el plano de color quedaba corrido: barra verde y colores duplicados.
unsafe fn nv12_bytes(s: &IMFSample, w: u32, h: u32, alloc_h: u32, stride: i32) -> Result<Vec<u8>> {
    let (w, h) = (w as usize, h as usize);
    let src = s.ConvertToContiguousBuffer()?;
    let mut out = vec![0u8; w * h * 3 / 2];
    let copy = |base: *const u8, pitch: usize, plane_rows: usize, out: &mut [u8]| {
        for y in 0..h {
            std::ptr::copy_nonoverlapping(base.add(y * pitch), out[y * w..].as_mut_ptr(), w);
        }
        let uv = base.add(pitch * plane_rows);
        for y in 0..h / 2 {
            std::ptr::copy_nonoverlapping(uv.add(y * pitch), out[w * h + y * w..].as_mut_ptr(), w);
        }
    };
    if let Ok(b2) = src.cast::<IMF2DBuffer2>() {
        let (mut p, mut pitch, mut start, mut len) = (std::ptr::null_mut(), 0i32, std::ptr::null_mut(), 0u32);
        b2.Lock2DSize(MF2DBuffer_LockFlags_Read, &mut p, &mut pitch, &mut start, &mut len)?;
        let pitch_u = pitch.unsigned_abs() as usize;
        // Filas reales del plano Y = tamaño del búfer / paso / 1,5.
        let rows = ((len as usize).saturating_sub(p.offset_from(start) as usize) / pitch_u.max(1) * 2 / 3).max(h);
        copy(p, pitch_u, rows, &mut out);
        b2.Unlock2D()?;
    } else if let Ok(b2) = src.cast::<IMF2DBuffer>() {
        let mut p = std::ptr::null_mut();
        let mut pitch = 0i32;
        b2.Lock2D(&mut p, &mut pitch)?;
        copy(p, pitch.unsigned_abs() as usize, alloc_h as usize, &mut out);
        b2.Unlock2D()?;
    } else {
        let mut p = std::ptr::null_mut();
        let mut len = 0u32;
        src.Lock(&mut p, None, Some(&mut len))?;
        let pitch = (stride.unsigned_abs() as usize).max(w);
        // El alto real del plano Y se deduce del tamaño del búfer (Y + UV/2).
        let rows = (len as usize / pitch * 2 / 3).max(h);
        copy(p, pitch, rows, &mut out);
        src.Unlock()?;
    }
    Ok(out)
}

/// Muestra en la memoria con un cuadro NV12 compacto.
unsafe fn mem_sample(px: &[u8]) -> Result<IMFSample> {
    let buf = MFCreateMemoryBuffer(px.len() as u32)?;
    let mut p = std::ptr::null_mut();
    buf.Lock(&mut p, None, None)?;
    std::ptr::copy_nonoverlapping(px.as_ptr(), p, px.len());
    buf.Unlock()?;
    buf.SetCurrentLength(px.len() as u32)?;
    let o = MFCreateSample()?;
    o.AddBuffer(&buf)?;
    Ok(o)
}
