//! Configuración persistente (%APPDATA%\Clippy\config.json).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::i18n::{self, tr, Language};
use crate::log::app_dir;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::UI::Shell::{FOLDERID_Videos, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

/// Qué imagen se graba. En config.json se guarda como "pantalla" / "juegos".
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[serde(from = "String", into = "&'static str")]
pub enum CaptureMode {
    /// Todo el monitor del juego (sin juego, el de la ventana activa).
    #[default]
    Screen,
    /// Solo el juego en pantalla completa mientras se ve (si no, negro).
    GamesOnly,
}

impl From<String> for CaptureMode {
    fn from(s: String) -> Self {
        if s == "juegos" {
            CaptureMode::GamesOnly
        } else {
            CaptureMode::Screen
        }
    }
}

impl From<CaptureMode> for &'static str {
    fn from(m: CaptureMode) -> Self {
        match m {
            CaptureMode::Screen => "pantalla",
            CaptureMode::GamesOnly => "juegos",
        }
    }
}

/// Duración del clip que se puede elegir. El máximo real de cada PC sale de su RAM (`max_seconds`).
pub const MIN_SECONDS: u32 = 5;
pub const MAX_SECONDS: u32 = 600;
/// Parte de la RAM total que puede ocupar el buffer en el peor caso (1/4).
const RAM_SHARE: u64 = 4;
/// Audio en crudo de cada canal en el buffer (bytes por segundo).
const AUDIO_BYTES_PER_SEC: u64 = 192_000;
/// Calidad de video (Mbps) que se puede elegir a mano en "Personalizada".
pub const MIN_MBPS: u32 = 5;
pub const MAX_MBPS: u32 = 150;
/// Resoluciones del clip que se pueden elegir (lado corto en px; 0 = como el monitor).
pub const RESOLUTIONS: [u32; 4] = [0, 1440, 1080, 720];

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub output_dir: PathBuf,
    pub seconds: u32,
    pub hotkey_vk: u32,
    /// MOD_ALT = 1, MOD_CONTROL = 2, MOD_SHIFT = 4
    pub hotkey_mods: u32,
    pub fps: u32,
    pub bitrate_mbps: u32,
    /// Lado corto del video en px (720, 1080, 1440); 0 = como el monitor. Nunca agranda la imagen.
    pub resolution: u32,
    /// Audio del juego (el proceso en pantalla completa que se detecta solo, ver `game`).
    pub game_audio: bool,
    pub game_volume: u32,
    /// Audio del escritorio (todo lo demás: Discord, música, etc.).
    pub desktop_audio: bool,
    pub desktop_volume: u32,
    /// Micrófono (apagado por defecto). `mic_device` vacío = el predeterminado de Windows.
    pub mic_audio: bool,
    pub mic_volume: u32,
    pub mic_device: String,
    pub capture_mode: CaptureMode,
    /// Dibujar el cursor del mouse en el video (apagado por defecto; ver `cursor_on`).
    pub cursor: bool,
    pub sound_on_save: bool,
    /// Atajo global para pausar/reanudar la grabación (vk 0 = sin asignar).
    pub toggle_vk: u32,
    pub toggle_mods: u32,
    /// Atajo global para abrir el editor (vk 0 = sin asignar).
    pub editor_vk: u32,
    pub editor_mods: u32,
    /// Atajo global para abrir la carpeta de clips (vk 0 = sin asignar).
    pub folder_vk: u32,
    pub folder_mods: u32,
    /// Idioma de la interfaz ("auto" = el de Windows).
    pub language: Language,
}

/// Atajos globales: funcionan en cualquier momento, incluso dentro del juego.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GlobalAction {
    SaveClip,
    ToggleRecording,
    OpenEditor,
    OpenFolder,
}

impl GlobalAction {
    pub const ALL: [GlobalAction; 4] =
        [GlobalAction::SaveClip, GlobalAction::ToggleRecording, GlobalAction::OpenEditor, GlobalAction::OpenFolder];

