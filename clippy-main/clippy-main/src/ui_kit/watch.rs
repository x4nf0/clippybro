//! Despierta una ventana cuando algo cambia afuera, sin repintar en reposo: archivos que escribe otro
//! proceso de Clippy (config.json, el registro de la grabadora), clips nuevos en la carpeta (o en la de un
//! juego) y la grabadora que se abre o se cierra. Un hilo espera esos avisos de Windows; la ventana pregunta
//! `take_changed` en cada cuadro y solo relee lo de afuera cuando hace falta.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{
    FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
};
use windows::Win32::System::Threading::{CreateEventW, OpenProcess, SetEvent, WaitForMultipleObjects, INFINITE, PROCESS_SYNCHRONIZE};

use crate::app::mode::tray_pid;
use crate::log::app_dir;

/// Mientras falte algo por mirar (una carpeta que todavía no existe, la grabadora cerrada),
/// el hilo vuelve a buscarlo cada tanto. No repinta: solo avisa si apareció.
const RETRY_MS: u32 = 2000;

struct Shared {
    /// Carpeta de los clips (con sus subcarpetas). La de la app se mira sin subcarpetas: la caché cambia seguido.
    clips: Mutex<PathBuf>,
    changed: AtomicBool,
    /// Evento para que el hilo vuelva a armar lo que mira (cambió la carpeta de los clips).
    wake: isize,
}

pub struct Watcher(Arc<Shared>);

impl Watcher {
    /// `tray`: avisar también cuando la grabadora se abre o se cierra.
    pub fn new(ctx: egui::Context, clips: PathBuf, tray: bool) -> Watcher {
        let wake = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.map_or(0, |h| h.0 as isize);
        let shared = Arc::new(Shared { clips: Mutex::new(clips), changed: AtomicBool::new(true), wake });
        if wake != 0 {
            let s = shared.clone();
            std::thread::spawn(move || run(&s, &ctx, tray));
        }
        Watcher(shared)
    }

    /// Cambia la carpeta de los clips que se mira (si se eligió otra).
    pub fn set_clips(&self, clips: &Path) {
        let mut d = self.0.clips.lock().unwrap();
        if *d != clips {
            *d = clips.to_path_buf();
            unsafe {
                let _ = SetEvent(HANDLE(self.0.wake as *mut _));
            }
        }
    }

    /// ¿Cambió algo desde la última vez que se preguntó? (la primera vez, sí).
    pub fn take_changed(&self) -> bool {
        self.0.changed.swap(false, Ordering::Relaxed)
    }
}

fn run(s: &Shared, ctx: &egui::Context, watch_tray: bool) {
    let wake = HANDLE(s.wake as *mut _);
    let changed = || {
        s.changed.store(true, Ordering::Relaxed);
        ctx.request_repaint();
    };
    loop {
        let dirs = [(app_dir(), false), (s.clips.lock().unwrap().clone(), true)];
        let filter = FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE;
        let notes: Vec<HANDLE> = dirs
            .iter()
            .filter_map(|(d, tree)| unsafe { FindFirstChangeNotificationW(&HSTRING::from(d.as_os_str()), *tree, filter).ok() })
            .collect();
        let tray = if watch_tray { tray_pid().and_then(|pid| unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid).ok() }) } else { None };
        let mut handles = vec![wake];
        handles.extend(&notes);
        handles.extend(tray);
        let missing = notes.len() < dirs.len() || (watch_tray && tray.is_none());
        loop {
            let r = unsafe { WaitForMultipleObjects(&handles, false, if missing { RETRY_MS } else { INFINITE }) };
            if r == WAIT_TIMEOUT {
                // ¿Apareció lo que faltaba? Se vuelve a armar; si es la grabadora, la ventana se entera.
                if watch_tray && tray.is_none() && tray_pid().is_some() {
                    changed();
                }
                break;
            }
            let i = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
            if i == 0 {
                break; // otras carpetas
            }
            if i > handles.len() - 1 {
                std::thread::sleep(Duration::from_millis(RETRY_MS as u64)); // falló la espera
                break;
            }
            changed();
            if i > notes.len() {
                break; // se cerró la grabadora
            }
            unsafe {
                let _ = FindNextChangeNotification(notes[i - 1]);
            }
        }
        unsafe {
            for h in notes {
                let _ = FindCloseChangeNotification(h);
            }
            if let Some(t) = tray {
                let _ = CloseHandle(t);
            }
        }
    }
}
