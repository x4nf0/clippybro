//! Panel de la biblioteca: los videos de la carpeta de clips (y de sus carpetas) y los importados.
//! Un clic elige (con Ctrl se suma, con Shift se elige un tramo) y arrastrar desde cualquier lado elige con un
//! recuadro; doble clic o el "+" de una tarjeta la suma a la línea de tiempo. Lo elegido se mueve a una carpeta
//! o se manda a la Papelera de reciclaje (los importados solo salen de la lista: el archivo no se toca).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use eframe::egui::{self, Align, Align2, Color32, CursorIcon, FontId, Layout, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2};

use crate::i18n::{tr, trf};
use crate::ui_kit::theme::*;
use crate::ui_kit::window::open_folder;
use super::library::{self, Asset, Folder};
use super::shortcuts::{self, Act};
use super::EditorApp;

/// Alto del título y los datos debajo de la miniatura de una tarjeta.
const CARD_TEXT_H: f32 = 42.0;
const GAP: f32 = 10.0;
const ROW_GAP: f32 = 6.0;
/// Radio del "+" de una tarjeta.
const ADD_R: f32 = 13.0;
/// Con el recuadro de selección a menos de esto del borde de arriba o de abajo, la lista se desplaza sola.
const EDGE: f32 = 40.0;
const AUTO_SCROLL: f32 = 700.0;

#[derive(Default)]
pub(super) struct Browser {
    /// Carpeta que se ve (None = la de los clips, con sus carpetas y los importados).
    pub(super) dir: Option<PathBuf>,
    /// Las carpetas de adentro de la de los clips.
    pub(super) folders: Vec<Folder>,
    /// Lo elegido.
    pub(super) sel: HashSet<PathBuf>,
    /// Desde dónde elige Shift + clic (lugar en la lista de videos).
    anchor: Option<usize>,
    /// Recuadro de selección: dónde empezó (en el contenido de la lista) y lo que estaba elegido antes (con Ctrl).
    marquee: Option<(Pos2, HashSet<PathBuf>)>,
    scroll: f32,
    set_scroll: Option<f32>,
    /// Lo último que se tocó es la biblioteca: Supr borra lo elegido acá y no en la línea de tiempo.
    pub(super) focus: bool,
    dialog: Option<Dialog>,
}

enum Dialog {
    /// Eliminar videos: los de la carpeta de clips van a la Papelera; los importados solo salen de la lista.
    Delete(Vec<PathBuf>),
    /// Eliminar una carpeta con lo que tiene.
    DeleteFolder(PathBuf),
    /// Carpeta nueva (y mover ahí `files`) o cambiarle el nombre a `rename`.
    Name { name: String, files: Vec<PathBuf>, rename: Option<PathBuf>, error: Option<String> },
}

/// "1 video", "3 videos".
fn videos(n: usize) -> String {
    let c = fmt_count(n);
    match n {
        1 => tr!("1 video", "1 video", "1 vídeo").into(),
        _ => trf!("{c} videos", "{c} videos", "{c} vídeos"),
    }
}

