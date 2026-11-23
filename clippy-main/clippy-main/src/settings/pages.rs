//! Páginas de la ventana principal: navegación, Inicio (grabación y audio), Atajos y General.

use std::time::SystemTime;

use eframe::egui::{self, Align, Layout, Margin, RichText};

use crate::app::autostart;
use crate::app::keys::hotkey_name;
use crate::app::mode::Mode;
use crate::app::update;
use crate::config::{CaptureMode, GlobalAction, MAX_MBPS, MAX_SECONDS, MIN_MBPS, MIN_SECONDS, RESOLUTIONS};
use crate::i18n::{tr, trf, Lang, Language};

/// Calidades de video de un toque (Mbps): baja, media y alta.
const QUALITY: [u32; 3] = [15, 30, 60];
use crate::log::app_dir;
use crate::record::{audio, Device, CPU_MAX_FPS, CPU_MAX_SHORT};
use crate::ui_kit::sound;
use crate::ui_kit::theme::*;
use crate::ui_kit::window::open_folder;

use super::{capture_hint, Page, SettingsApp, LABEL_W};

pub(super) fn page_title(ui: &mut egui::Ui, title: &str, sub: &str) {
    ui.label(RichText::new(title).family(bold()).size(26.0).color(TEXT));
    ui.label(RichText::new(sub).color(MUTED));
    ui.add_space(14.0);
}

