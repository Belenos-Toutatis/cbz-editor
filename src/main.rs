// Éditeur CBZ — supprimer/réordonner/insérer des pages, recadrer une couverture, voir en grand.
// egui (GUI) + image (décodage/recadrage) + zip (lecture/écriture) + rfd (dialogue fichier).

use eframe::egui;
use egui::load::SizedTexture;
use egui::{
    Align2, Color32, ColorImage, CursorIcon, FontId, Id, Key, LayerId, Order, Pos2, Rect, RichText,
    Sense, Stroke, TextureHandle, TextureOptions, Vec2,
};
use std::io::{Cursor, Read, Write as _};
use std::path::{Path, PathBuf};

fn main() -> eframe::Result<()> {
    // Forcer X11/XWayland : le glisser-déposer de fichiers y est fiable.
    if std::env::var_os("DISPLAY").is_some() {
        std::env::remove_var("WAYLAND_DISPLAY");
    }
    let arg = std::env::args().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 820.0])
            .with_title("Éditeur CBZ — pages & couverture")
            .with_app_id("cbz-editor")
            .with_icon(load_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "Éditeur CBZ",
        options,
        Box::new(move |_cc| {
            let mut app = CbzApp::default();
            app.last_dir = load_last_dir();
            if let Some(p) = arg {
                app.open_cbz(p);
            }
            Ok(Box::new(app))
        }),
    )
}

struct Page {
    name: String,
    bytes: Vec<u8>,
    thumb: Option<TextureHandle>,
    failed: bool,
    delete: bool,
    uid: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum Handle {
    NW,
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
}

#[derive(Clone, Copy)]
enum DragKind {
    New,
    Move,
    Resize(Handle),
}

struct CropState {
    idx: usize,
    img: image::DynamicImage,
    tex: TextureHandle,
    sel: Option<[u32; 4]>,
    drag: Option<DragKind>,
    press: Pos2,
    sel_press: [u32; 4],
}

struct ViewerState {
    idx: usize,
    tex: Option<TextureHandle>,
}

struct SplitState {
    idx: usize,
    img: image::DynamicImage,
    tex: TextureHandle,
    cut: u32, // position de coupe en pixels image
    rtl: bool,
}

enum GridAction {
    Enlarge(usize),
    Crop(usize),
    MoveLeft(usize),
    MoveRight(usize),
    MoveStart(usize),
    MoveEnd(usize),
    InsertBefore(usize),
    InsertAfter(usize),
    MergeNext(usize),
    Split(usize),
    Extract(usize),
}

#[derive(Default)]
struct CbzApp {
    path: Option<PathBuf>,
    last_dir: Option<PathBuf>,
    comicinfo: Option<Vec<u8>>,
    pages: Vec<Page>,
    cover: Option<(Vec<u8>, Option<TextureHandle>)>,
    crop: Option<CropState>,
    viewer: Option<ViewerState>,
    split: Option<SplitState>,
    rtl: bool,
    generation: u64,
    next_uid: u64,
    dirty_order: bool,
    status: String,
    confirm_overwrite: bool,
    card_rects: Vec<(usize, Rect)>,
    drop_pos: Option<Pos2>,
}

impl CbzApp {
    fn open_cbz(&mut self, path: PathBuf) {
        match Self::read_cbz(&path) {
            Ok((mut pages, ci)) => {
                self.generation += 1;
                for (i, p) in pages.iter_mut().enumerate() {
                    p.uid = i as u64;
                }
                self.next_uid = pages.len() as u64;
                let n = pages.len();
                self.pages = pages;
                self.rtl = ci
                    .as_ref()
                    .map_or(true, |x| String::from_utf8_lossy(x).contains("RightToLeft"));
                self.comicinfo = ci;
                if let Some(parent) = path.parent() {
                    self.last_dir = Some(parent.to_path_buf());
                    save_last_dir(parent);
                }
                self.path = Some(path);
                self.cover = None;
                self.crop = None;
                self.viewer = None;
                self.split = None;
                self.dirty_order = false;
                self.confirm_overwrite = false;
                self.status = format!("Ouvert : {n} pages.");
            }
            Err(e) => self.status = format!("Erreur d'ouverture : {e}"),
        }
    }

    fn open_dialog(&mut self) {
        let mut dlg = rfd::FileDialog::new()
            .add_filter("Comic Book (cbz, zip)", &["cbz", "zip"])
            .set_title("Ouvrir un CBZ");
        if let Some(dir) = self.last_dir.as_ref().filter(|d| d.is_dir()) {
            dlg = dlg.set_directory(dir);
        } else if let Some(home) = std::env::var_os("HOME") {
            dlg = dlg.set_directory(home);
        }
        if let Some(path) = dlg.pick_file() {
            self.open_cbz(path);
        }
    }

    fn pick_image_page(&mut self) -> Option<Page> {
        let mut dlg = rfd::FileDialog::new()
            .add_filter("Images (jpg, png, webp, gif)", &["jpg", "jpeg", "png", "webp", "gif"])
            .set_title("Image à insérer");
        if let Some(d) = self.last_dir.as_ref().filter(|d| d.is_dir()) {
            dlg = dlg.set_directory(d);
        }
        let path = dlg.pick_file()?;
        let bytes = std::fs::read(&path).ok()?;
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image.jpg".into());
        let uid = self.next_uid;
        self.next_uid += 1;
        Some(Page {
            name,
            bytes,
            thumb: None,
            failed: false,
            delete: false,
            uid,
        })
    }

    // Extrait une page vers un fichier image sur le disque (utile p.ex. pour récupérer
    // la couverture du tome suivant rangée par erreur dans le mauvais tome).
    fn extract_page(&mut self, idx: usize) {
        let Some(page) = self.pages.get(idx) else { return };
        let ext = Path::new(&page.name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("jpg")
            .to_string();
        let stem = self
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image".into());
        let base = Path::new(&page.name)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("p{idx:03}.{ext}"));
        let default = format!("{stem} - {base}");
        let bytes = page.bytes.clone();
        let mut dlg = rfd::FileDialog::new()
            .set_title("Extraire l'image vers un fichier")
            .set_file_name(default)
            .add_filter("Image", &[ext]);
        if let Some(d) = self.last_dir.as_ref().filter(|d| d.is_dir()) {
            dlg = dlg.set_directory(d);
        }
        if let Some(out) = dlg.save_file() {
            match std::fs::write(&out, &bytes) {
                Ok(_) => {
                    self.status = format!(
                        "🖼 Image extraite : {}",
                        out.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
                    )
                }
                Err(e) => self.status = format!("Extraction impossible : {e}"),
            }
        }
    }