/// Lo que entra en el renglón de abajo de una tarjeta: tamaño y de dónde viene.
fn asset_card(ui: &mut egui::Ui, rect: Rect, a: &Asset, info: Option<&library::Info>, selected: bool, used: bool) -> (egui::Response, bool) {
    let resp = ui.interact(rect, ui.id().with(("asset", &a.path)), Sense::click());
    let thumb = Rect::from_min_size(rect.min, egui::vec2(rect.width(), rect.height() - CARD_TEXT_H));
    let p = ui.painter();
    let mut shapes = vec![Shape::rect_filled(thumb, 6.0, CONTROL)];
    match info.and_then(|i| i.cover.as_ref()) {
        Some(t) => shapes.push(Shape::image(t.id(), thumb, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE)),
        None => {
            let msg = if info.and_then(|i| i.error.as_ref()).is_some() {
                tr!("No se puede leer", "Can't be read", "Não dá para ler")
            } else {
                tr!("Cargando…", "Loading…", "Carregando…")
            };
            shapes.push(ui.fonts(|f| Shape::text(f, thumb.center(), Align2::CENTER_CENTER, msg, FontId::proportional(12.0), MUTED)));
        }
    }
    if let Some(pr) = info.and_then(|i| i.probe.as_ref()) {
        let g = p.layout_no_wrap(super::panels::fmt_len(pr.duration), FontId::proportional(11.5), WHITE);
        let r = Rect::from_min_size(thumb.right_bottom() - g.size() - egui::vec2(14.0, 10.0), g.size() + egui::vec2(8.0, 4.0));
        shapes.push(Shape::rect_filled(r, 4.0, BADGE));
        shapes.push(Shape::galley(r.min + egui::vec2(4.0, 2.0), g, WHITE));
    }
    if used {
        let c = thumb.min + egui::vec2(15.0, 15.0);
        shapes.push(Shape::circle_filled(c, 9.0, WHITE));
        shapes.push(Shape::line(vec![c + egui::vec2(-4.0, 0.0), c + egui::vec2(-1.0, 3.5), c + egui::vec2(4.5, -3.5)], Stroke::new(2.0_f32, BLACK)));
    }
    let hovered = ui.rect_contains_pointer(rect);
    if selected {
        shapes.push(Shape::rect_filled(thumb, 6.0, HOVER_DIM));
        shapes.push(Shape::rect_stroke(thumb, 6.0, Stroke::new(2.0_f32, WHITE)));
    } else if hovered {
        shapes.push(Shape::rect_stroke(thumb, 6.0, Stroke::new(1.0_f32, BORDER)));
    }
    let title = one_line(p, a.title.clone(), FontId::proportional(13.0), rect.width() - 4.0);
    shapes.push(Shape::galley(Pos2::new(rect.left() + 2.0, thumb.bottom() + 6.0), title, TEXT));
    let sub = format!("{}{}", fmt_bytes(a.size as f64), if a.imported { tr!(" · importado", " · imported", " · importado") } else { "" });
    let sub = one_line(p, sub, FontId::proportional(11.5), rect.width() - 4.0);
    shapes.push(Shape::galley(Pos2::new(rect.left() + 2.0, thumb.bottom() + 25.0), sub, MUTED));
    paint_pressed(ui, rect.center(), press(ui, &resp), PRESS_DEPTH, shapes);
    // "+": sumarla a la línea de tiempo, con el mouse encima o elegida.
    let mut add = false;
    if hovered || selected {
        let c = Pos2::new(thumb.right() - ADD_R - 6.0, thumb.top() + ADD_R + 6.0);
        let r = ui.interact(Rect::from_center_size(c, Vec2::splat(ADD_R * 2.0)), resp.id.with("add"), Sense::click());
        let fill = if r.hovered() { WHITE } else { WHITE.gamma_multiply(0.85) };
        let mut plus = vec![Shape::circle_filled(c, ADD_R, fill)];
        plus.extend(glyph(Glyph::Plus, c, 12.0, BLACK));
        paint_pressed(ui, c, press(ui, &r), PRESS_DEPTH_ICON, plus);
        add = r.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(add_label()).clicked();
    }
    let title = &a.title;
    let resp = resp.on_hover_text(trf!(
        "{title}\nDoble clic para agregarlo a la línea de tiempo",
        "{title}\nDouble-click to add it to the timeline",
        "{title}\nClique duplo para adicioná-lo à linha do tempo"
    ));
    (resp, add)
}

fn folder_card(ui: &mut egui::Ui, rect: Rect, f: &Folder) -> egui::Response {
    let resp = ui.interact(rect, ui.id().with(("folder", &f.path)), Sense::click());
    let thumb = Rect::from_min_size(rect.min, egui::vec2(rect.width(), rect.height() - CARD_TEXT_H));
    let p = ui.painter();
    let mut shapes = vec![Shape::rect_filled(thumb, 6.0, CONTROL)];
    if ui.rect_contains_pointer(rect) {
        shapes.push(Shape::rect_stroke(thumb, 6.0, Stroke::new(1.0_f32, BORDER)));
    }
    shapes.extend(glyph(Glyph::Folder, thumb.center(), (thumb.height() * 0.32).clamp(18.0, 40.0), ICON));
    let title = one_line(p, f.name.clone(), FontId::proportional(13.0), rect.width() - 4.0);
    shapes.push(Shape::galley(Pos2::new(rect.left() + 2.0, thumb.bottom() + 6.0), title, TEXT));
    let sub = one_line(p, videos(f.count), FontId::proportional(11.5), rect.width() - 4.0);
    shapes.push(Shape::galley(Pos2::new(rect.left() + 2.0, thumb.bottom() + 25.0), sub, MUTED));
    paint_pressed(ui, rect.center(), press(ui, &resp), PRESS_DEPTH, shapes);
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text({
        let name = &f.name;
        trf!("{name}\nClic para abrirla", "{name}\nClick to open it", "{name}\nClique para abri-la")
    })
}

/// Lo que se pidió desde la lista (se hace después de dibujarla).
enum Do {
    Add(Vec<PathBuf>),
    Open(Option<PathBuf>),
    Reveal(PathBuf),
    Move(Vec<PathBuf>, PathBuf),
    Dialog(Dialog),
}

impl EditorApp {
    /// Vuelve a leer la carpeta que se ve (si ya no existe, la de los clips).
    pub(super) fn scan(&mut self) {
        self.imported.retain(|p| p.exists());
        if self.browser.dir.as_ref().is_some_and(|d| !d.is_dir()) {
            self.open_dir(None);
            return;
        }
        let (folders, assets) = library::scan(&self.cfg.output_dir, self.browser.dir.as_deref(), &self.imported);
        self.browser.folders = folders;
        self.assets = assets;
        let shown: HashSet<&Path> = self.assets.iter().map(|a| a.path.as_path()).collect();
        self.browser.sel.retain(|p| shown.contains(p.as_path()));
    }

