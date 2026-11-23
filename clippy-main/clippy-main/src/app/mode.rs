//! Los modos en que corre el mismo .exe y cómo se encuentran entre sí los procesos:
//! argumentos, candados de instancia única, títulos de ventana y mensajes a la bandeja.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId, PostMessageW, WM_APP, WM_CLOSE};

use crate::i18n::{self, Lang};
use crate::ui_kit::window::find_own_window;

const APPLY_UPDATE: &str = "--apply-update";
const SETTINGS: &str = "--settings";
const EDITOR: &str = "--editor";
const BACKGROUND: &str = "--background";

pub enum Mode {
    /// Grabadora con ícono en la bandeja. `background`: sin abrir la ventana (inicio con Windows).
    Tray { background: bool },
    /// Ventana principal / ajustes.
    Settings,
    /// Editor de clips, opcionalmente con un archivo ya cargado.
    Editor(Option<PathBuf>),
    /// Corre desde el .exe descargado: reemplaza al instalado en esa ruta.
    ApplyUpdate(PathBuf),
}

impl Mode {
    pub fn parse(args: &[String]) -> Mode {
        let after = |flag: &str| args.iter().position(|a| a == flag).map(|i| args.get(i + 1).map(PathBuf::from));
        if let Some(target) = after(APPLY_UPDATE) {
            Mode::ApplyUpdate(target.unwrap_or_default())
        } else if args.iter().any(|a| a == SETTINGS) {
            Mode::Settings
        } else if let Some(file) = after(EDITOR) {
            Mode::Editor(file)
        } else {
            Mode::Tray { background: args.iter().any(|a| a == BACKGROUND) }
        }
    }

    fn args(&self) -> Vec<OsString> {
        match self {
            Mode::Tray { background: false } => vec![],
            Mode::Tray { background: true } => vec![BACKGROUND.into()],
            Mode::Settings => vec![SETTINGS.into()],
            Mode::Editor(None) => vec![EDITOR.into()],
            Mode::Editor(Some(f)) => vec![EDITOR.into(), f.into()],
            Mode::ApplyUpdate(t) => vec![APPLY_UPDATE.into(), t.into()],
        }
    }

    /// Abre este modo en un proceso nuevo del .exe indicado.
    pub fn spawn(&self, exe: &Path) -> std::io::Result<()> {
        std::process::Command::new(exe).args(self.args()).spawn().map(|_| ())
    }

    /// Abre este modo en un proceso nuevo de este mismo .exe.
    pub fn launch(&self) {
        if let Ok(exe) = std::env::current_exe() {
            let _ = self.spawn(&exe);
        }
    }

    /// Línea de comandos completa (para el inicio automático con Windows).
    pub fn command_line(&self, exe: &Path) -> String {
        let mut s = format!("\"{}\"", exe.display());
        for a in self.args() {
            s.push(' ');
            s.push_str(&a.to_string_lossy());
        }
        s
    }
}

/// Ventanas de Clippy que corren en su propio proceso.
#[derive(Clone, Copy)]
pub enum Window {
    Settings,
    Editor,
}

impl Window {
    /// Título en el idioma actual.
    pub fn title(self) -> &'static str {
        self.title_in(i18n::lang())
    }

    fn title_in(self, lang: Lang) -> &'static str {
        match (self, lang) {
            (Window::Settings, _) => "Clippy",
            (Window::Editor, Lang::Es) => "Editor de Clippy",
            (Window::Editor, Lang::En) => "Clippy Editor",
            (Window::Editor, Lang::Pt) => "Editor do Clippy",
        }
    }

    /// La ventana abierta en otro proceso, aunque se haya abierto con otro idioma.
    pub fn find(self) -> Option<HWND> {
        Lang::ALL.into_iter().find_map(|l| find_own_window(self.title_in(l)))
    }

    /// Candado de instancia única.
    pub fn mutex(self) -> PCWSTR {
        match self {
            Window::Settings => w!("Local\\Clippy.Settings"),
            Window::Editor => w!("Local\\Clippy.Editor"),
        }
    }

    /// ¿Está abierta (o terminando de cerrarse)?
    pub fn running(self) -> bool {
        mutex_exists(self.mutex())
    }

    /// Pide cerrarla (como si el usuario tocara la X).
    pub fn close(self) {
        if let Some(h) = self.find() {
            unsafe {
                let _ = PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }
}

// --- Proceso de la bandeja (grabadora) ------------------------------------------------

/// Candado de instancia única de la grabadora.
pub const TRAY_MUTEX: PCWSTR = w!("Local\\Clippy.Main");
/// Clase de la ventana oculta de la grabadora (recibe los mensajes de los otros procesos).
pub const TRAY_CLASS: PCWSTR = w!("ClippyHostWindow");
// Mensajes que recibe la ventana de la bandeja (todos acá para que no se pisen los números).
/// Clic en el ícono de la bandeja (lo manda Windows).
pub const WM_TRAY: u32 = WM_APP + 1;
/// El proceso de ajustes pide recargar config.json.
pub const WM_RELOAD: u32 = WM_APP + 2;
/// La grabadora dejó eventos en la cola.
pub const WM_REC_EVENT: u32 = WM_APP + 3;
/// wparam 1 = soltar los atajos globales (mientras se elige uno nuevo), 0 = volver a registrarlos.
pub const WM_HOTKEY_SUSPEND: u32 = WM_APP + 5;
/// Hay una versión nueva.
pub const WM_UPDATE: u32 = WM_APP + 6;

/// ¿Está abierta la grabadora? (aunque su ventana todavía no exista).
pub fn tray_running() -> bool {
    mutex_exists(TRAY_MUTEX)
}

/// ¿Algún proceso tiene tomado ese candado de instancia única?
fn mutex_exists(name: PCWSTR) -> bool {
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, name) {
            Ok(h) => {
                let _ = CloseHandle(h);
                true
            }
            Err(_) => false,
        }
    }
}

pub fn find_tray() -> Option<HWND> {
    unsafe { FindWindowW(TRAY_CLASS, PCWSTR::null()).ok().filter(|h| !h.is_invalid()) }
}

/// Proceso de la grabadora, si está abierta (para enterarse cuando se cierra).
pub fn tray_pid() -> Option<u32> {
    let h = find_tray()?;
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    (pid != 0).then_some(pid)
}

pub fn post_tray(msg: u32, wp: usize) -> bool {
    match find_tray() {
        Some(h) => unsafe { PostMessageW(h, msg, WPARAM(wp), LPARAM(0)).is_ok() },
        None => false,
    }
}

/// Cierra la grabadora (deja de grabar y sale).
pub fn close_tray() {
    post_tray(WM_CLOSE, 0);
}
