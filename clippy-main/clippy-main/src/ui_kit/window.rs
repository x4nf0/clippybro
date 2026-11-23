//! Ventanas nativas: instancia única, maximizar sin parpadeo y opciones comunes de eframe.

use eframe::egui;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};

/// Si la ventana ya está abierta (en otro proceso de Clippy, `find` la busca), la trae al frente y devuelve None.
pub fn single_instance(mutex: windows::core::PCWSTR, find: impl FnOnce() -> Option<HWND>) -> Option<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::Win32::UI::WindowsAndMessaging::{IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE};
    unsafe {
        let m = CreateMutexW(None, true, mutex).ok();
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Some(h) = find() {
                if IsIconic(h).as_bool() {
                    let _ = ShowWindow(h, SW_RESTORE);
                }
                let _ = SetForegroundWindow(h);
            }
            return None;
        }
        m
    }
}

/// Deja la ventana maximizada antes de que se muestre por primera vez.
///
/// eframe crea la ventana oculta y la muestra recién cuando el primer cuadro está dibujado.
/// En ese primer cuadro (todavía oculta) se le pone el estilo "maximizada" y el tamaño de la
/// pantalla con la API de Windows: así aparece una sola vez, ya maximizada, sin parpadeo.
/// (Pedirle a winit `with_maximized` la muestra vacía al crearla y después se oculta y reaparece.)
pub fn maximize_once(ctx: &egui::Context, done: &mut bool, title: &str) {
    if *done {
        return;
    }
    *done = true;
    #[cfg(feature = "devtools")]
    if test_window().is_some() {
        return;
    }
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
    use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows::Win32::UI::WindowsAndMessaging::*;
    let Some(h) = find_own_window(title) else {
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        return;
    };
    unsafe {
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let mon = MonitorFromWindow(h, MONITOR_DEFAULTTOPRIMARY);
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            return;
        }
        let wa = mi.rcWork;
        let dpi = GetDpiForWindow(h).max(96);
        // Una ventana maximizada se extiende el grosor del borde por fuera del área de trabajo.
        let f = GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
        let style = GetWindowLongPtrW(h, GWL_STYLE);
        SetWindowLongPtrW(h, GWL_STYLE, style | WS_MAXIMIZE.0 as isize);
        let _ = SetWindowPos(
            h,
            None,
            wa.left - f,
            wa.top - f,
            (wa.right - wa.left) + 2 * f,
            (wa.bottom - wa.top) + 2 * f,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
    ctx.request_repaint();
}

/// Opciones comunes de ventana: título, tamaño mínimo y el ícono de la app.
/// Se maximiza en el primer cuadro con `maximize_once`, antes de que se vea.
pub fn window_options(title: &str, min: [f32; 2]) -> eframe::NativeOptions {
    let viewport = egui::ViewportBuilder::default()
        .with_title(title)
        .with_min_inner_size(min)
        .with_icon(std::sync::Arc::new(super::icon::window_icon()));
    #[cfg(feature = "devtools")]
    let viewport = match test_window() {
        Some(size) => viewport.with_inner_size(size),
        None => viewport,
    };
    eframe::NativeOptions { viewport, ..Default::default() }
}

/// Prueba (CLIPPY_TEST_WINDOW=<ancho>x<alto>): la ventana se abre de ese tamaño y sin maximizar (lo más chico es su
/// tamaño mínimo), para ver cómo queda angosta.
#[cfg(feature = "devtools")]
fn test_window() -> Option<[f32; 2]> {
    let s = std::env::var("CLIPPY_TEST_WINDOW").ok()?;
    let (w, h) = s.split_once('x')?;
    Some([w.parse().ok()?, h.parse().ok()?])
}

/// Ventana de nivel superior con ese título que pertenezca a un proceso clippy.exe
/// (así no se confunde con, por ejemplo, una carpeta abierta que se llame igual).
pub fn find_own_window(title: &str) -> Option<HWND> {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW, GetWindowThreadProcessId};
    struct State {
        title: Vec<u16>,
        found: Option<HWND>,
    }
    unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> BOOL {
        let s = &mut *(lp.0 as *mut State);
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(h, &mut buf).max(0) as usize;
        if n == s.title.len() && buf[..n] == s.title[..] {
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            if process_is_clippy(pid) {
                s.found = Some(h);
                return false.into();
            }
        }
        true.into()
    }
    let mut s = State { title: title.encode_utf16().collect(), found: None };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut s as *mut State as isize));
    }
    s.found
}

fn process_is_clippy(pid: u32) -> bool {
    use windows::core::PWSTR;
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return false;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
        let me = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
            .unwrap_or_else(|| "clippy.exe".into());
        path.ends_with(&me)
    }
}

/// Abre la carpeta en el Explorador (la crea si no existe).
pub fn open_folder(dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(dir);
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
}