    /// Muestra otra carpeta (None = la de los clips).
    pub(super) fn open_dir(&mut self, dir: Option<PathBuf>) {
        self.browser.dir = dir;
        self.browser.sel.clear();
        self.browser.anchor = None;
        self.browser.set_scroll = Some(0.0);
        self.scan();
    }

    /// Lo elegido, en el orden de la lista.
    fn selected_assets(&self) -> Vec<PathBuf> {
        self.assets.iter().filter(|a| self.browser.sel.contains(&a.path)).map(|a| a.path.clone()).collect()
    }

    fn is_used(&self, p: &Path) -> bool {
        self.tracks.iter().any(|t| t.clips.iter().any(|c| c.path == p))
    }

    pub(super) fn library_panel(&mut self, ui: &mut egui::Ui) {
        let panel = ui.max_rect();
        if ui.input(|i| i.pointer.any_pressed()) {
            self.browser.focus = ui.rect_contains_pointer(panel);
        }
        let mut todo: Vec<Do> = Vec::new();
        ui.horizontal(|ui| {
            ui.set_height(32.0);
            match self.browser.dir.clone() {
                Some(dir) => {
                    if icon_only_button(ui, Glyph::Back, Kind::Ghost, tr!("Volver a la biblioteca", "Back to the library", "Voltar à biblioteca")).clicked() {
                        todo.push(Do::Open(None));
                    }
                    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    ui.add(egui::Label::new(RichText::new(name).family(bold()).size(17.0).color(TEXT)).truncate());
                }
                None => {
                    ui.label(RichText::new(tr!("Biblioteca", "Library", "Biblioteca")).family(bold()).size(17.0).color(TEXT));
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let key = shortcuts::short(&self.prefs.keys, Act::Import);
                let tip = trf!("Agregar videos de cualquier carpeta ({key})", "Add videos from any folder ({key})", "Adicionar vídeos de qualquer pasta ({key})");
                if icon_button(ui, Glyph::Plus, tr!("Importar", "Import", "Importar"), Kind::Secondary, Vec2::ZERO).on_hover_text(tip).clicked() {
                    self.import_dialog();
                }
                if self.browser.dir.is_none() && icon_only_button(ui, Glyph::NewFolder, Kind::Ghost, new_folder()).clicked() {
                    todo.push(Do::Dialog(Dialog::Name { name: "Carpeta nueva".into(), files: Vec::new(), rename: None, error: None }));
                }
            });
        });
        ui.add_space(10.0);

        // Abajo, lo elegido y qué hacer con eso.
        if !self.browser.sel.is_empty() {
            egui::TopBottomPanel::bottom("lib_sel")
                .frame(egui::Frame::none().inner_margin(egui::Margin { top: 8.0, ..Default::default() }))
                .show_separator_line(false)
                .show_inside(ui, |ui| {
                    ui.horizontal(|ui| {
                        let n = self.browser.sel.len();
                        let c = fmt_count(n);
                        let picked = match n {
                            1 => tr!("1 elegido", "1 selected", "1 escolhido").to_string(),
                            _ => trf!("{c} elegidos", "{c} selected", "{c} escolhidos"),
                        };
                        ui.label(RichText::new(picked).size(13.0).color(MUTED));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icon_only_button(ui, Glyph::Trash, Kind::Ghost, tr!("Eliminar (Supr)", "Delete (Del)", "Excluir (Del)")).clicked() {
                                todo.push(Do::Dialog(Dialog::Delete(self.selected_assets())));
                            }
                            if icon_button(ui, Glyph::Plus, tr!("Agregar", "Add", "Adicionar"), Kind::Secondary, Vec2::ZERO).on_hover_text(add_label()).clicked() {
                                todo.push(Do::Add(self.selected_assets()));
                            }
                        });
                    });
                });
        }

        let nf = if self.browser.dir.is_none() { self.browser.folders.len() } else { 0 };
        if nf + self.assets.len() == 0 {
            ui.add_space(24.0);
            if self.browser.dir.is_some() {
                ui.label(RichText::new(tr!("Esta carpeta está vacía.", "This folder is empty.", "Esta pasta está vazia.")).color(TEXT));
                let hint = tr!(
                    "Para traer clips, elegilos en la biblioteca y usá «Mover a» con el clic derecho.",
                    "To bring clips here, select them in the library and use “Move to” with a right-click.",
                    "Para trazer clipes, escolha-os na biblioteca e use “Mover para” com o botão direito."
                );
                ui.label(RichText::new(hint).color(MUTED));
            } else {
                ui.label(RichText::new(tr!("No hay videos todavía.", "No videos yet.", "Ainda não há vídeos.")).color(TEXT));
                let hint = tr!(
                    "Guardá un clip mientras jugás, tocá Importar o arrastrá videos a la ventana.",
                    "Save a clip while you play, click Import or drag videos into the window.",
                    "Salve um clipe enquanto joga, clique em Importar ou arraste vídeos para a janela."
                );
                ui.label(RichText::new(hint).color(MUTED));
            }
        } else {
            self.library_grid(ui, &mut todo);
        }
        for d in todo {
            match d {
                Do::Add(paths) => {
                    for p in paths {
                        self.add_to_timeline(&p);
                    }
                }
                Do::Open(dir) => self.open_dir(dir),
                Do::Reveal(p) => {
                    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", p.display())).spawn();
                }
                Do::Move(paths, dir) => self.move_assets(paths, &dir),
                Do::Dialog(d) => self.browser.dialog = Some(d),
            }
        }
    }

    fn library_grid(&mut self, ui: &mut egui::Ui, todo: &mut Vec<Do>) {
        let nf = if self.browser.dir.is_none() { self.browser.folders.len() } else { 0 };
        let w = ((ui.available_width() - GAP) / 2.0).floor();
        let card = egui::vec2(w, (w * 9.0 / 16.0).round() + CARD_TEXT_H);
        let row_h = card.y + ROW_GAP;
        let n = nf + self.assets.len();
        let used: HashSet<&Path> = self.tracks.iter().flat_map(|t| t.clips.iter().map(|p| p.path.as_path())).collect();
        let mut sa = egui::ScrollArea::vertical().id_salt(("lib", &self.browser.dir)).auto_shrink([false, false]).drag_to_scroll(false);
        if let Some(y) = self.browser.set_scroll.take() {
            sa = sa.vertical_scroll_offset(y);
        }
        let b = &mut self.browser;
        let out = sa.show_viewport(ui, |ui, view| {
            ui.set_height(n.div_ceil(2) as f32 * row_h);
            let origin = ui.max_rect().min;
            let at = |k: usize| Rect::from_min_size(Pos2::new((k % 2) as f32 * (w + GAP), (k / 2) as f32 * row_h), card);
            // Fondo: arrastrar elige con un recuadro (también empezando sobre una tarjeta); un clic ahí no elige nada.
            let bg = ui.interact(view.translate(origin.to_vec2()), ui.id().with("lib_bg"), Sense::click_and_drag());
            let first = (view.top() / row_h).floor().max(0.0) as usize * 2;
            let last = ((view.bottom() / row_h).ceil() as usize * 2).min(n);
            for k in first.max(nf)..last {
                let a = &self.assets[k - nf];
                self.loader.want_cover(&a.path, (a.size, a.modified));
            }
            let info = self.loader.info.lock().unwrap();
            for k in first..last {
                let rect = at(k).translate(origin.to_vec2());
                if k < nf {
                    let f = &b.folders[k];
                    let r = folder_card(ui, rect, f);
                    if r.clicked() {
                        todo.push(Do::Open(Some(f.path.clone())));
                    }
                    r.context_menu(|ui| {
                        if ui.button(tr!("Abrir", "Open", "Abrir")).clicked() {
                            todo.push(Do::Open(Some(f.path.clone())));
                            ui.close_menu();
                        }
                        if ui.button(tr!("Cambiar el nombre…", "Rename…", "Renomear…")).clicked() {
                            todo.push(Do::Dialog(Dialog::Name { name: f.name.clone(), files: Vec::new(), rename: Some(f.path.clone()), error: None }));
                            ui.close_menu();
                        }
                        if ui.button(tr!("Abrir en el Explorador", "Open in File Explorer", "Abrir no Explorador")).clicked() {
                            open_folder(&f.path);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button(RichText::new(tr!("Eliminar carpeta…", "Delete folder…", "Excluir pasta…")).color(DANGER)).clicked() {
                            todo.push(Do::Dialog(Dialog::DeleteFolder(f.path.clone())));
                            ui.close_menu();
                        }
                    });
                    continue;
                }
                let ai = k - nf;
                let a = &self.assets[ai];
                let (r, add) = asset_card(ui, rect, a, info.get(&a.path), b.sel.contains(&a.path), used.contains(a.path.as_path()));
                if add {
                    todo.push(Do::Add(if b.sel.contains(&a.path) { self.assets.iter().filter(|x| b.sel.contains(&x.path)).map(|x| x.path.clone()).collect() } else { vec![a.path.clone()] }));
                } else if r.double_clicked() {
                    todo.push(Do::Add(vec![a.path.clone()]));
                } else if r.clicked() {
                    let m = ui.input(|i| i.modifiers);
                    match b.anchor.filter(|_| m.shift) {
                        Some(from) => {
                            let (lo, hi) = (from.min(ai), from.max(ai));
                            if !m.command {
                                b.sel.clear();
                            }
                            b.sel.extend(self.assets[lo..=hi].iter().map(|x| x.path.clone()));
                        }
                        None if m.command => {
                            if !b.sel.remove(&a.path) {
                                b.sel.insert(a.path.clone());
                            }
                            b.anchor = Some(ai);
                        }
                        None => {
                            b.sel = HashSet::from([a.path.clone()]);
                            b.anchor = Some(ai);
                        }
                    }
                }
                if r.secondary_clicked() && !b.sel.contains(&a.path) {
                    b.sel = HashSet::from([a.path.clone()]);
                    b.anchor = Some(ai);
                }
                r.context_menu(|ui| {
                    let paths: Vec<PathBuf> = self.assets.iter().filter(|x| b.sel.contains(&x.path)).map(|x| x.path.clone()).collect();
                    let n = paths.len();
                    let imported = self.assets.iter().filter(|x| b.sel.contains(&x.path) && x.imported).count();
                    let label = if n == 1 {
                        add_label().to_string()
                    } else {
                        let v = videos(n);
                        trf!("Agregar {v} a la línea de tiempo", "Add {v} to the timeline", "Adicionar {v} à linha do tempo")
                    };
                    if ui.button(label).clicked() {
                        todo.push(Do::Add(paths.clone()));
                        ui.close_menu();
                    }
                    if n == 1 && ui.button(tr!("Mostrar en el Explorador", "Show in File Explorer", "Mostrar no Explorador")).clicked() {
                        todo.push(Do::Reveal(paths[0].clone()));
                        ui.close_menu();
                    }
                    if imported == 0 {
                        ui.menu_button(tr!("Mover a", "Move to", "Mover para"), |ui| {
                            if b.dir.is_some() && ui.button(tr!("Biblioteca", "Library", "Biblioteca")).clicked() {
                                todo.push(Do::Move(paths.clone(), self.cfg.output_dir.clone()));
                                ui.close_menu();
                            }
                            for f in b.folders.iter().filter(|f| Some(&f.path) != b.dir.as_ref()) {
                                if ui.button(&f.name).clicked() {
                                    todo.push(Do::Move(paths.clone(), f.path.clone()));
                                    ui.close_menu();
                                }
                            }
                            ui.separator();
                            if ui.button(format!("{}…", new_folder())).clicked() {
                                todo.push(Do::Dialog(Dialog::Name { name: new_folder().into(), files: paths.clone(), rename: None, error: None }));
                                ui.close_menu();
                            }
                        });
                    }
                    ui.separator();
                    let del = if imported == n {
                        tr!("Quitar de la biblioteca", "Remove from library", "Remover da biblioteca")
                    } else {
                        tr!("Eliminar…", "Delete…", "Excluir…")
                    };
                    if ui.button(RichText::new(del).color(DANGER)).clicked() {
                        todo.push(Do::Dialog(Dialog::Delete(paths.clone())));
                        ui.close_menu();
                    }
                });
            }
            drop(info);

            // Recuadro de selección.
            let pointer = ui.input(|i| i.pointer.interact_pos());
            if bg.drag_started() {
                let start = ui.input(|i| i.pointer.press_origin()).unwrap_or_default() - origin.to_vec2();
                let keep = if ui.input(|i| i.modifiers.command) { b.sel.clone() } else { HashSet::new() };
                b.marquee = Some((start, keep));
            }
            if let (Some((start, keep)), Some(h), true) = (&b.marquee, pointer, bg.dragged()) {
                let r = Rect::from_two_pos(*start, h - origin.to_vec2());
                b.sel = keep.clone();
                b.sel.extend((nf..n).filter(|&k| at(k).intersects(r)).map(|k| self.assets[k - nf].path.clone()));
                let shown = r.translate(origin.to_vec2());
                ui.painter().rect_filled(shown, 2.0, WHITE.gamma_multiply(0.08));
                ui.painter().rect_stroke(shown, 2.0, Stroke::new(1.0_f32, WHITE.gamma_multiply(0.6)));
                // Cerca del borde (o afuera), la lista se desplaza sola.
                let screen = view.translate(origin.to_vec2());
                let push = ((h.y - (screen.bottom() - EDGE)).max(0.0) - (screen.top() + EDGE - h.y).max(0.0)) / EDGE;
                if push != 0.0 {
                    let dt = ui.input(|i| i.stable_dt).min(0.05);
                    b.set_scroll = Some((b.scroll + push.clamp(-1.0, 1.0) * AUTO_SCROLL * dt).max(0.0));
                    ui.ctx().request_repaint();
                }
            }
            if bg.drag_stopped() {
                b.marquee = None;
            }
            if bg.clicked() {
                b.sel.clear();
                b.anchor = None;
            }
        });
        self.browser.scroll = out.state.offset.y;
    }

    /// Hay un diálogo de la biblioteca abierto (los atajos esperan).
    pub(super) fn library_dialog_open(&self) -> bool {
        self.browser.dialog.is_some()
    }

    /// Supr con la biblioteca elegida: pedir confirmación para borrar lo elegido.
    pub(super) fn delete_library_selection(&mut self) -> bool {
        if !self.browser.focus || self.browser.sel.is_empty() {
            return false;
        }
        self.browser.dialog = Some(Dialog::Delete(self.selected_assets()));
        true
    }

    /// No se mueve ni se borra lo que está en la línea de tiempo mientras se exporta (se está leyendo).
    fn busy_with(&mut self, paths: &[PathBuf]) -> bool {
        let busy = self.job.is_some() && paths.iter().any(|p| self.is_used(p));
        if busy {
            let msg = tr!(
                "Esperá a que termine la exportación: ese clip se está usando.",
                "Wait for the export to finish: that clip is in use.",
                "Espere a exportação terminar: esse clipe está em uso."
            );
            self.message = Some((false, msg.into(), None));
        }
        busy
    }

    fn move_assets(&mut self, paths: Vec<PathBuf>, dir: &Path) {
        if self.busy_with(&paths) {
            return;
        }
        self.release_files(&paths);
        let mut failed = Vec::new();
        for p in &paths {
            match library::move_into(p, dir) {
                Ok(to) => self.moved_file(p, &to),
                Err(e) => {
                    crate::log::log(&format!("biblioteca: no se pudo mover {}: {e}", p.display()));
                    failed.push(library::pretty_name(p));
                }
            }
        }
        self.browser.sel.clear();
        self.scan();
        if !failed.is_empty() {
            let files = failed.join(", ");
            self.message = Some((false, trf!("No se pudo mover: {files}", "Couldn't move: {files}", "Não foi possível mover: {files}"), None));
        }
    }

    fn delete_assets(&mut self, paths: Vec<PathBuf>) {
        let (imported, own): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(|p| self.imported.contains(p));
        if self.busy_with(&own) {
            return;
        }
        self.imported.retain(|p| !imported.contains(p));
        library::save_imported(&self.imported);
        self.release_files(&own);
        let mut gone = Vec::new();
        let mut failed = Vec::new();
        for p in own {
            match library::recycle(&p) {
                Ok(()) => gone.push(p),
                Err(e) => {
                    crate::log::log(&format!("biblioteca: no se pudo eliminar {}: {e}", p.display()));
                    failed.push(library::pretty_name(&p));
                }
            }
        }
        self.forget_files(&gone);
        self.browser.sel.clear();
        self.scan();
        if !failed.is_empty() {
            let files = failed.join(", ");
            self.message = Some((false, trf!("No se pudo eliminar: {files}", "Couldn't delete: {files}", "Não foi possível excluir: {files}"), None));
        }
    }

    fn delete_folder(&mut self, dir: &Path) {
        let inside: Vec<PathBuf> = library::scan(dir, None, &[]).1.into_iter().map(|a| a.path).collect();
        if self.busy_with(&inside) {
            return;
        }
        self.release_files(&inside);
        match library::recycle(dir) {
            Ok(()) => {
                self.forget_files(&inside);
                if self.browser.dir.as_deref() == Some(dir) {
                    self.open_dir(None);
                }
                self.scan();
            }
            Err(e) => {
                crate::log::log(&format!("biblioteca: no se pudo eliminar {}: {e}", dir.display()));
                let msg = tr!(
                    "No se pudo eliminar la carpeta: algo de adentro está abierto en otro programa.",
                    "Couldn't delete the folder: something inside is open in another program.",
                    "Não foi possível excluir a pasta: algo dentro dela está aberto em outro programa."
                );
                self.message = Some((false, msg.into(), None));
            }
        }
    }

    /// Crea la carpeta (y mueve ahí `files`) o le cambia el nombre a `rename`. Devuelve el error para mostrar.
    fn name_folder(&mut self, name: &str, files: Vec<PathBuf>, rename: Option<PathBuf>) -> Result<(), String> {
        let name = name.trim();
        if !library::folder_name_ok(name) {
            return Err(tr!(
                "Ese nombre no sirve: no puede llevar < > : \" / \\ | ? * ni terminar en punto.",
                "That name won't work: it can't contain < > : \" / \\ | ? * or end with a period.",
                "Esse nome não serve: não pode ter < > : \" / \\ | ? * nem terminar com ponto."
            )
            .into());
        }
        let to = self.cfg.output_dir.join(name);
        if rename.as_deref() == Some(to.as_path()) {
            return Ok(());
        }
        if to.exists() {
            return Err(tr!("Ya hay una carpeta con ese nombre.", "There's already a folder with that name.", "Já existe uma pasta com esse nome.").into());
        }
        match rename {
            Some(from) => {
                let inside: Vec<PathBuf> = library::scan(&from, None, &[]).1.into_iter().map(|a| a.path).collect();
                if self.busy_with(&inside) {
                    return Ok(());
                }
                self.release_files(&inside);
                let moved = library::rename_folder(&from, &to).map_err(|_| {
                    tr!(
                        "No se pudo cambiar el nombre: algo de adentro está abierto en otro programa.",
                        "Couldn't rename: something inside is open in another program.",
                        "Não foi possível renomear: algo dentro está aberto em outro programa."
                    )
                    .to_string()
                })?;
                for (a, b) in moved {
                    self.moved_file(&a, &b);
                }
                if self.browser.dir.as_deref() == Some(from.as_path()) {
                    self.open_dir(Some(to));
                }
            }
            None => {
                std::fs::create_dir(&to).map_err(|e| trf!("No se pudo crear la carpeta: {e}", "Couldn't create the folder: {e}", "Não foi possível criar a pasta: {e}"))?;
                if !files.is_empty() {
                    self.move_assets(files, &to);
                }
            }
        }
        self.scan();
        Ok(())
    }

    /// Los diálogos de la biblioteca: confirmar antes de borrar y elegir el nombre de una carpeta.
    pub(super) fn library_dialogs(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.browser.dialog.take() else { return };
        // 0 = sigue abierto, 1 = cancelar, 2 = hacerlo.
        let mut action = 0;
        let closed = match &mut d {
            Dialog::Delete(paths) => {
                let n = paths.len();
                let imported = paths.iter().filter(|p| self.imported.contains(p)).count();
                let own = n - imported;
                let used = paths.iter().filter(|p| self.is_used(p)).count();
                let v = videos(n);
                let title = match (own, n) {
                    (0, 1) => tr!("¿Quitar este video de la biblioteca?", "Remove this video from the library?", "Remover este vídeo da biblioteca?").to_string(),
                    (0, _) => trf!("¿Quitar {v} de la biblioteca?", "Remove {v} from the library?", "Remover {v} da biblioteca?"),
                    (_, 1) => tr!("¿Eliminar este clip?", "Delete this clip?", "Excluir este clipe?").to_string(),
                    _ => trf!("¿Eliminar {v}?", "Delete {v}?", "Excluir {v}?"),
                };
                let mut lines = Vec::new();
                let c = fmt_count(own);
                match (own, own == n) {
                    (0, _) => {}
                    (1, true) => lines.push(tr!(
                        "Se manda a la Papelera de reciclaje: si te arrepentís, lo recuperás desde ahí.",
                        "It goes to the Recycle Bin: if you change your mind, you can restore it from there.",
                        "Vai para a Lixeira: se você se arrepender, pode recuperá-lo de lá."
                    )
                    .to_string()),
                    (1, false) => lines.push(tr!(
                        "1 se manda a la Papelera de reciclaje: si te arrepentís, lo recuperás desde ahí.",
                        "1 goes to the Recycle Bin: if you change your mind, you can restore it from there.",
                        "1 vai para a Lixeira: se você se arrepender, pode recuperá-lo de lá."
                    )
                    .to_string()),
                    (_, true) => lines.push(tr!(
                        "Se mandan a la Papelera de reciclaje: si te arrepentís, los recuperás desde ahí.",
                        "They go to the Recycle Bin: if you change your mind, you can restore them from there.",
                        "Vão para a Lixeira: se você se arrepender, pode recuperá-los de lá."
                    )
                    .to_string()),
                    (_, false) => lines.push(trf!(
                        "{c} se mandan a la Papelera de reciclaje: si te arrepentís, los recuperás desde ahí.",
                        "{c} go to the Recycle Bin: if you change your mind, you can restore them from there.",
                        "{c} vão para a Lixeira: se você se arrepender, pode recuperá-los de lá."
                    )),
                }
                if imported > 0 {
                    let c = fmt_count(imported);
                    lines.push(match imported {
                        1 => tr!(
                            "El importado solo sale de la lista: el archivo queda donde está.",
                            "The imported one only leaves the list: the file stays where it is.",
                            "O importado só sai da lista: o arquivo fica onde está."
                        )
                        .to_string(),
                        _ => trf!(
                            "Los {c} importados solo salen de la lista: los archivos quedan donde están.",
                            "The {c} imported ones only leave the list: the files stay where they are.",
                            "Os {c} importados só saem da lista: os arquivos ficam onde estão."
                        ),
                    });
                }
                if used > 0 && own > 0 {
                    lines.push(in_timeline(used, true));
                }
                let ok = if own == 0 { tr!("Quitar", "Remove", "Remover") } else { tr!("Eliminar", "Delete", "Excluir") };
                confirm(ctx, &title, &lines, ok, &mut action)
            }
            Dialog::DeleteFolder(dir) => {
                let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let inside = library::scan(dir, None, &[]).1;
                let used = inside.iter().filter(|a| self.is_used(&a.path)).count();
                let mut lines = vec![if inside.is_empty() {
                    tr!("Se manda a la Papelera de reciclaje.", "It goes to the Recycle Bin.", "Vai para a Lixeira.").to_string()
                } else {
                    let v = videos(inside.len());
                    trf!(
                        "Se manda a la Papelera de reciclaje con {v} adentro: si te arrepentís, la recuperás desde ahí.",
                        "It goes to the Recycle Bin with {v} inside: if you change your mind, you can restore it from there.",
                        "Vai para a Lixeira com {v} dentro: se você se arrepender, pode recuperá-la de lá."
                    )
                }];
                if used > 0 {
                    lines.push(in_timeline(used, false));
                }
                let title = trf!("¿Eliminar la carpeta «{name}»?", "Delete the folder “{name}”?", "Excluir a pasta “{name}”?");
                confirm(ctx, &title, &lines, tr!("Eliminar", "Delete", "Excluir"), &mut action)
            }
            Dialog::Name { name, rename, error, .. } => {
                let title = if rename.is_some() { tr!("Cambiar el nombre", "Rename", "Renomear") } else { new_folder() };
                let ok = if rename.is_some() { tr!("Guardar", "Save", "Salvar") } else { tr!("Crear", "Create", "Criar") };
                let sub = tr!("Se crea dentro de la carpeta de clips.", "It's created inside the clips folder.", "É criada dentro da pasta de clipes.");
                modal(ctx, "lib_name", title, sub, 460.0, |ui| {
                    modal_body(ui, |ui| {
                        let r = ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY).margin(egui::vec2(10.0, 7.0)).font(FontId::proportional(14.0)));
                        if !r.has_focus() && !r.lost_focus() {
                            r.request_focus();
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            action = 2;
                        }
                        if let Some(e) = error {
                            ui.label(RichText::new(e.as_str()).size(12.5).color(DANGER));
                        }
                    });
                    modal_footer(ui, |ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if button_sized(ui, ok, Kind::Primary, egui::vec2(120.0, 38.0)).clicked() {
                                action = 2;
                            }
                            if button_sized(ui, tr!("Cancelar", "Cancel", "Cancelar"), Kind::Secondary, egui::vec2(120.0, 38.0)).clicked() {
                                action = 1;
                            }
                        });
                    });
                })
            }
        };
        if closed && action == 0 {
            action = 1;
        }
        match (action, d) {
            (2, Dialog::Delete(paths)) => self.delete_assets(paths),
            (2, Dialog::DeleteFolder(dir)) => self.delete_folder(&dir),
            (2, Dialog::Name { name, files, rename, .. }) => {
                if let Err(e) = self.name_folder(&name, files.clone(), rename.clone()) {
                    self.browser.dialog = Some(Dialog::Name { name, files, rename, error: Some(e) });
                }
            }
            (0, d) => self.browser.dialog = Some(d),
            _ => {}
        }
    }
}

