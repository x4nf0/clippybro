//! Ventana principal / ajustes (proceso aparte: `clippy.exe --settings`).
//! Al cerrarla el proceso termina, así que mientras jugás no ocupa nada.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Align, Layout, Margin, RichText};

use crate::app::autostart;
use crate::app::keys::{self, hotkey_name};
use crate::app::mode::{close_tray, find_tray, post_tray, Window, WM_HOTKEY_SUSPEND, WM_RELOAD};
use crate::app::status::{self, Status};
use crate::app::update;
use crate::config::{self, Config, GlobalAction};
use crate::i18n::{self, tr, trf};
use crate::record::audio;
use crate::ui_kit::theme::*;
use crate::ui_kit::watch::Watcher;
use crate::ui_kit::window::{maximize_once, single_instance, window_options};
use pages::page_title;

mod news;
mod pages;

const LABEL_W: f32 = 168.0;
/// Ayuda mientras se espera una tecla nueva (Inicio y Atajos).
fn capture_hint() -> &'static str {
    tr!(
        "Esc cancela. Podés combinarla con Ctrl, Shift o Alt.",
        "Esc cancels. You can combine it with Ctrl, Shift or Alt.",
        "Esc cancela. Você pode combiná-la com Ctrl, Shift ou Alt."
    )
}

pub fn run() {
    let win = Window::Settings;
    let Some(_mutex) = single_instance(win.mutex(), || win.find()) else { return };
    let _ = eframe::run_native(
        win.title(),
        window_options(win.title(), [760.0, 560.0]),
        Box::new(|cc| {
            setup_style(&cc.egui_ctx);
            Ok(Box::new(SettingsApp::new(cc.egui_ctx.clone())))
        }),
    );
}

/// Resultado de un trabajo en segundo plano (None = todavía no terminó).
type Shared<T> = std::sync::Arc<std::sync::Mutex<Option<T>>>;

struct SettingsApp {
    cfg: Config,
    saved: Config,
    prev: Config,
    last_edit: Option<Instant>,
    /// Atajo global que se está eligiendo.
    capturing: Option<GlobalAction>,
    global_note: Option<String>,
    ignore_clicks_until: Instant,
    autostart: bool,
    main_running: bool,
    /// Con qué graba la grabadora y por qué no puede.
    recorder: Status,
    /// Avisa cuando cambia algo afuera (config, clips, la grabadora): no se sondea.
    watcher: Watcher,
    mtime: Option<SystemTime>,
    clips: (usize, Option<SystemTime>),
    /// Carpeta de los clips y las de cada juego, con sus fechas, la última vez que se contaron.
    clips_seen: Vec<(PathBuf, Option<SystemTime>)>,
    mics: Vec<(String, String)>,
    /// Versión nueva disponible y estado de la descarga.
    update: Option<(String, String)>,
    update_status: Shared<Result<(), String>>,
    updating: bool,
    /// "Buscar actualizaciones": la consulta en curso y su resultado (versión nueva, si hay).
    check: Shared<Result<Option<String>, String>>,
    checking: bool,
    /// Lo que dijo la última búsqueda ("Tenés la última versión.", un error). Queda a la vista mientras
    /// se busca de nuevo, así la fila no se achica y vuelve a crecer.
    check_note: Option<String>,
    /// Novedades de la versión recién instalada (versión, texto), hasta que se cierran.
    news: Option<(String, String)>,
    /// Confirmación de "Actualizar": la versión nueva y lo que trae, si el release tiene texto.
    confirm_update: Option<(String, Option<String>)>,
    maximized: bool,
    page: Page,
    confirm_quit: bool,
    /// Se eligió "Personalizada" en la calidad (aunque el valor coincida con uno de los fijos).
    custom_quality: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Home,
    Keys,
    General,
}