    /// Id para RegisterHotKey / WM_HOTKEY.
    pub fn id(self) -> i32 {
        self as i32 + 1
    }

    pub fn from_id(id: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            GlobalAction::SaveClip => tr!("Guardar clip", "Save clip", "Salvar clipe"),
            GlobalAction::ToggleRecording => tr!("Pausar / reanudar grabación", "Pause / resume recording", "Pausar / retomar gravação"),
            GlobalAction::OpenEditor => tr!("Abrir el editor", "Open the editor", "Abrir o editor"),
            GlobalAction::OpenFolder => tr!("Abrir la carpeta de clips", "Open the clips folder", "Abrir a pasta de clipes"),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            output_dir: default_output_dir(),
            seconds: 30,
            hotkey_vk: 0x78, // F9
            hotkey_mods: 0,
            fps: 60,
            bitrate_mbps: 30, // "Media"
            resolution: 0,
            game_audio: true,
            game_volume: 100,
            desktop_audio: true,
            desktop_volume: 100,
            mic_audio: false,
            mic_volume: 100,
            mic_device: String::new(),
            capture_mode: CaptureMode::Screen,
            cursor: false,
            sound_on_save: true,
            toggle_vk: 0,
            toggle_mods: 0,
            editor_vk: 0,
            editor_mods: 0,
            folder_vk: 0,
            folder_mods: 0,
            language: Language::Auto,
        }
    }
}

/// `full` achicado sin deformar (en pares) para que el lado corto no pase de `short`.
pub fn fit_short(full: (u32, u32), short: u32) -> (u32, u32) {
    let s = full.0.min(full.1);
    if short >= s {
        return full;
    }
    let k = |x: u32| ((x as u64 * short as u64 / s as u64) as u32).max(2) & !1;
    (k(full.0), k(full.1))
}

impl Config {
    /// Valores que obligan a reiniciar la captura de video si cambian (se pierde el buffer).
    pub fn video_params(&self) -> (bool, u32, u32, u32) {
        (self.enabled, self.fps, self.bitrate_mbps, self.resolution)
    }

    /// Tamaño del video (pares) para un monitor que se grabaría a `full`, con la resolución elegida.
    pub fn video_size(&self, full: (u32, u32)) -> (u32, u32) {
        match self.resolution {
            0 => full,
            r => fit_short(full, r),
        }
    }

    /// El cursor va solo con "Toda la pantalla" ("Solo juegos" graba el juego); con un juego detectado, tampoco
    /// (lo decide la grabadora).
    pub fn cursor_on(&self) -> bool {
        self.cursor && self.capture_mode == CaptureMode::Screen
    }

    pub fn audio_on(&self) -> bool {
        self.game_audio || self.desktop_audio || self.mic_audio
    }

    pub fn sanitize(&mut self) {
        if !matches!(self.fps, 30 | 60 | 120) {
            self.fps = 60;
        }
        self.bitrate_mbps = self.bitrate_mbps.clamp(MIN_MBPS, MAX_MBPS);
        self.seconds = self.seconds.clamp(MIN_SECONDS, self.max_seconds());
        if !RESOLUTIONS.contains(&self.resolution) {
            self.resolution = 0;
        }
        self.game_volume = self.game_volume.min(100);
        self.desktop_volume = self.desktop_volume.min(100);
        self.mic_volume = self.mic_volume.min(200);
        if self.output_dir.as_os_str().is_empty() {
            self.output_dir = default_output_dir();
        }
        if self.hotkey_vk == 0 || self.hotkey_vk > 0xFE {
            self.hotkey_vk = 0x78;
        }
        self.hotkey_mods &= 0x7;
        self.toggle_mods &= 0x7;
        for a in GlobalAction::ALL {
            if a == GlobalAction::SaveClip {
                continue;
            }
            let (vk, mods) = self.hotkey_mut(a);
            *mods &= 0x7;
            if *vk > 0xFE {
                *vk = 0;
            }
        }
    }