impl SettingsApp {
    /// Barra lateral: logo, estado de la grabación, secciones y acceso al editor.
    pub(super) fn nav(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            app_mark(ui, 28.0);
            ui.label(RichText::new("Clippy").family(bold()).size(20.0).color(TEXT));
        });
        ui.add_space(16.0);

        egui::Frame::none().fill(BG).stroke(egui::Stroke::new(1.0_f32, LINE)).rounding(8.0).inner_margin(Margin::same(12.0)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if self.main_running {
                // Si no puede grabar se ve acá, en todas las páginas, con el motivo (la bandeja avisa una sola vez).
                let error = self.recorder.error.clone().filter(|_| self.cfg.enabled);
                ui.horizontal(|ui| {
                    let (txt, col) = if error.is_some() {
                        (tr!("No puede grabar", "Can't record", "Não consegue gravar"), DANGER)
                    } else if self.cfg.enabled {
                        (tr!("Grabando", "Recording", "Gravando"), TEXT)
                    } else {
                        (tr!("En pausa", "Paused", "Pausado"), MUTED)
                    };
                    let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.0, col);
                    ui.label(RichText::new(txt).family(bold()).color(col));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        toggle(ui, &mut self.cfg.enabled).on_hover_text(if self.cfg.enabled { tr!("Pausar", "Pause", "Pausar") } else { tr!("Reanudar", "Resume", "Retomar") });
                    });
                });
                let sub = if let Some(e) = error {
                    trf!("{e} Clippy vuelve a intentar solo.", "{e} Clippy keeps trying by itself.", "{e} O Clippy tenta de novo sozinho.")
                } else if self.cfg.enabled {
                    let (key, secs) = (hotkey_name(self.cfg.hotkey_vk, self.cfg.hotkey_mods), fmt_secs(self.cfg.seconds));
                    trf!("{key} guarda los últimos {secs}", "{key} saves the last {secs}", "{key} salva os últimos {secs}")
                } else {
                    tr!("No se está guardando nada", "Nothing is being saved", "Nada está sendo salvo").into()
                };
                ui.add(egui::Label::new(RichText::new(sub).size(12.5).color(MUTED)).wrap());
            } else {
                ui.label(RichText::new(tr!("La grabadora está cerrada", "The recorder is closed", "O gravador está fechado")).color(TEXT));
                ui.add_space(4.0);
                if wide_button(ui, tr!("Iniciar grabadora", "Start recorder", "Iniciar gravador"), Kind::Primary, 34.0).clicked() {
                    Mode::Tray { background: false }.launch();
                }
            }
        });
        ui.add_space(16.0);

        for (page, label) in [
            (Page::Home, tr!("Inicio", "Home", "Início")),
            (Page::Keys, tr!("Atajos", "Shortcuts", "Atalhos")),
            (Page::General, tr!("General", "General", "Geral")),
        ] {
            if nav_item(ui, label, self.page == page).clicked() {
                self.page = page;
                self.end_capture();
            }
        }

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            if wide_button(ui, tr!("Salir de Clippy", "Quit Clippy", "Sair do Clippy"), Kind::Danger, 36.0)
                .on_hover_text(tr!("Deja de grabar y cierra todo", "Stops recording and closes everything", "Para de gravar e fecha tudo"))
                .clicked()
            {
                self.confirm_quit = true;
            }
            ui.add_space(14.0);
            divider(ui);
            ui.add_space(14.0);
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                let w = ui.available_width();
                if icon_button(ui, Glyph::Folder, tr!("Carpeta de clips", "Clips folder", "Pasta de clipes"), Kind::Secondary, egui::vec2(w, 36.0)).clicked() {
                    open_folder(&self.cfg.output_dir);
                }
                // Un solo botón principal: con la grabadora cerrada, lo principal es abrirla.
                let editor = if self.main_running { Kind::Primary } else { Kind::Secondary };
                if icon_button(ui, Glyph::Film, tr!("Abrir editor", "Open editor", "Abrir editor"), editor, egui::vec2(w, 42.0)).clicked() {
                    Mode::Editor(None).launch();
                }
                self.update_button(ui);
            });
        });
    }

    pub(super) fn home_page(&mut self, ui: &mut egui::Ui) {
        page_title(ui, tr!("Inicio", "Home", "Início"), tr!("Cómo se graban tus clips.", "How your clips are recorded.", "Como seus clipes são gravados."));
        // Lo primero que se busca: con qué tecla se guarda un clip. Va arriba y se cambia ahí mismo.
        self.clip_key_hero(ui);
        ui.add_space(12.0);
        self.editor_hero(ui);
        ui.add_space(18.0);
        self.recording_section(ui);
        self.audio_section(ui);
    }

    /// Versión nueva: un botón más al pie de la barra lateral, del tamaño de los otros (como en
    /// Discord o Medal), no un cartel. La bandeja ya avisa con una notificación.
    fn update_button(&mut self, ui: &mut egui::Ui) {
        let Some((v, _)) = self.update.clone() else { return };
        let busy = self.update_busy();
        // El pie se arma de abajo hacia arriba: lo primero que se agrega queda abajo del botón.
        if let Some(Err(e)) = self.update_status.lock().unwrap().clone() {
            let msg = trf!("No se pudo actualizar: {e}", "Couldn't update: {e}", "Não foi possível atualizar: {e}");
            ui.add(egui::Label::new(RichText::new(msg).size(12.0).color(DANGER)).wrap());
        }
        let label = if busy {
            trf!("Bajando la {v}…", "Downloading {v}…", "Baixando a {v}…")
        } else {
            trf!("Actualizar a {v}", "Update to {v}", "Atualizar para {v}")
        };
        let cur = update::current();
        let tip = trf!("Tenés la {cur}.", "You have {cur}.", "Você tem a {cur}.");
        let size = egui::vec2(ui.available_width(), 36.0);
        let r = ui.add_enabled_ui(!busy, |ui| icon_button(ui, Glyph::Download, &label, Kind::Secondary, size)).inner;
        // Primero se confirma, viendo qué trae la versión nueva.
        if r.on_hover_text(&tip).on_disabled_hover_text(&tip).clicked() {
            self.confirm_update = Some((v.clone(), update::notes(&v)));
        }
    }

    /// Tarjeta principal: la tecla para guardar clips, dibujada como una tecla que se toca para cambiar.
    fn clip_key_hero(&mut self, ui: &mut egui::Ui) {
        let a = GlobalAction::SaveClip;
        let (vk, mods) = self.cfg.hotkey(a);
        let waiting = self.capturing == Some(a);
        hero_card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 22.0;
                    let text = if waiting { tr!("Apretá…", "Press…", "Aperte…").to_string() } else { hotkey_name(vk, mods) };
                    let tip = tr!("Tocá para cambiar la tecla", "Click to change the key", "Clique para mudar a tecla");
                    if keycap(ui, &text, waiting).on_hover_text(tip).clicked() {
                        self.start_capture(a);
                    }
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.add_space(4.0);
                        let title = tr!("Tu tecla para guardar clips", "Your key to save clips", "Sua tecla para salvar clipes");
                        ui.label(RichText::new(title).family(bold()).size(20.0).color(TEXT));
                        let sub = if waiting {
                            let hint = capture_hint();
                            trf!("Apretá la tecla nueva. {hint}", "Press the new key. {hint}", "Aperte a tecla nova. {hint}")
                        } else {
                            let (key, secs) = (hotkey_name(vk, mods), fmt_secs(self.cfg.seconds));
                            trf!(
                                "Apretá {key} en cualquier momento, incluso en el juego, y se guardan los últimos {secs}.",
                                "Press {key} at any time, even in game, and the last {secs} are saved.",
                                "Aperte {key} a qualquer momento, até no jogo, e os últimos {secs} são salvos."
                            )
                        };
                        ui.add(egui::Label::new(RichText::new(sub).color(MUTED)).wrap());
                        let note = match &self.global_note {
                            Some(n) if !waiting => n.clone(),
                            _ => tr!("Tocá la tecla para cambiarla.", "Click the key to change it.", "Clique na tecla para mudá-la.").to_string(),
                        };
                        ui.label(RichText::new(note).size(12.5).color(MUTED));
                    });
                });
            });
    }

    /// Tarjeta destacada para abrir el editor.
    fn editor_hero(&mut self, ui: &mut egui::Ui) {
        hero_card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(tr!("Editor de clips", "Clip editor", "Editor de clipes")).family(bold()).size(17.0).color(TEXT));
                        let sub = tr!(
                            "Recortá, uní y exportá tus jugadas en un solo video.",
                            "Trim, join and export your plays into a single video.",
                            "Corte, junte e exporte suas jogadas em um só vídeo."
                        );
                        ui.label(RichText::new(sub).color(MUTED));
                        let info = match self.clips {
                            (0, _) => tr!("Todavía no hay clips guardados.", "No saved clips yet.", "Ainda não há clipes salvos.").to_string(),
                            (n, last) => {
                                let (count, when) = (fmt_count(n), last.map(ago).unwrap_or_default());
                                match n {
                                    1 => trf!("{count} clip guardado · el último {when}", "{count} saved clip · the last one {when}", "{count} clipe salvo · o último {when}"),
                                    _ => trf!("{count} clips guardados · el último {when}", "{count} saved clips · the last one {when}", "{count} clipes salvos · o último {when}"),
                                }
                            }
                        };
                        ui.label(RichText::new(info).size(12.5).color(MUTED));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, Glyph::Film, tr!("Abrir editor", "Open editor", "Abrir editor"), Kind::Secondary, egui::vec2(150.0, 40.0)).clicked() {
                            Mode::Editor(None).launch();
                        }
                    });
                });
            });
    }

    pub(super) fn about_section(&mut self, ui: &mut egui::Ui) {
        section(ui, tr!("Acerca de", "About", "Sobre"), |ui| {
            row(ui, tr!("Versión", "Version", "Versão"), LABEL_W, |ui| {
                ui.label(RichText::new(format!("Clippy {}", env!("CARGO_PKG_VERSION"))).color(TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| match self.update.clone() {
                    Some((v, _)) => {
                        let busy = self.update_busy();
                        let label = if busy {
                            tr!("Actualizando…", "Updating…", "Atualizando…").to_string()
                        } else {
                            trf!("Actualizar a {v}", "Update to {v}", "Atualizar para {v}")
                        };
                        if ui.add_enabled_ui(!busy, |ui| button(ui, &label, Kind::Secondary)).inner.clicked() {
                            self.start_update();
                        }
                    }
                    None => {
                        let label = if self.checking {
                            tr!("Buscando…", "Checking…", "Verificando…")
                        } else {
                            tr!("Buscar actualizaciones", "Check for updates", "Buscar atualizações")
                        };
                        // Ancho fijo: al pasar a "Buscando…" no se achica ni se corre.
                        let size = egui::vec2(190.0, 0.0);
                        if ui.add_enabled_ui(!self.checking, |ui| button_sized(ui, label, Kind::Secondary, size)).inner.clicked() {
                            self.start_check();
                        }
                    }
                });
            });
            self.finish_check(ui.ctx());
            if let Some(note) = self.check_note.clone() {
                hint(ui, LABEL_W + 8.0, note);
            }
            divider(ui);
            row(ui, tr!("Datos y registro", "Data and log", "Dados e registro"), LABEL_W, |ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, Glyph::Folder, tr!("Abrir carpeta", "Open folder", "Abrir pasta"), Kind::Secondary, egui::Vec2::ZERO).clicked() {
                        open_folder(&app_dir());
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(app_dir().display().to_string()).color(MUTED)).truncate());
                    });
                });
            });
        });
    }

    /// Con qué se codifica, mientras graba. Si es el procesador, por qué y qué cambia.
    fn encoder_row(&self, ui: &mut egui::Ui) {
        let Some(d) = self.recorder.device.as_ref().filter(|_| self.main_running && self.cfg.enabled) else { return };
        row(ui, tr!("Codifica con", "Encoding with", "Codifica com"), LABEL_W, |ui| {
            let label = match d {
                Device::Gpu(Some(v)) => trf!("Placa de video ({v})", "Graphics card ({v})", "Placa de vídeo ({v})"),
                Device::Gpu(None) => tr!("Placa de video", "Graphics card", "Placa de vídeo").into(),
                Device::Cpu => tr!("Procesador", "Processor", "Processador").into(),
            };
            ui.label(RichText::new(label).color(TEXT));
        });
        if *d == Device::Cpu {
            let (p, fps) = (CPU_MAX_SHORT, CPU_MAX_FPS);
            hint(
                ui,
                LABEL_W + 8.0,
                trf!(
                    "Tu placa de video no tiene codificador de video o no se pudo usar, así que Clippy usa el procesador y graba hasta {p}p a {fps} cuadros por segundo para no frenar el juego.",
                    "Your graphics card has no video encoder or it couldn't be used, so Clippy uses the processor and records up to {p}p at {fps} frames per second so it doesn't slow down your game.",
                    "Sua placa de vídeo não tem codificador de vídeo ou ele não pôde ser usado, então o Clippy usa o processador e grava até {p}p a {fps} quadros por segundo para não deixar o jogo lento."
                ),
            );
        }
    }

    fn recording_section(&mut self, ui: &mut egui::Ui) {
        section(ui, tr!("Grabación", "Recording", "Gravação"), |ui| {
            let max = self.cfg.max_seconds();
            row(ui, tr!("Duración del clip", "Clip length", "Duração do clipe"), LABEL_W, |ui| {
                ui.spacing_mut().slider_width = (ui.available_width() - 104.0).max(80.0);
                ui.add(
                    egui::Slider::new(&mut self.cfg.seconds, MIN_SECONDS..=max)
                        .step_by(5.0)
                        .custom_formatter(|v, _| fmt_secs(v as u32)),
                );
            });
            let secs = fmt_secs(self.cfg.seconds);
            let mut why = trf!(
                "Guarda los últimos {secs} antes de apretar la tecla.",
                "Saves the last {secs} before you press the key.",
                "Salva os últimos {secs} antes de você apertar a tecla."
            );
            if max < MAX_SECONDS {
                let max = fmt_secs(max);
                why += &trf!(
                    " Con esta calidad y la RAM de tu PC, llega hasta {max}.",
                    " With this quality and your PC's RAM, it goes up to {max}.",
                    " Com esta qualidade e a RAM do seu PC, vai até {max}."
                );
            }
            hint(ui, LABEL_W + 8.0, why);
            divider(ui);
            row(ui, tr!("Calidad", "Quality", "Qualidade"), LABEL_W, |ui| {
                // 0 = "Personalizada": se elige el bitrate a mano.
                let mut q = if self.custom_quality || !QUALITY.contains(&self.cfg.bitrate_mbps) { 0 } else { self.cfg.bitrate_mbps };
                let opts = [
                    (QUALITY[0], tr!("Baja", "Low", "Baixa")),
                    (QUALITY[1], tr!("Media", "Medium", "Média")),
                    (QUALITY[2], tr!("Alta", "High", "Alta")),
                    (0, tr!("Personalizada", "Custom", "Personalizada")),
                ];
                if seg(ui, &mut q, &opts) {
                    self.custom_quality = q == 0;
                    if q != 0 {
                        self.cfg.bitrate_mbps = q;
                    }
                }
            });
            if self.custom_quality || !QUALITY.contains(&self.cfg.bitrate_mbps) {
                row(ui, "", LABEL_W, |ui| {
                    ui.spacing_mut().slider_width = (ui.available_width() - 104.0).max(80.0);
                    ui.add(egui::Slider::new(&mut self.cfg.bitrate_mbps, MIN_MBPS..=MAX_MBPS).suffix(" Mbps"));
                });
                hint(
                    ui,
                    LABEL_W + 8.0,
                    tr!(
                        "Más Mbps, mejor imagen y clips más pesados. A 1080p y 60 fps, 40 a 60 ya se ve muy bien; a 1440p o 4K, conviene 80 o más.",
                        "More Mbps, better image and bigger clips. At 1080p and 60 fps, 40 to 60 already looks great; at 1440p or 4K, go for 80 or more.",
                        "Mais Mbps, imagem melhor e clipes mais pesados. Em 1080p e 60 fps, 40 a 60 já fica ótimo; em 1440p ou 4K, use 80 ou mais."
                    ),
                );
            }
            // Más calidad, menos segundos entran en la RAM: se recorta en el mismo cuadro.
            self.cfg.seconds = self.cfg.seconds.min(self.cfg.max_seconds());
            row(ui, tr!("Resolución", "Resolution", "Resolução"), LABEL_W, |ui| {
                let native = tr!("Como el monitor", "Same as monitor", "Igual ao monitor");
                let labels = RESOLUTIONS.map(|r| if r == 0 { native.to_string() } else { format!("{r}p") });
                let opts: Vec<(u32, &str)> = RESOLUTIONS.iter().zip(&labels).map(|(&r, l)| (r, l.as_str())).collect();
                seg(ui, &mut self.cfg.resolution, &opts);
            });
            hint(
                ui,
                LABEL_W + 8.0,
                tr!(
                    "Con la forma de tu monitor: si jugás con resolución estirada, el clip sale estirado como lo ves. Menos resolución, clips más livianos.",
                    "With your monitor's shape: if you play stretched, the clip comes out stretched just as you see it. Lower resolution, lighter clips.",
                    "Com o formato do seu monitor: se você joga com resolução esticada, o clipe sai esticado como você vê. Menos resolução, clipes mais leves."
                ),
            );
            row(ui, tr!("Cuadros por segundo", "Frames per second", "Quadros por segundo"), LABEL_W, |ui| {
                seg(ui, &mut self.cfg.fps, &[(30, "30"), (60, "60"), (120, "120")]);
            });
            hint(
                ui,
                LABEL_W + 8.0,
                tr!(
                    "Video MP4 (H.264): se reproduce en cualquier PC, celular, Discord o WhatsApp.",
                    "MP4 video (H.264): plays on any PC, phone, Discord or WhatsApp.",
                    "Vídeo MP4 (H.264): roda em qualquer PC, celular, Discord ou WhatsApp."
                ),
            );
            self.encoder_row(ui);
            divider(ui);
            row(ui, tr!("Qué se graba", "What's recorded", "O que é gravado"), LABEL_W, |ui| {
                let opts = [
                    (CaptureMode::Screen, tr!("Toda la pantalla", "Whole screen", "Tela inteira")),
                    (CaptureMode::GamesOnly, tr!("Solo juegos", "Games only", "Só jogos")),
                ];
                seg(ui, &mut self.cfg.capture_mode, &opts);
            });
            hint(
                ui,
                LABEL_W + 8.0,
                if self.cfg.capture_mode == CaptureMode::GamesOnly {
                    tr!(
                        "Graba solo el juego en pantalla completa, en el monitor donde esté. Si lo minimizás o lo tapás con otra ventana, esa parte del clip queda en negro y no se ve tu escritorio.",
                        "Records only the fullscreen game, on whichever monitor it's on. If you minimize it or cover it with another window, that part of the clip goes black and your desktop isn't shown.",
                        "Grava só o jogo em tela cheia, no monitor onde ele estiver. Se você minimizá-lo ou cobri-lo com outra janela, essa parte do clipe fica preta e sua área de trabalho não aparece."
                    )
                } else {
                    tr!(
                        "Graba todo lo que se ve en el monitor del juego (sin juego, el de la ventana activa): juegos, escritorio y otras ventanas.",
                        "Records everything on the game's monitor (with no game, the active window's): games, desktop and other windows.",
                        "Grava tudo o que aparece no monitor do jogo (sem jogo, o da janela ativa): jogos, área de trabalho e outras janelas."
                    )
                },
            );
            // "Solo juegos" graba el juego, y en un juego el cursor no va: ahí no aplica.
            if self.cfg.capture_mode == CaptureMode::Screen {
                // Al principio de la columna de controles, como los interruptores de audio de esta página, con la
                // aclaración al lado.
                row(ui, tr!("Grabar el cursor", "Record the cursor", "Gravar o cursor"), LABEL_W, |ui| {
                    toggle(ui, &mut self.cfg.cursor);
                    ui.add_space(10.0);
                    hint_label(
                        ui,
                        tr!("Solo cuando no hay un juego en pantalla.", "Only when there's no game on screen.", "Só quando não há um jogo na tela."),
                    );
                });
            }
            divider(ui);
            row(ui, tr!("Memoria", "Memory", "Memória"), LABEL_W, |ui| {
                let ram = fmt_bytes(self.cfg.typical_ram_mb() as f64 * 1e6);
                let txt = trf!("Unos {ram} de RAM mientras graba", "About {ram} of RAM while recording", "Cerca de {ram} de RAM enquanto grava");
                ui.label(RichText::new(txt).family(bold()).color(TEXT));
            });
            hint(
                ui,
                LABEL_W + 8.0,
                tr!(
                    "Aproximado, como lo muestra el Administrador de tareas. Sube un poco con más segundos, más calidad o mucho movimiento en pantalla.",
                    "Approximate, as Task Manager shows it. It goes up a bit with more seconds, higher quality or lots of motion on screen.",
                    "Aproximado, como o Gerenciador de Tarefas mostra. Sobe um pouco com mais segundos, mais qualidade ou muito movimento na tela."
                ),
            );
            divider(ui);
            row(ui, tr!("Carpeta", "Folder", "Pasta"), LABEL_W, |ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if button(ui, tr!("Cambiar", "Change", "Alterar"), Kind::Secondary).clicked() {
                        if let Some(p) = rfd::FileDialog::new()
                            .set_title(tr!("Carpeta para los clips", "Folder for clips", "Pasta para os clipes"))
                            .set_directory(&self.cfg.output_dir)
                            .pick_folder()
                        {
                            self.cfg.output_dir = p;
                        }
                    }
                    let path = self.cfg.output_dir.display().to_string();
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(&path).color(MUTED)).truncate()).on_hover_text(path);
                    });
                });
            });
        });
    }

    pub(super) fn global_keys_section(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            for a in GlobalAction::ALL {
                // Guardar clip siempre tiene tecla; los demás se pueden quitar.
                let optional = a != GlobalAction::SaveClip;
                if optional {
                    divider(ui);
                }
                row(ui, a.label(), LABEL_W + 40.0, |ui| {
                    let (vk, mods) = self.cfg.hotkey(a);
                    let capturing = self.capturing == Some(a);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if optional && vk != 0 && !capturing && button(ui, tr!("Quitar", "Remove", "Remover"), Kind::Ghost).clicked() {
                            let (v, m) = self.cfg.hotkey_mut(a);
                            *v = 0;
                            *m = 0;
                        }
                        let text = if capturing {
                            tr!("Apretá una tecla…", "Press a key…", "Aperte uma tecla…").to_string()
                        } else if vk == 0 {
                            tr!("Sin asignar", "Not set", "Não definido").to_string()
                        } else {
                            hotkey_name(vk, mods)
                        };
                        let kind = if capturing { Kind::Selected } else { Kind::Secondary };
                        let w = ui.available_width();
                        if button_sized(ui, &text, kind, egui::vec2(w, 34.0)).clicked() {
                            self.start_capture(a);
                        }
                    });
                });
            }
            let note = if self.capturing.is_some() {
                capture_hint().to_string()
            } else if let Some(n) = &self.global_note {
                n.clone()
            } else {
                tr!(
                    "Los atajos del editor se cambian desde el editor, en «Ajustes del editor» (la tuerca).",
                    "Editor shortcuts are changed from the editor, in “Editor settings” (the gear).",
                    "Os atalhos do editor são alterados no próprio editor, em “Configurações do editor” (a engrenagem)."
                )
                .to_string()
            };
            ui.label(RichText::new(note).size(12.5).color(MUTED));
        });
    }

    fn audio_section(&mut self, ui: &mut egui::Ui) {
        section(ui, tr!("Audio", "Audio", "Áudio"), |ui| {
            volume_row(ui, tr!("Juego", "Game", "Jogo"), &mut self.cfg.game_audio, &mut self.cfg.game_volume);
            hint(
                ui,
                LABEL_W + 8.0,
                if self.cfg.game_audio {
                    tr!(
                        "Se detecta solo: el juego que tengas en pantalla completa. Mientras no haya uno, todo va a Escritorio.",
                        "Detected automatically: whatever game you have in fullscreen. While there isn't one, everything goes to Desktop.",
                        "Detectado sozinho: o jogo que estiver em tela cheia. Enquanto não houver um, tudo vai para Área de trabalho."
                    )
                } else {
                    tr!(
                        "Silenciado: el juego no se va a escuchar en los clips.",
                        "Muted: the game won't be heard in clips.",
                        "Silenciado: o jogo não vai ser ouvido nos clipes."
                    )
                },
            );
            divider(ui);
            volume_row(ui, tr!("Escritorio", "Desktop", "Área de trabalho"), &mut self.cfg.desktop_audio, &mut self.cfg.desktop_volume);
            hint(ui, LABEL_W + 8.0, tr!("Todo lo demás: Discord, música, navegador.", "Everything else: Discord, music, browser.", "Todo o resto: Discord, música, navegador."));
            divider(ui);
            row(ui, tr!("Micrófono", "Microphone", "Microfone"), LABEL_W, |ui| {
                let tip = if self.cfg.mic_audio {
                    tr!("Apagar micrófono", "Turn off microphone", "Desligar microfone")
                } else {
                    tr!("Grabar tu voz", "Record your voice", "Gravar sua voz")
                };
                toggle(ui, &mut self.cfg.mic_audio).on_hover_text(tip);
                ui.add_space(8.0);
                ui.add_enabled_ui(self.cfg.mic_audio, |ui| {
                    ui.spacing_mut().slider_width = (ui.available_width() - 86.0).max(60.0);
                    ui.add(egui::Slider::new(&mut self.cfg.mic_volume, 0..=200).suffix(" %"));
                });
            });
            if self.cfg.mic_audio {
                row(ui, "", LABEL_W, |ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if button(ui, tr!("Actualizar", "Refresh", "Atualizar"), Kind::Secondary)
                            .on_hover_text(tr!("Buscar micrófonos conectados", "Look for connected microphones", "Procurar microfones conectados"))
                            .clicked()
                        {
                            self.mics = audio::capture_devices();
                        }
                        let default = tr!("Predeterminado de Windows", "Windows default", "Padrão do Windows");
                        let current = if self.cfg.mic_device.is_empty() {
                            default.to_string()
                        } else {
                            self.mics
                                .iter()
                                .find(|(id, _)| *id == self.cfg.mic_device)
                                .map(|(_, n)| n.clone())
                                .unwrap_or_else(|| tr!("Micrófono desconectado", "Microphone disconnected", "Microfone desconectado").into())
                        };
                        let w = ui.available_width();
                        egui::ComboBox::from_id_salt("mic")
                            .width(w - 8.0)
                            .truncate()
                            .selected_text(current.clone())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.cfg.mic_device, String::new(), default);
                                for (id, name) in self.mics.clone() {
                                    ui.selectable_value(&mut self.cfg.mic_device, id, name);
                                }
                            })
                            .response
                            .on_hover_text(current);
                    });
                });
            }
            hint(
                ui,
                LABEL_W + 8.0,
                if self.cfg.mic_audio {
                    tr!(
                        "Tu voz se mezcla en el clip. También lo podés prender o apagar desde el ícono de la bandeja.",
                        "Your voice is mixed into the clip. You can also turn it on or off from the tray icon.",
                        "Sua voz é mixada no clipe. Você também pode ligá-lo ou desligá-lo pelo ícone da bandeja."
                    )
                } else {
                    tr!("Apagado: tu voz no queda en los clips.", "Off: your voice isn't in clips.", "Desligado: sua voz não fica nos clipes.")
                },
            );

        });
    }

    pub(super) fn general_section(&mut self, ui: &mut egui::Ui) {
        section(ui, tr!("Preferencias", "Preferences", "Preferências"), |ui| {
            row(ui, tr!("Idioma", "Language", "Idioma"), LABEL_W, |ui| {
                let mut opts = vec![(Language::Auto, tr!("Automático", "Automatic", "Automático"))];
                opts.extend(Lang::ALL.map(|l| (Language::from(l), l.name())));
                seg(ui, &mut self.cfg.language, &opts);
            });
            let system = Language::Auto.resolve().name();
            hint(
                ui,
                LABEL_W + 8.0,
                trf!(
                    "Automático usa el idioma de Windows ({system}).",
                    "Automatic follows the Windows language ({system}).",
                    "Automático segue o idioma do Windows ({system})."
                ),
            );
            divider(ui);
            row(ui, tr!("Sonido al guardar", "Sound on save", "Som ao salvar"), LABEL_W, |ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    toggle(ui, &mut self.cfg.sound_on_save);
                    if button(ui, tr!("Probar sonido", "Test sound", "Testar som"), Kind::Secondary).clicked() {
                        sound::play_saved();
                    }
                });
            });
            divider(ui);
            row(ui, tr!("Iniciar con Windows", "Start with Windows", "Iniciar com o Windows"), LABEL_W, |ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let mut a = self.autostart;
                    if toggle(ui, &mut a).changed() {
                        autostart::set(a);
                        self.autostart = autostart::enabled();
                    }
                });
            });
        });
    }
}

