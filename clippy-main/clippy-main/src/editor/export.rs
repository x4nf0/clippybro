//! Exportar: diálogo, plan (tamaño, calidad, copiar o renderizar) y el trabajo en segundo plano.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self, Align, Layout, RichText};

use crate::log::file_stamp;
use crate::i18n::{tr, trf};
use crate::ui_kit::theme::*;
use super::media::Progress;
use crate::mf::Mf;
use super::library;
use super::media;
use super::EditorApp;

#[derive(Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(from = "String", into = "&'static str")]
pub(super) enum Quality {
    Recommended,
    High,
    Medium,
    Low,
    MaxSize,
}

impl Quality {
    pub(super) fn id(self) -> &'static str {
        match self {
            Quality::Recommended => "recomendada",
            Quality::High => "alta",
            Quality::Medium => "media",
            Quality::Low => "baja",
            Quality::MaxSize => "tamano",
        }
    }
    fn from_id(s: &str) -> Self {
        match s {
            "alta" => Quality::High,
            "media" => Quality::Medium,
            "baja" => Quality::Low,
            "tamano" => Quality::MaxSize,
            _ => Quality::Recommended,
        }
    }
    /// Bits por píxel por cuadro (H.264, video de juegos).
    fn bits_per_pixel(self) -> f64 {
        match self {
            Quality::High => 0.18,
            Quality::Recommended => 0.13,
            Quality::Medium => 0.10,
            Quality::Low => 0.06,
            Quality::MaxSize => 0.13,
        }
    }
}

pub(super) struct ExportDialog {
    name: String,
    dir: PathBuf,
    height: u32,
    fps: u32,
    quality: Quality,
    size_mb: u32,
}

struct ExportPlan {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) fps: f64,
    pub(super) bitrate: u32,
    pub(super) constant: bool,
    /// Some(true) = se copia sin recodificar, Some(false) = se renderiza, None = todavía no se sabe.
    pub(super) copy: Option<bool>,
    pub(super) bytes: f64,
    pub(super) seconds: f64,
    pub(super) original: bool,
    pub(super) low_quality: bool,
}

pub(super) struct ExportJob {
    pub(super) progress: Arc<Progress>,
    pub(super) out: PathBuf,
}

impl EditorApp {
    /// Abre el diálogo de exportación (con las últimas opciones usadas).
    pub(super) fn export(&mut self) {
        if self.is_empty() || self.job.is_some() {
            return;
        }
        self.pause();
        self.export_dlg = Some(ExportDialog {
            name: format!("Clippy_edit_{}", file_stamp()),
            dir: self.cfg.output_dir.clone(),
            height: self.prefs.export_height,
            fps: self.prefs.export_fps,
            quality: self.prefs.export_quality,
            size_mb: self.prefs.export_size_mb.max(1),
        });
    }

    /// Lo que va a salir con las opciones del diálogo.
    fn export_plan(&self, d: &ExportDialog) -> Option<ExportPlan> {
        let edit = self.edit();
        let src = self.loader.probe(&edit.first_video()?.path)?;
        let total_s = (edit.len() as f64 / 1e7).max(0.1);
        let src_fps = src.fps().max(1.0);
        let (ow, oh) = media::canvas_size((src.width, src.height), edit.ratio, d.height);
        let ofps = if d.fps == 0 || d.fps as f64 >= src_fps - 0.5 { src_fps } else { d.fps as f64 };
        let original = d.height == 0 && d.fps == 0 && d.quality == Quality::Recommended;

        // ¿Se puede copiar sin recodificar? Una sola pista de piezas pegadas, del mismo formato y con
        // cortes en fotogramas clave.
        let info = self.loader.info.lock().unwrap();
        let parts = edit.copy_parts((src.width, src.height));
        let mut copy = Some(original && parts.is_some());
        for p in parts.unwrap_or_default() {
            let i = info.get(&p.path);
            let same = i.and_then(|i| i.probe.as_ref()).is_some_and(|q| q.same_format(&src));
            if !same {
                copy = Some(false);
            }
            match i.and_then(|i| i.keyframes.as_ref()) {
                Some(k) => {
                    if !media::on_keyframe(k, p.src_in) {
                        copy = Some(false);
                    }
                }
                None => {
                    if copy == Some(true) {
                        copy = None; // todavía no se sabe
                    }
                }
            }
        }
        let copy_size: f64 = parts
            .unwrap_or_default()
            .iter()
            .map(|p| {
                let size = std::fs::metadata(&p.path).map_or(0, |m| m.len()) as f64;
                let dur = info.get(&p.path).and_then(|i| i.probe.as_ref()).map(|q| q.duration).unwrap_or(1).max(1) as f64;
                size * p.len as f64 / dur
            })
            .sum();
        drop(info);

        let (bitrate, constant) = match d.quality {
            Quality::MaxSize => {
                let bits = d.size_mb as f64 * 1_000_000.0 * 8.0 * 0.94 / total_s - 192_000.0;
                (bits.clamp(300_000.0, 80_000_000.0) as u32, true)
            }
            q => {
                let b = ow as f64 * oh as f64 * ofps * q.bits_per_pixel();
                (b.clamp(1_500_000.0, 80_000_000.0) as u32, false)
            }
        };
        let render_bytes = (bitrate as f64 + 192_000.0) * total_s / 8.0;
        let low = (bitrate as f64) / (ow as f64 * oh as f64 * ofps) < 0.045;
        Some(ExportPlan {
            width: ow,
            height: oh,
            fps: ofps,
            bitrate,
            constant,
            copy,
            bytes: if copy == Some(true) { copy_size } else { render_bytes },
            seconds: total_s,
            original,
            low_quality: low,
        })
    }

