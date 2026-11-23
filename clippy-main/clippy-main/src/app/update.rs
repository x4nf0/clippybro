//! Actualización automática, lo más liviana posible:
//! - Una consulta a GitHub como mucho por día, aunque Clippy se abra y se cierre muchas veces
//!   (la fecha queda en update.json). Si falla, se reintenta a la hora, no en cada arranque.
//! - La grabadora la revisa al arrancar y cada hora (puede quedar abierta semanas).
//! - Si hay versión nueva se avisa (globo de la bandeja y aviso en la ventana principal).
//! - Al tocar "Actualizar" se descarga el .exe nuevo y se reemplaza el actual solo.
//! - Después de actualizar, la ventana principal muestra una vez el texto del release (novedades).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::*;

use super::mode::{tray_running, Mode, Window};
use crate::i18n::{tr, trf};
use crate::log::{app_dir, log};

/// Repositorio público donde se publican las versiones (Releases).
pub const REPO: &str = "santiremedi/clippy";
const HOUR: u64 = 60 * 60;
const DAY: u64 = 24 * HOUR;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct State {
    /// Última consulta que salió bien (segundos desde 1970).
    pub last_check: u64,
    /// Último intento, haya salido bien o no.
    #[serde(default)]
    pub last_try: u64,
    pub latest: Option<String>,
    pub url: Option<String>,
    /// Qué trae `latest`: el texto del release en GitHub.
    #[serde(default)]
    pub notes: Option<String>,
    /// Última versión que arrancó la grabadora: si cambia, Clippy se acaba de actualizar.
    #[serde(default)]
    pub ran: Option<String>,
    /// Versión recién instalada cuyas novedades todavía no se mostraron.
    #[serde(default)]
    pub news: Option<String>,
}

fn state_file() -> PathBuf {
    app_dir().join("update.json")
}

pub fn load() -> State {
    std::fs::read_to_string(state_file()).ok().and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok()).unwrap_or_default()
}

fn save(s: &State) {
    let _ = std::fs::create_dir_all(app_dir());
    let _ = std::fs::write(state_file(), serde_json::to_string_pretty(s).unwrap_or_default());
}

pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn parse(v: &str) -> (u32, u32, u32) {
    let mut it = v.trim_start_matches('v').split('.').map(|x| x.trim().parse::<u32>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// Versión nueva disponible (ya consultada), si hay.
pub fn available() -> Option<(String, String)> {
    let s = load();
    let (v, url) = (s.latest?, s.url?);
    (parse(&v) > parse(current())).then_some((v, url))
}

/// Qué trae la versión `v`, si ya se consultó y el release tiene texto.
pub fn notes(v: &str) -> Option<String> {
    let s = load();
    if s.latest.as_deref() != Some(v) {
        return None;
    }
    s.notes.filter(|n| !n.trim().is_empty())
}

/// Novedades de la versión recién instalada que todavía no se mostraron: (versión, texto).
pub fn news() -> Option<(String, String)> {
    let v = load().news?;
    let text = notes(&v)?;
    Some((v, text))
}

/// Ya se mostraron las novedades: no vuelven a aparecer.
pub fn news_seen() {
    let mut s = load();
    s.news = None;
    save(&s);
}

/// Al arrancar la grabadora: si la versión subió desde la última vez, sus novedades quedan pendientes
/// hasta que se abra la ventana principal. En una instalación nueva no hay nada que mostrar (tampoco
/// hay consultas guardadas; las versiones anteriores a esta no anotaban `ran`).
/// Si falta el texto de esta versión (se instaló a mano, o la anterior no lo guardaba) se consulta ya;
/// si el release no tiene texto, no hay novedades.
fn prepare_news() {
    let mut s = load();
    let cur = current();
    if s.ran.as_deref() != Some(cur) {
        let updated = match &s.ran {
            Some(v) => parse(v) < parse(cur),
            None => s.last_check != 0,
        };
        if updated {
            s.news = Some(cur.to_string());
        }
        s.ran = Some(cur.to_string());
        save(&s);
    }
    if s.news.is_none() || notes(cur).is_some() {
        return;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    match fetch_and_save(&mut s, now) {
        Ok(()) if notes(cur).is_none() => news_seen(),
        Ok(()) => {}
        Err(e) => log(&format!("novedades: {e}")),
    }
}

/// Consulta GitHub si pasó un día desde la última consulta buena y una hora desde el último intento.
fn check_if_due() {
    let mut s = load();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if now.saturating_sub(s.last_check) < DAY || now.saturating_sub(s.last_try) < HOUR {
        return;
    }
    if let Err(e) = fetch_and_save(&mut s, now) {
        log(&format!("actualizaciones: {e}"));
    }
}

/// Botón "Buscar actualizaciones": una consulta ya, sin esperar el día. Devuelve la versión nueva si hay.
pub fn check_now() -> Result<Option<String>> {
    let mut s = load();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    fetch_and_save(&mut s, now)?;
    Ok(available().map(|(v, _)| v))
}

/// Consulta GitHub y guarda el resultado (el intento cuenta aunque falle).
fn fetch_and_save(s: &mut State, now: u64) -> Result<()> {
    s.last_try = now;
    let r = fetch_latest().map(|(tag, url, notes)| {
        s.latest = Some(tag.trim_start_matches('v').to_string());
        s.url = Some(url);
        s.notes = notes;
        s.last_check = now;
    });
    save(s);
    r
}

/// En segundo plano, mientras corra la grabadora: revisa cada hora si toca consultar y llama a
/// `notify` con cada versión nueva (también al arrancar, si ya se sabía de una). Antes deja listas las
/// novedades si Clippy se acaba de actualizar.
pub fn watch(notify: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        prepare_news();
        let mut told: Option<String> = None;
        loop {
            check_if_due();
            if let Some((v, _)) = available() {
                if told.as_ref() != Some(&v) {
                    log(&format!("hay una versión nueva: {v}"));
                    notify();
                    told = Some(v);
                }
            }
            std::thread::sleep(Duration::from_secs(HOUR));
        }
    });
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    /// Texto del release (Markdown); GitHub manda null si está vacío.
    #[serde(default)]
    body: Option<String>,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Última versión publicada: (tag, enlace del .exe, texto del release).
fn fetch_latest() -> Result<(String, String, Option<String>)> {
    let body = http_get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))?;
    let r: Release = serde_json::from_slice(&body)?;
    let a = r
        .assets
        .into_iter()
        .find(|a| a.name.eq_ignore_ascii_case("Clippy.exe"))
        .ok_or_else(|| anyhow!(tr!("la versión no tiene Clippy.exe", "the release has no Clippy.exe", "a versão não tem Clippy.exe")))?;
    Ok((r.tag_name, a.browser_download_url, r.body))
}

/// Donde queda la versión descargada hasta instalarla.
fn downloaded_path() -> PathBuf {
    std::env::temp_dir().join("Clippy-update.exe")
}

/// La versión anterior, renombrada mientras se instala la nueva.
fn old_path(exe: &Path) -> PathBuf {
    exe.with_extension("old.exe")
}

/// Descarga la versión nueva y la deja lista para instalar. Devuelve la ruta del .exe descargado.
pub fn download(url: &str) -> Result<PathBuf> {
    let data = http_get(url)?;
    if data.len() < 1_000_000 || &data[..2] != b"MZ" {
        bail!(tr!("la descarga no es un programa válido", "the download isn't a valid program", "o download não é um programa válido"));
    }
    let p = downloaded_path();
    std::fs::write(&p, data)?;
    Ok(p)
}

/// Lanza el .exe descargado para que reemplace al actual (este proceso tiene que cerrarse después).
pub fn start_install(downloaded: &Path) -> Result<()> {
    let target = std::env::current_exe()?;
    Mode::ApplyUpdate(target).spawn(downloaded)?;
    Ok(())
}