fn add_label() -> &'static str {
    tr!("Agregar a la línea de tiempo", "Add to the timeline", "Adicionar à linha do tempo")
}

fn new_folder() -> &'static str {
    tr!("Carpeta nueva", "New folder", "Nova pasta")
}

/// "Está en la línea de tiempo…" para `used` videos que se van a quitar de ahí (`these`: de los elegidos;
/// si no, de los de una carpeta).
fn in_timeline(used: usize, these: bool) -> String {
    let c = fmt_count(used);
    match (used, these) {
        (1, true) => tr!(
            "Está en la línea de tiempo: se va a quitar de ahí.",
            "It's on the timeline: it'll be removed from there.",
            "Está na linha do tempo: vai ser removido de lá."
        )
        .to_string(),
        (1, false) => tr!(
            "Uno está en la línea de tiempo: se va a quitar de ahí.",
            "One is on the timeline: it'll be removed from there.",
            "Um está na linha do tempo: vai ser removido de lá."
        )
        .to_string(),
        _ => trf!(
            "{c} están en la línea de tiempo: se van a quitar de ahí.",
            "{c} are on the timeline: they'll be removed from there.",
            "{c} estão na linha do tempo: vão ser removidos de lá."
        ),
    }
}

/// Diálogo para confirmar algo que borra: texto, Cancelar y el botón rojo. Devuelve si se cerró con la X o Esc.
fn confirm(ctx: &egui::Context, title: &str, lines: &[String], ok: &str, action: &mut i32) -> bool {
    modal(ctx, "lib_confirm", title, "", 500.0, |ui| {
        modal_body(ui, |ui| {
            for l in lines {
                ui.add(egui::Label::new(RichText::new(l).color(MUTED)).wrap());
            }
        });
        modal_footer(ui, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if button_sized(ui, ok, Kind::Danger, egui::vec2(120.0, 38.0)).clicked() {
                    *action = 2;
                }
                if button_sized(ui, tr!("Cancelar", "Cancel", "Cancelar"), Kind::Secondary, egui::vec2(120.0, 38.0)).clicked() {
                    *action = 1;
                }
            });
        });
    })
}