    pub(super) fn export_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.export_dlg.take() else { return };
        let edit = self.edit();
        let src = edit.first_video().and_then(|c| self.loader.probe(&c.path));
        // Las resoluciones son del lado corto (1080p también en un video vertical).
        let (src_h, src_fps) = src
            .as_ref()
            .map(|s| {
                let (w, h) = media::canvas_size((s.width, s.height), edit.ratio, 0);
                (w.min(h), s.fps().round() as u32)
            })
            .unwrap_or((1080, 60));
        let plan = self.export_plan(&d);
        let max_h = (ctx.screen_rect().height() - 240.0).max(200.0);
        let mut action = 0;
        let title = tr!("Exportar video", "Export video", "Exportar vídeo");
        let sub = tr!("Elegí cómo querés guardar tu video.", "Choose how you want to save your video.", "Escolha como quer salvar seu vídeo.");
        let x = modal(ctx, "export", title, sub, 720.0, |ui| {
            modal_body(ui, |ui| {
                // Con la ventana baja, el cuerpo se desplaza y los botones de abajo siguen a la vista.
                egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                    caption(ui, tr!("Archivo", "File", "Arquivo"));
                    row(ui, tr!("Nombre", "Name", "Nome"), 170.0, |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut d.name)
                                .desired_width(ui.available_width() - 50.0)
                                .margin(egui::vec2(10.0, 7.0))
                                .font(egui::FontId::proportional(14.0)),
                        );
                        ui.label(RichText::new(".mp4").color(MUTED));
                    });
                    row(ui, tr!("Guardar en", "Save to", "Salvar em"), 170.0, |ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if button(ui, tr!("Cambiar", "Change", "Alterar"), Kind::Secondary).clicked() {
                                let dialog = rfd::FileDialog::new().set_title(tr!("Carpeta de destino", "Destination folder", "Pasta de destino"));
                                if let Some(p) = dialog.set_directory(&d.dir).pick_folder() {
                                    d.dir = p;
                                }
                            }
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.add(egui::Label::new(RichText::new(d.dir.display().to_string()).color(MUTED)).truncate());
                            });
                        });
                    });
                    ui.add_space(10.0);
                    caption(ui, tr!("Formato", "Format", "Formato"));
                    let orig_res = format!("Original · {src_h}p");
                    let mut res_opts: Vec<(u32, String)> = vec![(0, orig_res)];
                    for h in [1440u32, 1080, 720] {
                        if h < src_h {
                            res_opts.push((h, format!("{h}p")));
                        }
                    }
                    row(ui, tr!("Resolución", "Resolution", "Resolução"), 170.0, |ui| {
                        let opts: Vec<(u32, &str)> = res_opts.iter().map(|(v, l)| (*v, l.as_str())).collect();
                        seg(ui, &mut d.height, &opts);
                    });
                    let orig_fps = format!("Original · {src_fps}");
                    let mut fps_opts: Vec<(u32, String)> = vec![(0, orig_fps)];
                    for f in [60u32, 30] {
                        if f < src_fps {
                            fps_opts.push((f, f.to_string()));
                        }
                    }
                    row(ui, tr!("Cuadros por segundo", "Frames per second", "Quadros por segundo"), 170.0, |ui| {
                        let opts: Vec<(u32, &str)> = fps_opts.iter().map(|(v, l)| (*v, l.as_str())).collect();
                        seg(ui, &mut d.fps, &opts);
                    });
                    row(ui, tr!("Calidad", "Quality", "Qualidade"), 170.0, |ui| {
                        seg(
                            ui,
                            &mut d.quality,
                            &[
                                (Quality::Recommended, tr!("Recomendada", "Recommended", "Recomendada")),
                                (Quality::High, tr!("Alta", "High", "Alta")),
                                (Quality::Medium, tr!("Media", "Medium", "Média")),
                                (Quality::Low, tr!("Baja", "Low", "Baixa")),
                                (Quality::MaxSize, tr!("Por tamaño", "By size", "Por tamanho")),
                            ],
                        );
                    });
                    if d.quality == Quality::MaxSize {
                        row(ui, tr!("Tamaño máximo", "Maximum size", "Tamanho máximo"), 170.0, |ui| {
                            seg(ui, &mut d.size_mb, &[(10, "10 MB"), (25, "25 MB"), (50, "50 MB"), (100, "100 MB")]);
                        });
                    }
                    ui.add_space(10.0);
                    note_box(ui, |ui| match &plan {
                        Some(p) => {
                            let format = if p.copy == Some(true) {
                                format!("{}×{} · {:.0} fps · {}", p.width, p.height, p.fps, tr!("igual al original", "same as the original", "igual ao original"))
                            } else {
                                format!("{}×{} · {:.0} fps · {:.1} Mbps · MP4 H.264", p.width, p.height, p.fps, p.bitrate as f64 / 1e6)
                            };
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format).family(bold()).color(TEXT));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(RichText::new(format!("~{}", fmt_bytes(p.bytes))).family(bold()).size(16.0).color(TEXT));
                                });
                            });
                            let how = match p.copy {
                                Some(true) => tr!(
                                    "Se copia sin recodificar: es instantáneo y no pierde calidad.",
                                    "It's copied without re-encoding: instant and lossless.",
                                    "É copiado sem recodificar: é instantâneo e não perde qualidade."
                                )
                                .to_string(),
                                None => tr!(
                                    "Si los cortes caen en fotogramas clave se copia al instante; si no, se renderiza con la GPU.",
                                    "If the cuts land on keyframes it's copied instantly; if not, it's rendered on the GPU.",
                                    "Se os cortes caírem em quadros-chave é copiado na hora; se não, é renderizado na GPU."
                                )
                                .to_string(),
                                Some(false) => {
                                    let secs = fmt_secs((p.seconds / 3.0).ceil().max(1.0) as u32);
                                    match p.original {
                                        true => trf!(
                                            "Se renderiza con la GPU para cortar justo donde marcaste: tarda unos {secs}.",
                                            "It's rendered on the GPU to cut exactly where you marked: takes about {secs}.",
                                            "É renderizado na GPU para cortar exatamente onde você marcou: leva uns {secs}."
                                        ),
                                        false => trf!(
                                            "Se renderiza con la GPU: tarda unos {secs}.",
                                            "It's rendered on the GPU: takes about {secs}.",
                                            "É renderizado na GPU: leva uns {secs}."
                                        ),
                                    }
                                }
                            };
                            let len = fmt_time((p.seconds * 1e7) as i64);
                            ui.label(RichText::new(trf!("Duración {len} · {how}", "Duration {len} · {how}", "Duração {len} · {how}")).size(12.5).color(MUTED));
                            if p.low_quality {
                                ui.label(
                                    RichText::new(tr!(
                                        "Con ese tamaño la imagen se va a ver con poca calidad. Probá bajar la resolución o los cuadros por segundo.",
                                        "At that size the picture will look low quality. Try lowering the resolution or the frames per second.",
                                        "Com esse tamanho a imagem vai ficar com pouca qualidade. Tente baixar a resolução ou os quadros por segundo."
                                    ))
                                        .size(12.5)
                                        .color(DANGER),
                                );
                            }
                        }
                        None => {
                            let msg = tr!(
                                "No hay video para exportar: mostrá una pista de video o agregá un clip.",
                                "There's no video to export: show a video track or add a clip.",
                                "Não há vídeo para exportar: mostre uma faixa de vídeo ou adicione um clipe."
                            );
                            ui.label(RichText::new(msg).color(DANGER));
                        }
                    });
                });
            });
            modal_footer(ui, |ui| {
                if button_sized(ui, tr!("Restablecer", "Reset", "Restaurar"), Kind::Secondary, egui::vec2(0.0, 38.0))
                    .on_hover_text(tr!("Volver a Original / Recomendada", "Back to Original / Recommended", "Voltar para Original / Recomendada"))
                    .clicked()
                {
                    d.height = 0;
                    d.fps = 0;
                    d.quality = Quality::Recommended;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let ok = plan.is_some() && !d.name.trim().is_empty();
                    let r = ui.add_enabled_ui(ok, |ui| icon_button(ui, Glyph::Export, tr!("Exportar", "Export", "Exportar"), Kind::Primary, egui::vec2(130.0, 38.0))).inner;
                    if r.clicked() {
                        action = 2;
                    }
                    if button_sized(ui, tr!("Cancelar", "Cancel", "Cancelar"), Kind::Secondary, egui::vec2(120.0, 38.0)).clicked() {
                        action = 1;
                    }
                });
            });
        });
        if x {
            action = 1;
        }
        let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
        if action == 1 {
            return;
        }
        if action == 2 || (enter && plan.is_some() && !d.name.trim().is_empty()) {
            if let Some(p) = plan {
                self.remember_export(&d);
                self.start_export(&d, &p);
            }
            return;
        }
        self.export_dlg = Some(d);
    }

    fn remember_export(&mut self, d: &ExportDialog) {
        self.prefs.export_height = d.height;
        self.prefs.export_fps = d.fps;
        self.prefs.export_quality = d.quality;
        self.prefs.export_size_mb = d.size_mb;
        self.prefs.save();
    }

    fn start_export(&mut self, d: &ExportDialog, plan: &ExportPlan) {
        let clean = library::clean_name(&d.name);
        let mut out = d.dir.join(format!("{clean}.mp4"));
        let mut n = 2;
        while out.exists() {
            out = d.dir.join(format!("{clean} ({n}).mp4"));
            n += 1;
        }
        let edit = self.edit();
        let target = (!plan.original).then_some(media::Target {
            height: d.height,
            fps: d.fps,
            bitrate: plan.bitrate,
            constant: plan.constant,
        });
        let opts = media::ExportOpts { bitrate: plan.bitrate, target };
        let progress = Arc::new(Progress::new());
        {
            let (p, out, ctx) = (progress.clone(), out.clone(), self.ctx.clone());
            std::thread::spawn(move || {
                let _mf = Mf::init();
                let r = media::export(&edit, &out, &opts, &p);
                if let Err(e) = &r {
                    crate::log::log(&format!("exportar: {e:?}"));
                }
                *p.result.lock().unwrap() = Some(r.map_err(|e| e.to_string()));
                ctx.request_repaint();
            });
        }
        self.message = None;
        self.job = Some(ExportJob { progress, out });
    }

    pub(super) fn poll_export(&mut self) {
        let Some(job) = &self.job else { return };
        let res = job.progress.result.lock().unwrap().take();
        match res {
            Some(Ok(mode)) => {
                let name = job.out.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                self.message = Some((true, trf!("Listo: {name} ({mode})", "Done: {name} ({mode})", "Pronto: {name} ({mode})"), Some(job.out.clone())));
                self.dirty = false;
                self.job = None;
                self.scan();
            }
            Some(Err(e)) => {
                let canceled = job.progress.cancel.load(std::sync::atomic::Ordering::Relaxed);
                let msg = if canceled {
                    tr!("Exportación cancelada.", "Export canceled.", "Exportação cancelada.").to_string()
                } else {
                    trf!("No se pudo exportar: {e}", "Couldn't export: {e}", "Não foi possível exportar: {e}")
                };
                self.message = Some((false, msg, None));
                self.job = None;
            }
            None => self.ctx.request_repaint_after(Duration::from_millis(100)),
        }
    }
}

impl From<String> for Quality {
    fn from(s: String) -> Self {
        Quality::from_id(&s)
    }
}

impl From<Quality> for &'static str {
    fn from(q: Quality) -> Self {
        q.id()
    }
}
