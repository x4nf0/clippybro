//! Caché por archivo de la forma de onda y los fotogramas clave (para que el editor cargue al instante).
//! La grabadora la escribe al guardar cada clip; el editor la calcula una vez para videos importados.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::log::app_dir;

/// Un valor de forma de onda cada 5 ms (240 muestras a 48 kHz): con zoom se ve el detalle real.
pub const WAVE_BUCKET: i64 = 50_000;
/// Cambia cuando cambia el formato de la caché: las viejas se ignoran y se vuelven a calcular.
const CACHE_VERSION: u32 = 2;
/// Una caché que no se usa hace este tiempo se borra (el clip seguramente ya no existe o se movió).
const UNUSED_FOR: Duration = Duration::from_secs(60 * 24 * 60 * 60);

#[derive(Serialize, Deserialize)]
struct Cached {
    #[serde(default)]
    v: u32,
    size: u64,
    modified: u64,
    /// Pico y RMS de cada `WAVE_BUCKET`, 0..255, en hexadecimal (la mitad de largo que una lista JSON).
    peak: String,
    rms: String,
    keyframes: Vec<i64>,
}

/// Forma de onda como la guardan los editores: pico y volumen medio (RMS) de cada `WAVE_BUCKET`, 0..255.
#[derive(Clone, Default)]
pub struct Wave {
    peak: Vec<u8>,
    rms: Vec<u8>,
}

impl Wave {
    /// Cantidad de valores.
    #[cfg(feature = "devtools")]
    pub fn len(&self) -> usize {
        self.peak.len()
    }

    /// (pico, RMS) entre 0 y 1 de lo que suena entre `t0` y `t1` (100 ns): el pico más alto del tramo y
    /// su volumen medio de verdad (raíz de la media de los cuadrados). Lee todos los valores del tramo:
    /// es exacto y, aun con una hora de audio en pantalla, cuesta menos de medio milisegundo por cuadro.
    pub fn range(&self, t0: i64, t1: i64) -> (f32, f32) {
        let n = self.peak.len().min(self.rms.len());
        let i0 = ((t0.max(0) / WAVE_BUCKET) as usize).min(n);
        let i1 = (((t1.max(0) + WAVE_BUCKET - 1) / WAVE_BUCKET) as usize).max(i0 + 1).min(n);
        let (p, r) = (&self.peak[i0..i1], &self.rms[i0..i1]);
        let peak = p.iter().copied().max().unwrap_or(0);
        let sq: u64 = r.iter().map(|&v| (v as u32 * v as u32) as u64).sum();
        let rms = (sq as f32 / r.len().max(1) as f32).sqrt();
        (peak as f32 / 255.0, rms / 255.0)
    }
}

/// Arma la forma de onda a medida que llegan las muestras (estéreo, 48 kHz).
pub struct WaveBuilder {
    peak: Vec<u8>,
    rms: Vec<u8>,
    cur: u16,
    sum: f64,
    n: usize,
}

/// Muestras por valor de la forma de onda.
const PER: usize = crate::mf::frames_at(WAVE_BUCKET) as usize;

impl WaveBuilder {
    pub fn new() -> Self {
        Self { peak: Vec::new(), rms: Vec::new(), cur: 0, sum: 0.0, n: 0 }
    }

    /// Una muestra estéreo.
    pub fn push(&mut self, l: i16, r: i16) {
        self.cur = self.cur.max(l.unsigned_abs()).max(r.unsigned_abs());
        let (fl, fr) = (l as f64, r as f64);
        self.sum += (fl * fl + fr * fr) / 2.0;
        self.n += 1;
        if self.n == PER {
            self.flush();
        }
    }

    /// Silencio hasta el valor `len` (por ejemplo si el audio empieza más tarde que el video).
    pub fn pad_to_time(&mut self, t: i64) {
        let len = (t.max(0) / WAVE_BUCKET) as usize;
        while self.peak.len() < len {
            self.peak.push(0);
            self.rms.push(0);
        }
    }

    fn flush(&mut self) {
        let to_u8 = |v: f64| (v / 32768.0 * 255.0).round().clamp(0.0, 255.0) as u8;
        self.peak.push(to_u8(self.cur as f64));
        self.rms.push(to_u8((self.sum / self.n.max(1) as f64).sqrt()));
        self.cur = 0;
        self.sum = 0.0;
        self.n = 0;
    }

    pub fn finish(mut self) -> Wave {
        if self.n > 0 {
            self.flush();
        }
        Wave { peak: self.peak, rms: self.rms }
    }
}

/// Forma de onda de PCM estéreo intercalado de 48 kHz.
pub fn wave_from_pcm(pcm: &[i16]) -> Wave {
    let mut b = WaveBuilder::new();
    for c in pcm.as_chunks::<2>().0 {
        b.push(c[0], c[1]);
    }
    b.finish()
}

fn to_hex(v: &[u8]) -> String {
    v.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    (0..s.len()).step_by(2).map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok())).collect()
}

fn cache_dir() -> PathBuf {
    app_dir().join("cache")
}

/// Archivo de la caché de `p` (mientras `p` existe: su ruta canónica es la clave).
pub fn cache_file(p: &Path) -> PathBuf {
    // Ruta canónica: la misma clave aunque la ruta venga escrita con otras barras o mayúsculas.
    let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    // FNV-1a: da lo mismo en cualquier versión de Rust (el hash de la biblioteca estándar puede cambiar).
    let h = canon.to_string_lossy().to_lowercase().bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3));
    cache_dir().join(format!("{h:016x}.json"))
}

/// Borra las cachés que no se usaron en mucho tiempo (de clips borrados o movidos).
pub fn prune() {
    let Some(limit) = SystemTime::now().checked_sub(UNUSED_FOR) else { return };
    let Ok(rd) = std::fs::read_dir(cache_dir()) else { return };
    for e in rd.flatten() {
        if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < limit) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn file_stamp(p: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(p).ok()?;
    let t = m.modified().ok()?.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
    Some((m.len(), t))
}

/// (forma de onda, fotogramas clave en 100 ns) si la caché corresponde a este archivo tal como está.
pub fn load(p: &Path) -> Option<(Wave, Vec<i64>)> {
    let (size, modified) = file_stamp(p)?;
    let f = cache_file(p);
    let c: Cached = serde_json::from_str(&std::fs::read_to_string(&f).ok()?).ok()?;
    if c.v != CACHE_VERSION || c.size != size || c.modified != modified {
        return None;
    }
    let (peak, rms) = (from_hex(&c.peak)?, from_hex(&c.rms)?);
    if peak.len() != rms.len() {
        return None;
    }
    // Marcarla como usada, para que `prune` no la borre.
    if let Ok(file) = std::fs::File::options().write(true).open(&f) {
        let _ = file.set_modified(SystemTime::now());
    }
    Some((Wave { peak, rms }, c.keyframes))
}

pub fn save(p: &Path, wave: &Wave, keyframes: &[i64]) {
    let Some((size, modified)) = file_stamp(p) else { return };
    let c = Cached {
        v: CACHE_VERSION,
        size,
        modified,
        peak: to_hex(&wave.peak),
        rms: to_hex(&wave.rms),
        keyframes: keyframes.to_vec(),
    };
    let f = cache_file(p);
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(f, serde_json::to_string(&c).unwrap_or_default());
}