    fn read_cbz(path: &Path) -> Result<(Vec<Page>, Option<Vec<u8>>), String> {
        let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
        let mut pages = Vec::new();
        let mut ci = None;
        for i in 0..z.len() {
            let mut e = z.by_index(i).map_err(|e| e.to_string())?;
            if e.is_dir() {
                continue;
            }
            let name = e.name().to_string();
            let mut buf = Vec::new();
            e.read_to_end(&mut buf).map_err(|e| e.to_string())?;
            let lower = name.to_lowercase();
            if lower.ends_with(".xml") {
                ci = Some(buf);
            } else if lower.ends_with(".jpg")
                || lower.ends_with(".jpeg")
                || lower.ends_with(".png")
                || lower.ends_with(".webp")
                || lower.ends_with(".gif")
            {
                pages.push(Page {
                    name,
                    bytes: buf,
                    thumb: None,
                    failed: false,
                    delete: false,
                    uid: 0,
                });
            }
        }
        pages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok((pages, ci))
    }

    fn generate_thumbnails(&mut self, ctx: &egui::Context) {
        let mut budget = 6;
        let gen = self.generation;
        let mut pending = false;
        for page in self.pages.iter_mut() {
            if page.thumb.is_some() || page.failed {
                continue;
            }
            if budget == 0 {
                pending = true;
                break;
            }
            budget -= 1;
            match image::load_from_memory(&page.bytes) {
                Ok(img) => {
                    let t = img.thumbnail(220, 320);
                    let rgba = t.to_rgba8();
                    let ci = ColorImage::from_rgba_unmultiplied(
                        [t.width() as usize, t.height() as usize],
                        rgba.as_raw(),
                    );
                    page.thumb = Some(ctx.load_texture(
                        format!("thumb-{gen}-{}", page.uid),
                        ci,
                        TextureOptions::LINEAR,
                    ));
                }
                Err(_) => page.failed = true,
            }
        }
        if pending {
            ctx.request_repaint();
        }
    }

    fn open_viewer(&mut self, idx: usize) {
        self.viewer = Some(ViewerState { idx, tex: None });
    }

    fn viewer_go(&mut self, d: i32) {
        if let Some(vs) = self.viewer.as_mut() {
            let total = self.pages.len() as i32;
            if total == 0 {
                return;
            }
            let n = (vs.idx as i32 + d).clamp(0, total - 1);
            if n as usize != vs.idx {
                vs.idx = n as usize;
                vs.tex = None;
            }
        }
    }

    // Index d'insertion correspondant à une position écran, dans la grille (ordre de lecture).
    fn drop_index(&self, pos: Pos2) -> usize {
        for (i, r) in &self.card_rects {
            let later_row = r.top() > pos.y;
            let same_row = pos.y >= r.top() && pos.y <= r.bottom();
            if later_row || (same_row && pos.x <= r.center().x) {
                return *i;
            }
        }
        self.pages.len()
    }

    // Petite barre verticale marquant l'endroit où l'image sera insérée.
    fn insert_marker(&self, idx: usize) -> Option<Rect> {
        if let Some((_, r)) = self.card_rects.iter().find(|(i, _)| *i == idx) {
            Some(Rect::from_min_max(Pos2::new(r.left() - 4.0, r.top()), Pos2::new(r.left(), r.bottom())))
        } else {
            let (_, r) = self.card_rects.last()?;
            Some(Rect::from_min_max(Pos2::new(r.right(), r.top()), Pos2::new(r.right() + 4.0, r.bottom())))
        }
    }

    fn enter_crop(&mut self, idx: usize, ctx: &egui::Context) {
        let Some(page) = self.pages.get(idx) else { return };
        match image::load_from_memory(&page.bytes) {
            Ok(img) => {
                let disp = if img.width() > 1600 {
                    img.thumbnail(1600, 4000)
                } else {
                    img.clone()
                };
                let rgba = disp.to_rgba8();
                let ci = ColorImage::from_rgba_unmultiplied(
                    [disp.width() as usize, disp.height() as usize],
                    rgba.as_raw(),
                );
                let tex = ctx.load_texture("crop-disp", ci, TextureOptions::LINEAR);
                self.crop = Some(CropState {
                    idx,
                    img,
                    tex,
                    sel: None,
                    drag: None,
                    press: Pos2::ZERO,
                    sel_press: [0, 0, 0, 0],
                });
                self.status =
                    "Trace un rectangle, déplace-le ou ajuste ses poignées, puis « Valider ».".into();
            }
            Err(e) => self.status = format!("Impossible de décoder cette page : {e}"),
        }
    }

    fn finish_crop(&mut self, whole: bool, ctx: &egui::Context) {
        let Some(cs) = self.crop.take() else { return };
        let cropped = if whole {
            cs.img.clone()
        } else if let Some([x, y, w, h]) = cs.sel {
            cs.img.crop_imm(x, y, w, h)
        } else {
            self.status = "Aucune sélection — trace un rectangle ou clique « toute l'image ».".into();
            self.crop = Some(cs);
            return;
        };
        let bytes = encode_jpeg(&cropped, 92);
        let t = cropped.thumbnail(220, 320);
        let rgba = t.to_rgba8();
        let ci = ColorImage::from_rgba_unmultiplied(
            [t.width() as usize, t.height() as usize],
            rgba.as_raw(),
        );
        let tex = ctx.load_texture("cover-preview", ci, TextureOptions::LINEAR);
        self.cover = Some((bytes, Some(tex)));
        self.status = "Couverture recadrée prête ✔ — enregistre pour l'appliquer.".into();
    }

