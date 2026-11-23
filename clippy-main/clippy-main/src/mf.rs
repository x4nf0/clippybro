//! Media Foundation compartido y el formato de audio de toda la app (PCM de 16 bits, estéreo, 48 kHz):
//! así lo graba la grabadora, así se mezcla y comprime a AAC al guardar y así lo lee el editor.

use anyhow::Result;
use windows::Win32::Media::Audio::{WAVEFORMATEX, WAVE_FORMAT_PCM};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// Muestras de audio por segundo.
pub const RATE: i64 = 48_000;
/// Bytes de una muestra estéreo (dos canales de 16 bits).
pub const FRAME_BYTES: usize = 4;
/// AAC de los clips y las exportaciones: 192 kbps.
const AAC_BYTES_PER_SECOND: u32 = 24_000;

/// El formato de audio de Clippy para WASAPI (Windows convierte el del dispositivo a este).
pub const WAVE_FORMAT: WAVEFORMATEX = WAVEFORMATEX {
    wFormatTag: WAVE_FORMAT_PCM as u16,
    nChannels: 2,
    nSamplesPerSec: RATE as u32,
    nAvgBytesPerSec: RATE as u32 * FRAME_BYTES as u32,
    nBlockAlign: FRAME_BYTES as u16,
    wBitsPerSample: 16,
    cbSize: 0,
};

/// Muestras de audio que entran en `t` (100 ns).
pub const fn frames_at(t: i64) -> i64 {
    t * RATE / 10_000_000
}

/// Tiempo (100 ns) que duran `frames` muestras de audio.
pub const fn time_of(frames: i64) -> i64 {
    frames * 10_000_000 / RATE
}

/// Inicializa COM + Media Foundation en el hilo actual; se libera al soltarlo.
pub struct Mf {
    /// COM se inicializó acá (si el hilo ya lo tenía en otro modo, no se toca).
    com: bool,
}

impl Mf {
    pub fn init() -> Mf {
        unsafe {
            let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
            Mf { com }
        }
    }
}

impl Drop for Mf {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            if self.com {
                CoUninitialize();
            }
        }
    }
}

/// Tipo de Media Foundation del audio de Clippy (PCM).
pub unsafe fn pcm_type() -> Result<IMFMediaType> {
    let t = MFCreateMediaType()?;
    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
    t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
    t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
    t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, RATE as u32)?;
    t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
    t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, FRAME_BYTES as u32)?;
    t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, RATE as u32 * FRAME_BYTES as u32)?;
    t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
    Ok(t)
}

/// Tipo de Media Foundation del audio comprimido de los MP4 (AAC).
pub unsafe fn aac_type() -> Result<IMFMediaType> {
    let t = MFCreateMediaType()?;
    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
    t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
    t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
    t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, RATE as u32)?;
    t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
    t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AAC_BYTES_PER_SECOND)?;
    Ok(t)
}

/// Muestra de Media Foundation con una copia de `data`.
pub unsafe fn sample(data: &[u8], time: i64, dur: i64) -> Result<IMFSample> {
    let buf = MFCreateMemoryBuffer(data.len() as u32)?;
    let mut p = std::ptr::null_mut();
    buf.Lock(&mut p, None, None)?;
    std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
    buf.Unlock()?;
    buf.SetCurrentLength(data.len() as u32)?;
    let s = MFCreateSample()?;
    s.AddBuffer(&buf)?;
    s.SetSampleTime(time)?;
    s.SetSampleDuration(dur)?;
    Ok(s)
}

/// Muestra de audio PCM que empieza en `time` (la duración sale de la cantidad de bytes).
pub unsafe fn pcm_sample(pcm: &[u8], time: i64) -> Result<IMFSample> {
    sample(pcm, time, time_of((pcm.len() / FRAME_BYTES) as i64))
}

/// Copia de los bytes de una muestra.
pub unsafe fn sample_bytes(s: &IMFSample) -> Result<Vec<u8>> {
    let b = s.ConvertToContiguousBuffer()?;
    let mut p = std::ptr::null_mut();
    let mut len = 0u32;
    b.Lock(&mut p, None, Some(&mut len))?;
    let v = std::slice::from_raw_parts(p, len as usize).to_vec();
    b.Unlock()?;
    Ok(v)
}