impl SettingsApp {
    fn new(ctx: egui::Context) -> Self {
        let cfg = config::load();
        let watcher = Watcher::new(ctx, cfg.output_dir.clone(), true);
        let mut s = Self {
            saved: cfg.clone(),
            prev: cfg.clone(),
            cfg,
            last_edit: None,
            capturing: None,
            global_note: None,
            ignore_clicks_until: Instant::now(),
            autostart: autostart::enabled(),
            main_running: false,
            recorder: Status::default(),
            watcher,
            mtime: config::config_mtime(),
            clips: (0, None),
            clips_seen: Vec::new(),
            mics: audio::capture_devices(),
            update: None,
            update_status: Default::default(),
            updating: false,
            check: Default::default(),
            checking: false,
            check_note: None,
            news: None,
            confirm_update: None,
            maximized: false,
            page: Page::Home,
            confirm_quit: false,
            custom_quality: false,
        };
        s.poll_outside();
        s
    }

    fn apply(&mut self) {
        self.cfg.sanitize();
        if config::save(&self.cfg).is_ok() {
            self.saved = self.cfg.clone();
            self.prev = self.cfg.clone();
            self.mtime = config::config_mtime();
            post_tray(WM_RELOAD, 0);
        }
    }

    /// Descarga la versión nueva en segundo plano y la instala (cierra todo y vuelve a abrir Clippy).
    fn start_update(&mut self) {
        let Some((_, url)) = self.update.clone() else { return };
        if self.update_busy() {
            return;
        }
        self.updating = true;
        let st = self.update_status.clone();
        *st.lock().unwrap() = None;
        std::thread::spawn(move || {
            let r = update::download(&url).and_then(|p| update::start_install(&p)).map_err(|e| e.to_string());
            *st.lock().unwrap() = Some(r);
        });
    }

    /// "Buscar actualizaciones": una consulta en segundo plano.
    fn start_check(&mut self) {
        self.checking = true;
        let st = self.check.clone();
        *st.lock().unwrap() = None;
        std::thread::spawn(move || {
            let r = update::check_now().map_err(|e| e.to_string());
            *st.lock().unwrap() = Some(r);
        });
    }

    /// Mientras se busca, mirar si terminó; si hay versión nueva aparece el botón para actualizar.
    fn finish_check(&mut self, ctx: &egui::Context) {
        if !self.checking {
            return;
        }
        let note = match &*self.check.lock().unwrap() {
            Some(Ok(None)) => Some(tr!("Tenés la última versión.", "You have the latest version.", "Você tem a versão mais recente.").to_string()),
            Some(Ok(Some(_))) => None,
            Some(Err(e)) => Some(trf!("No se pudo buscar: {e}", "Couldn't check: {e}", "Não foi possível verificar: {e}")),
            None => {
                ctx.request_repaint_after(Duration::from_millis(200));
                return;
            }
        };
        self.checking = false;
        self.check_note = note;
        self.update = update::available();
    }

    /// Hay una descarga en curso (todavía sin resultado).
    fn update_busy(&self) -> bool {
        self.updating && self.update_status.lock().unwrap().is_none()
    }

    /// Empieza a esperar la tecla nueva para ese atajo (la bandeja suelta sus atajos mientras tanto).
    fn start_capture(&mut self, a: GlobalAction) {
        if self.capturing.is_some() || Instant::now() <= self.ignore_clicks_until {
            return;
        }
        self.capturing = Some(a);
        self.global_note = None;
        post_tray(WM_HOTKEY_SUSPEND, 1);
    }

    /// Deja de esperar una tecla (la elegiste, apretaste Esc, cambiaste de página o cerraste la
    /// ventana) y la bandeja vuelve a escuchar sus atajos. Si no se estaba esperando, no hace nada.
    fn end_capture(&mut self) {
        if self.capturing.take().is_some() {
            self.ignore_clicks_until = Instant::now() + Duration::from_millis(400);
            post_tray(WM_HOTKEY_SUSPEND, 0);
        }
    }

