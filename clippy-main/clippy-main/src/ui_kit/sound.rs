//! Sonidos cortos generados por código (no dependen del esquema de sonidos de Windows).

use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

/// Clip guardado: dos notas ascendentes agudas (Do6 -> Sol6), con su duración en segundos.
const SAVED_NOTES: [(f32, f32); 2] = [(1046.5, 0.075), (1568.0, 0.130)];
/// Duración del sonido de guardado (100 ns), para silenciarlo en el audio grabado.
pub const SAVED_LEN: i64 = ((SAVED_NOTES[0].1 + SAVED_NOTES[1].1) * 1e7) as i64;

static SAVED: OnceLock<Vec<u8>> = OnceLock::new();
static PAUSED: OnceLock<Vec<u8>> = OnceLock::new();
static RESUMED: OnceLock<Vec<u8>> = OnceLock::new();

fn play(w: &'static [u8]) {
    unsafe {
        let _ = PlaySoundW(PCWSTR(w.as_ptr() as *const u16), HMODULE::default(), SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

pub fn play_saved() {
    play(SAVED.get_or_init(|| make(&SAVED_NOTES)));
}

/// Grabación en pausa: dos notas descendentes graves (Sol5 -> Do5).
pub fn play_paused() {
    play(PAUSED.get_or_init(|| make(&[(784.0, 0.08), (523.25, 0.16)])));
}

/// Grabación reanudada: dos notas ascendentes graves (Do5 -> Sol5).
pub fn play_resumed() {
    play(RESUMED.get_or_init(|| make(&[(523.25, 0.08), (784.0, 0.16)])));
}

fn make(notes: &[(f32, f32)]) -> Vec<u8> {
    const RATE: u32 = 44_100;
    let mut pcm: Vec<i16> = Vec::new();
    for &(freq, dur) in notes {
        let n = (RATE as f32 * dur) as usize;
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            let attack = (t / 0.004).min(1.0);
            let decay = (-t * 22.0).exp();
            let s = (2.0 * std::f32::consts::PI * freq * t).sin() * 0.85
                + (2.0 * std::f32::consts::PI * freq * 2.0 * t).sin() * 0.15;
            pcm.push((s * attack * decay * 0.30 * i16::MAX as f32) as i16);
        }
    }
    let data_len = (pcm.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // mono
    v.extend_from_slice(&RATE.to_le_bytes());
    v.extend_from_slice(&(RATE * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}