    fn merge_next(&mut self, i: usize) {
        if i + 1 >= self.pages.len() {
            self.status = "Pas de page suivante à fusionner.".into();
            return;
        }
        let a = match image::load_from_memory(&self.pages[i].bytes) {
            Ok(x) => x,
            Err(e) => {
                self.status = format!("Décodage page {} : {e}", i + 1);
                return;
            }
        };
        let b = match image::load_from_memory(&self.pages[i + 1].bytes) {
            Ok(x) => x,
            Err(e) => {
                self.status = format!("Décodage page {} : {e}", i + 2);
                return;
            }
        };
        // a est lue en 1er, b en 2e. En RTL : a à droite, b à gauche.
        let (left, right) = if self.rtl { (b, a) } else { (a, b) };
        let merged = concat_h(left, right);
        let bytes = encode_jpeg(&merged, 92);
        let uid = self.next_uid;
        self.next_uid += 1;
        self.pages[i] = Page {
            name: format!("merged-{uid}.jpg"),
            bytes,
            thumb: None,
            failed: false,
            delete: false,
            uid,
        };
        self.pages.remove(i + 1);
        self.dirty_order = true;
        self.status = "Pages fusionnées en double-page.".into();
    }

    fn enter_split(&mut self, idx: usize, ctx: &egui::Context) {
        let Some(page) = self.pages.get(idx) else { return };
        match image::load_from_memory(&page.bytes) {
            Ok(img) => {
                let disp = if img.width() > 1800 {
                    img.thumbnail(1800, 4000)
                } else {
                    img.clone()
                };
                let rgba = disp.to_rgba8();
                let ci = ColorImage::from_rgba_unmultiplied(
                    [disp.width() as usize, disp.height() as usize],
                    rgba.as_raw(),
                );
                let tex = ctx.load_texture("split-disp", ci, TextureOptions::LINEAR);
                let cut = img.width() / 2;
                self.split = Some(SplitState {
                    idx,
                    cut,
                    rtl: self.rtl,
                    img,
                    tex,
                });
                self.status = "Place la ligne de coupe (glisse), puis « Valider ».".into();
            }
            Err(e) => self.status = format!("Impossible de décoder : {e}"),
        }
    }

    fn finish_split(&mut self) {
        let Some(ss) = self.split.take() else { return };
        let w = ss.img.width();
        let cut = ss.cut.clamp(1, w.saturating_sub(1));
        let left = ss.img.crop_imm(0, 0, cut, ss.img.height());
        let right = ss.img.crop_imm(cut, 0, w - cut, ss.img.height());
        let (first, second) = if ss.rtl { (right, left) } else { (left, right) };
        let fb = encode_jpeg(&first, 92);
        let sb = encode_jpeg(&second, 92);
        let u1 = self.next_uid;
        let u2 = self.next_uid + 1;
        self.next_uid += 2;
        let p1 = Page {
            name: format!("split-{u1}.jpg"),
            bytes: fb,
            thumb: None,
            failed: false,
            delete: false,
            uid: u1,
        };
        let p2 = Page {
            name: format!("split-{u2}.jpg"),
            bytes: sb,
            thumb: None,
            failed: false,
            delete: false,
            uid: u2,
        };
        if ss.idx < self.pages.len() {
            self.pages.remove(ss.idx);
            self.pages.insert(ss.idx, p2);
            self.pages.insert(ss.idx, p1);
            self.dirty_order = true;
            self.status = "Page découpée en deux (ordre de lecture respecté).".into();
        }
    }

