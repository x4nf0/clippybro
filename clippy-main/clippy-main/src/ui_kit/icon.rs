//! Íconos dibujados por código (sin archivos de recursos), monocromáticos.

use windows::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

#[path = "icon_draw.rs"]
mod draw;

pub use draw::app_rgba;

/// Ícono de la bandeja (estado grabando/pausado).
pub fn hicon(size: u32, on: bool) -> HICON {
    let px = draw::status_rgba(size, on);
    // BGRA premultiplicado para GDI.
    let mut bgra = vec![0u8; px.len()];
    for i in (0..px.len()).step_by(4) {
        let a = px[i + 3] as u32;
        bgra[i] = (px[i + 2] as u32 * a / 255) as u8;
        bgra[i + 1] = (px[i + 1] as u32 * a / 255) as u8;
        bgra[i + 2] = (px[i] as u32 * a / 255) as u8;
        bgra[i + 3] = a as u8;
    }
    let mask = vec![0u8; size.div_ceil(16) as usize * 2 * size as usize];
    unsafe {
        let color = CreateBitmap(size as i32, size as i32, 1, 32, Some(bgra.as_ptr() as *const _));
        let mask_bmp = CreateBitmap(size as i32, size as i32, 1, 1, Some(mask.as_ptr() as *const _));
        let ii = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask_bmp, hbmColor: color };
        let icon = CreateIconIndirect(&ii).unwrap_or_default();
        let _ = DeleteObject(color);
        let _ = DeleteObject(mask_bmp);
        icon
    }
}

/// Ícono de las ventanas (egui).
pub fn window_icon() -> eframe::egui::IconData {
    eframe::egui::IconData { rgba: app_rgba(64), width: 64, height: 64 }
}