/// Fila de audio: interruptor + volumen + porcentaje.
fn volume_row(ui: &mut egui::Ui, label: &str, on: &mut bool, vol: &mut u32) {
    row(ui, label, LABEL_W, |ui| {
        toggle(ui, on).on_hover_text(if *on { tr!("Silenciar", "Mute", "Silenciar") } else { tr!("Activar", "Unmute", "Ativar") });
        ui.add_space(8.0);
        ui.add_enabled_ui(*on, |ui| {
            ui.spacing_mut().slider_width = (ui.available_width() - 86.0).max(60.0);
            ui.add(egui::Slider::new(vol, 0..=100).suffix(" %"));
        });
    });
}

fn ago(t: SystemTime) -> String {
    let s = SystemTime::now().duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
    // La unidad en singular y plural.
    let hace = |n: u64, [one, many]: [&str; 2]| {
        let unit = if n == 1 { one } else { many };
        trf!("hace {n} {unit}", "{n} {unit} ago", "há {n} {unit}")
    };
    match s {
        0..=59 => tr!("hace un momento", "a moment ago", "agora há pouco").into(),
        60..=3599 => hace(s / 60, ["min", "min"]),
        3600..=86_399 => hace(s / 3600, ["h", "h"]),
        86_400..=172_799 => tr!("ayer", "yesterday", "ontem").into(),
        172_800..=604_799 => hace(s / 86_400, tr!(["día", "días"], ["day", "days"], ["dia", "dias"])),
        604_800..=2_591_999 => hace(s / 604_800, tr!(["semana", "semanas"], ["week", "weeks"], ["semana", "semanas"])),
        2_592_000..=31_535_999 => hace(s / 2_592_000, tr!(["mes", "meses"], ["month", "months"], ["mês", "meses"])),
        _ => hace(s / 31_536_000, tr!(["año", "años"], ["year", "years"], ["ano", "anos"])),
    }
}