    fn split_ui(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        enum SAct {
            None,
            Cancel,
            Validate,
            Flip,
        }
        let mut sa = SAct::None;
        if let Some(ss) = self.split.as_mut() {
            let pname = self.pages.get(ss.idx).map(|p| p.name.clone()).unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("🪓 Découpe en deux — {pname}")).strong());
            });
            ui.label("Glisse pour déplacer la ligne de coupe. La 1ʳᵉ page lue est surlignée en vert.");
            ui.horizontal(|ui| {
                if ui.button(RichText::new("✅ Valider la découpe").color(Color32::from_rgb(120, 200, 120))).clicked() {
                    sa = SAct::Validate;
                }
                if ui.button("✖ Annuler").clicked() {
                    sa = SAct::Cancel;
                }
                let dir = if ss.rtl {
                    "droite → gauche (1ʳᵉ = droite)"
                } else {
                    "gauche → droite (1ʳᵉ = gauche)"
                };
                if ui.button(format!("⇄ Sens : {dir}")).on_hover_text("Inverser le sens de lecture").clicked() {
                    sa = SAct::Flip;
                }
                ui.label(format!("Coupe à x = {} / {}", ss.cut, ss.img.width()));
            });
            ui.separator();

            let iw = ss.img.width();
            let ih = ss.img.height();
            let avail = ui.available_size();
            let disp = fit([iw as usize, ih as usize], Vec2::new(avail.x.max(64.0), (avail.y - 6.0).max(64.0)));
            let (resp, painter) = ui.allocate_painter(disp, Sense::drag());
            let area = resp.rect;
            painter.image(ss.tex.id(), area, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);

            if resp.dragged() || resp.drag_started() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let fx = ((p.x - area.left()) / area.width().max(1.0) * iw as f32).round();
                    ss.cut = (fx as i32).clamp(1, iw as i32 - 1) as u32;
                }
            }
            if resp.hovered() {
                _ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
            }

            let cx = area.left() + ss.cut as f32 / iw as f32 * area.width();
            let left_rect = Rect::from_min_max(area.min, Pos2::new(cx, area.bottom()));
            let right_rect = Rect::from_min_max(Pos2::new(cx, area.top()), area.max);
            let (first_rect, second_rect) = if ss.rtl {
                (right_rect, left_rect)
            } else {
                (left_rect, right_rect)
            };
            painter.rect_filled(first_rect, 0.0, Color32::from_rgba_unmultiplied(0, 200, 80, 30));
            painter.rect_filled(second_rect, 0.0, Color32::from_rgba_unmultiplied(120, 120, 120, 30));
            painter.line_segment([Pos2::new(cx, area.top()), Pos2::new(cx, area.bottom())], Stroke::new(2.0, Color32::from_rgb(255, 70, 70)));
            painter.text(first_rect.center(), Align2::CENTER_CENTER, "1ʳᵉ", FontId::proportional(30.0), Color32::from_rgb(180, 255, 200));
            painter.text(second_rect.center(), Align2::CENTER_CENTER, "2ᵉ", FontId::proportional(30.0), Color32::from_rgb(225, 225, 225));
        }
        match sa {
            SAct::Cancel => {
                self.split = None;
                self.status.clear();
            }
            SAct::Flip => {
                if let Some(ss) = self.split.as_mut() {
                    ss.rtl = !ss.rtl;
                }
            }
            SAct::Validate => self.finish_split(),
            SAct::None => {}
        }
    }

    fn build_cbz(&self) -> Result<Vec<u8>, String> {
        let new_cover = self.cover.as_ref().map(|(b, _)| b.clone());
        let existing_cover = self
            .pages
            .iter()
            .find(|p| !p.delete && p.name.to_lowercase().contains("000_cover"))
            .map(|p| p.bytes.clone());
        let cover_bytes = new_cover.or(existing_cover);

        let content: Vec<&Page> = self
            .pages
            .iter()
            .filter(|p| !p.delete && !p.name.to_lowercase().contains("000_cover"))
            .collect();

        let n_images = content.len() + if cover_bytes.is_some() { 1 } else { 0 };

        let mut cur = Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut cur);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            if let Some(ci) = &self.comicinfo {
                let patched = update_pagecount(ci, n_images);
                zw.start_file("ComicInfo.xml", opts).map_err(|e| e.to_string())?;
                zw.write_all(&patched).map_err(|e| e.to_string())?;
            }
            if let Some(cover) = &cover_bytes {
                zw.start_file("000_cover.jpg", opts).map_err(|e| e.to_string())?;
                zw.write_all(cover).map_err(|e| e.to_string())?;
            }
            for (i, p) in content.iter().enumerate() {
                let name = if self.dirty_order {
                    let ext = Path::new(&p.name)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("jpg");
                    format!("p{:04}.{}", i, ext)
                } else {
                    p.name.clone()
                };
                zw.start_file(name, opts).map_err(|e| e.to_string())?;
                zw.write_all(&p.bytes).map_err(|e| e.to_string())?;
            }
            zw.finish().map_err(|e| e.to_string())?;
        }
        Ok(cur.into_inner())
    }

    fn save(&mut self, overwrite: bool) {
        let Some(path) = self.path.clone() else {
            self.status = "Aucun fichier ouvert.".into();
            return;
        };
        let data = match self.build_cbz() {
            Ok(d) => d,
            Err(e) => {
                self.status = format!("Erreur de construction : {e}");
                return;
            }
        };
        let kept = self.pages.iter().filter(|p| !p.delete).count();
        if overwrite {
            let mut bak = path.clone().into_os_string();
            bak.push(".bak");
            let bak = PathBuf::from(bak);
            if !bak.exists() {
                if let Err(e) = std::fs::copy(&path, &bak) {
                    self.status = format!("Sauvegarde .bak impossible : {e}");
                    return;
                }
            }
            let mut tmp = path.clone().into_os_string();
            tmp.push(".tmp");
            let tmp = PathBuf::from(tmp);
            if let Err(e) = std::fs::write(&tmp, &data) {
                self.status = format!("Écriture impossible : {e}");
                return;
            }
            if let Err(e) = std::fs::rename(&tmp, &path) {
                self.status = format!("Remplacement impossible : {e}");
                return;
            }
            self.status = format!("✅ Original écrasé ({kept} pages gardées). Sauvegarde .bak créée.");
        } else {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "sortie".into());
            let parent = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            let out = parent.join(format!("{stem} (édité).cbz"));
            if let Err(e) = std::fs::write(&out, &data) {
                self.status = format!("Écriture impossible : {e}");
                return;
            }
            self.status = format!(
                "✅ Copie enregistrée : {}",
                out.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
            );
        }
    }

    fn top_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button(RichText::new("📂 Ouvrir un CBZ…").size(15.0)).clicked() {
                self.open_dialog();
            }
            ui.separator();
            match &self.path {
                Some(p) => {
                    ui.label(RichText::new("📖").size(18.0));
                    ui.label(
                        RichText::new(p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default())
                            .strong(),
                    );
                }
                None => {
                    ui.label("Clique « Ouvrir un CBZ… » ou glisse un .cbz sur la fenêtre.");
                }
            }
        });
        if self.path.is_some() {
            ui.horizontal(|ui| {
                let total = self.pages.len();
                let del = self.pages.iter().filter(|p| p.delete).count();
                ui.label(format!("{total} pages"));
                if del > 0 {
                    ui.colored_label(Color32::from_rgb(220, 90, 90), format!("· {del} à supprimer"));
                }
                if self.dirty_order {
                    ui.colored_label(Color32::from_rgb(150, 150, 220), "· ordre modifié");
                }
                if self.cover.is_some() {
                    ui.colored_label(Color32::from_rgb(80, 180, 100), "· couverture ✔");
                }
                ui.separator();
                let dir = if self.rtl { "➡⬅ sens : D→G (manga)" } else { "⬅➡ sens : G→D" };
                if ui
                    .button(dir)
                    .on_hover_text("Sens de lecture, utilisé pour fusionner/découper les doubles-pages. Cliquer pour inverser.")
                    .clicked()
                {
                    self.rtl = !self.rtl;
                }
                ui.separator();
                if ui.button("💾 Enregistrer une copie").clicked() {
                    self.save(false);
                }
                if ui
                    .add(egui::Button::new(RichText::new("⚠ Écraser l'original").color(Color32::from_rgb(230, 160, 60))))
                    .on_hover_text("Remplace le .cbz d'origine (sauvegarde .bak automatique)")
                    .clicked()
                {
                    self.confirm_overwrite = true;
                }
                if let Some((_, Some(tex))) = &self.cover {
                    let sz = fit(tex.size(), Vec2::new(60.0, 60.0));
                    ui.image(SizedTexture::new(tex.id(), sz));
                }
            });
        }
        if !self.status.is_empty() {
            ui.label(RichText::new(&self.status).italics().weak());
        }
    }

    fn grid_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut action: Option<GridAction> = None;
        let mut rects: Vec<(usize, Rect)> = Vec::new();
        let n = self.pages.len();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (idx, page) in self.pages.iter_mut().enumerate() {
                    let card = ui.allocate_ui(Vec2::new(178.0, 252.0), |ui| {
                        egui::Frame::group(ui.style())
                            .fill(if page.delete {
                                Color32::from_rgb(60, 30, 30)
                            } else {
                                ui.style().visuals.faint_bg_color
                            })
                            .show(ui, |ui| {
                                ui.set_width(156.0);
                                ui.set_height(230.0);
                                ui.vertical_centered(|ui| {
                                    let thumb = match &page.thumb {
                                        Some(tex) => {
                                            let sz = fit(tex.size(), Vec2::new(150.0, 150.0));
                                            ui.add(egui::Image::new(SizedTexture::new(tex.id(), sz)).sense(Sense::click()))
                                        }
                                        None => ui.add_sized(Vec2::new(150.0, 150.0), egui::Spinner::new()),
                                    };
                                    if thumb.clicked() {
                                        action = Some(GridAction::Enlarge(idx));
                                    }
                                    thumb
                                        .on_hover_text("Clic = agrandir · clic droit = plus d'options")
                                        .context_menu(|ui| {
                                            if ui.button("🔍 Voir en grand").clicked() {
                                                action = Some(GridAction::Enlarge(idx));
                                                ui.close_menu();
                                            }
                                            ui.separator();
                                            if ui.button("🪓 Découper en deux pages…").clicked() {
                                                action = Some(GridAction::Split(idx));
                                                ui.close_menu();
                                            }
                                            if ui.add_enabled(idx + 1 < n, egui::Button::new("🔗 Fusionner avec la suivante")).clicked() {
                                                action = Some(GridAction::MergeNext(idx));
                                                ui.close_menu();
                                            }
                                            ui.separator();
                                            if ui.button("➕ Insérer une image AVANT").clicked() {
                                                action = Some(GridAction::InsertBefore(idx));
                                                ui.close_menu();
                                            }
                                            if ui.button("➕ Insérer une image APRÈS").clicked() {
                                                action = Some(GridAction::InsertAfter(idx));
                                                ui.close_menu();
                                            }
                                            ui.separator();
                                            if ui.button("⤒ Déplacer au début").clicked() {
                                                action = Some(GridAction::MoveStart(idx));
                                                ui.close_menu();
                                            }
                                            if ui.button("⤓ Déplacer à la fin").clicked() {
                                                action = Some(GridAction::MoveEnd(idx));
                                                ui.close_menu();
                                            }
                                            ui.separator();
                                            if ui.button("💾 Extraire l'image (fichier)…").clicked() {
                                                action = Some(GridAction::Extract(idx));
                                                ui.close_menu();
                                            }
                                        });
                                    ui.label(RichText::new(short_name(&page.name)).small());
                                    ui.horizontal(|ui| {
                                        let lbl = if page.delete { "↩" } else { "🗑" };
                                        if ui.button(lbl).on_hover_text(if page.delete { "Garder" } else { "Supprimer" }).clicked() {
                                            page.delete = !page.delete;
                                        }
                                        if ui.button("✂").on_hover_text("Recadrer en couverture").clicked() {
                                            action = Some(GridAction::Crop(idx));
                                        }
                                        if ui.add_enabled(idx > 0, egui::Button::new("◀")).on_hover_text("Déplacer avant").clicked() {
                                            action = Some(GridAction::MoveLeft(idx));
                                        }
                                        if ui.add_enabled(idx + 1 < n, egui::Button::new("▶")).on_hover_text("Déplacer après").clicked() {
                                            action = Some(GridAction::MoveRight(idx));
                                        }
                                    });
                                });
                            });
                    });
                    rects.push((idx, card.response.rect));
                }
            });
        });
        self.card_rects = rects;

        match action {
            Some(GridAction::Enlarge(i)) => self.open_viewer(i),
            Some(GridAction::Crop(i)) => self.enter_crop(i, ctx),
            Some(GridAction::MoveLeft(i)) if i > 0 => {
                self.pages.swap(i, i - 1);
                self.dirty_order = true;
            }
            Some(GridAction::MoveRight(i)) if i + 1 < self.pages.len() => {
                self.pages.swap(i, i + 1);
                self.dirty_order = true;
            }
            Some(GridAction::MoveStart(i)) if i < self.pages.len() => {
                let p = self.pages.remove(i);
                self.pages.insert(0, p);
                self.dirty_order = true;
            }
            Some(GridAction::MoveEnd(i)) if i < self.pages.len() => {
                let p = self.pages.remove(i);
                self.pages.push(p);
                self.dirty_order = true;
            }
            Some(GridAction::InsertBefore(i)) => {
                if let Some(pg) = self.pick_image_page() {
                    let at = i.min(self.pages.len());
                    self.pages.insert(at, pg);
                    self.dirty_order = true;
                    self.status = "Image insérée. (À l'enregistrement, les pages seront renumérotées dans le nouvel ordre.)".into();
                }
            }
            Some(GridAction::InsertAfter(i)) => {
                if let Some(pg) = self.pick_image_page() {
                    let at = (i + 1).min(self.pages.len());
                    self.pages.insert(at, pg);
                    self.dirty_order = true;
                    self.status = "Image insérée. (À l'enregistrement, les pages seront renumérotées dans le nouvel ordre.)".into();
                }
            }
            Some(GridAction::MergeNext(i)) => self.merge_next(i),
            Some(GridAction::Split(i)) => self.enter_split(i, ctx),
            Some(GridAction::Extract(i)) => self.extract_page(i),
            _ => {}
        }
    }

    fn viewer_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        enum VAct {
            None,
            Close,
            Prev,
            Next,
            Crop,
            ToggleDelete,
            Extract,
        }
        let mut va = VAct::None;
        if let Some(vs) = self.viewer.as_mut() {
            if vs.tex.is_none() {
                if let Some(page) = self.pages.get(vs.idx) {
                    vs.tex = decode_texture(ctx, &page.bytes, 2200);
                }
            }
            let idx = vs.idx;
            let total = self.pages.len();
            let (name, is_del) = self
                .pages
                .get(idx)
                .map(|p| (p.name.clone(), p.delete))
                .unwrap_or_default();
            ui.horizontal(|ui| {
                if ui.button("✖ Fermer (Échap)").clicked() {
                    va = VAct::Close;
                }
                ui.separator();
                if ui.add_enabled(idx > 0, egui::Button::new("◀ Précédente")).clicked() {
                    va = VAct::Prev;
                }
                ui.label(RichText::new(format!("{} / {}", idx + 1, total)).strong());
                if ui.add_enabled(idx + 1 < total, egui::Button::new("Suivante ▶")).clicked() {
                    va = VAct::Next;
                }
                ui.separator();
                ui.label(RichText::new(short_name(&name)).weak());
                if ui.selectable_label(is_del, if is_del { "↩ garder" } else { "🗑 supprimer" }).clicked() {
                    va = VAct::ToggleDelete;
                }
                if ui.button("✂ couverture").clicked() {
                    va = VAct::Crop;
                }
                if ui.button("💾 extraire").clicked() {
                    va = VAct::Extract;
                }
            });
            ui.separator();
            match &vs.tex {
                Some(tex) => {
                    let avail = ui.available_size();
                    let sz = fit(tex.size(), Vec2::new(avail.x, (avail.y - 4.0).max(64.0)));
                    ui.centered_and_justified(|ui| {
                        ui.image(SizedTexture::new(tex.id(), sz));
                    });
                }
                None => {
                    ui.centered_and_justified(|ui| {
                        ui.add(egui::Spinner::new());
                    });
                }
            }
        }
        match va {
            VAct::Close => self.viewer = None,
            VAct::Prev => self.viewer_go(-1),
            VAct::Next => self.viewer_go(1),
            VAct::ToggleDelete => {
                if let Some(vs) = self.viewer.as_ref() {
                    let i = vs.idx;
                    if let Some(p) = self.pages.get_mut(i) {
                        p.delete = !p.delete;
                    }
                }
            }
            VAct::Crop => {
                if let Some(vs) = self.viewer.take() {
                    self.enter_crop(vs.idx, ctx);
                }
            }
            VAct::Extract => {
                if let Some(vs) = self.viewer.as_ref() {
                    let i = vs.idx;
                    self.extract_page(i);
                }
            }
            VAct::None => {}
        }
    }

    fn crop_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        #[derive(PartialEq)]
        enum Act {
            None,
            Cancel,
            Validate,
            Whole,
        }
        let mut act = Act::None;
        if let Some(cs) = self.crop.as_mut() {
            let pname = self.pages.get(cs.idx).map(|p| p.name.clone()).unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("✂ Recadrage couverture — {pname}")).strong());
            });
            ui.label("Trace un rectangle, puis déplace-le (centre) ou ajuste les poignées (coins/bords).");
            ui.horizontal(|ui| {
                if ui.button(RichText::new("✅ Valider le recadrage").color(Color32::from_rgb(120, 200, 120))).clicked() {
                    act = Act::Validate;
                }
                if ui.button("🖼 Utiliser toute l'image").clicked() {
                    act = Act::Whole;
                }
                if ui.button("✖ Annuler").clicked() {
                    act = Act::Cancel;
                }
                if let Some([x, y, w, h]) = cs.sel {
                    ui.label(format!("Sélection : {w}×{h} px (coin {x},{y})"));
                }
            });
            ui.separator();

            let img_w = cs.img.width();
            let img_h = cs.img.height();
            let avail = ui.available_size();
            let disp = fit(
                [img_w as usize, img_h as usize],
                Vec2::new(avail.x.max(64.0), (avail.y - 6.0).max(64.0)),
            );
            let (resp, painter) = ui.allocate_painter(disp, Sense::drag());
            let area = resp.rect;
            painter.image(cs.tex.id(), area, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);

            let sel_screen = cs.sel.map(|s| img_to_screen(s, area, img_w as f32, img_h as f32));

            if cs.drag.is_none() {
                if let (Some(scr), Some(hp)) = (sel_screen, resp.hover_pos()) {
                    if let Some(h) = handle_at(scr, hp, 10.0) {
                        ctx.set_cursor_icon(resize_cursor(h));
                    } else if scr.contains(hp) {
                        ctx.set_cursor_icon(CursorIcon::Move);
                    }
                }
            }

            if resp.drag_started() {
                let p = resp.interact_pointer_pos().unwrap_or(area.center());
                cs.press = p;
                let kind = match sel_screen {
                    Some(scr) => {
                        if let Some(h) = handle_at(scr, p, 10.0) {
                            DragKind::Resize(h)
                        } else if scr.contains(p) {
                            DragKind::Move
                        } else {
                            DragKind::New
                        }
                    }
                    None => DragKind::New,
                };
                cs.drag = Some(kind);
                cs.sel_press = cs.sel.unwrap_or([0, 0, 0, 0]);
            }
            if resp.dragged() {
                if let (Some(kind), Some(cur)) = (cs.drag, resp.interact_pointer_pos()) {
                    let new_screen = match kind {
                        DragKind::New => Rect::from_two_pos(cs.press, cur),
                        DragKind::Move => {
                            let base = img_to_screen(cs.sel_press, area, img_w as f32, img_h as f32);
                            translate_clamped(base, cur - cs.press, area)
                        }
                        DragKind::Resize(h) => {
                            let base = img_to_screen(cs.sel_press, area, img_w as f32, img_h as f32);
                            apply_resize(base, h, cur, area)
                        }
                    }
                    .intersect(area);
                    if new_screen.width() > 3.0 && new_screen.height() > 3.0 {
                        cs.sel = Some(screen_to_img(new_screen, area, img_w, img_h));
                    }
                }
            }
            if resp.drag_stopped() {
                cs.drag = None;
            }

            if let Some(s) = cs.sel {
                let scr = img_to_screen(s, area, img_w as f32, img_h as f32);
                painter.rect_filled(scr, 0.0, Color32::from_rgba_unmultiplied(0, 130, 255, 25));
                painter.rect_stroke(scr, 0.0, Stroke::new(2.0, Color32::from_rgb(0, 160, 255)));
                for (_, hp) in handles(scr) {
                    let hr = Rect::from_center_size(hp, Vec2::splat(9.0));
                    painter.rect_filled(hr, 0.0, Color32::WHITE);
                    painter.rect_stroke(hr, 0.0, Stroke::new(1.0, Color32::from_rgb(0, 120, 200)));
                }
            }
        }
        match act {
            Act::Cancel => {
                self.crop = None;
                self.status.clear();
            }
            Act::Validate => self.finish_crop(false, ctx),
            Act::Whole => self.finish_crop(true, ctx),
            Act::None => {}
        }
    }
}