    /// Mientras se descarga, seguir mirando el estado; si el instalador ya arrancó, cerrar todo
    /// para que pueda reemplazar el programa (en cualquier página, no solo en Inicio).
    fn finish_update(&mut self, ctx: &egui::Context) {
        if !self.updating {
            return;
        }
        let status = self.update_status.lock().unwrap().clone();
        match status {
            Some(Ok(())) => {
                close_tray();
                Window::Editor.close();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            // Falló: el error queda a la vista y se deja de mirar (nada de repintar en reposo).
            Some(Err(_)) => self.updating = false,
            None => ctx.request_repaint_after(Duration::from_millis(200)),
        }
    }

    /// Relee lo que cambia desde afuera (la grabadora, la bandeja, clips nuevos), solo si hubo un aviso.
    fn poll_outside(&mut self) {
        self.watcher.set_clips(&self.cfg.output_dir);
        if !self.watcher.take_changed() {
            return;
        }
        self.main_running = find_tray().is_some();
        self.recorder = status::load();
        // La grabadora escribe update.json cuando encuentra una versión nueva: el botón aparece al toque,
        // aunque esta ventana se haya abierto antes de que terminara la consulta.
        if !self.updating {
            self.update = update::available();
        }
        // Las novedades quedan listas cuando arranca la grabadora recién actualizada (a veces después
        // de abrir esta ventana, si hubo que bajar el texto).
        self.news = update::news();
        let seen = clip_dirs(&self.cfg.output_dir);
        if self.clips_seen != seen {
            self.clips = count_clips(&seen);
            self.clips_seen = seen;
        }
        // Si se cambió algo desde la bandeja (ej. pausar), reflejarlo acá.
        let m = config::config_mtime();
        if m != self.mtime {
            self.mtime = m;
            if self.cfg == self.saved {
                let c = config::load();
                self.cfg = c.clone();
                self.saved = c.clone();
                self.prev = c;
            }
        }
    }

    fn capture_key(&mut self, ctx: &egui::Context) {
        let Some(slot) = self.capturing else { return };
        if let Some((vk, mods)) = keys::pressed_key() {
            if vk != 0x1B {
                // Si otro atajo global usaba la misma tecla, queda libre.
                self.global_note = None;
                for other in GlobalAction::ALL {
                    if other != slot && self.cfg.hotkey(other) == (vk, mods) {
                        let (v, m) = self.cfg.hotkey_mut(other);
                        *v = 0;
                        *m = 0;
                        let (label, key) = (other.label(), hotkey_name(vk, mods));
                        self.global_note = Some(trf!(
                            "«{label}» quedó sin tecla porque usaba {key}.",
                            "“{label}” no longer has a key because it used {key}.",
                            "“{label}” ficou sem tecla porque usava {key}."
                        ));
                    }
                }
                let (v, m) = self.cfg.hotkey_mut(slot);
                *v = vk;
                *m = mods;
            }
            self.end_capture();
        }
        ctx.request_repaint_after(Duration::from_millis(15));
    }
}

impl Drop for SettingsApp {
    fn drop(&mut self) {
        self.end_capture();
        if self.cfg != self.saved {
            self.apply();
        }
    }
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        maximize_once(ctx, &mut self.maximized, Window::Settings.title());
        // El idioma se aplica al toque al elegirlo (y si cambia desde afuera, al recargar config.json).
        i18n::set(self.cfg.language);
        self.poll_outside();
        self.capture_key(ctx);
        self.finish_update(ctx);