    /// Tecla (vk, mods) de un atajo global; vk 0 = sin asignar.
    pub fn hotkey(&self, a: GlobalAction) -> (u32, u32) {
        match a {
            GlobalAction::SaveClip => (self.hotkey_vk, self.hotkey_mods),
            GlobalAction::ToggleRecording => (self.toggle_vk, self.toggle_mods),
            GlobalAction::OpenEditor => (self.editor_vk, self.editor_mods),
            GlobalAction::OpenFolder => (self.folder_vk, self.folder_mods),
        }
    }

    /// RAM aproximada mientras graba (MB), como la muestra el Administrador de tareas: la app en sí
    /// (unos 40 MB), el audio en crudo y el video. El encoder casi nunca llega al bitrate elegido
    /// (solo con mucho movimiento), así que se cuenta una décima parte: da un número realista, no el máximo.
    pub fn typical_ram_mb(&self) -> u32 {
        let channels = self.game_audio as u32 + self.desktop_audio as u32 + self.mic_audio as u32;
        let audio = channels * self.seconds * (AUDIO_BYTES_PER_SEC / 1000) as u32 / 1000;
        let video = self.bitrate_mbps * self.seconds / 80;
        (40 + audio + video).div_ceil(10) * 10
    }

    /// Duración máxima del clip en esta PC con la calidad elegida (múltiplo de 5 s): aunque el video
    /// llegue al bitrate completo (todo en movimiento), el buffer no pasa de un cuarto de la RAM.
    pub fn max_seconds(&self) -> u32 {
        // Se cuentan los tres canales de audio: así prender el micrófono no recorta la duración.
        let per_sec = self.bitrate_mbps as u64 * 125_000 + 3 * AUDIO_BYTES_PER_SEC;
        let secs = (total_ram() / RAM_SHARE / per_sec).min(MAX_SECONDS as u64) as u32;
        (secs / 5 * 5).max(MIN_SECONDS)
    }

    pub fn hotkey_mut(&mut self, a: GlobalAction) -> (&mut u32, &mut u32) {
        match a {
            GlobalAction::SaveClip => (&mut self.hotkey_vk, &mut self.hotkey_mods),
            GlobalAction::ToggleRecording => (&mut self.toggle_vk, &mut self.toggle_mods),
            GlobalAction::OpenEditor => (&mut self.editor_vk, &mut self.editor_mods),
            GlobalAction::OpenFolder => (&mut self.folder_vk, &mut self.folder_mods),
        }
    }
}

/// RAM física total (bytes). Si Windows no contesta, se asume una PC de 8 GB.
fn total_ram() -> u64 {
    static RAM: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *RAM.get_or_init(|| {
        let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        match unsafe { GlobalMemoryStatusEx(&mut m) } {
            Ok(()) if m.ullTotalPhys > 0 => m.ullTotalPhys,
            _ => 8 << 30,
        }
    })
}

pub fn config_path() -> PathBuf {
    app_dir().join("config.json")
}

pub fn exists() -> bool {
    config_path().exists()
}

/// Lee config.json y aplica su idioma a este proceso (así cada ventana y la bandeja lo siguen al recargar).
pub fn load() -> Config {
    let mut cfg: Config = std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_default();
    cfg.sanitize();
    i18n::set(cfg.language);
    cfg
}

pub fn save(cfg: &Config) -> std::io::Result<()> {
    std::fs::create_dir_all(app_dir())?;
    let tmp = app_dir().join("config.json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(cfg).unwrap_or_default())?;
    std::fs::rename(&tmp, config_path())
}

pub fn config_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(config_path()).and_then(|m| m.modified()).ok()
}

fn default_output_dir() -> PathBuf {
    unsafe {
        if let Ok(p) = SHGetKnownFolderPath(&FOLDERID_Videos, KF_FLAG_DEFAULT, HANDLE::default()) {
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const _));
            if let Some(s) = s {
                return PathBuf::from(s).join("Clippy");
            }
        }
    }
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("Videos")
        .join("Clippy")
}