impl eframe::App for CbzApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // clavier : Échap = retour ; flèches = naviguer en mode agrandi
        let (esc, left, right) = ctx.input(|i| {
            (
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
            )
        });
        if esc {
            if self.viewer.is_some() {
                self.viewer = None;
            } else if self.split.is_some() {
                self.split = None;
                self.status.clear();
            } else if self.crop.is_some() {
                self.crop = None;
                self.status.clear();
            }
        } else if self.viewer.is_some() {
            if left {
                self.viewer_go(-1);
            }
            if right {
                self.viewer_go(1);
            }
        }

        // glisser-déposer : un .cbz s'ouvre, une image s'insère à la position du lâcher
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if hovering {
            if let Some(p) = ctx.input(|i| i.pointer.latest_pos()) {
                self.drop_pos = Some(p);
            }
        }
        let dropped: Vec<(Option<PathBuf>, Option<std::sync::Arc<[u8]>>, String)> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| (f.path.clone(), f.bytes.clone(), f.name.clone())).collect()
        });
        if !dropped.is_empty() {
            if let Some(p) = dropped.iter().find_map(|(p, _, _)| p.clone().filter(|p| is_cbz_path(p))) {
                self.open_cbz(p);
            } else if self.path.is_some() && self.crop.is_none() && self.split.is_none() && self.viewer.is_none() {
                let mut at = self.drop_pos.map(|pos| self.drop_index(pos)).unwrap_or(self.pages.len());
                let mut count = 0;
                for (path, bytes, name) in &dropped {
                    let ext = path
                        .as_ref()
                        .and_then(|p| p.extension())
                        .map(|e| e.to_string_lossy().to_lowercase())
                        .or_else(|| Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()))
                        .unwrap_or_default();
                    if !is_image_ext(&ext) {
                        continue;
                    }
                    let data = match path {
                        Some(p) => std::fs::read(p).ok(),
                        None => bytes.as_ref().map(|b| b.to_vec()),
                    };
                    if let Some(data) = data {
                        let uid = self.next_uid;
                        self.next_uid += 1;
                        let nm = path
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| name.clone());
                        let at_clamped = at.min(self.pages.len());
                        self.pages.insert(
                            at_clamped,
                            Page { name: nm, bytes: data, thumb: None, failed: false, delete: false, uid },
                        );
                        at = at_clamped + 1;
                        count += 1;
                    }
                }
                if count > 0 {
                    self.dirty_order = true;
                    self.status = format!("{count} image(s) insérée(s) — renumérotation à l'enregistrement.");
                }
            }
        }

        self.generate_thumbnails(ctx);

        egui::TopBottomPanel::top("top").show(ctx, |ui| self.top_ui(ui));

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.viewer.is_some() {
                self.viewer_ui(ui, ctx);
            } else if self.split.is_some() {
                self.split_ui(ui, ctx);
            } else if self.crop.is_some() {
                self.crop_ui(ui, ctx);
            } else if self.path.is_some() {
                self.grid_ui(ui, ctx);
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new("Glisse un .cbz ici, ou clique « 📂 Ouvrir un CBZ… »").size(20.0).weak());
                });
            }
        });

        if hovering {
            let p = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("dnd-overlay")));
            let in_grid = self.path.is_some() && self.crop.is_none() && self.split.is_none() && self.viewer.is_none();
            if in_grid {
                if let Some(pos) = self.drop_pos {
                    if let Some(m) = self.insert_marker(self.drop_index(pos)) {
                        p.rect_filled(m, 2.0, Color32::from_rgb(0, 180, 255));
                    }
                }
                let sr = ctx.screen_rect();
                p.text(
                    Pos2::new(sr.center().x, sr.top() + 92.0),
                    Align2::CENTER_CENTER,
                    "Lâcher pour insérer l'image ici",
                    FontId::proportional(20.0),
                    Color32::from_rgb(0, 180, 255),
                );
            } else {
                let r = ctx.screen_rect();
                p.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 130));
                p.text(r.center(), Align2::CENTER_CENTER, "Déposez un .cbz ici", FontId::proportional(30.0), Color32::WHITE);
            }
        }

        if self.confirm_overwrite {
            egui::Window::new("Confirmer l'écrasement")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("Le .cbz original va être remplacé.");
                    ui.label("Une sauvegarde « .cbz.bak » est créée automatiquement.");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("Oui, écraser").color(Color32::from_rgb(230, 160, 60))).clicked() {
                            self.confirm_overwrite = false;
                            self.save(true);
                        }
                        if ui.button("Annuler").clicked() {
                            self.confirm_overwrite = false;
                        }
                    });
                });
        }
    }
}

