//! Inicio con Windows: el valor "Clippy" en HKCU\...\Run, que abre la grabadora sin ventanas.

use std::path::Path;

use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::{RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ};

use super::mode::Mode;

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const NAME: PCWSTR = w!("Clippy");

/// Línea de comandos registrada, si hay.
fn registered() -> Option<String> {
    unsafe {
        let mut len = 0u32;
        if !RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, NAME, RRF_RT_REG_SZ, None, None, Some(&mut len)).is_ok() {
            return None;
        }
        let mut buf = vec![0u16; (len as usize).div_ceil(2)];
        if !RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, NAME, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut len)).is_ok() {
            return None;
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..n]))
    }
}

pub fn enabled() -> bool {
    registered().is_some()
}

pub fn set(on: bool) {
    unsafe {
        if on {
            let Ok(exe) = std::env::current_exe() else { return };
            // Al iniciar Windows arranca en la bandeja sin abrir ventanas.
            let v: Vec<u16> = Mode::Tray { background: true }.command_line(&exe).encode_utf16().chain(std::iter::once(0)).collect();
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, NAME, REG_SZ.0, Some(v.as_ptr() as *const _), (v.len() * 2) as u32);
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, NAME);
        }
    }
}

/// Si el inicio con Windows apunta a un .exe que ya no existe (Clippy se movió de carpeta),
/// lo apunta a este. Si apunta a otro .exe que sí existe, no lo toca.
pub fn repair() {
    let Some(cmd) = registered() else { return };
    let exe = cmd.trim_start().strip_prefix('"').and_then(|s| s.split('"').next()).unwrap_or(&cmd);
    if !Path::new(exe).exists() {
        set(true);
    }
}