/// Corre desde el .exe descargado: espera a que Clippy se cierre, lo reemplaza y lo vuelve a abrir.
pub fn apply(target: &Path) -> i32 {
    // Esperar a que se cierren la grabadora y la ventana principal: si la ventana sigue cerrándose
    // cuando arranca la versión nueva, esta cree que ya hay una abierta y no se abre.
    for _ in 0..100 {
        if !tray_running() && !Window::Settings.running() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let Ok(me) = std::env::current_exe() else {
        log("actualizar: no se encontró el programa descargado");
        return 1;
    };
    let old = old_path(target);
    let _ = std::fs::remove_file(&old);
    // Windows deja renombrar un .exe en uso, así que esto funciona aunque algo siga abierto.
    if let Err(e) = std::fs::rename(target, &old) {
        log(&format!("actualizar: no se pudo mover la versión anterior: {e}"));
        return 1;
    }
    if let Err(e) = std::fs::copy(&me, target) {
        log(&format!("actualizar: no se pudo copiar la versión nueva: {e}"));
        let _ = std::fs::rename(&old, target);
        return 1;
    }
    log(&format!("Clippy actualizado a {}", current()));
    let _ = Mode::Tray { background: false }.spawn(target);
    0
}

/// Al arrancar: borrar restos de una actualización anterior.
/// Justo después de actualizar, la versión vieja y la descarga todavía pueden estar abiertas
/// (la ventana vieja cerrándose, el instalador terminando): se reintenta un rato en segundo plano.
pub fn cleanup() {
    let mut files = vec![downloaded_path()];
    if let Ok(me) = std::env::current_exe() {
        files.push(old_path(&me));
    }
    std::thread::spawn(move || {
        for _ in 0..30 {
            files.retain(|f| f.exists() && std::fs::remove_file(f).is_err());
            if files.is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

// --- HTTPS con WinHTTP (viene con Windows: no suma librerías) ------------------------

struct Handle(*mut core::ffi::c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn http_get(url: &str) -> Result<Vec<u8>> {
    let rest = url.strip_prefix("https://").ok_or_else(|| anyhow!("solo https"))?;
    let (host, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, "/".into()));
    unsafe {
        let session = Handle(WinHttpOpen(w!("Clippy-Updater"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            bail!(tr!("no se pudo iniciar WinHTTP", "couldn't start WinHTTP", "não foi possível iniciar o WinHTTP"));
        }
        let conn = Handle(WinHttpConnect(session.0, &HSTRING::from(host), INTERNET_DEFAULT_HTTPS_PORT, 0));
        if conn.0.is_null() {
            bail!(trf!("no se pudo conectar a {host}", "couldn't connect to {host}", "não foi possível conectar a {host}"));
        }
        let req = Handle(WinHttpOpenRequest(
            conn.0,
            w!("GET"),
            &HSTRING::from(path),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if req.0.is_null() {
            bail!(tr!("no se pudo crear la consulta", "couldn't create the request", "não foi possível criar a consulta"));
        }
        let headers: Vec<u16> = "Accept: application/vnd.github+json\r\n".encode_utf16().collect();
        WinHttpSendRequest(req.0, Some(&headers), None, 0, 0, 0)?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut())?;
        let mut status = 0u32;
        let mut len = 4u32;
        let mut idx = 0u32;
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            &mut idx,
        )?;
        match status {
            200 => {}
            403 | 429 => bail!(tr!(
                "GitHub está limitando las consultas desde tu conexión. Probá de nuevo en un rato",
                "GitHub is rate-limiting requests from your connection. Try again in a while",
                "O GitHub está limitando as consultas da sua conexão. Tente de novo daqui a pouco"
            )),
            _ => bail!(trf!("el servidor respondió {status}", "the server answered {status}", "o servidor respondeu {status}")),
        }
        let mut body = Vec::new();
        loop {
            let mut avail = 0u32;
            WinHttpQueryDataAvailable(req.0, &mut avail)?;
            if avail == 0 {
                break;
            }
            let start = body.len();
            body.resize(start + avail as usize, 0);
            let mut read = 0u32;
            WinHttpReadData(req.0, body[start..].as_mut_ptr() as *mut _, avail, &mut read)?;
            body.truncate(start + read as usize);
        }
        Ok(body)
    }
}