// ---------- géométrie du recadrage ----------

fn img_to_screen(sel: [u32; 4], area: Rect, img_w: f32, img_h: f32) -> Rect {
    let sx = area.width() / img_w.max(1.0);
    let sy = area.height() / img_h.max(1.0);
    Rect::from_min_size(
        Pos2::new(area.left() + sel[0] as f32 * sx, area.top() + sel[1] as f32 * sy),
        Vec2::new(sel[2] as f32 * sx, sel[3] as f32 * sy),
    )
}

fn screen_to_img(r: Rect, area: Rect, img_w: u32, img_h: u32) -> [u32; 4] {
    let sx = area.width() / img_w.max(1) as f32;
    let sy = area.height() / img_h.max(1) as f32;
    let r = r.intersect(area);
    let x = (((r.left() - area.left()) / sx).round().max(0.0)) as u32;
    let y = (((r.top() - area.top()) / sy).round().max(0.0)) as u32;
    let w = ((r.width() / sx).round() as u32).min(img_w.saturating_sub(x)).max(1);
    let h = ((r.height() / sy).round() as u32).min(img_h.saturating_sub(y)).max(1);
    [x, y, w, h]
}

fn handles(rect: Rect) -> [(Handle, Pos2); 8] {
    let c = rect.center();
    [
        (Handle::NW, rect.left_top()),
        (Handle::N, Pos2::new(c.x, rect.top())),
        (Handle::NE, rect.right_top()),
        (Handle::E, Pos2::new(rect.right(), c.y)),
        (Handle::SE, rect.right_bottom()),
        (Handle::S, Pos2::new(c.x, rect.bottom())),
        (Handle::SW, rect.left_bottom()),
        (Handle::W, Pos2::new(rect.left(), c.y)),
    ]
}