        egui::SidePanel::left("nav")
            .exact_width(260.0)
            .resizable(false)
            .frame(egui::Frame::none().fill(CARD).inner_margin(Margin::symmetric(16.0, 18.0)))
            .show(ctx, |ui| self.nav(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG).inner_margin(Margin::symmetric(36.0, 28.0)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    centered(ui, 860.0, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                        match self.page {
                            Page::Home => self.home_page(ui),
                            Page::Keys => {
                                page_title(
                                    ui,
                                    tr!("Atajos", "Shortcuts", "Atalhos"),
                                    tr!(
                                        "Teclas que funcionan en cualquier momento, incluso dentro del juego.",
                                        "Keys that work at any time, even inside the game.",
                                        "Teclas que funcionam a qualquer momento, até dentro do jogo."
                                    ),
                                );
                                self.global_keys_section(ui);
                            }
                            Page::General => {
                                page_title(
                                    ui,
                                    tr!("General", "General", "Geral"),
                                    tr!(
                                        "Idioma, inicio con Windows, sonidos y datos de la app.",
                                        "Language, start with Windows, sounds and app data.",
                                        "Idioma, iniciar com o Windows, sons e dados do app."
                                    ),
                                );
                                self.general_section(ui);
                                self.about_section(ui);
                            }
                        }
                    });
                });
            });

        if self.cfg != self.prev {
            self.prev = self.cfg.clone();
            self.last_edit = Some(Instant::now());
        }
        if let Some(t) = self.last_edit {
            if t.elapsed() >= Duration::from_millis(400) {
                self.last_edit = None;
                self.apply();
            } else {
                ctx.request_repaint_after(Duration::from_millis(420));
            }
        }
        self.news_modals(ctx);
        if self.confirm_quit {
            let mut quit = false;
            let title = tr!("¿Salir de Clippy?", "Quit Clippy?", "Sair do Clippy?");
            let sub = tr!("Deja de grabar y se cierra todo.", "Recording stops and everything closes.", "A gravação para e tudo é fechado.");
            let x = modal(ctx, "quit", title, sub, 480.0, |ui| {
                modal_body(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(tr!(
                                "Se cierran el editor y esta ventana. Lo que todavía no guardaste como clip se pierde.\n\n\
                                 Si solo querés cerrar la ventana, usá la X: Clippy sigue grabando en segundo plano.",
                                "The editor and this window close. Anything you haven't saved as a clip is lost.\n\n\
                                 If you only want to close the window, use the X: Clippy keeps recording in the background.",
                                "O editor e esta janela são fechados. O que você ainda não salvou como clipe se perde.\n\n\
                                 Se quiser só fechar a janela, use o X: o Clippy continua gravando em segundo plano.",
                            ))
                            .color(MUTED),
                        )
                        .wrap(),
                    );
                });
                modal_footer(ui, |ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if button_sized(ui, tr!("Salir", "Quit", "Sair"), Kind::Danger, egui::vec2(120.0, 38.0)).clicked() {
                            quit = true;
                        }
                        if button_sized(ui, tr!("Cancelar", "Cancel", "Cancelar"), Kind::Secondary, egui::vec2(120.0, 38.0)).clicked() {
                            self.confirm_quit = false;
                        }
                    });
                });
            });
            if x {
                self.confirm_quit = false;
            }
            if quit {
                if self.cfg != self.saved {
                    self.apply();
                }
                close_tray();
                Window::Editor.close();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

/// La carpeta de los clips y las de adentro (un nivel, como la biblioteca), con la fecha en que cambiaron.
/// La fecha se pide a cada carpeta: la que trae la lista de la de arriba Windows la actualiza tarde.
fn clip_dirs(root: &Path) -> Vec<(PathBuf, Option<SystemTime>)> {
    let subdirs = std::fs::read_dir(root).into_iter().flatten().flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir()));
    std::iter::once(root.to_path_buf())
        .chain(subdirs.map(|e| e.path()))
        .map(|d| {
            let m = std::fs::metadata(&d).and_then(|m| m.modified()).ok();
            (d, m)
        })
        .collect()
}

fn count_clips(dirs: &[(PathBuf, Option<SystemTime>)]) -> (usize, Option<SystemTime>) {
    let mut n = 0;
    let mut last = None;
    for (dir, _) in dirs {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mp4")) {
                n += 1;
                if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                    if last.is_none_or(|l| m > l) {
                        last = Some(m);
                    }
                }
            }
        }
    }
    (n, last)
}