fn handle_at(rect: Rect, p: Pos2, tol: f32) -> Option<Handle> {
    handles(rect)
        .into_iter()
        .map(|(h, hp)| (h, hp.distance(p)))
        .filter(|(_, d)| *d <= tol)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(h, _)| h)
}

fn apply_resize(base: Rect, h: Handle, p: Pos2, area: Rect) -> Rect {
    let (mut l, mut t, mut r, mut b) = (base.left(), base.top(), base.right(), base.bottom());
    match h {
        Handle::NW => {
            l = p.x;
            t = p.y;
        }
        Handle::N => t = p.y,
        Handle::NE => {
            r = p.x;
            t = p.y;
        }
        Handle::E => r = p.x,
        Handle::SE => {
            r = p.x;
            b = p.y;
        }
        Handle::S => b = p.y,
        Handle::SW => {
            l = p.x;
            b = p.y;
        }
        Handle::W => l = p.x,
    }
    Rect::from_min_max(Pos2::new(l.min(r), t.min(b)), Pos2::new(l.max(r), t.max(b))).intersect(area)
}

fn translate_clamped(base: Rect, delta: Vec2, area: Rect) -> Rect {
    let r = base.translate(delta);
    let dx = if r.left() < area.left() {
        area.left() - r.left()
    } else if r.right() > area.right() {
        area.right() - r.right()
    } else {
        0.0
    };
    let dy = if r.top() < area.top() {
        area.top() - r.top()
    } else if r.bottom() > area.bottom() {
        area.bottom() - r.bottom()
    } else {
        0.0
    };
    r.translate(Vec2::new(dx, dy))
}

fn resize_cursor(h: Handle) -> CursorIcon {
    match h {
        Handle::NW | Handle::SE => CursorIcon::ResizeNwSe,
        Handle::NE | Handle::SW => CursorIcon::ResizeNeSw,
        Handle::N | Handle::S => CursorIcon::ResizeVertical,
        Handle::E | Handle::W => CursorIcon::ResizeHorizontal,
    }
}

// ---------- utilitaires ----------

fn decode_texture(ctx: &egui::Context, bytes: &[u8], max: u32) -> Option<TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width() > max || img.height() > max {
        img.thumbnail(max, max)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let ci = ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], rgba.as_raw());
    Some(ctx.load_texture("viewer", ci, TextureOptions::LINEAR))
}

fn fit(size: [usize; 2], max: Vec2) -> Vec2 {
    let (w, h) = (size[0] as f32, size[1] as f32);
    if w <= 0.0 || h <= 0.0 {
        return max;
    }
    let s = (max.x / w).min(max.y / h);
    Vec2::new(w * s, h * s)
}

fn is_cbz_path(p: &Path) -> bool {
    p.extension().map_or(false, |e| {
        let e = e.to_string_lossy().to_lowercase();
        e == "cbz" || e == "zip"
    })
}

fn is_image_ext(ext: &str) -> bool {
    matches!(ext, "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp")
}

fn short_name(name: &str) -> String {
    let base = name.rsplit('/').next().unwrap_or(name);
    if base.chars().count() > 20 {
        let tail: String = base.chars().rev().take(19).collect::<Vec<_>>().into_iter().rev().collect();
        format!("…{tail}")
    } else {
        base.to_string()
    }
}

fn encode_jpeg(img: &image::DynamicImage, quality: u8) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    let rgb = img.to_rgb8();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    let _ = enc.encode_image(&rgb);
    out.into_inner()
}

fn to_height(img: image::DynamicImage, h: u32) -> image::DynamicImage {
    if img.height() == h || img.height() == 0 {
        img
    } else {
        let w = ((img.width() as u64 * h as u64) / img.height() as u64).max(1) as u32;
        img.resize_exact(w, h, image::imageops::FilterType::Lanczos3)
    }
}

// Concatène deux images côte à côte (gauche | droite) à hauteur commune.
fn concat_h(left: image::DynamicImage, right: image::DynamicImage) -> image::DynamicImage {
    let h = left.height().max(right.height()).max(1);
    let l = to_height(left, h).to_rgb8();
    let r = to_height(right, h).to_rgb8();
    let w = l.width() + r.width();
    let mut canvas = image::RgbImage::new(w, h);
    image::imageops::overlay(&mut canvas, &l, 0, 0);
    image::imageops::overlay(&mut canvas, &r, l.width() as i64, 0);
    image::DynamicImage::ImageRgb8(canvas)
}

fn update_pagecount(xml: &[u8], n: usize) -> Vec<u8> {
    let s = String::from_utf8_lossy(xml);
    if let (Some(a), Some(b)) = (s.find("<PageCount>"), s.find("</PageCount>")) {
        if b > a {
            let mut out = String::with_capacity(s.len());
            out.push_str(&s[..a + "<PageCount>".len()]);
            out.push_str(&n.to_string());
            out.push_str(&s[b..]);
            return out.into_bytes();
        }
    }
    xml.to_vec()
}

fn config_file() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(dir.join("cbz-editor").join("last_dir"))
}

fn load_last_dir() -> Option<PathBuf> {
    let s = std::fs::read_to_string(config_file()?).ok()?;
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(PathBuf::from(s))
    }
}

fn save_last_dir(dir: &Path) {
    if let Some(p) = config_file() {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(p, dir.to_string_lossy().as_bytes());
    }
}

fn load_icon() -> egui::IconData {
    let bytes = include_bytes!("../assets/icon-256.png");
    match image::load_from_memory(bytes) {
        Ok(img) => {
            let img = img.to_rgba8();
            let (w, h) = img.dimensions();
            egui::IconData {
                rgba: img.into_raw(),
                width: w,
                height: h,
            }
        }
        Err(_) => egui::IconData {
            rgba: vec![0, 0, 0, 0],
            width: 1,
            height: 1,
        },
    }
}
