// Éditeur CBZ — supprimer/réordonner/insérer des pages, recadrer une couverture, voir en grand.
// egui (GUI) + image (décodage/recadrage) + zip (lecture/écriture) + rfd (dialogue fichier).

use eframe::egui;
use egui::load::SizedTexture;
use egui::{
    Align2, Color32, ColorImage, CursorIcon, FontId, Id, Key, LayerId, Order, Pos2, Rect, RichText,
    Sense, Stroke, TextureHandle, TextureOptions, Vec2,
};
use std::collections::HashSet;
use std::io::{Cursor, Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::mpsc::TryRecvError;
use std::sync::{mpsc, Arc, Mutex};

// Durée d'affichage du petit message éphémère (« toast ») après un dépôt d'image.
const TOAST_SECS: f64 = 2.0;
// Nombre maximal d'étapes mémorisées pour Défaire/Refaire.
const UNDO_MAX: usize = 100;
// Taille maximale (px) des miniatures de la grille.
const THUMB_W: u32 = 220;
const THUMB_H: u32 = 320;
// Taille maximale (px) d'une page affichée en grand.
const VIEWER_MAX: u32 = 2200;

fn main() -> eframe::Result<()> {
    // Forcer X11/XWayland : le glisser-déposer de fichiers y est fiable.
    if std::env::var_os("DISPLAY").is_some() {
        std::env::remove_var("WAYLAND_DISPLAY");
    }
    let arg = std::env::args().nth(1).map(PathBuf::from);
    // Langue : choix persisté sinon déduit de la locale du système.
    let lang = load_lang().unwrap_or_else(Lang::detect);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 820.0])
            .with_min_inner_size([760.0, 480.0])
            .with_title(tr(lang, T::WindowTitle))
            .with_app_id("cbz-editor")
            .with_icon(load_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "cbz-editor",
        options,
        Box::new(move |cc| {
            let mut app = CbzApp {
                lang,
                last_dir: load_last_dir(),
                thumbs: Some(ThumbWorkers::start(cc.egui_ctx.clone())),
                egui_ctx: Some(cc.egui_ctx.clone()),
                ..Default::default()
            };
            if let Some(p) = arg {
                app.open_cbz(p);
            }
            Ok(Box::new(app))
        }),
    )
}

// Les octets sont partagés (`Arc`) : cloner une page (historique Défaire, envoi au
// décodeur de miniatures) ne recopie jamais l'image.
#[derive(Clone)]
struct Page {
    name: String,
    bytes: Arc<[u8]>,
    thumb: Option<TextureHandle>,
    failed: bool,
    delete: bool,
    uid: u64,
}

impl Page {
    fn new(name: String, bytes: Arc<[u8]>, uid: u64) -> Page {
        Page {
            name,
            bytes,
            thumb: None,
            failed: false,
            delete: false,
            uid,
        }
    }
}

// Couverture recadrée en attente d'enregistrement (`id` unique : sert à l'empreinte
// des modifications).
#[derive(Clone)]
struct Cover {
    bytes: Arc<[u8]>,
    ext: &'static str, // « png » si la source était sans perte, sinon « jpg »
    tex: TextureHandle,
    id: u64,
}

// État restaurable par Défaire/Refaire.
#[derive(Clone)]
struct Snapshot {
    pages: Vec<Page>,
    cover: Option<Cover>,
    dirty_order: bool,
}

// Action différée derrière la confirmation « modifications non enregistrées ».
enum Pending {
    Open(Option<PathBuf>),
    Quit,
}

// ---------- décodage des miniatures en arrière-plan ----------
// Le décodage d'une page (JPEG de plusieurs Mpx) peut prendre des dizaines de ms :
// fait sur le thread de l'UI, il figeait la fenêtre à l'ouverture. Des threads
// dédiés décodent ; l'UI ne fait que téléverser la texture (rapide).
struct ThumbJob {
    gen: u64,
    uid: u64,
    bytes: Arc<[u8]>,
}

struct ThumbDone {
    gen: u64,
    uid: u64,
    img: Option<ColorImage>,
}

struct ThumbWorkers {
    jobs: mpsc::Sender<ThumbJob>,
    done: mpsc::Receiver<ThumbDone>,
    // Génération courante : les travaux d'un fichier précédent sont ignorés.
    gen: Arc<AtomicU64>,
}

impl ThumbWorkers {
    fn start(ctx: egui::Context) -> ThumbWorkers {
        let (job_tx, job_rx) = mpsc::channel::<ThumbJob>();
        let (done_tx, done_rx) = mpsc::channel::<ThumbDone>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let gen = Arc::new(AtomicU64::new(0));
        let n = std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .clamp(1, 4);
        for _ in 0..n {
            let (job_rx, done_tx, ctx, gen) =
                (job_rx.clone(), done_tx.clone(), ctx.clone(), gen.clone());
            std::thread::spawn(move || loop {
                let job = {
                    let Ok(rx) = job_rx.lock() else { return };
                    match rx.recv() {
                        Ok(j) => j,
                        Err(_) => return,
                    }
                };
                if job.gen != gen.load(AtomicOrdering::Relaxed) {
                    continue;
                }
                let img = image::load_from_memory(&job.bytes).ok().map(|img| {
                    let t = img.thumbnail(THUMB_W, THUMB_H);
                    let rgba = t.to_rgba8();
                    ColorImage::from_rgba_unmultiplied(
                        [t.width() as usize, t.height() as usize],
                        rgba.as_raw(),
                    )
                });
                if done_tx
                    .send(ThumbDone {
                        gen: job.gen,
                        uid: job.uid,
                        img,
                    })
                    .is_err()
                {
                    return;
                }
                ctx.request_repaint();
            });
        }
        ThumbWorkers {
            jobs: job_tx,
            done: done_rx,
            gen,
        }
    }
}

// ---------- palette ----------
// Couleurs centralisées, déclinées pour les thèmes sombre et clair (egui suit le
// thème du système) : une seule couleur d'accent pour tous les repères.
#[derive(Clone, Copy)]
struct Pal {
    accent: Color32,
    ok: Color32,
    warn: Color32,
    danger: Color32,
    info: Color32,
    del_fill: Color32,
    drag_fill: Color32,
    bad_fill: Color32,
    bad_text: Color32,
}

fn pal(ctx: &egui::Context) -> Pal {
    if ctx.style().visuals.dark_mode {
        Pal {
            accent: Color32::from_rgb(0, 170, 255),
            ok: Color32::from_rgb(110, 200, 120),
            warn: Color32::from_rgb(230, 160, 60),
            danger: Color32::from_rgb(230, 100, 100),
            info: Color32::from_rgb(150, 150, 220),
            del_fill: Color32::from_rgb(66, 30, 30),
            drag_fill: Color32::from_rgb(28, 60, 42),
            bad_fill: Color32::from_rgb(54, 40, 40),
            bad_text: Color32::from_rgb(210, 170, 170),
        }
    } else {
        Pal {
            accent: Color32::from_rgb(0, 120, 215),
            ok: Color32::from_rgb(30, 130, 50),
            warn: Color32::from_rgb(185, 105, 0),
            danger: Color32::from_rgb(190, 40, 40),
            info: Color32::from_rgb(70, 70, 170),
            del_fill: Color32::from_rgb(248, 214, 214),
            drag_fill: Color32::from_rgb(205, 240, 215),
            bad_fill: Color32::from_rgb(240, 225, 225),
            bad_text: Color32::from_rgb(150, 60, 60),
        }
    }
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
    lossless: bool,
    tex: TextureHandle,
    sel: Option<[u32; 4]>,
    drag: Option<DragKind>,
    press: Pos2,
    sel_press: [u32; 4],
}

struct ViewerState {
    idx: usize,
    tex: Option<TextureHandle>, // page courante (recalculée par `pump`)
    failed: bool,
    zoom: bool, // taille réelle (sinon : ajusté à la fenêtre)
    // Pages décodées (courante, suivante, précédente) par uid ; None = illisible.
    cache: Vec<(u64, Option<TextureHandle>)>,
    jobs: Vec<(u64, mpsc::Receiver<Option<ColorImage>>)>,
}

impl ViewerState {
    // Décode en arrière-plan la page courante PUIS ses voisines (feuilletage instantané),
    // et ne garde en mémoire que ces trois pages.
    fn pump(&mut self, ctx: &egui::Context, pages: &[Page]) {
        let cur = pages.get(self.idx).map(|p| p.uid);
        let wanted: Vec<u64> = [
            Some(self.idx),
            self.idx.checked_add(1),
            self.idx.checked_sub(1),
        ]
        .into_iter()
        .flatten()
        .filter_map(|i| pages.get(i).map(|p| p.uid))
        .collect();
        let mut i = 0;
        while i < self.jobs.len() {
            match self.jobs[i].1.try_recv() {
                Ok(img) => {
                    let uid = self.jobs.remove(i).0;
                    let tex = img.map(|ci| {
                        ctx.load_texture(format!("viewer-{uid}"), ci, TextureOptions::LINEAR)
                    });
                    self.cache.push((uid, tex));
                }
                Err(TryRecvError::Disconnected) => {
                    let uid = self.jobs.remove(i).0;
                    self.cache.push((uid, None));
                }
                Err(TryRecvError::Empty) => i += 1,
            }
        }
        self.cache.retain(|(u, _)| wanted.contains(u));
        self.jobs.retain(|(u, _)| wanted.contains(u));
        for &uid in &wanted {
            if self.cache.iter().any(|c| c.0 == uid) || self.jobs.iter().any(|j| j.0 == uid) {
                continue;
            }
            if let Some(p) = pages.iter().find(|p| p.uid == uid) {
                let bytes = p.bytes.clone();
                let rx = spawn_job(Some(ctx.clone()), move || {
                    decode_color_image(&bytes, VIEWER_MAX)
                });
                self.jobs.push((uid, rx));
            }
        }
        match cur.and_then(|u| self.cache.iter().find(|c| c.0 == u)) {
            Some((_, t)) => {
                self.tex = t.clone();
                self.failed = t.is_none();
            }
            None => {
                self.tex = None;
                self.failed = false;
            }
        }
    }
}

// Résultat de la lecture d'une archive : pages + ComicInfo éventuel.
type ReadResult = Result<(Vec<Page>, Option<Vec<u8>>), String>;

// Ouverture d'un .cbz en cours : lue dans un thread pour ne pas figer la fenêtre
// (une archive de plusieurs centaines de Mo prend quelques secondes).
struct Opening {
    path: PathBuf,
    rx: mpsc::Receiver<ReadResult>,
    done: Arc<AtomicU64>,  // entrées lues…
    total: Arc<AtomicU64>, // …sur le nombre d'entrées de l'archive
}

struct SplitState {
    idx: usize,
    img: image::DynamicImage,
    lossless: bool,
    tex: TextureHandle,
    cut: u32, // position de coupe en pixels image
    rtl: bool,
}

// Résultat de `build_cbz` : octets de l'archive + (nom écrit, uid source) par image.
struct Built {
    data: Vec<u8>,
    entries: Vec<(String, Option<u64>)>,
    comicinfo: Option<Vec<u8>>,
}

enum GridAction {
    ToggleDelete(usize),
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
    MarkIdentical(usize),
}

#[derive(Default)]
struct CbzApp {
    path: Option<PathBuf>,
    last_dir: Option<PathBuf>,
    comicinfo: Option<Vec<u8>>,
    pages: Vec<Page>,
    cover: Option<Cover>,
    crop: Option<CropState>,
    viewer: Option<ViewerState>,
    split: Option<SplitState>,
    rtl: bool,
    generation: u64,
    next_uid: u64,
    dirty_order: bool,
    status: String,
    status_err: bool,
    confirm_overwrite: bool,
    confirm_discard: Option<Pending>,
    allow_close: bool,
    saved_sig: u64,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    thumbs: Option<ThumbWorkers>,
    egui_ctx: Option<egui::Context>,
    opening: Option<Opening>,
    thumb_pending: HashSet<u64>,
    window_title: String,
    card_rects: Vec<(usize, Rect)>,
    drag_page: Option<u64>,
    toast: Option<(String, f64)>,
    lang: Lang,
}

impl CbzApp {
    // ---------- barre d'état ----------
    fn info(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_err = false;
    }

    fn fail(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_err = true;
    }

    // ---------- modifications / Défaire ----------
    // Empreinte de l'état éditable (ordre, pages cochées, couverture) : comparée à
    // celle du dernier enregistrement pour savoir s'il reste des modifications.
    fn edit_sig(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for p in &self.pages {
            (p.uid, p.delete).hash(&mut h);
        }
        self.cover.as_ref().map(|c| c.id).hash(&mut h);
        h.finish()
    }

    fn is_modified(&self) -> bool {
        self.path.is_some() && self.edit_sig() != self.saved_sig
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            pages: self.pages.clone(),
            cover: self.cover.clone(),
            dirty_order: self.dirty_order,
        }
    }

    // À appeler juste AVANT chaque modification (mémorise l'état pour Défaire).
    fn checkpoint(&mut self) {
        self.undo.push(self.snapshot());
        if self.undo.len() > UNDO_MAX {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn restore(&mut self, s: Snapshot) {
        self.pages = s.pages;
        self.cover = s.cover;
        self.dirty_order = s.dirty_order;
        if let Some(vs) = self.viewer.as_mut() {
            vs.idx = vs.idx.min(self.pages.len().saturating_sub(1));
            vs.tex = None;
            vs.failed = false;
        }
    }

    fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            self.redo.push(self.snapshot());
            self.restore(s);
            self.info(tr(self.lang, T::Undone));
        }
    }

    fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.undo.push(self.snapshot());
            self.restore(s);
            self.info(tr(self.lang, T::Redone));
        }
    }

    fn new_uid(&mut self) -> u64 {
        let u = self.next_uid;
        self.next_uid += 1;
        u
    }

    // ---------- ouverture ----------
    // Ouvre un fichier (ou le dialogue si `None`) après confirmation s'il reste
    // des modifications non enregistrées.
    fn request_open(&mut self, path: Option<PathBuf>) {
        if self.opening.is_some() {
            return;
        }
        if self.is_modified() {
            self.confirm_discard = Some(Pending::Open(path));
        } else {
            self.do_open(path);
        }
    }

    fn do_open(&mut self, path: Option<PathBuf>) {
        match path {
            Some(p) => self.open_cbz(p),
            None => self.open_dialog(),
        }
    }

    // Lance la lecture en arrière-plan ; `poll_opening` applique le résultat.
    fn open_cbz(&mut self, path: PathBuf) {
        if self.opening.is_some() {
            return;
        }
        let lang = self.lang;
        let (done, total) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let (d, t, p, ctx) = (
            done.clone(),
            total.clone(),
            path.clone(),
            self.egui_ctx.clone(),
        );
        let rx = spawn_job(self.egui_ctx.clone(), move || {
            Self::read_cbz(&p, lang, &|i, n| {
                d.store(i, AtomicOrdering::Relaxed);
                t.store(n, AtomicOrdering::Relaxed);
                if let Some(c) = &ctx {
                    c.request_repaint();
                }
            })
        });
        self.opening = Some(Opening {
            path,
            rx,
            done,
            total,
        });
    }

    fn poll_opening(&mut self) {
        let Some(op) = &self.opening else { return };
        let res = match op.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(tr(self.lang, T::Interrupted).to_string()),
        };
        if let Some(op) = self.opening.take() {
            self.apply_open(op.path, res);
        }
    }

    fn apply_open(&mut self, path: PathBuf, res: ReadResult) {
        let lang = self.lang;
        match res {
            Ok((mut pages, ci)) => {
                self.generation += 1;
                if let Some(t) = &self.thumbs {
                    t.gen.store(self.generation, AtomicOrdering::Relaxed);
                }
                self.thumb_pending.clear();
                for (i, p) in pages.iter_mut().enumerate() {
                    p.uid = i as u64;
                }
                self.next_uid = pages.len() as u64;
                let n = pages.len();
                self.pages = pages;
                self.rtl = ci
                    .as_ref()
                    .is_none_or(|x| String::from_utf8_lossy(x).contains("RightToLeft"));
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
                self.undo.clear();
                self.redo.clear();
                self.saved_sig = self.edit_sig();
                self.info(tr(lang, T::OpenedNPages).replace("{n}", &n.to_string()));
            }
            Err(e) => self.fail(tr(lang, T::OpenError).replace("{e}", &e)),
        }
    }

    fn open_dialog(&mut self) {
        let mut dlg = rfd::FileDialog::new()
            .add_filter(tr(self.lang, T::DlgComicFilter), &["cbz", "zip"])
            .set_title(tr(self.lang, T::DlgOpenTitle));
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
            .add_filter(
                tr(self.lang, T::DlgImagesFilter),
                &["jpg", "jpeg", "png", "webp", "gif", "bmp"],
            )
            .set_title(tr(self.lang, T::DlgImageInsertTitle));
        if let Some(d) = self.last_dir.as_ref().filter(|d| d.is_dir()) {
            dlg = dlg.set_directory(d);
        }
        let path = dlg.pick_file()?;
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.fail(tr(self.lang, T::ReadError).replace("{e}", &e.to_string()));
                return None;
            }
        };
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image.jpg".into());
        let uid = self.new_uid();
        Some(Page::new(name, bytes.into(), uid))
    }

    // Extrait une page vers un fichier image sur le disque (utile p.ex. pour récupérer
    // la couverture du tome suivant rangée par erreur dans le mauvais tome).
    fn extract_page(&mut self, idx: usize) {
        let lang = self.lang;
        let Some(page) = self.pages.get(idx) else {
            return;
        };
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
            .set_title(tr(lang, T::DlgExtractTitle))
            .set_file_name(default)
            .add_filter(tr(lang, T::DlgImageFilter), &[ext]);
        if let Some(d) = self.last_dir.as_ref().filter(|d| d.is_dir()) {
            dlg = dlg.set_directory(d);
        }
        if let Some(out) = dlg.save_file() {
            match std::fs::write(&out, &bytes) {
                Ok(_) => {
                    let nm = out
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.info(tr(lang, T::Extracted).replace("{name}", &nm));
                }
                Err(e) => self.fail(tr(lang, T::ExtractError).replace("{e}", &e.to_string())),
            }
        }
    }

    // Coche pour suppression toutes les pages d'un groupe de doublons (contenu
    // strictement identique). On n'en garde aucune : toutes les copies sont cochées.
    fn mark_all_duplicates(&mut self) {
        let lang = self.lang;
        let groups = self.duplicate_groups();
        let to_mark: Vec<usize> = groups
            .iter()
            .flatten()
            .copied()
            .filter(|&i| !self.pages[i].delete)
            .collect();
        if to_mark.is_empty() {
            self.info(tr(lang, T::NoDuplicates));
            return;
        }
        self.checkpoint();
        for &i in &to_mark {
            self.pages[i].delete = true;
        }
        self.info(
            tr(lang, T::DupMarked)
                .replace("{marked}", &to_mark.len().to_string())
                .replace("{groups}", &groups.len().to_string()),
        );
    }

    // Coche pour suppression toutes les pages au contenu identique à `idx` (y compris
    // celle-ci), à condition qu'il existe au moins une autre copie.
    fn mark_identical(&mut self, idx: usize) {
        let same: Vec<usize> = {
            let Some(rp) = self.pages.get(idx) else {
                return;
            };
            let rb = &rp.bytes;
            self.pages
                .iter()
                .enumerate()
                .filter(|(_, p)| &p.bytes == rb)
                .map(|(i, _)| i)
                .collect()
        };
        if same.len() < 2 {
            self.info(tr(self.lang, T::NoIdenticalCopy));
            return;
        }
        self.checkpoint();
        for &i in &same {
            self.pages[i].delete = true;
        }
        self.info(tr(self.lang, T::IdenticalMarked).replace("{n}", &same.len().to_string()));
    }

    // Groupes de pages au contenu strictement identique (octets), de taille ≥ 2.
    fn duplicate_groups(&self) -> Vec<Vec<usize>> {
        use std::collections::HashMap;
        let mut by_hash: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, p) in self.pages.iter().enumerate() {
            by_hash.entry(content_hash(&p.bytes)).or_default().push(i);
        }
        let mut groups = Vec::new();
        for idxs in by_hash.into_values() {
            if idxs.len() < 2 {
                continue;
            }
            // Lever les collisions de hash : regrouper par égalité d'octets exacte.
            let mut buckets: Vec<Vec<usize>> = Vec::new();
            for i in idxs {
                match buckets
                    .iter_mut()
                    .find(|b| self.pages[b[0]].bytes == self.pages[i].bytes)
                {
                    Some(b) => b.push(i),
                    None => buckets.push(vec![i]),
                }
            }
            groups.extend(buckets.into_iter().filter(|b| b.len() >= 2));
        }
        groups.sort();
        groups
    }

    // `progress(lues, total)` est appelé après chaque entrée de l'archive.
    fn read_cbz(path: &Path, lang: Lang, progress: &dyn Fn(u64, u64)) -> ReadResult {
        // Garde-fous anti « bombe zip » : on borne la taille décompressée lue, par
        // entrée et au total, pour ne pas saturer la mémoire avec un fichier piégé.
        const MAX_ENTRY: u64 = 512 * 1024 * 1024;
        const MAX_TOTAL: u64 = 4 * 1024 * 1024 * 1024;
        let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
        let mut pages = Vec::new();
        let mut ci: Option<Vec<u8>> = None;
        let mut ci_is_comicinfo = false;
        let mut total: u64 = 0;
        let n_entries = z.len() as u64;
        for i in 0..z.len() {
            progress(i as u64, n_entries);
            let mut e = z.by_index(i).map_err(|e| e.to_string())?;
            if e.is_dir() {
                continue;
            }
            let name = e.name().to_string();
            // Métadonnées macOS (« __MACOSX/ », fichiers « ._xxx.jpg ») : ce ne sont
            // pas des images, elles apparaîtraient comme pages illisibles.
            let base = name.rsplit('/').next().unwrap_or(&name);
            if name.starts_with("__MACOSX/") || base.starts_with("._") {
                continue;
            }
            let lower = name.to_lowercase();
            let is_xml = lower.ends_with(".xml");
            let is_img = Path::new(&lower)
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(is_image_ext);
            if !is_xml && !is_img {
                continue;
            }
            let mut buf = Vec::new();
            e.by_ref()
                .take(MAX_ENTRY + 1)
                .read_to_end(&mut buf)
                .map_err(|e| e.to_string())?;
            if buf.len() as u64 > MAX_ENTRY {
                return Err(tr(lang, T::ZipEntryTooBig).replace("{name}", &short_name(&name)));
            }
            total += buf.len() as u64;
            if total > MAX_TOTAL {
                return Err(tr(lang, T::ZipTotalTooBig).to_string());
            }
            if is_xml {
                // S'il y a plusieurs .xml, préférer celui qui s'appelle ComicInfo.xml.
                let is_comicinfo = lower.ends_with("comicinfo.xml");
                if ci.is_none() || (is_comicinfo && !ci_is_comicinfo) {
                    ci = Some(buf);
                    ci_is_comicinfo = is_comicinfo;
                }
            } else {
                pages.push(Page::new(name, buf.into(), 0));
            }
        }
        progress(n_entries, n_entries);
        // Tri « naturel » : page2 < page10 (un tri lexical donnerait page10 < page2).
        pages.sort_by(|a, b| natural_cmp(&a.name, &b.name));
        Ok((pages, ci))
    }

    // Envoie au décodeur les pages sans miniature, et récupère les miniatures prêtes.
    fn generate_thumbnails(&mut self, ctx: &egui::Context) {
        let Some(t) = &self.thumbs else { return };
        let gen = self.generation;
        while let Ok(done) = t.done.try_recv() {
            if done.gen != gen {
                continue;
            }
            self.thumb_pending.remove(&done.uid);
            if let Some(page) = self.pages.iter_mut().find(|p| p.uid == done.uid) {
                match done.img {
                    Some(ci) => {
                        page.thumb = Some(ctx.load_texture(
                            format!("thumb-{gen}-{}", done.uid),
                            ci,
                            TextureOptions::LINEAR,
                        ))
                    }
                    None => page.failed = true,
                }
            }
        }
        for page in &self.pages {
            if page.thumb.is_none() && !page.failed && self.thumb_pending.insert(page.uid) {
                let _ = t.jobs.send(ThumbJob {
                    gen,
                    uid: page.uid,
                    bytes: page.bytes.clone(),
                });
            }
        }
    }

    fn open_viewer(&mut self, idx: usize) {
        let zoom = self.viewer.as_ref().is_some_and(|v| v.zoom);
        self.viewer = Some(ViewerState {
            idx,
            tex: None,
            failed: false,
            zoom,
            cache: Vec::new(),
            jobs: Vec::new(),
        });
    }

    fn viewer_go(&mut self, d: i32) {
        let total = self.pages.len() as i64;
        if let Some(vs) = self.viewer.as_mut() {
            if total == 0 {
                return;
            }
            let n = (vs.idx as i64 + d as i64).clamp(0, total - 1) as usize;
            if n != vs.idx {
                vs.idx = n;
                vs.tex = None;
                vs.failed = false;
            }
        }
    }

    // Index d'insertion correspondant à une position écran, dans la grille (ordre de lecture).
    fn drop_index(&self, pos: Pos2) -> usize {
        if self.card_rects.is_empty() {
            return self.pages.len();
        }
        // carte dont le centre est le plus proche du curseur (robuste aux interstices)
        let mut best = self.card_rects[0];
        let mut best_d = f32::MAX;
        for &(i, r) in &self.card_rects {
            let d = r.center().distance_sq(pos);
            if d < best_d {
                best_d = d;
                best = (i, r);
            }
        }
        // insérer avant ou après cette carte selon le côté du curseur
        if pos.x < best.1.center().x {
            best.0
        } else {
            best.0 + 1
        }
    }

    // Petite barre verticale marquant l'endroit où l'image sera insérée.
    fn insert_marker(&self, idx: usize) -> Option<Rect> {
        if let Some((_, r)) = self.card_rects.iter().find(|(i, _)| *i == idx) {
            Some(Rect::from_min_max(
                Pos2::new(r.left() - 4.0, r.top()),
                Pos2::new(r.left(), r.bottom()),
            ))
        } else {
            let (_, r) = self.card_rects.last()?;
            Some(Rect::from_min_max(
                Pos2::new(r.right(), r.top()),
                Pos2::new(r.right() + 4.0, r.bottom()),
            ))
        }
    }

    fn enter_crop(&mut self, idx: usize, ctx: &egui::Context) {
        let Some(page) = self.pages.get(idx) else {
            return;
        };
        let lossless = is_lossless(&page.bytes);
        match image::load_from_memory(&page.bytes) {
            Ok(img) => {
                let disp = display_copy(&img, 1600);
                let rgba = disp.to_rgba8();
                let ci = ColorImage::from_rgba_unmultiplied(
                    [disp.width() as usize, disp.height() as usize],
                    rgba.as_raw(),
                );
                let tex = ctx.load_texture("crop-disp", ci, TextureOptions::LINEAR);
                self.crop = Some(CropState {
                    idx,
                    img,
                    lossless,
                    tex,
                    sel: None,
                    drag: None,
                    press: Pos2::ZERO,
                    sel_press: [0, 0, 0, 0],
                });
                self.info(tr(self.lang, T::CropHintEnter));
            }
            Err(e) => self.fail(tr(self.lang, T::DecodePageError).replace("{e}", &e.to_string())),
        }
    }

    fn finish_crop(&mut self, whole: bool, ctx: &egui::Context) {
        let Some(cs) = self.crop.take() else { return };
        let cropped = if whole {
            cs.img.clone()
        } else if let Some([x, y, w, h]) = cs.sel {
            cs.img.crop_imm(x, y, w, h)
        } else {
            self.fail(tr(self.lang, T::NoSelection));
            self.crop = Some(cs);
            return;
        };
        let (bytes, ext) = match encode_page(&cropped, cs.lossless) {
            Ok(b) => b,
            Err(e) => {
                self.fail(tr(self.lang, T::CoverEncodeError).replace("{e}", &e));
                self.crop = Some(cs);
                return;
            }
        };
        let t = cropped.thumbnail(THUMB_W, THUMB_H);
        let rgba = t.to_rgba8();
        let ci = ColorImage::from_rgba_unmultiplied(
            [t.width() as usize, t.height() as usize],
            rgba.as_raw(),
        );
        let id = self.new_uid();
        let tex = ctx.load_texture(format!("cover-{id}"), ci, TextureOptions::LINEAR);
        self.checkpoint();
        self.cover = Some(Cover {
            bytes: bytes.into(),
            ext,
            tex,
            id,
        });
        self.info(tr(self.lang, T::CoverReadyStatus));
    }

    fn remove_cover(&mut self) {
        if self.cover.is_some() {
            self.checkpoint();
            self.cover = None;
            self.info(tr(self.lang, T::CoverRemoved));
        }
    }

    fn toggle_delete(&mut self, idx: usize) {
        if idx < self.pages.len() {
            self.checkpoint();
            self.pages[idx].delete = !self.pages[idx].delete;
        }
    }

    fn merge_next(&mut self, i: usize) {
        let lang = self.lang;
        if i + 1 >= self.pages.len() {
            self.fail(tr(lang, T::NoNextToMerge));
            return;
        }
        let a = match image::load_from_memory(&self.pages[i].bytes) {
            Ok(x) => x,
            Err(e) => {
                self.fail(
                    tr(lang, T::DecodePageN)
                        .replace("{page}", &(i + 1).to_string())
                        .replace("{e}", &e.to_string()),
                );
                return;
            }
        };
        let b = match image::load_from_memory(&self.pages[i + 1].bytes) {
            Ok(x) => x,
            Err(e) => {
                self.fail(
                    tr(lang, T::DecodePageN)
                        .replace("{page}", &(i + 2).to_string())
                        .replace("{e}", &e.to_string()),
                );
                return;
            }
        };
        // a est lue en 1er, b en 2e. En RTL : a à droite, b à gauche.
        let (left, right) = if self.rtl { (b, a) } else { (a, b) };
        let merged = concat_h(left, right);
        let lossless = is_lossless(&self.pages[i].bytes) && is_lossless(&self.pages[i + 1].bytes);
        let (bytes, ext) = match encode_page(&merged, lossless) {
            Ok(b) => b,
            Err(e) => {
                self.fail(tr(lang, T::MergeEncodeError).replace("{e}", &e));
                return;
            }
        };
        self.checkpoint();
        let uid = self.new_uid();
        self.pages[i] = Page::new(format!("merged-{uid}.{ext}"), bytes.into(), uid);
        self.pages.remove(i + 1);
        self.dirty_order = true;
        self.info(tr(lang, T::Merged));
    }

    fn enter_split(&mut self, idx: usize, ctx: &egui::Context) {
        let Some(page) = self.pages.get(idx) else {
            return;
        };
        let lossless = is_lossless(&page.bytes);
        match image::load_from_memory(&page.bytes) {
            Ok(img) => {
                if img.width() < 2 {
                    self.fail(tr(self.lang, T::TooNarrowToSplit));
                    return;
                }
                let disp = display_copy(&img, 1800);
                let rgba = disp.to_rgba8();
                let ci = ColorImage::from_rgba_unmultiplied(
                    [disp.width() as usize, disp.height() as usize],
                    rgba.as_raw(),
                );
                let tex = ctx.load_texture("split-disp", ci, TextureOptions::LINEAR);
                let cut = img.width() / 2;
                self.split = Some(SplitState {
                    idx,
                    lossless,
                    cut,
                    rtl: self.rtl,
                    img,
                    tex,
                });
                self.info(tr(self.lang, T::SplitHintEnter));
            }
            Err(e) => self.fail(tr(self.lang, T::DecodeError).replace("{e}", &e.to_string())),
        }
    }

    fn finish_split(&mut self) {
        let Some(ss) = self.split.take() else { return };
        let w = ss.img.width();
        if w < 2 {
            self.fail(tr(self.lang, T::TooNarrowToSplit));
            return;
        }
        let cut = ss.cut.clamp(1, w - 1);
        let left = ss.img.crop_imm(0, 0, cut, ss.img.height());
        let right = ss.img.crop_imm(cut, 0, w - cut, ss.img.height());
        let (first, second) = if ss.rtl { (right, left) } else { (left, right) };
        let (fb, sb, ext) = match (
            encode_page(&first, ss.lossless),
            encode_page(&second, ss.lossless),
        ) {
            (Ok((f, ext)), Ok((s, _))) => (f, s, ext),
            _ => {
                self.fail(tr(self.lang, T::SplitEncodeError));
                self.split = Some(ss);
                return;
            }
        };
        if ss.idx < self.pages.len() {
            self.checkpoint();
            let (u1, u2) = (self.new_uid(), self.new_uid());
            self.pages.remove(ss.idx);
            self.pages.insert(
                ss.idx,
                Page::new(format!("split-{u2}.{ext}"), sb.into(), u2),
            );
            self.pages.insert(
                ss.idx,
                Page::new(format!("split-{u1}.{ext}"), fb.into(), u1),
            );
            self.dirty_order = true;
            self.info(tr(self.lang, T::SplitDone));
        }
    }

    fn split_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        enum SAct {
            None,
            Cancel,
            Validate,
            Flip,
        }
        let mut sa = SAct::None;
        let lang = self.lang;
        let pal = pal(ctx);
        if let Some(ss) = self.split.as_mut() {
            let pname = self
                .pages
                .get(ss.idx)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            ui.label(
                RichText::new(tr(lang, T::SplitTitle).replace("{pname}", &short_name(&pname)))
                    .strong(),
            );
            ui.label(tr(lang, T::SplitHint2));
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button(RichText::new(tr(lang, T::SplitApply)).color(pal.ok))
                    .on_hover_text(tr(lang, T::KeyEnter))
                    .clicked()
                {
                    sa = SAct::Validate;
                }
                if ui
                    .button(tr(lang, T::Cancel))
                    .on_hover_text(tr(lang, T::KeyEsc))
                    .clicked()
                {
                    sa = SAct::Cancel;
                }
                let dir = if ss.rtl {
                    tr(lang, T::DirRtlSplit)
                } else {
                    tr(lang, T::DirLtrSplit)
                };
                if ui
                    .button(tr(lang, T::SplitDirBtn).replace("{dir}", dir))
                    .on_hover_text(tr(lang, T::FlipDirHover))
                    .clicked()
                {
                    sa = SAct::Flip;
                }
                ui.label(
                    RichText::new(
                        tr(lang, T::CutAt)
                            .replace("{cut}", &ss.cut.to_string())
                            .replace("{width}", &ss.img.width().to_string()),
                    )
                    .weak(),
                );
            });
            ui.separator();

            let iw = ss.img.width();
            let ih = ss.img.height();
            let avail = ui.available_size();
            let disp = fit(
                [iw as usize, ih as usize],
                Vec2::new(avail.x.max(64.0), (avail.y - 6.0).max(64.0)),
            );
            let (resp, painter) = ui.allocate_painter(disp, Sense::click_and_drag());
            let area = resp.rect;
            painter.image(
                ss.tex.id(),
                area,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );

            // Un simple clic place aussi la ligne de coupe (pas seulement le glisser).
            if resp.dragged() || resp.drag_started() || resp.clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let fx = ((p.x - area.left()) / area.width().max(1.0) * iw as f32).round();
                    let hi = (iw as i32 - 1).max(1);
                    ss.cut = (fx as i32).clamp(1, hi) as u32;
                }
            }
            if resp.hovered() {
                ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
            }

            let cx = area.left() + ss.cut as f32 / iw as f32 * area.width();
            let left_rect = Rect::from_min_max(area.min, Pos2::new(cx, area.bottom()));
            let right_rect = Rect::from_min_max(Pos2::new(cx, area.top()), area.max);
            let (first_rect, second_rect) = if ss.rtl {
                (right_rect, left_rect)
            } else {
                (left_rect, right_rect)
            };
            // Superpositions sur l'image : couleurs fixes (indépendantes du thème).
            painter.rect_filled(
                first_rect,
                0.0,
                Color32::from_rgba_unmultiplied(0, 200, 80, 30),
            );
            painter.rect_filled(
                second_rect,
                0.0,
                Color32::from_rgba_unmultiplied(120, 120, 120, 30),
            );
            painter.line_segment(
                [Pos2::new(cx, area.top()), Pos2::new(cx, area.bottom())],
                Stroke::new(2.0, Color32::from_rgb(255, 70, 70)),
            );
            for (r, txt, col) in [
                (
                    first_rect,
                    tr(lang, T::FirstPage),
                    Color32::from_rgb(180, 255, 200),
                ),
                (
                    second_rect,
                    tr(lang, T::SecondPage),
                    Color32::from_rgb(235, 235, 235),
                ),
            ] {
                // Pastille sombre derrière le texte : lisible sur une page blanche.
                let g = painter.layout_no_wrap(txt.to_string(), FontId::proportional(30.0), col);
                let bg = Rect::from_center_size(r.center(), g.size() + Vec2::new(20.0, 10.0));
                painter.rect_filled(bg, 8.0, Color32::from_black_alpha(150));
                painter.galley(r.center() - g.size() / 2.0, g, col);
            }
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

    // Construit l'archive à écrire. Renvoie aussi, pour chaque image écrite, son nom
    // dans l'archive et l'uid de la page d'origine (None = nouvelle couverture), afin
    // de resynchroniser l'état en mémoire après un « Écraser l'original ».
    fn build_cbz(&self) -> Result<Built, String> {
        // Page couverture existante (« 000_cover… ») : UNE seule, la première gardée.
        // Une nouvelle couverture recadrée la remplace.
        let old_cover = self
            .pages
            .iter()
            .position(|p| !p.delete && is_cover_name(&p.name));
        let (cover_bytes, cover_uid, cover_ext): (Option<Arc<[u8]>>, Option<u64>, String) =
            match (&self.cover, old_cover) {
                (Some(c), _) => (Some(c.bytes.clone()), None, c.ext.to_string()),
                (None, Some(i)) => (
                    Some(self.pages[i].bytes.clone()),
                    Some(self.pages[i].uid),
                    ext_of(&self.pages[i].name),
                ),
                (None, None) => (None, None, String::new()),
            };
        let cover_name = format!("000_cover.{cover_ext}");

        let content: Vec<&Page> = self
            .pages
            .iter()
            .enumerate()
            .filter(|&(i, p)| !p.delete && Some(i) != old_cover)
            .map(|(_, p)| p)
            .collect();
        if content.is_empty() && cover_bytes.is_none() {
            return Err(tr(self.lang, T::NoPagesToSave).to_string());
        }

        // Noms d'origine conservés, sauf si l'ordre a changé, ou si la couverture ne
        // serait pas triée en tête (tri naturel ET lexical, selon le lecteur).
        let cover_not_first = cover_bytes.is_some()
            && content.iter().any(|p| {
                natural_cmp(&p.name, &cover_name) != std::cmp::Ordering::Greater
                    || p.name.as_str() <= cover_name.as_str()
            });
        let renumber = self.dirty_order || cover_not_first;
        let n_images = content.len() + usize::from(cover_bytes.is_some());

        let mut entries: Vec<(String, Option<u64>)> = Vec::with_capacity(n_images);
        let mut comicinfo = None;
        let mut cur = Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut cur);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            if let Some(ci) = &self.comicinfo {
                let mut patched = update_pagecount(ci, n_images);
                // Le bloc <Pages> (types de page, doubles-pages…) décrit l'ancienne
                // liste : s'il ne correspond plus, mieux vaut le retirer qu'induire
                // le lecteur en erreur.
                let changed =
                    renumber || self.cover.is_some() || self.pages.iter().any(|p| p.delete);
                if changed {
                    patched = strip_pages_block(&patched);
                }
                zw.start_file("ComicInfo.xml", opts)
                    .map_err(|e| e.to_string())?;
                zw.write_all(&patched).map_err(|e| e.to_string())?;
                comicinfo = Some(patched);
            }
            if let Some(cover) = &cover_bytes {
                zw.start_file(cover_name.as_str(), opts)
                    .map_err(|e| e.to_string())?;
                zw.write_all(cover).map_err(|e| e.to_string())?;
                entries.push((cover_name.clone(), cover_uid));
            }
            for (i, p) in content.iter().enumerate() {
                let name = if renumber {
                    format!("p{:04}.{}", i, ext_of(&p.name))
                } else {
                    p.name.clone()
                };
                zw.start_file(name.as_str(), opts)
                    .map_err(|e| e.to_string())?;
                zw.write_all(&p.bytes).map_err(|e| e.to_string())?;
                entries.push((name, Some(p.uid)));
            }
            zw.finish().map_err(|e| e.to_string())?;
        }
        Ok(Built {
            data: cur.into_inner(),
            entries,
            comicinfo,
        })
    }

    // Enregistre ; renvoie true en cas de succès.
    fn save(&mut self, overwrite: bool) -> bool {
        let lang = self.lang;
        let Some(path) = self.path.clone() else {
            self.fail(tr(lang, T::NoFileOpen));
            return false;
        };
        let built = match self.build_cbz() {
            Ok(d) => d,
            Err(e) => {
                self.fail(tr(lang, T::BuildError).replace("{e}", &e));
                return false;
            }
        };
        let kept = built.entries.len();
        if overwrite {
            let mut bak = path.clone().into_os_string();
            bak.push(".bak");
            let bak = PathBuf::from(bak);
            let bak_created = !bak.exists();
            if bak_created {
                if let Err(e) = std::fs::copy(&path, &bak) {
                    self.fail(tr(lang, T::BakError).replace("{e}", &e.to_string()));
                    return false;
                }
            }
            let mut tmp = path.clone().into_os_string();
            tmp.push(".tmp");
            let tmp = PathBuf::from(tmp);
            if let Err(e) = write_synced(&tmp, &built.data) {
                let _ = std::fs::remove_file(&tmp);
                self.fail(tr(lang, T::WriteError).replace("{e}", &e.to_string()));
                return false;
            }
            if let Err(e) = std::fs::rename(&tmp, &path) {
                let _ = std::fs::remove_file(&tmp);
                self.fail(tr(lang, T::ReplaceError).replace("{e}", &e.to_string()));
                return false;
            }
            self.sync_after_overwrite(built);
            let msg = if bak_created {
                T::Overwritten
            } else {
                T::OverwrittenBakKept
            };
            self.info(tr(lang, msg).replace("{kept}", &kept.to_string()));
        } else {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "sortie".into());
            let parent = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            // Ne jamais écraser une copie existante : suffixe incrémental si besoin.
            let suffix = tr(lang, T::EditedSuffix);
            let mut out = parent.join(format!("{stem} ({suffix}).cbz"));
            let mut k = 2;
            while out.exists() {
                out = parent.join(format!("{stem} ({suffix} {k}).cbz"));
                k += 1;
            }
            if let Err(e) = write_synced(&out, &built.data) {
                let _ = std::fs::remove_file(&out);
                self.fail(tr(lang, T::WriteError).replace("{e}", &e.to_string()));
                return false;
            }
            // Le travail est en sécurité dans la copie : plus rien de « non enregistré ».
            self.saved_sig = self.edit_sig();
            let nm = out
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.info(tr(lang, T::CopySaved).replace("{name}", &nm));
        }
        true
    }

    // Après écrasement, l'état en mémoire reflète exactement le fichier écrit : pages
    // supprimées retirées, noms définitifs, couverture intégrée comme page 1.
    // Les miniatures déjà calculées sont réutilisées (par uid).
    fn sync_after_overwrite(&mut self, built: Built) {
        let mut by_uid: std::collections::HashMap<u64, Page> =
            self.pages.drain(..).map(|p| (p.uid, p)).collect();
        let cover = self.cover.take();
        for (name, uid) in built.entries {
            match uid.and_then(|u| by_uid.remove(&u)) {
                Some(mut p) => {
                    p.name = name;
                    self.pages.push(p);
                }
                None => {
                    if let Some(c) = &cover {
                        let uid = self.new_uid();
                        let mut p = Page::new(name, c.bytes.clone(), uid);
                        p.thumb = Some(c.tex.clone());
                        self.pages.push(p);
                    }
                }
            }
        }
        self.comicinfo = built.comicinfo;
        self.dirty_order = false;
        self.undo.clear();
        self.redo.clear();
        if let Some(vs) = self.viewer.as_mut() {
            vs.idx = vs.idx.min(self.pages.len().saturating_sub(1));
            vs.tex = None;
            vs.failed = false;
        }
        self.saved_sig = self.edit_sig();
    }

    fn top_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let lang = self.lang;
        let pal = pal(ctx);
        let modified = self.is_modified();
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if ui
                .button(RichText::new(tr(lang, T::OpenCbz)).size(15.0))
                .on_hover_text("Ctrl+O")
                .clicked()
            {
                self.request_open(None);
            }
            ui.separator();
            match &self.path {
                Some(p) => {
                    ui.label(RichText::new("📄").size(16.0));
                    ui.label(
                        RichText::new(
                            p.file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                        )
                        .strong(),
                    )
                    .on_hover_text(p.to_string_lossy());
                    if modified {
                        ui.colored_label(pal.warn, tr(lang, T::Unsaved));
                    }
                }
                None => {
                    ui.label(RichText::new(tr(lang, T::OpenHintNoFile)).weak());
                }
            }
            // Sélecteur de langue, aligné à droite.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let cur = self.lang;
                let mut new_lang = cur;
                egui::ComboBox::from_id_salt("lang-select")
                    .selected_text(cur.native_name())
                    .show_ui(ui, |ui| {
                        for l in Lang::ALL {
                            ui.selectable_value(&mut new_lang, l, l.native_name());
                        }
                    })
                    .response
                    .on_hover_text(tr(cur, T::LangHover));
                if new_lang != cur {
                    self.lang = new_lang;
                    save_lang(new_lang);
                }
            });
        });
        if self.path.is_some() {
            // Pendant un recadrage/une découpe, la barre d'édition est grisée : il faut
            // d'abord valider ou annuler l'opération en cours.
            let editing = self.crop.is_none() && self.split.is_none();
            ui.add_enabled_ui(editing, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(!self.undo.is_empty(), egui::Button::new(tr(lang, T::Undo)))
                        .on_hover_text(tr(lang, T::UndoHover))
                        .clicked()
                    {
                        self.undo();
                    }
                    if ui
                        .add_enabled(!self.redo.is_empty(), egui::Button::new(tr(lang, T::Redo)))
                        .on_hover_text(tr(lang, T::RedoHover))
                        .clicked()
                    {
                        self.redo();
                    }
                    ui.separator();
                    let total = self.pages.len();
                    let del = self.pages.iter().filter(|p| p.delete).count();
                    ui.label(tr(lang, T::PagesCount).replace("{n}", &total.to_string()));
                    if del > 0 {
                        ui.colored_label(
                            pal.danger,
                            tr(lang, T::ToDelete).replace("{n}", &del.to_string()),
                        );
                    }
                    if self.dirty_order {
                        ui.colored_label(pal.info, tr(lang, T::OrderChanged));
                    }
                    let mut remove_cover = false;
                    if let Some(c) = &self.cover {
                        ui.colored_label(pal.ok, tr(lang, T::CoverFlag));
                        let sz = fit(c.tex.size(), Vec2::new(40.0, 40.0));
                        ui.image(SizedTexture::new(c.tex.id(), sz));
                        if ui
                            .small_button("✖")
                            .on_hover_text(tr(lang, T::RemoveCoverHover))
                            .clicked()
                        {
                            remove_cover = true;
                        }
                    }
                    if remove_cover {
                        self.remove_cover();
                    }
                    ui.separator();
                    if ui
                        .button(tr(lang, T::MarkDuplicates))
                        .on_hover_text(tr(lang, T::MarkDuplicatesHover))
                        .clicked()
                    {
                        self.mark_all_duplicates();
                    }
                    let dir = if self.rtl {
                        tr(lang, T::DirRtl)
                    } else {
                        tr(lang, T::DirLtr)
                    };
                    if ui
                        .button(dir)
                        .on_hover_text(tr(lang, T::DirHover))
                        .clicked()
                    {
                        self.rtl = !self.rtl;
                    }
                    ui.separator();
                    if ui
                        .button(tr(lang, T::SaveCopy))
                        .on_hover_text(tr(lang, T::SaveCopyHover))
                        .clicked()
                    {
                        self.save(false);
                    }
                    if ui
                        .button(RichText::new(tr(lang, T::Overwrite)).color(pal.warn))
                        .on_hover_text(tr(lang, T::OverwriteHover))
                        .clicked()
                    {
                        self.confirm_overwrite = true;
                    }
                });
            });
        }
        ui.add_space(2.0);
    }

    fn grid_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut action: Option<GridAction> = None;
        let mut rects: Vec<(usize, Rect)> = Vec::new();
        let mut dragged_uid: Option<u64> = None;
        let mut released = false;
        let drag_uid = self.drag_page;
        let n = self.pages.len();
        let lang = self.lang;
        let pal = pal(ctx);
        if n == 0 {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new(tr(lang, T::NoImages)).size(18.0).weak());
            });
            return;
        }
        egui::ScrollArea::vertical()
            .drag_to_scroll(false)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Défilement automatique quand on glisse une page près du bord haut/bas :
                // permet de la déplacer loin sans lâcher.
                if drag_uid.is_some() {
                    if let Some(p) = ctx.pointer_hover_pos() {
                        let clip = ui.clip_rect();
                        let margin = 60.0;
                        let dy = if p.y < clip.top() + margin {
                            clip.top() + margin - p.y
                        } else if p.y > clip.bottom() - margin {
                            clip.bottom() - margin - p.y
                        } else {
                            0.0
                        };
                        if dy != 0.0 {
                            ui.scroll_with_delta(Vec2::new(0.0, dy.clamp(-margin, margin) * 0.3));
                            ctx.request_repaint();
                        }
                    }
                }
                // Grille en rangées explicites (horizontal_wrapped décalait la 1re carte
                // de chaque rangée de quelques pixels).
                let gap = ui.spacing().item_spacing.x;
                let cols = (((ui.available_width() + gap) / (178.0 + gap)).floor() as usize).max(1);
                for row in (0..n).step_by(cols) {
                    ui.horizontal(|ui| {
                        for (idx, page) in self.pages.iter().enumerate().skip(row).take(cols) {
                            let card = ui.allocate_ui(Vec2::new(178.0, 222.0), |ui| {
                                egui::Frame::group(ui.style())
                                    .fill(if Some(page.uid) == drag_uid {
                                        pal.drag_fill
                                    } else if page.delete {
                                        pal.del_fill
                                    } else {
                                        ui.style().visuals.faint_bg_color
                                    })
                                    .show(ui, |ui| {
                                        ui.set_width(156.0);
                                        ui.set_height(200.0);
                                        ui.vertical_centered(|ui| {
                                            // Emplacement fixe 150×150 quel que soit le format de la page :
                                            // noms et boutons restent alignés d'une carte à l'autre.
                                            let thumb = ui.allocate_response(
                                                Vec2::splat(150.0),
                                                Sense::click_and_drag(),
                                            );
                                            let slot = thumb.rect;
                                            match &page.thumb {
                                                Some(tex) => {
                                                    let r = Rect::from_center_size(
                                                        slot.center(),
                                                        fit(tex.size(), slot.size()),
                                                    );
                                                    // Page cochée pour suppression : miniature assombrie.
                                                    let tint = if page.delete {
                                                        Color32::from_gray(90)
                                                    } else {
                                                        Color32::WHITE
                                                    };
                                                    ui.painter().image(
                                                        tex.id(),
                                                        r,
                                                        Rect::from_min_max(
                                                            Pos2::ZERO,
                                                            Pos2::new(1.0, 1.0),
                                                        ),
                                                        tint,
                                                    );
                                                }
                                                None if page.failed => {
                                                    // Image non décodable : cadre cliquable (clic droit/déplacer
                                                    // restent possibles) plutôt qu'un spinner qui tourne sans fin.
                                                    let p = ui.painter();
                                                    p.rect_filled(slot, 4.0, pal.bad_fill);
                                                    p.text(
                                                        slot.center(),
                                                        Align2::CENTER_CENTER,
                                                        tr(lang, T::Illegible),
                                                        FontId::proportional(15.0),
                                                        pal.bad_text,
                                                    );
                                                }
                                                None => egui::Spinner::new().paint_at(
                                                    ui,
                                                    Rect::from_center_size(
                                                        slot.center(),
                                                        Vec2::splat(28.0),
                                                    ),
                                                ),
                                            }
                                            if page.delete {
                                                ui.painter().text(
                                                    thumb.rect.center(),
                                                    Align2::CENTER_CENTER,
                                                    "🗑",
                                                    FontId::proportional(40.0),
                                                    pal.danger,
                                                );
                                            }
                                            if thumb.clicked() {
                                                action = Some(GridAction::Enlarge(idx));
                                            }
                                            if thumb.dragged() {
                                                dragged_uid = Some(page.uid);
                                            }
                                            if thumb.drag_stopped() {
                                                dragged_uid = Some(page.uid);
                                                released = true;
                                            }
                                            thumb
                                                .on_hover_text(tr(lang, T::ThumbHover))
                                                .context_menu(|ui| {
                                                    if ui.button(tr(lang, T::MenuEnlarge)).clicked()
                                                    {
                                                        action = Some(GridAction::Enlarge(idx));
                                                        ui.close_menu();
                                                    }
                                                    let del_lbl = if page.delete {
                                                        T::MenuKeep
                                                    } else {
                                                        T::MenuDelete
                                                    };
                                                    if ui.button(tr(lang, del_lbl)).clicked() {
                                                        action =
                                                            Some(GridAction::ToggleDelete(idx));
                                                        ui.close_menu();
                                                    }
                                                    ui.separator();
                                                    if ui.button(tr(lang, T::MenuCrop)).clicked() {
                                                        action = Some(GridAction::Crop(idx));
                                                        ui.close_menu();
                                                    }
                                                    if ui.button(tr(lang, T::MenuSplit)).clicked() {
                                                        action = Some(GridAction::Split(idx));
                                                        ui.close_menu();
                                                    }
                                                    if ui
                                                        .add_enabled(
                                                            idx + 1 < n,
                                                            egui::Button::new(tr(
                                                                lang,
                                                                T::MenuMerge,
                                                            )),
                                                        )
                                                        .clicked()
                                                    {
                                                        action = Some(GridAction::MergeNext(idx));
                                                        ui.close_menu();
                                                    }
                                                    ui.separator();
                                                    if ui
                                                        .button(tr(lang, T::MenuInsertBefore))
                                                        .clicked()
                                                    {
                                                        action =
                                                            Some(GridAction::InsertBefore(idx));
                                                        ui.close_menu();
                                                    }
                                                    if ui
                                                        .button(tr(lang, T::MenuInsertAfter))
                                                        .clicked()
                                                    {
                                                        action = Some(GridAction::InsertAfter(idx));
                                                        ui.close_menu();
                                                    }
                                                    ui.separator();
                                                    if ui
                                                        .add_enabled(
                                                            idx > 0,
                                                            egui::Button::new(tr(
                                                                lang,
                                                                T::MenuMoveStart,
                                                            )),
                                                        )
                                                        .clicked()
                                                    {
                                                        action = Some(GridAction::MoveStart(idx));
                                                        ui.close_menu();
                                                    }
                                                    if ui
                                                        .add_enabled(
                                                            idx + 1 < n,
                                                            egui::Button::new(tr(
                                                                lang,
                                                                T::MenuMoveEnd,
                                                            )),
                                                        )
                                                        .clicked()
                                                    {
                                                        action = Some(GridAction::MoveEnd(idx));
                                                        ui.close_menu();
                                                    }
                                                    ui.separator();
                                                    if ui.button(tr(lang, T::MenuExtract)).clicked()
                                                    {
                                                        action = Some(GridAction::Extract(idx));
                                                        ui.close_menu();
                                                    }
                                                    if ui
                                                        .button(tr(lang, T::MenuMarkIdentical))
                                                        .on_hover_text(tr(
                                                            lang,
                                                            T::MenuMarkIdenticalHover,
                                                        ))
                                                        .clicked()
                                                    {
                                                        action =
                                                            Some(GridAction::MarkIdentical(idx));
                                                        ui.close_menu();
                                                    }
                                                });
                                            // Numéro de page (repère des messages « page N ») + nom du fichier.
                                            let mut name = RichText::new(format!(
                                                "{} · {}",
                                                idx + 1,
                                                short_name(&page.name)
                                            ))
                                            .small();
                                            if page.delete {
                                                name = name.strikethrough().color(pal.danger);
                                            }
                                            ui.label(name).on_hover_text(&page.name);
                                            ui.horizontal(|ui| {
                                                let lbl = if page.delete { "↩" } else { "🗑" };
                                                if ui
                                                    .button(lbl)
                                                    .on_hover_text(if page.delete {
                                                        tr(lang, T::KeepHover)
                                                    } else {
                                                        tr(lang, T::DeleteHover)
                                                    })
                                                    .clicked()
                                                {
                                                    action = Some(GridAction::ToggleDelete(idx));
                                                }
                                                if ui
                                                    .button("✂")
                                                    .on_hover_text(tr(lang, T::CropToCoverHover))
                                                    .clicked()
                                                {
                                                    action = Some(GridAction::Crop(idx));
                                                }
                                                if ui
                                                    .add_enabled(idx > 0, egui::Button::new("◀"))
                                                    .on_hover_text(tr(lang, T::MoveBeforeHover))
                                                    .clicked()
                                                {
                                                    action = Some(GridAction::MoveLeft(idx));
                                                }
                                                if ui
                                                    .add_enabled(
                                                        idx + 1 < n,
                                                        egui::Button::new("▶"),
                                                    )
                                                    .on_hover_text(tr(lang, T::MoveAfterHover))
                                                    .clicked()
                                                {
                                                    action = Some(GridAction::MoveRight(idx));
                                                }
                                            });
                                        });
                                    });
                            });
                            rects.push((idx, card.response.rect));
                        }
                    });
                }
            });
        self.card_rects = rects;

        // déplacement interne d'une page par glisser (avec barre à la position cible)
        if let Some(uid) = dragged_uid {
            let pos = ctx.input(|i| i.pointer.interact_pos());
            if released {
                self.drag_page = None;
                if let Some(p) = pos {
                    let t = self.drop_index(p);
                    self.move_page_to(uid, t);
                }
            } else {
                self.drag_page = Some(uid);
                if let Some(p) = pos {
                    let painter =
                        ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("reorder")));
                    if let Some(m) = self.insert_marker(self.drop_index(p)) {
                        painter.rect_filled(m, 2.0, pal.accent);
                    }
                    // « Fantôme » de la page déplacée sous le curseur.
                    if let Some(tex) = self
                        .pages
                        .iter()
                        .find(|pg| pg.uid == uid)
                        .and_then(|pg| pg.thumb.as_ref())
                    {
                        let sz = fit(tex.size(), Vec2::new(80.0, 80.0));
                        let r = Rect::from_min_size(p + Vec2::new(12.0, 12.0), sz);
                        painter.image(
                            tex.id(),
                            r,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::from_white_alpha(190),
                        );
                        painter.rect_stroke(r, 0.0, Stroke::new(1.5, pal.accent));
                    }
                }
            }
        } else {
            self.drag_page = None;
        }

        match action {
            Some(GridAction::ToggleDelete(i)) => self.toggle_delete(i),
            Some(GridAction::Enlarge(i)) => self.open_viewer(i),
            Some(GridAction::Crop(i)) => self.enter_crop(i, ctx),
            Some(GridAction::MoveLeft(i)) if i > 0 && i < self.pages.len() => {
                self.checkpoint();
                self.pages.swap(i, i - 1);
                self.dirty_order = true;
            }
            Some(GridAction::MoveRight(i)) if i + 1 < self.pages.len() => {
                self.checkpoint();
                self.pages.swap(i, i + 1);
                self.dirty_order = true;
            }
            Some(GridAction::MoveStart(i)) if i > 0 && i < self.pages.len() => {
                self.checkpoint();
                let p = self.pages.remove(i);
                self.pages.insert(0, p);
                self.dirty_order = true;
            }
            Some(GridAction::MoveEnd(i)) if i + 1 < self.pages.len() => {
                self.checkpoint();
                let p = self.pages.remove(i);
                self.pages.push(p);
                self.dirty_order = true;
            }
            Some(GridAction::InsertBefore(i)) => {
                if let Some(pg) = self.pick_image_page() {
                    self.checkpoint();
                    let at = i.min(self.pages.len());
                    self.pages.insert(at, pg);
                    self.dirty_order = true;
                    self.info(tr(lang, T::ImageInserted));
                }
            }
            Some(GridAction::InsertAfter(i)) => {
                if let Some(pg) = self.pick_image_page() {
                    self.checkpoint();
                    let at = (i + 1).min(self.pages.len());
                    self.pages.insert(at, pg);
                    self.dirty_order = true;
                    self.info(tr(lang, T::ImageInserted));
                }
            }
            Some(GridAction::MergeNext(i)) => self.merge_next(i),
            Some(GridAction::Split(i)) => self.enter_split(i, ctx),
            Some(GridAction::Extract(i)) => self.extract_page(i),
            Some(GridAction::MarkIdentical(i)) => self.mark_identical(i),
            _ => {}
        }
    }

    fn move_page_to(&mut self, uid: u64, target: usize) {
        if let Some(src) = self.pages.iter().position(|p| p.uid == uid) {
            let mut t = target.min(self.pages.len());
            if src < t {
                t -= 1;
            }
            if t == src {
                return; // lâchée à sa place : rien à faire (pas d'« ordre modifié »)
            }
            self.checkpoint();
            let page = self.pages.remove(src);
            t = t.min(self.pages.len());
            self.pages.insert(t, page);
            self.dirty_order = true;
            self.info(tr(self.lang, T::PageMoved).replace("{n}", &(t + 1).to_string()));
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
        let lang = self.lang;
        let pal = pal(ctx);
        if let Some(vs) = self.viewer.as_mut() {
            vs.pump(ctx, &self.pages);
            let idx = vs.idx;
            let total = self.pages.len();
            let (name, is_del) = self
                .pages
                .get(idx)
                .map(|p| (p.name.clone(), p.delete))
                .unwrap_or_default();
            ui.horizontal_wrapped(|ui| {
                if ui.button(tr(lang, T::ViewerClose)).clicked() {
                    va = VAct::Close;
                }
                ui.separator();
                if ui
                    .add_enabled(idx > 0, egui::Button::new(tr(lang, T::ViewerPrev)))
                    .on_hover_text("⬅")
                    .clicked()
                {
                    va = VAct::Prev;
                }
                ui.label(RichText::new(format!("{} / {}", idx + 1, total)).strong());
                if ui
                    .add_enabled(idx + 1 < total, egui::Button::new(tr(lang, T::ViewerNext)))
                    .on_hover_text("➡")
                    .clicked()
                {
                    va = VAct::Next;
                }
                ui.separator();
                ui.label(RichText::new(short_name(&name)).weak())
                    .on_hover_text(&name);
                let del_txt = if is_del {
                    RichText::new(tr(lang, T::ViewerKeep))
                } else {
                    RichText::new(tr(lang, T::ViewerDelete)).color(pal.danger)
                };
                if ui
                    .button(del_txt)
                    .on_hover_text(tr(lang, T::KeyDel))
                    .clicked()
                {
                    va = VAct::ToggleDelete;
                }
                if ui.button(tr(lang, T::ViewerCrop)).clicked() {
                    va = VAct::Crop;
                }
                if ui.button(tr(lang, T::ViewerExtract)).clicked() {
                    va = VAct::Extract;
                }
                if is_del {
                    ui.colored_label(pal.danger, tr(lang, T::MarkedForDeletion));
                }
            });
            ui.separator();
            match &vs.tex {
                Some(tex) => {
                    let avail = ui.available_size();
                    let clicked = if vs.zoom {
                        // Taille réelle : défilement (molette ou glisser) dans les deux sens.
                        egui::ScrollArea::both()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let r = ui.add(
                                    egui::Image::new(SizedTexture::new(tex.id(), tex.size_vec2()))
                                        .sense(Sense::click()),
                                );
                                r.clone()
                                    .on_hover_cursor(CursorIcon::ZoomOut)
                                    .on_hover_text(tr(lang, T::ZoomHover));
                                r.clicked()
                            })
                            .inner
                    } else {
                        let sz = fit(tex.size(), Vec2::new(avail.x, (avail.y - 4.0).max(64.0)));
                        ui.centered_and_justified(|ui| {
                            let r = ui.add(
                                egui::Image::new(SizedTexture::new(tex.id(), sz))
                                    .sense(Sense::click()),
                            );
                            if is_del {
                                ui.painter()
                                    .rect_stroke(r.rect, 0.0, Stroke::new(3.0, pal.danger));
                            }
                            r.clone()
                                .on_hover_cursor(CursorIcon::ZoomIn)
                                .on_hover_text(tr(lang, T::ZoomHover));
                            r.clicked()
                        })
                        .inner
                    };
                    if clicked {
                        vs.zoom = !vs.zoom;
                    }
                }
                None => {
                    let failed = vs.failed;
                    ui.centered_and_justified(|ui| {
                        if failed {
                            ui.label(
                                RichText::new(tr(lang, T::IllegibleViewer))
                                    .size(18.0)
                                    .color(pal.bad_text),
                            );
                        } else {
                            ui.add(egui::Spinner::new());
                        }
                    });
                }
            }
        }
        match va {
            VAct::Close => self.viewer = None,
            VAct::Prev => self.viewer_go(-1),
            VAct::Next => self.viewer_go(1),
            VAct::ToggleDelete => {
                if let Some(i) = self.viewer.as_ref().map(|v| v.idx) {
                    self.toggle_delete(i);
                }
            }
            VAct::Crop => {
                if let Some(vs) = self.viewer.take() {
                    self.enter_crop(vs.idx, ctx);
                }
            }
            VAct::Extract => {
                if let Some(i) = self.viewer.as_ref().map(|v| v.idx) {
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
        let lang = self.lang;
        let pal = pal(ctx);
        if let Some(cs) = self.crop.as_mut() {
            let pname = self
                .pages
                .get(cs.idx)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            ui.label(
                RichText::new(tr(lang, T::CropTitle).replace("{pname}", &short_name(&pname)))
                    .strong(),
            );
            ui.label(tr(lang, T::CropHint2));
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        cs.sel.is_some(),
                        egui::Button::new(RichText::new(tr(lang, T::CropApply)).color(pal.ok)),
                    )
                    .on_hover_text(tr(lang, T::KeyEnter))
                    .clicked()
                {
                    act = Act::Validate;
                }
                if ui.button(tr(lang, T::CropWhole)).clicked() {
                    act = Act::Whole;
                }
                if ui
                    .button(tr(lang, T::Cancel))
                    .on_hover_text(tr(lang, T::KeyEsc))
                    .clicked()
                {
                    act = Act::Cancel;
                }
                if let Some([x, y, w, h]) = cs.sel {
                    ui.label(
                        RichText::new(
                            tr(lang, T::SelectionInfo)
                                .replace("{w}", &w.to_string())
                                .replace("{h}", &h.to_string())
                                .replace("{x}", &x.to_string())
                                .replace("{y}", &y.to_string()),
                        )
                        .weak(),
                    );
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
            painter.image(
                cs.tex.id(),
                area,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );

            let sel_screen = cs
                .sel
                .map(|s| img_to_screen(s, area, img_w as f32, img_h as f32));

            if cs.drag.is_none() {
                if let Some(hp) = resp.hover_pos() {
                    match sel_screen {
                        Some(scr) if handle_at(scr, hp, 10.0).is_some() => ctx.set_cursor_icon(
                            resize_cursor(handle_at(scr, hp, 10.0).unwrap_or(Handle::SE)),
                        ),
                        Some(scr) if scr.contains(hp) => ctx.set_cursor_icon(CursorIcon::Move),
                        _ => ctx.set_cursor_icon(CursorIcon::Crosshair),
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
                            let base =
                                img_to_screen(cs.sel_press, area, img_w as f32, img_h as f32);
                            translate_clamped(base, cur - cs.press, area)
                        }
                        DragKind::Resize(h) => {
                            let base =
                                img_to_screen(cs.sel_press, area, img_w as f32, img_h as f32);
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
                // Assombrir ce qui sera rogné : on voit directement le résultat.
                let shade = Color32::from_black_alpha(140);
                painter.rect_filled(
                    Rect::from_min_max(area.min, Pos2::new(area.right(), scr.top())),
                    0.0,
                    shade,
                );
                painter.rect_filled(
                    Rect::from_min_max(Pos2::new(area.left(), scr.bottom()), area.max),
                    0.0,
                    shade,
                );
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(area.left(), scr.top()),
                        Pos2::new(scr.left(), scr.bottom()),
                    ),
                    0.0,
                    shade,
                );
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(scr.right(), scr.top()),
                        Pos2::new(area.right(), scr.bottom()),
                    ),
                    0.0,
                    shade,
                );
                painter.rect_stroke(scr, 0.0, Stroke::new(2.0, pal.accent));
                for (_, hp) in handles(scr) {
                    let hr = Rect::from_center_size(hp, Vec2::splat(9.0));
                    painter.rect_filled(hr, 0.0, Color32::WHITE);
                    painter.rect_stroke(hr, 0.0, Stroke::new(1.0, pal.accent));
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

    // Raccourcis clavier (désactivés quand une boîte de confirmation est ouverte).
    fn handle_keys(&mut self, ctx: &egui::Context) {
        use egui::Modifiers;
        // Ctrl+Maj+Z d'abord : `consume_key(COMMAND, Z)` accepterait aussi Maj.
        let (open, save, redo2, undo, redo) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::O),
                i.consume_key(Modifiers::COMMAND, Key::S),
                i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z),
                i.consume_key(Modifiers::COMMAND, Key::Z),
                i.consume_key(Modifiers::COMMAND, Key::Y),
            )
        });
        let (esc, enter, left, right, home, end, del, shift) = ctx.input(|i| {
            (
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::Enter),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.key_pressed(Key::Delete),
                i.modifiers.shift,
            )
        });
        if open {
            self.request_open(None);
        }
        let in_crop_or_split = self.crop.is_some() || self.split.is_some();
        if save && self.path.is_some() && !in_crop_or_split {
            self.save(false);
        }
        if !in_crop_or_split {
            if undo {
                self.undo();
            }
            if redo || redo2 {
                self.redo();
            }
        }

        if self.viewer.is_some() {
            if esc {
                self.viewer = None;
            } else {
                if left {
                    self.viewer_go(-1);
                }
                if right {
                    self.viewer_go(1);
                }
                if home {
                    self.viewer_go(i32::MIN / 2);
                }
                if end {
                    self.viewer_go(i32::MAX / 2);
                }
                if del {
                    if let Some(i) = self.viewer.as_ref().map(|v| v.idx) {
                        self.toggle_delete(i);
                    }
                }
            }
        } else if let Some(ss) = self.split.as_mut() {
            if esc {
                self.split = None;
                self.status.clear();
            } else if enter {
                self.finish_split();
            } else if left || right {
                // Ajustement fin de la ligne de coupe (Maj = pas plus grand).
                let step = if shift { 20 } else { 2 };
                let w = ss.img.width();
                let c = ss.cut as i64 + if left { -step } else { step };
                ss.cut = c.clamp(1, (w as i64 - 1).max(1)) as u32;
            }
        } else if self.crop.is_some() {
            if esc {
                self.crop = None;
                self.status.clear();
            } else if enter && self.crop.as_ref().is_some_and(|c| c.sel.is_some()) {
                self.finish_crop(false, ctx);
            }
        }
    }

    // Dépôt de fichiers depuis le gestionnaire de fichiers.
    fn handle_drops(&mut self, ctx: &egui::Context, now: f64) {
        let lang = self.lang;
        // (chemin, octets si pas de chemin, nom) de chaque fichier déposé
        type Dropped = (Option<PathBuf>, Option<Arc<[u8]>>, String);
        let dropped: Vec<Dropped> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| (f.path.clone(), f.bytes.clone(), f.name.clone()))
                .collect()
        });
        if dropped.is_empty() {
            return;
        }
        if let Some(p) = dropped
            .iter()
            .find_map(|(p, _, _)| p.clone().filter(|p| is_cbz_path(p)))
        {
            self.request_open(Some(p));
            return;
        }
        let in_grid = self.path.is_some()
            && self.crop.is_none()
            && self.split.is_none()
            && self.viewer.is_none();
        if !in_grid {
            if self.path.is_some() {
                self.fail(tr(lang, T::DropOnlyInGrid));
            }
            return;
        }
        // Une image externe s'insère toujours en page 1 (sous X11 la position d'un
        // drag externe n'est pas fiable) + petit toast.
        let mut new_pages = Vec::new();
        for (path, bytes, name) in &dropped {
            let ext = path
                .as_ref()
                .and_then(|p| p.extension())
                .map(|e| e.to_string_lossy().to_lowercase())
                .or_else(|| {
                    Path::new(name)
                        .extension()
                        .map(|e| e.to_string_lossy().to_lowercase())
                })
                .unwrap_or_default();
            if !is_image_ext(&ext) {
                continue;
            }
            let data: Option<Arc<[u8]>> = match path {
                Some(p) => std::fs::read(p).ok().map(Into::into),
                None => bytes.clone(),
            };
            if let Some(data) = data {
                let nm = path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| name.clone());
                let uid = self.new_uid();
                new_pages.push(Page::new(nm, data, uid));
            }
        }
        let count = new_pages.len();
        if count == 0 {
            self.fail(tr(lang, T::DropNothingUsable));
            return;
        }
        self.checkpoint();
        self.pages.splice(0..0, new_pages);
        self.dirty_order = true;
        let msg = if count == 1 {
            tr(lang, T::ToastImageAdded).to_string()
        } else {
            tr(lang, T::ToastImagesAdded).replace("{count}", &count.to_string())
        };
        self.info(msg.clone());
        self.toast = Some((msg, now + TOAST_SECS));
    }

    // Boîtes de confirmation (écrasement, modifications non enregistrées).
    fn dialogs_ui(&mut self, ctx: &egui::Context) {
        let lang = self.lang;
        let pal = pal(ctx);
        let esc = ctx.input(|i| i.key_pressed(Key::Escape));
        if self.confirm_overwrite {
            let mut close = esc;
            egui::Window::new(tr(lang, T::ConfirmTitle))
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(tr(lang, T::ConfirmLine1));
                    ui.label(tr(lang, T::ConfirmLine2));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button(RichText::new(tr(lang, T::ConfirmYes)).color(pal.warn))
                            .clicked()
                        {
                            close = true;
                            self.save(true);
                        }
                        if ui.button(tr(lang, T::CancelPlain)).clicked() {
                            close = true;
                        }
                    });
                });
            if close {
                self.confirm_overwrite = false;
            }
        }
        if self.confirm_discard.is_some() {
            let mut choice: Option<bool> = if esc { Some(false) } else { None }; // true = continuer
            egui::Window::new(tr(lang, T::DiscardTitle))
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(tr(lang, T::DiscardLine));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr(lang, T::SaveCopyFirst)).clicked() && self.save(false) {
                            choice = Some(true);
                        }
                        if ui
                            .button(RichText::new(tr(lang, T::DiscardYes)).color(pal.warn))
                            .clicked()
                        {
                            choice = Some(true);
                        }
                        if ui.button(tr(lang, T::CancelPlain)).clicked() {
                            choice = Some(false);
                        }
                    });
                });
            match choice {
                Some(true) => match self.confirm_discard.take() {
                    Some(Pending::Open(p)) => self.do_open(p),
                    Some(Pending::Quit) => {
                        self.allow_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    None => {}
                },
                Some(false) => self.confirm_discard = None,
                None => {}
            }
        }
    }
}

impl eframe::App for CbzApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = ctx.input(|i| i.time);
        let lang = self.lang;
        let pal = pal(ctx);

        // Fermeture de la fenêtre avec des modifications non enregistrées : demander.
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && self.is_modified()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_discard = Some(Pending::Quit);
        }

        self.poll_opening();
        // Pendant une ouverture, l'interface est en lecture seule (comme sous une boîte).
        let modal =
            self.confirm_overwrite || self.confirm_discard.is_some() || self.opening.is_some();
        if !modal {
            self.handle_keys(ctx);
            self.handle_drops(ctx, now);
        }
        self.generate_thumbnails(ctx);

        // Titre de fenêtre : fichier courant + « ● » s'il reste des modifications.
        let title = match &self.path {
            Some(p) => format!(
                "{}{} — {}",
                if self.is_modified() { "● " } else { "" },
                p.file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                tr(lang, T::WindowTitle)
            ),
            None => tr(lang, T::WindowTitle).to_string(),
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            if modal {
                ui.disable();
            }
            self.top_ui(ui, ctx);
        });

        // Barre d'état en bas : position fixe (la barre du haut ne « saute » plus),
        // erreurs en rouge.
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let txt = if self.status.is_empty() {
                    " "
                } else {
                    self.status.as_str()
                };
                if self.status_err {
                    ui.colored_label(pal.danger, txt);
                } else {
                    ui.label(RichText::new(txt).weak());
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if modal {
                ui.disable();
            }
            if self.viewer.is_some() {
                self.viewer_ui(ui, ctx);
            } else if self.split.is_some() {
                self.split_ui(ui, ctx);
            } else if self.crop.is_some() {
                self.crop_ui(ui, ctx);
            } else if self.path.is_some() {
                self.grid_ui(ui, ctx);
            } else if self.opening.is_none() {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new(tr(lang, T::EmptyState)).size(20.0).weak());
                });
            }
        });

        if ctx.input(|i| !i.raw.hovered_files.is_empty()) && !modal {
            let p = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("dnd-overlay")));
            let in_grid = self.path.is_some()
                && self.crop.is_none()
                && self.split.is_none()
                && self.viewer.is_none();
            if in_grid && !self.pages.is_empty() {
                if let Some(m) = self.insert_marker(0) {
                    p.rect_filled(m, 2.0, pal.accent);
                }
                let sr = ctx.screen_rect();
                let g = p.layout_no_wrap(
                    tr(lang, T::DropToPage1).to_string(),
                    FontId::proportional(20.0),
                    Color32::WHITE,
                );
                let r = Rect::from_center_size(
                    Pos2::new(sr.center().x, sr.top() + 110.0),
                    g.size() + Vec2::new(28.0, 16.0),
                );
                p.rect_filled(r, 10.0, Color32::from_rgba_unmultiplied(20, 22, 26, 235));
                p.rect_stroke(r, 10.0, Stroke::new(1.5, pal.accent));
                p.galley(r.center() - g.size() / 2.0, g, Color32::WHITE);
            } else {
                let r = ctx.screen_rect();
                p.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 130));
                p.text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    tr(lang, T::DropCbzHere),
                    FontId::proportional(30.0),
                    Color32::WHITE,
                );
            }
        }

        self.dialogs_ui(ctx);

        // Indicateur d'ouverture en cours (lecture de l'archive en arrière-plan).
        if let Some(op) = &self.opening {
            let name = op
                .path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let txt = tr(lang, T::OpeningProgress)
                .replace("{name}", &name)
                .replace("{done}", &op.done.load(AtomicOrdering::Relaxed).to_string())
                .replace(
                    "{total}",
                    &op.total.load(AtomicOrdering::Relaxed).to_string(),
                );
            egui::Area::new(Id::new("opening"))
                .order(Order::Foreground)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new());
                            ui.label(txt);
                        });
                    });
                });
        }

        // Petit message éphémère qui disparaît tout seul (≈ TOAST_SECS secondes).
        if let Some((msg, exp)) = self.toast.clone() {
            if now >= exp {
                self.toast = None;
            } else {
                let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("toast")));
                let galley = ctx
                    .fonts(|f| f.layout_no_wrap(msg, FontId::proportional(18.0), Color32::WHITE));
                let sr = ctx.screen_rect();
                let center = Pos2::new(sr.center().x, sr.bottom() - 72.0);
                let pad = Vec2::new(18.0, 12.0);
                let rect = Rect::from_center_size(center, galley.size() + pad * 2.0);
                painter.rect_filled(rect, 10.0, Color32::from_rgba_unmultiplied(20, 22, 26, 235));
                painter.rect_stroke(rect, 10.0, Stroke::new(1.5, pal.accent));
                painter.galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
                ctx.request_repaint_after(std::time::Duration::from_secs_f64((exp - now).max(0.0)));
            }
        }
    }
}

// ---------- géométrie du recadrage ----------

fn img_to_screen(sel: [u32; 4], area: Rect, img_w: f32, img_h: f32) -> Rect {
    let sx = area.width() / img_w.max(1.0);
    let sy = area.height() / img_h.max(1.0);
    Rect::from_min_size(
        Pos2::new(
            area.left() + sel[0] as f32 * sx,
            area.top() + sel[1] as f32 * sy,
        ),
        Vec2::new(sel[2] as f32 * sx, sel[3] as f32 * sy),
    )
}

fn screen_to_img(r: Rect, area: Rect, img_w: u32, img_h: u32) -> [u32; 4] {
    let sx = area.width() / img_w.max(1) as f32;
    let sy = area.height() / img_h.max(1) as f32;
    let r = r.intersect(area);
    let x = (((r.left() - area.left()) / sx).round().max(0.0)) as u32;
    let y = (((r.top() - area.top()) / sy).round().max(0.0)) as u32;
    let w = ((r.width() / sx).round() as u32)
        .min(img_w.saturating_sub(x))
        .max(1);
    let h = ((r.height() / sy).round() as u32)
        .min(img_h.saturating_sub(y))
        .max(1);
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

// Décode une page pour l'affichage (réduite si une dimension dépasse `max`).
// Appelée hors du thread de l'UI.
fn decode_color_image(bytes: &[u8], max: u32) -> Option<ColorImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = display_copy(&img, max);
    let rgba = img.to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied(
        [img.width() as usize, img.height() as usize],
        rgba.as_raw(),
    ))
}

// Lance `f` dans un thread ; le résultat arrive par le canal et l'UI est réveillée.
fn spawn_job<T: Send + 'static>(
    ctx: Option<egui::Context>,
    f: impl FnOnce() -> T + Send + 'static,
) -> mpsc::Receiver<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
        if let Some(c) = ctx {
            c.request_repaint();
        }
    });
    rx
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
    p.extension().is_some_and(|e| {
        let e = e.to_string_lossy().to_lowercase();
        e == "cbz" || e == "zip"
    })
}

fn is_image_ext(ext: &str) -> bool {
    matches!(ext, "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp")
}

// ================= Internationalisation (FR / EN / ES / DE / IT) =================
// L'ordre des variantes de `Lang` = l'ordre des colonnes dans `tr` (indexées par
// `lang as usize`). Les libellés peuvent contenir des marqueurs {n} {e} {name}…
// remplacés à l'usage via `.replace(...)`.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Lang {
    #[default]
    Fr,
    En,
    Es,
    De,
    It,
}

impl Lang {
    const ALL: [Lang; 5] = [Lang::Fr, Lang::En, Lang::Es, Lang::De, Lang::It];
    fn code(self) -> &'static str {
        ["fr", "en", "es", "de", "it"][self as usize]
    }
    fn native_name(self) -> &'static str {
        ["Français", "English", "Español", "Deutsch", "Italiano"][self as usize]
    }
    fn from_code(s: &str) -> Option<Lang> {
        Lang::ALL.into_iter().find(|&l| l.code() == s)
    }
    // Langue déduite de l'environnement (LC_ALL/LC_MESSAGES/LANG), repli anglais.
    fn detect() -> Lang {
        for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Some(v) = std::env::var_os(var) {
                let v = v.to_string_lossy().to_lowercase();
                let code = v.split(['_', '.', '-']).next().unwrap_or("");
                if let Some(l) = Lang::from_code(code) {
                    return l;
                }
            }
        }
        Lang::En
    }
}

#[derive(Clone, Copy)]
enum T {
    WindowTitle,
    OpenCbz,
    OpenHintNoFile,
    LangHover,
    PagesCount,
    ToDelete,
    OrderChanged,
    CoverFlag,
    MarkDuplicates,
    MarkDuplicatesHover,
    DirRtl,
    DirLtr,
    DirHover,
    SaveCopy,
    Overwrite,
    OverwriteHover,
    OpenedNPages,
    OpenError,
    ZipEntryTooBig,
    ZipTotalTooBig,
    DlgComicFilter,
    DlgOpenTitle,
    DlgImagesFilter,
    DlgImageInsertTitle,
    DlgExtractTitle,
    DlgImageFilter,
    Extracted,
    ExtractError,
    NoDuplicates,
    DupMarked,
    NoIdenticalCopy,
    IdenticalMarked,
    CropHintEnter,
    DecodePageError,
    NoSelection,
    CoverEncodeError,
    CoverReadyStatus,
    CropTitle,
    CropHint2,
    CropApply,
    CropWhole,
    Cancel,
    SelectionInfo,
    NoNextToMerge,
    DecodePageN,
    MergeEncodeError,
    Merged,
    TooNarrowToSplit,
    SplitHintEnter,
    DecodeError,
    SplitEncodeError,
    SplitDone,
    SplitTitle,
    SplitHint2,
    SplitApply,
    DirRtlSplit,
    DirLtrSplit,
    SplitDirBtn,
    FlipDirHover,
    CutAt,
    FirstPage,
    SecondPage,
    NoFileOpen,
    BuildError,
    BakError,
    WriteError,
    ReplaceError,
    Overwritten,
    CopySaved,
    EditedSuffix,
    ThumbHover,
    MenuEnlarge,
    MenuSplit,
    MenuMerge,
    MenuInsertBefore,
    MenuInsertAfter,
    MenuMoveStart,
    MenuMoveEnd,
    MenuExtract,
    MenuMarkIdentical,
    MenuMarkIdenticalHover,
    Illegible,
    KeepHover,
    DeleteHover,
    CropToCoverHover,
    MoveBeforeHover,
    MoveAfterHover,
    ImageInserted,
    PageMoved,
    ViewerClose,
    ViewerPrev,
    ViewerNext,
    ViewerKeep,
    ViewerDelete,
    ViewerCrop,
    ViewerExtract,
    IllegibleViewer,
    DropToPage1,
    DropCbzHere,
    EmptyState,
    ToastImageAdded,
    ToastImagesAdded,
    ConfirmTitle,
    ConfirmLine1,
    ConfirmLine2,
    ConfirmYes,
    CancelPlain,
    ReadError,
    Undone,
    Redone,
    Undo,
    Redo,
    UndoHover,
    RedoHover,
    CoverRemoved,
    RemoveCoverHover,
    NoPagesToSave,
    OverwrittenBakKept,
    Unsaved,
    SaveCopyHover,
    KeyEnter,
    KeyEsc,
    KeyDel,
    NoImages,
    MenuKeep,
    MenuDelete,
    MenuCrop,
    MarkedForDeletion,
    ZoomHover,
    DropOnlyInGrid,
    DropNothingUsable,
    DiscardTitle,
    DiscardLine,
    DiscardYes,
    SaveCopyFirst,
    Interrupted,
    OpeningProgress,
}

fn tr(lang: Lang, key: T) -> &'static str {
    let i = lang as usize;
    match key {
        T::WindowTitle => ["Éditeur CBZ — pages & couverture", "CBZ Editor — pages & cover", "Editor CBZ — páginas y portada", "CBZ-Editor — Seiten & Cover", "Editor CBZ — pagine e copertina"][i],
        T::OpenCbz => ["📂 Ouvrir un CBZ…", "📂 Open a CBZ…", "📂 Abrir un CBZ…", "📂 CBZ öffnen…", "📂 Apri un CBZ…"][i],
        T::OpenHintNoFile => ["Clique « Ouvrir un CBZ… » ou glisse un .cbz sur la fenêtre.", "Click “Open a CBZ…” or drop a .cbz onto the window.", "Haz clic en «Abrir un CBZ…» o arrastra un .cbz a la ventana.", "Auf „CBZ öffnen…“ klicken oder eine .cbz ins Fenster ziehen.", "Clicca «Apri un CBZ…» o trascina un .cbz nella finestra."][i],
        T::LangHover => ["Langue de l'interface", "Interface language", "Idioma de la interfaz", "Sprache der Oberfläche", "Lingua dell'interfaccia"][i],
        T::PagesCount => ["{n} pages", "{n} pages", "{n} páginas", "{n} Seiten", "{n} pagine"][i],
        T::ToDelete => ["· {n} à supprimer", "· {n} to delete", "· {n} a eliminar", "· {n} zu löschen", "· {n} da eliminare"][i],
        T::OrderChanged => ["· ordre modifié", "· order changed", "· orden modificado", "· Reihenfolge geändert", "· ordine modificato"][i],
        T::CoverFlag => ["· couverture ✔", "· cover ✔", "· portada ✔", "· Cover ✔", "· copertina ✔"][i],
        T::MarkDuplicates => ["🗐 Marquer les doublons", "🗐 Mark duplicates", "🗐 Marcar duplicados", "🗐 Duplikate markieren", "🗐 Segna i doppioni"][i],
        T::MarkDuplicatesHover => ["Coche pour suppression toutes les pages au contenu strictement identique (toutes les copies, sans en garder une).", "Mark for deletion every page with byte-identical content (all copies, keeping none).", "Marca para eliminar todas las páginas con contenido idéntico (todas las copias, sin conservar ninguna).", "Markiert alle Seiten mit byte-identischem Inhalt zum Löschen (alle Kopien, keine bleibt übrig).", "Seleziona per l'eliminazione tutte le pagine con contenuto identico (tutte le copie, senza tenerne nessuna)."][i],
        T::DirRtl => ["⬅ Lecture : droite à gauche (manga)", "⬅ Reading: right to left (manga)", "⬅ Lectura: derecha a izquierda (manga)", "⬅ Lesen: rechts nach links (Manga)", "⬅ Lettura: da destra a sinistra (manga)"][i],
        T::DirLtr => ["➡ Lecture : gauche à droite", "➡ Reading: left to right", "➡ Lectura: izquierda a derecha", "➡ Lesen: links nach rechts", "➡ Lettura: da sinistra a destra"][i],
        T::DirHover => ["Sens de lecture, utilisé pour fusionner/découper les doubles-pages. Cliquer pour inverser.", "Reading direction, used when merging/splitting double pages. Click to flip.", "Sentido de lectura, usado al fusionar/dividir páginas dobles. Clic para invertir.", "Leserichtung, beim Zusammenführen/Teilen von Doppelseiten verwendet. Zum Umkehren klicken.", "Senso di lettura, usato per unire/dividere le pagine doppie. Clic per invertire."][i],
        T::SaveCopy => ["💾 Enregistrer une copie", "💾 Save a copy", "💾 Guardar una copia", "💾 Kopie speichern", "💾 Salva una copia"][i],
        T::Overwrite => ["⚠ Écraser l'original", "⚠ Overwrite the original", "⚠ Sobrescribir el original", "⚠ Original überschreiben", "⚠ Sovrascrivi l'originale"][i],
        T::OverwriteHover => ["Remplace le .cbz d'origine (sauvegarde .bak automatique)", "Replace the original .cbz (automatic .bak backup)", "Reemplaza el .cbz original (copia .bak automática)", "Ersetzt die originale .cbz (automatisches .bak-Backup)", "Sostituisce il .cbz originale (backup .bak automatico)"][i],
        T::OpenedNPages => ["Ouvert : {n} pages.", "Opened: {n} pages.", "Abierto: {n} páginas.", "Geöffnet: {n} Seiten.", "Aperto: {n} pagine."][i],
        T::OpenError => ["Erreur d'ouverture : {e}", "Open error: {e}", "Error al abrir: {e}", "Fehler beim Öffnen: {e}", "Errore di apertura: {e}"][i],
        T::ZipEntryTooBig => ["entrée « {name} » trop volumineuse une fois décompressée (bombe zip ?).", "entry “{name}” too large once decompressed (zip bomb?).", "entrada «{name}» demasiado grande al descomprimir (¿bomba zip?).", "Eintrag „{name}“ ist entpackt zu groß (Zip-Bombe?).", "voce «{name}» troppo grande una volta decompressa (zip bomb?)."][i],
        T::ZipTotalTooBig => ["archive trop volumineuse une fois décompressée — ouverture interrompue.", "archive too large once decompressed — opening aborted.", "archivo demasiado grande al descomprimir — apertura cancelada.", "Archiv ist entpackt zu groß — Öffnen abgebrochen.", "archivio troppo grande una volta decompresso — apertura interrotta."][i],
        T::DlgComicFilter => ["Bande dessinée (cbz, zip)", "Comic Book (cbz, zip)", "Cómic (cbz, zip)", "Comic (cbz, zip)", "Fumetto (cbz, zip)"][i],
        T::DlgOpenTitle => ["Ouvrir un CBZ", "Open a CBZ", "Abrir un CBZ", "CBZ öffnen", "Apri un CBZ"][i],
        T::DlgImagesFilter => ["Images (jpg, png, webp, gif, bmp)", "Images (jpg, png, webp, gif, bmp)", "Imágenes (jpg, png, webp, gif, bmp)", "Bilder (jpg, png, webp, gif, bmp)", "Immagini (jpg, png, webp, gif, bmp)"][i],
        T::DlgImageInsertTitle => ["Image à insérer", "Image to insert", "Imagen a insertar", "Einzufügendes Bild", "Immagine da inserire"][i],
        T::DlgExtractTitle => ["Extraire l'image vers un fichier", "Extract image to a file", "Extraer la imagen a un archivo", "Bild in eine Datei extrahieren", "Estrai l'immagine in un file"][i],
        T::DlgImageFilter => ["Image", "Image", "Imagen", "Bild", "Immagine"][i],
        T::Extracted => ["🖼 Image extraite : {name}", "🖼 Image extracted: {name}", "🖼 Imagen extraída: {name}", "🖼 Bild extrahiert: {name}", "🖼 Immagine estratta: {name}"][i],
        T::ExtractError => ["Extraction impossible : {e}", "Extraction failed: {e}", "No se pudo extraer: {e}", "Extraktion fehlgeschlagen: {e}", "Estrazione non riuscita: {e}"][i],
        T::NoDuplicates => ["Aucun doublon (contenu identique) détecté.", "No duplicates (identical content) found.", "No se detectaron duplicados (contenido idéntico).", "Keine Duplikate (identischer Inhalt) gefunden.", "Nessun doppione (contenuto identico) trovato."][i],
        T::DupMarked => ["🗐 {marked} page(s) en double cochée(s) pour suppression ({groups} groupe(s)).", "🗐 {marked} duplicate page(s) marked for deletion ({groups} group(s)).", "🗐 {marked} página(s) duplicada(s) marcada(s) para eliminar ({groups} grupo(s)).", "🗐 {marked} doppelte Seite(n) zum Löschen markiert ({groups} Gruppe(n)).", "🗐 {marked} pagina/e doppia/e segnata/e per l'eliminazione ({groups} gruppo/i)."][i],
        T::NoIdenticalCopy => ["Aucune autre copie identique à cette page.", "No other copy identical to this page.", "No hay otra copia idéntica a esta página.", "Keine weitere mit dieser Seite identische Kopie.", "Nessun'altra copia identica a questa pagina."][i],
        T::IdenticalMarked => ["🗐 {n} copies identiques cochées pour suppression.", "🗐 {n} identical copies marked for deletion.", "🗐 {n} copias idénticas marcadas para eliminar.", "🗐 {n} identische Kopien zum Löschen markiert.", "🗐 {n} copie identiche segnate per l'eliminazione."][i],
        T::CropHintEnter => ["Trace un rectangle, déplace-le ou ajuste ses poignées, puis « Valider ».", "Draw a rectangle, move it or adjust its handles, then “Apply”.", "Dibuja un rectángulo, muévelo o ajusta sus tiradores, luego «Aplicar».", "Rechteck ziehen, verschieben oder an den Griffen anpassen, dann „Übernehmen“.", "Disegna un rettangolo, spostalo o regola le maniglie, poi «Applica»."][i],
        T::DecodePageError => ["Impossible de décoder cette page : {e}", "Cannot decode this page: {e}", "No se puede decodificar esta página: {e}", "Diese Seite kann nicht dekodiert werden: {e}", "Impossibile decodificare questa pagina: {e}"][i],
        T::NoSelection => ["Aucune sélection — trace un rectangle ou clique « toute l'image ».", "No selection — draw a rectangle or click “whole image”.", "Sin selección — dibuja un rectángulo o pulsa «toda la imagen».", "Keine Auswahl — Rechteck ziehen oder „ganzes Bild“ klicken.", "Nessuna selezione — disegna un rettangolo o clicca «tutta l'immagine»."][i],
        T::CoverEncodeError => ["Encodage de la couverture impossible : {e}", "Cover encoding failed: {e}", "No se pudo codificar la portada: {e}", "Cover-Kodierung fehlgeschlagen: {e}", "Codifica della copertina non riuscita: {e}"][i],
        T::CoverReadyStatus => ["Couverture recadrée prête ✔ — enregistre pour l'appliquer.", "Cropped cover ready ✔ — save to apply it.", "Portada recortada lista ✔ — guarda para aplicarla.", "Zugeschnittenes Cover bereit ✔ — zum Anwenden speichern.", "Copertina ritagliata pronta ✔ — salva per applicarla."][i],
        T::CropTitle => ["✂ Recadrage couverture — {pname}", "✂ Cover crop — {pname}", "✂ Recorte de portada — {pname}", "✂ Cover-Zuschnitt — {pname}", "✂ Ritaglio copertina — {pname}"][i],
        T::CropHint2 => ["Trace un rectangle, puis déplace-le (centre) ou ajuste les poignées (coins/bords).", "Draw a rectangle, then move it (center) or adjust the handles (corners/edges).", "Dibuja un rectángulo, luego muévelo (centro) o ajusta los tiradores (esquinas/bordes).", "Rechteck ziehen, dann verschieben (Mitte) oder Griffe anpassen (Ecken/Kanten).", "Disegna un rettangolo, poi spostalo (centro) o regola le maniglie (angoli/bordi)."][i],
        T::CropApply => ["✅ Valider le recadrage", "✅ Apply crop", "✅ Aplicar recorte", "✅ Zuschnitt übernehmen", "✅ Applica ritaglio"][i],
        T::CropWhole => ["🖼 Utiliser toute l'image", "🖼 Use the whole image", "🖼 Usar toda la imagen", "🖼 Ganzes Bild verwenden", "🖼 Usa tutta l'immagine"][i],
        T::Cancel => ["✖ Annuler", "✖ Cancel", "✖ Cancelar", "✖ Abbrechen", "✖ Annulla"][i],
        T::SelectionInfo => ["Sélection : {w}×{h} px (coin {x},{y})", "Selection: {w}×{h} px (corner {x},{y})", "Selección: {w}×{h} px (esquina {x},{y})", "Auswahl: {w}×{h} px (Ecke {x},{y})", "Selezione: {w}×{h} px (angolo {x},{y})"][i],
        T::NoNextToMerge => ["Pas de page suivante à fusionner.", "No next page to merge.", "No hay página siguiente para fusionar.", "Keine nächste Seite zum Zusammenführen.", "Nessuna pagina successiva da unire."][i],
        T::DecodePageN => ["Décodage page {page} : {e}", "Decoding page {page}: {e}", "Decodificando la página {page}: {e}", "Dekodierung Seite {page}: {e}", "Decodifica pagina {page}: {e}"][i],
        T::MergeEncodeError => ["Fusion impossible (encodage) : {e}", "Merge failed (encoding): {e}", "No se pudo fusionar (codificación): {e}", "Zusammenführen fehlgeschlagen (Kodierung): {e}", "Unione non riuscita (codifica): {e}"][i],
        T::Merged => ["Pages fusionnées en double-page.", "Pages merged into a double page.", "Páginas fusionadas en una página doble.", "Seiten zu einer Doppelseite zusammengeführt.", "Pagine unite in una pagina doppia."][i],
        T::TooNarrowToSplit => ["Cette image est trop étroite pour être découpée.", "This image is too narrow to split.", "Esta imagen es demasiado estrecha para dividirla.", "Dieses Bild ist zu schmal zum Teilen.", "Questa immagine è troppo stretta per essere divisa."][i],
        T::SplitHintEnter => ["Place la ligne de coupe (glisse), puis « Valider ».", "Place the cut line (drag), then “Apply”.", "Coloca la línea de corte (arrastra), luego «Aplicar».", "Schnittlinie platzieren (ziehen), dann „Übernehmen“.", "Posiziona la linea di taglio (trascina), poi «Applica»."][i],
        T::DecodeError => ["Impossible de décoder : {e}", "Cannot decode: {e}", "No se puede decodificar: {e}", "Kann nicht dekodiert werden: {e}", "Impossibile decodificare: {e}"][i],
        T::SplitEncodeError => ["Découpe impossible (encodage des deux moitiés).", "Split failed (encoding both halves).", "No se pudo dividir (codificación de ambas mitades).", "Teilen fehlgeschlagen (Kodierung beider Hälften).", "Divisione non riuscita (codifica di entrambe le metà)."][i],
        T::SplitDone => ["Page découpée en deux (ordre de lecture respecté).", "Page split in two (reading order kept).", "Página dividida en dos (se respeta el orden de lectura).", "Seite in zwei geteilt (Lesereihenfolge beibehalten).", "Pagina divisa in due (ordine di lettura rispettato)."][i],
        T::SplitTitle => ["📖 Découpe en deux — {pname}", "📖 Split in two — {pname}", "📖 Dividir en dos — {pname}", "📖 In zwei teilen — {pname}", "📖 Dividi in due — {pname}"][i],
        T::SplitHint2 => ["Glisse ou clique pour placer la ligne de coupe (⬅/➡ pour l'ajuster, Maj = pas plus grand). La 1re page lue est surlignée en vert.", "Drag or click to place the cut line (⬅/➡ to fine-tune, Shift = bigger steps). The first page read is highlighted in green.", "Arrastra o haz clic para colocar la línea de corte (⬅/➡ para ajustar, Mayús = pasos mayores). La 1.ª página leída se resalta en verde.", "Ziehen oder klicken, um die Schnittlinie zu setzen (⬅/➡ zum Feinjustieren, Umschalt = größere Schritte). Die zuerst gelesene Seite ist grün hervorgehoben.", "Trascina o clicca per posizionare la linea di taglio (⬅/➡ per regolare, Maiusc = passi più grandi). La 1ª pagina letta è evidenziata in verde."][i],
        T::SplitApply => ["✅ Valider la découpe", "✅ Apply split", "✅ Aplicar división", "✅ Teilung übernehmen", "✅ Applica divisione"][i],
        T::DirRtlSplit => ["droite à gauche (1re = droite)", "right to left (1st = right)", "derecha a izquierda (1.ª = derecha)", "rechts nach links (1. = rechts)", "da destra a sinistra (1ª = destra)"][i],
        T::DirLtrSplit => ["gauche à droite (1re = gauche)", "left to right (1st = left)", "izquierda a derecha (1.ª = izquierda)", "links nach rechts (1. = links)", "da sinistra a destra (1ª = sinistra)"][i],
        T::SplitDirBtn => ["↔ Sens : {dir}", "↔ Direction: {dir}", "↔ Sentido: {dir}", "↔ Richtung: {dir}", "↔ Senso: {dir}"][i],
        T::FlipDirHover => ["Inverser le sens de lecture", "Flip the reading direction", "Invertir el sentido de lectura", "Leserichtung umkehren", "Inverti il senso di lettura"][i],
        T::CutAt => ["Coupe à x = {cut} / {width}", "Cut at x = {cut} / {width}", "Corte en x = {cut} / {width}", "Schnitt bei x = {cut} / {width}", "Taglio a x = {cut} / {width}"][i],
        T::FirstPage => ["1re", "1st", "1.ª", "1.", "1ª"][i],
        T::SecondPage => ["2e", "2nd", "2.ª", "2.", "2ª"][i],
        T::NoFileOpen => ["Aucun fichier ouvert.", "No file open.", "Ningún archivo abierto.", "Keine Datei geöffnet.", "Nessun file aperto."][i],
        T::BuildError => ["Erreur de construction : {e}", "Build error: {e}", "Error de construcción: {e}", "Erstellungsfehler: {e}", "Errore di costruzione: {e}"][i],
        T::BakError => ["Sauvegarde .bak impossible : {e}", ".bak backup failed: {e}", "No se pudo crear la copia .bak: {e}", ".bak-Backup fehlgeschlagen: {e}", "Backup .bak non riuscito: {e}"][i],
        T::WriteError => ["Écriture impossible : {e}", "Write failed: {e}", "No se pudo escribir: {e}", "Schreiben fehlgeschlagen: {e}", "Scrittura non riuscita: {e}"][i],
        T::ReplaceError => ["Remplacement impossible : {e}", "Replace failed: {e}", "No se pudo reemplazar: {e}", "Ersetzen fehlgeschlagen: {e}", "Sostituzione non riuscita: {e}"][i],
        T::Overwritten => ["✅ Original écrasé ({kept} pages). Sauvegarde .bak créée.", "✅ Original overwritten ({kept} pages). .bak backup created.", "✅ Original sobrescrito ({kept} páginas). Copia .bak creada.", "✅ Original überschrieben ({kept} Seiten). .bak-Backup erstellt.", "✅ Originale sovrascritto ({kept} pagine). Backup .bak creato."][i],
        T::CopySaved => ["✅ Copie enregistrée : {name}", "✅ Copy saved: {name}", "✅ Copia guardada: {name}", "✅ Kopie gespeichert: {name}", "✅ Copia salvata: {name}"][i],
        T::EditedSuffix => ["édité", "edited", "editado", "bearbeitet", "modificato"][i],
        T::ThumbHover => ["Clic = agrandir · glisser = déplacer · clic droit = plus d'options", "Click = enlarge · drag = move · right-click = more options", "Clic = ampliar · arrastrar = mover · clic derecho = más opciones", "Klick = vergrößern · Ziehen = verschieben · Rechtsklick = weitere Optionen", "Clic = ingrandisci · trascina = sposta · clic destro = altre opzioni"][i],
        T::MenuEnlarge => ["🔍 Voir en grand", "🔍 View large", "🔍 Ver en grande", "🔍 Groß ansehen", "🔍 Vedi in grande"][i],
        T::MenuSplit => ["📖 Découper en deux pages…", "📖 Split into two pages…", "📖 Dividir en dos páginas…", "📖 In zwei Seiten teilen…", "📖 Dividi in due pagine…"][i],
        T::MenuMerge => ["🔗 Fusionner avec la suivante", "🔗 Merge with next", "🔗 Fusionar con la siguiente", "🔗 Mit nächster zusammenführen", "🔗 Unisci con la successiva"][i],
        T::MenuInsertBefore => ["➕ Insérer une image AVANT", "➕ Insert an image BEFORE", "➕ Insertar una imagen ANTES", "➕ Bild DAVOR einfügen", "➕ Inserisci un'immagine PRIMA"][i],
        T::MenuInsertAfter => ["➕ Insérer une image APRÈS", "➕ Insert an image AFTER", "➕ Insertar una imagen DESPUÉS", "➕ Bild DANACH einfügen", "➕ Inserisci un'immagine DOPO"][i],
        T::MenuMoveStart => ["⏮ Déplacer au début", "⏮ Move to start", "⏮ Mover al principio", "⏮ An den Anfang verschieben", "⏮ Sposta all'inizio"][i],
        T::MenuMoveEnd => ["⏭ Déplacer à la fin", "⏭ Move to end", "⏭ Mover al final", "⏭ Ans Ende verschieben", "⏭ Sposta alla fine"][i],
        T::MenuExtract => ["💾 Extraire l'image (fichier)…", "💾 Extract image (file)…", "💾 Extraer la imagen (archivo)…", "💾 Bild extrahieren (Datei)…", "💾 Estrai l'immagine (file)…"][i],
        T::MenuMarkIdentical => ["🗐 Cocher les copies identiques", "🗐 Mark identical copies", "🗐 Marcar las copias idénticas", "🗐 Identische Kopien markieren", "🗐 Segna le copie identiche"][i],
        T::MenuMarkIdenticalHover => ["Coche pour suppression toutes les pages au contenu identique à celle-ci (cette page comprise).", "Mark for deletion all pages with content identical to this one (this page included).", "Marca para eliminar todas las páginas con contenido idéntico a esta (incluida esta).", "Markiert alle Seiten mit zu dieser identischem Inhalt zum Löschen (diese Seite eingeschlossen).", "Seleziona per l'eliminazione tutte le pagine identiche a questa (questa inclusa)."][i],
        T::Illegible => ["⚠\nillisible", "⚠\nunreadable", "⚠\nilegible", "⚠\nunlesbar", "⚠\nilleggibile"][i],
        T::KeepHover => ["Garder", "Keep", "Conservar", "Behalten", "Mantieni"][i],
        T::DeleteHover => ["Supprimer", "Delete", "Eliminar", "Löschen", "Elimina"][i],
        T::CropToCoverHover => ["Recadrer en couverture", "Crop to cover", "Recortar como portada", "Als Cover zuschneiden", "Ritaglia come copertina"][i],
        T::MoveBeforeHover => ["Déplacer avant", "Move before", "Mover antes", "Nach vorne verschieben", "Sposta prima"][i],
        T::MoveAfterHover => ["Déplacer après", "Move after", "Mover después", "Nach hinten verschieben", "Sposta dopo"][i],
        T::ImageInserted => ["Image insérée. (À l'enregistrement, les pages seront renumérotées dans le nouvel ordre.)", "Image inserted. (On save, pages will be renumbered in the new order.)", "Imagen insertada. (Al guardar, las páginas se renumerarán en el nuevo orden.)", "Bild eingefügt. (Beim Speichern werden die Seiten in der neuen Reihenfolge neu nummeriert.)", "Immagine inserita. (Al salvataggio, le pagine saranno rinumerate nel nuovo ordine.)"][i],
        T::PageMoved => ["Page déplacée en position {n}.", "Page moved to position {n}.", "Página movida a la posición {n}.", "Seite an Position {n} verschoben.", "Pagina spostata in posizione {n}."][i],
        T::ViewerClose => ["✖ Fermer (Échap)", "✖ Close (Esc)", "✖ Cerrar (Esc)", "✖ Schließen (Esc)", "✖ Chiudi (Esc)"][i],
        T::ViewerPrev => ["◀ Précédente", "◀ Previous", "◀ Anterior", "◀ Zurück", "◀ Precedente"][i],
        T::ViewerNext => ["Suivante ▶", "Next ▶", "Siguiente ▶", "Weiter ▶", "Successiva ▶"][i],
        T::ViewerKeep => ["↩ Garder", "↩ Keep", "↩ Conservar", "↩ Behalten", "↩ Mantieni"][i],
        T::ViewerDelete => ["🗑 Supprimer", "🗑 Delete", "🗑 Eliminar", "🗑 Löschen", "🗑 Elimina"][i],
        T::ViewerCrop => ["✂ Couverture", "✂ Cover", "✂ Portada", "✂ Cover", "✂ Copertina"][i],
        T::ViewerExtract => ["💾 Extraire", "💾 Extract", "💾 Extraer", "💾 Extrahieren", "💾 Estrai"][i],
        T::IllegibleViewer => ["⚠ Image illisible (format non pris en charge ou fichier corrompu).", "⚠ Unreadable image (unsupported format or corrupt file).", "⚠ Imagen ilegible (formato no compatible o archivo dañado).", "⚠ Unlesbares Bild (nicht unterstütztes Format oder beschädigte Datei).", "⚠ Immagine illeggibile (formato non supportato o file danneggiato)."][i],
        T::DropToPage1 => ["Lâcher : l'image sera ajoutée en page 1", "Drop: the image will be added as page 1", "Suelta: la imagen se añadirá como página 1", "Loslassen: das Bild wird als Seite 1 hinzugefügt", "Rilascia: l'immagine sarà aggiunta come pagina 1"][i],
        T::DropCbzHere => ["Déposez un .cbz ici", "Drop a .cbz here", "Suelta un .cbz aquí", "Eine .cbz hier ablegen", "Rilascia un .cbz qui"][i],
        T::EmptyState => ["Glisse un .cbz ici, ou clique « 📂 Ouvrir un CBZ… »", "Drop a .cbz here, or click “📂 Open a CBZ…”", "Arrastra un .cbz aquí o haz clic en «📂 Abrir un CBZ…»", "Eine .cbz hierher ziehen oder „📂 CBZ öffnen…“ klicken", "Trascina un .cbz qui o clicca «📂 Apri un CBZ…»"][i],
        T::ToastImageAdded => ["🖼 Image ajoutée en page 1 — renumérotation à l'enregistrement.", "🖼 Image added as page 1 — renumbering on save.", "🖼 Imagen añadida como página 1 — renumeración al guardar.", "🖼 Bild als Seite 1 hinzugefügt — Neunummerierung beim Speichern.", "🖼 Immagine aggiunta come pagina 1 — rinumerazione al salvataggio."][i],
        T::ToastImagesAdded => ["🖼 {count} images ajoutées en tête (pages 1–{count}) — renumérotation à l'enregistrement.", "🖼 {count} images added at the start (pages 1–{count}) — renumbering on save.", "🖼 {count} imágenes añadidas al principio (páginas 1–{count}) — renumeración al guardar.", "🖼 {count} Bilder am Anfang hinzugefügt (Seiten 1–{count}) — Neunummerierung beim Speichern.", "🖼 {count} immagini aggiunte all'inizio (pagine 1–{count}) — rinumerazione al salvataggio."][i],
        T::ConfirmTitle => ["Confirmer l'écrasement", "Confirm overwrite", "Confirmar sobrescritura", "Überschreiben bestätigen", "Conferma sovrascrittura"][i],
        T::ConfirmLine1 => ["Le .cbz original va être remplacé.", "The original .cbz will be replaced.", "El .cbz original será reemplazado.", "Die originale .cbz wird ersetzt.", "Il .cbz originale sarà sostituito."][i],
        T::ConfirmLine2 => ["Une sauvegarde « .cbz.bak » de l'original est créée automatiquement (une seule fois).", "A “.cbz.bak” backup of the original is created automatically (once).", "Se crea automáticamente una copia «.cbz.bak» del original (una sola vez).", "Ein „.cbz.bak“-Backup des Originals wird automatisch erstellt (einmalig).", "Un backup «.cbz.bak» dell'originale viene creato automaticamente (una sola volta)."][i],
        T::ConfirmYes => ["Oui, écraser", "Yes, overwrite", "Sí, sobrescribir", "Ja, überschreiben", "Sì, sovrascrivi"][i],
        T::CancelPlain => ["Annuler", "Cancel", "Cancelar", "Abbrechen", "Annulla"][i],
        T::ReadError => ["Lecture impossible : {e}", "Cannot read: {e}", "No se puede leer: {e}", "Lesen fehlgeschlagen: {e}", "Lettura non riuscita: {e}"][i],
        T::Undone => ["⟲ Action défaite.", "⟲ Action undone.", "⟲ Acción deshecha.", "⟲ Aktion rückgängig gemacht.", "⟲ Azione annullata."][i],
        T::Redone => ["⟳ Action refaite.", "⟳ Action redone.", "⟳ Acción rehecha.", "⟳ Aktion wiederhergestellt.", "⟳ Azione ripristinata."][i],
        T::Undo => ["⟲ Défaire", "⟲ Undo", "⟲ Deshacer", "⟲ Rückgängig", "⟲ Annulla"][i],
        T::Redo => ["⟳ Refaire", "⟳ Redo", "⟳ Rehacer", "⟳ Wiederholen", "⟳ Ripeti"][i],
        T::UndoHover => ["Défaire la dernière action (Ctrl+Z)", "Undo the last action (Ctrl+Z)", "Deshacer la última acción (Ctrl+Z)", "Letzte Aktion rückgängig machen (Strg+Z)", "Annulla l'ultima azione (Ctrl+Z)"][i],
        T::RedoHover => ["Refaire l'action défaite (Ctrl+Y)", "Redo the undone action (Ctrl+Y)", "Rehacer la acción deshecha (Ctrl+Y)", "Rückgängig gemachte Aktion wiederholen (Strg+Y)", "Ripeti l'azione annullata (Ctrl+Y)"][i],
        T::CoverRemoved => ["Couverture recadrée retirée.", "Cropped cover removed.", "Portada recortada eliminada.", "Zugeschnittenes Cover entfernt.", "Copertina ritagliata rimossa."][i],
        T::RemoveCoverHover => ["Retirer la couverture recadrée", "Remove the cropped cover", "Quitar la portada recortada", "Zugeschnittenes Cover entfernen", "Rimuovi la copertina ritagliata"][i],
        T::NoPagesToSave => ["rien à enregistrer : toutes les pages sont marquées pour suppression.", "nothing to save: every page is marked for deletion.", "nada que guardar: todas las páginas están marcadas para eliminar.", "nichts zu speichern: alle Seiten sind zum Löschen markiert.", "niente da salvare: tutte le pagine sono segnate per l'eliminazione."][i],
        T::OverwrittenBakKept => ["✅ Original écrasé ({kept} pages). La sauvegarde .bak d'origine est conservée.", "✅ Original overwritten ({kept} pages). The original .bak backup is kept.", "✅ Original sobrescrito ({kept} páginas). Se conserva la copia .bak original.", "✅ Original überschrieben ({kept} Seiten). Das ursprüngliche .bak-Backup bleibt erhalten.", "✅ Originale sovrascritto ({kept} pagine). Il backup .bak originale è conservato."][i],
        T::Unsaved => ["• non enregistré", "• unsaved", "• sin guardar", "• nicht gespeichert", "• non salvato"][i],
        T::SaveCopyHover => ["Enregistre « nom (édité).cbz » à côté de l'original, sans le toucher (Ctrl+S)", "Save “name (edited).cbz” next to the original, leaving it untouched (Ctrl+S)", "Guarda «nombre (editado).cbz» junto al original, sin modificarlo (Ctrl+S)", "Speichert „Name (bearbeitet).cbz“ neben dem Original, ohne es zu ändern (Strg+S)", "Salva «nome (modificato).cbz» accanto all'originale, senza toccarlo (Ctrl+S)"][i],
        T::KeyEnter => ["Entrée", "Enter", "Intro", "Eingabe", "Invio"][i],
        T::KeyEsc => ["Échap", "Esc", "Esc", "Esc", "Esc"][i],
        T::KeyDel => ["Suppr", "Delete", "Supr", "Entf", "Canc"][i],
        T::NoImages => ["Aucune image dans cette archive.", "No images in this archive.", "No hay imágenes en este archivo.", "Keine Bilder in diesem Archiv.", "Nessuna immagine in questo archivio."][i],
        T::MenuKeep => ["↩ Garder cette page", "↩ Keep this page", "↩ Conservar esta página", "↩ Diese Seite behalten", "↩ Mantieni questa pagina"][i],
        T::MenuDelete => ["🗑 Marquer pour suppression", "🗑 Mark for deletion", "🗑 Marcar para eliminar", "🗑 Zum Löschen markieren", "🗑 Segna per l'eliminazione"][i],
        T::MenuCrop => ["✂ Recadrer en couverture…", "✂ Crop to cover…", "✂ Recortar como portada…", "✂ Als Cover zuschneiden…", "✂ Ritaglia come copertina…"][i],
        T::MarkedForDeletion => ["Page marquée pour suppression", "Page marked for deletion", "Página marcada para eliminar", "Seite zum Löschen markiert", "Pagina segnata per l'eliminazione"][i],
        T::ZoomHover => ["Clic : taille réelle ↔ ajuster à la fenêtre", "Click: actual size ↔ fit to window", "Clic: tamaño real ↔ ajustar a la ventana", "Klick: Originalgröße ↔ an Fenster anpassen", "Clic: dimensione reale ↔ adatta alla finestra"][i],
        T::DropOnlyInGrid => ["Pour insérer une image, revenir d'abord à la grille des pages.", "To insert an image, first go back to the page grid.", "Para insertar una imagen, vuelve primero a la cuadrícula de páginas.", "Um ein Bild einzufügen, zuerst zur Seitenübersicht zurückkehren.", "Per inserire un'immagine, torna prima alla griglia delle pagine."][i],
        T::DropNothingUsable => ["Aucune image utilisable dans le dépôt (formats : jpg, png, webp, gif, bmp).", "No usable image in the drop (formats: jpg, png, webp, gif, bmp).", "Ninguna imagen utilizable (formatos: jpg, png, webp, gif, bmp).", "Kein verwendbares Bild abgelegt (Formate: jpg, png, webp, gif, bmp).", "Nessuna immagine utilizzabile (formati: jpg, png, webp, gif, bmp)."][i],
        T::DiscardTitle => ["Modifications non enregistrées", "Unsaved changes", "Cambios sin guardar", "Nicht gespeicherte Änderungen", "Modifiche non salvate"][i],
        T::DiscardLine => ["Les modifications en cours seront perdues.", "Your current changes will be lost.", "Se perderán los cambios actuales.", "Die aktuellen Änderungen gehen verloren.", "Le modifiche in corso andranno perse."][i],
        T::DiscardYes => ["Abandonner les modifications", "Discard changes", "Descartar los cambios", "Änderungen verwerfen", "Scarta le modifiche"][i],
        T::SaveCopyFirst => ["💾 Enregistrer une copie d'abord", "💾 Save a copy first", "💾 Guardar una copia primero", "💾 Zuerst eine Kopie speichern", "💾 Salva prima una copia"][i],
        T::Interrupted => ["lecture interrompue (erreur interne).", "reading interrupted (internal error).", "lectura interrumpida (error interno).", "Lesen abgebrochen (interner Fehler).", "lettura interrotta (errore interno)."][i],
        T::OpeningProgress => ["Ouverture de « {name} »… {done} / {total}", "Opening “{name}”… {done} / {total}", "Abriendo «{name}»… {done} / {total}", "„{name}“ wird geöffnet… {done} / {total}", "Apertura di «{name}»… {done} / {total}"][i],
    }
}

// Comparaison « naturelle » de deux noms : les suites de chiffres sont comparées
// comme des nombres (« p2 » < « p10 »), le reste octet par octet en minuscules.
// Sans risque de débordement (les nombres sont comparés comme des chaînes).
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let (na, nb) = (&a[si..i], &b[sj..j]);
            let (ta, tb) = (trim_leading_zeros(na), trim_leading_zeros(nb));
            let by_value = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
            match by_value {
                // Même valeur numérique : départager par longueur brute (zéros de tête).
                Ordering::Equal => match na.len().cmp(&nb.len()) {
                    Ordering::Equal => {}
                    o => return o,
                },
                o => return o,
            }
        } else {
            match a[i].to_ascii_lowercase().cmp(&b[j].to_ascii_lowercase()) {
                Ordering::Equal => {
                    i += 1;
                    j += 1;
                }
                o => return o,
            }
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

fn trim_leading_zeros(s: &[u8]) -> &[u8] {
    let mut k = 0;
    while k + 1 < s.len() && s[k] == b'0' {
        k += 1;
    }
    &s[k..]
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

// Copie réduite d'une image pour l'affichage GPU si une dimension dépasse `max`
// (évite de dépasser la taille de texture maximale du pilote sur les pages très hautes).
fn display_copy(img: &image::DynamicImage, max: u32) -> image::DynamicImage {
    if img.width() > max || img.height() > max {
        img.thumbnail(max, max)
    } else {
        img.clone()
    }
}

// Empreinte rapide du contenu d'une page, pour regrouper les doublons.
fn content_hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

fn short_name(name: &str) -> String {
    let base = name.rsplit('/').next().unwrap_or(name);
    if base.chars().count() > 20 {
        let tail: String = base
            .chars()
            .rev()
            .take(19)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{tail}")
    } else {
        base.to_string()
    }
}

// Source sans perte (PNG, BMP, GIF) : une image recalculée à partir d'elle reste
// en PNG. Sinon JPEG q92 : on ne dégrade pas un PNG, et on n'alourdit pas un JPEG.
fn is_lossless(bytes: &[u8]) -> bool {
    matches!(
        image::guess_format(bytes),
        Ok(image::ImageFormat::Png | image::ImageFormat::Bmp | image::ImageFormat::Gif)
    )
}

// Encode une page recalculée ; renvoie les octets et l'extension à utiliser.
fn encode_page(
    img: &image::DynamicImage,
    lossless: bool,
) -> Result<(Vec<u8>, &'static str), String> {
    if lossless {
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        Ok((out.into_inner(), "png"))
    } else {
        encode_jpeg(img, 92).map(|b| (b, "jpg"))
    }
}

fn encode_jpeg(img: &image::DynamicImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut out = Cursor::new(Vec::new());
    // JPEG n'a pas de transparence : aplatir sur fond blanc (sinon les zones
    // transparentes d'un PNG deviendraient noires).
    let rgb = if img.color().has_alpha() {
        let mut rgba = img.to_rgba8();
        for px in rgba.pixels_mut() {
            let a = px[3] as u32;
            for c in 0..3 {
                px[c] = ((px[c] as u32 * a + 255 * (255 - a)) / 255) as u8;
            }
        }
        image::DynamicImage::ImageRgba8(rgba).to_rgb8()
    } else {
        img.to_rgb8()
    };
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&rgb)
        .map_err(|e| e.to_string())?;
    Ok(out.into_inner())
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
    // Travail au niveau octets : ne pas altérer un XML non‑UTF‑8 (UTF‑16, etc.).
    let open = b"<PageCount>";
    let close = b"</PageCount>";
    if let (Some(a), Some(b)) = (find_bytes(xml, open), find_bytes(xml, close)) {
        if b >= a + open.len() {
            let mut out = Vec::with_capacity(xml.len());
            out.extend_from_slice(&xml[..a + open.len()]);
            out.extend_from_slice(n.to_string().as_bytes());
            out.extend_from_slice(&xml[b..]);
            return out;
        }
    }
    xml.to_vec()
}

// Retire le bloc <Pages>…</Pages> (ou <Pages/>) d'un ComicInfo, au niveau octets.
// Un XML non‑UTF‑8 (UTF‑16…) ne contient pas ces motifs ASCII : laissé intact.
fn strip_pages_block(xml: &[u8]) -> Vec<u8> {
    let Some(a) = find_bytes(xml, b"<Pages") else {
        return xml.to_vec();
    };
    // « <Pages » doit être suivi de « > », « / » ou d'un blanc (pas « <PagesFoo »).
    let after = xml.get(a + 6).copied().unwrap_or(b'>');
    if !(after == b'>' || after == b'/' || after.is_ascii_whitespace()) {
        return xml.to_vec();
    }
    let Some(gt) = find_bytes(&xml[a..], b">").map(|k| a + k) else {
        return xml.to_vec();
    };
    let end = if xml[gt - 1] == b'/' {
        gt + 1
    } else {
        match find_bytes(&xml[a..], b"</Pages>") {
            Some(k) => a + k + b"</Pages>".len(),
            None => return xml.to_vec(),
        }
    };
    // Supprimer aussi l'indentation qui précède et le saut de ligne qui suit.
    let mut start = a;
    while start > 0 && (xml[start - 1] == b' ' || xml[start - 1] == b'\t') {
        start -= 1;
    }
    let mut stop = end;
    if xml.get(stop) == Some(&b'\r') {
        stop += 1;
    }
    if xml.get(stop) == Some(&b'\n') {
        stop += 1;
    }
    let mut out = Vec::with_capacity(xml.len());
    out.extend_from_slice(&xml[..start]);
    out.extend_from_slice(&xml[stop..]);
    out
}

// Écrit puis force l'écriture sur disque (fsync) avant de rendre la main : un
// « rename » atomique ne protège rien si le contenu n'est pas encore sur le disque.
fn write_synced(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(data)?;
    f.sync_all()
}

// Nom de fichier de couverture (« 000_cover.jpg »…), dossier éventuel ignoré.
fn is_cover_name(name: &str) -> bool {
    name.rsplit('/')
        .next()
        .unwrap_or(name)
        .to_lowercase()
        .contains("000_cover")
}

// Extension (en minuscules) d'un nom de page, « jpg » par défaut.
fn ext_of(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_else(|| "jpg".into())
}

fn config_path(name: &str) -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(dir.join("cbz-editor").join(name))
}

fn config_file() -> Option<PathBuf> {
    config_path("last_dir")
}

fn load_lang() -> Option<Lang> {
    let s = std::fs::read_to_string(config_path("lang")?).ok()?;
    Lang::from_code(s.trim())
}

fn save_lang(lang: Lang) {
    if let Some(p) = config_path("lang") {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(p, lang.code());
    }
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

// Tests de la logique hors interface (construction du CBZ, ComicInfo, tri, Défaire).
#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32, rgba: [u8; 4]) -> Arc<[u8]> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba(rgba));
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner().into()
    }

    fn app(names: &[&str], ci: Option<&str>) -> CbzApp {
        let mut a = CbzApp {
            path: Some(PathBuf::from("/tmp/x.cbz")),
            ..Default::default()
        };
        for (i, n) in names.iter().enumerate() {
            a.pages.push(Page::new(
                n.to_string(),
                png(2, 2, [i as u8, 0, 0, 255]),
                i as u64,
            ));
        }
        a.next_uid = names.len() as u64;
        a.comicinfo = ci.map(|s| s.as_bytes().to_vec());
        a.saved_sig = a.edit_sig();
        a
    }

    fn names(b: &Built) -> Vec<String> {
        b.entries.iter().map(|(n, _)| n.clone()).collect()
    }

    fn zip_names(data: &[u8]) -> Vec<String> {
        let z = zip::ZipArchive::new(Cursor::new(data)).unwrap();
        z.file_names().map(String::from).collect()
    }

    #[test]
    fn tri_naturel() {
        let mut v = vec!["p10.jpg", "p2.jpg", "P1.jpg", "p02.jpg"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["P1.jpg", "p2.jpg", "p02.jpg", "p10.jpg"]);
    }

    #[test]
    fn noms_conserves_sans_modification() {
        let a = app(&["d/001.jpg", "d/002.jpg"], None);
        let b = a.build_cbz().unwrap();
        assert_eq!(names(&b), ["d/001.jpg", "d/002.jpg"]);
        assert_eq!(zip_names(&b.data), ["d/001.jpg", "d/002.jpg"]);
    }

    #[test]
    fn suppression_pagecount_et_bloc_pages() {
        let ci = "<ComicInfo>\n  <PageCount>3</PageCount>\n  <Pages>\n    <Page Image=\"0\"/>\n  </Pages>\n</ComicInfo>";
        let mut a = app(&["1.jpg", "2.jpg", "3.jpg"], Some(ci));
        a.pages[1].delete = true;
        let b = a.build_cbz().unwrap();
        assert_eq!(names(&b), ["1.jpg", "3.jpg"]);
        let xml = String::from_utf8(b.comicinfo.unwrap()).unwrap();
        assert_eq!(xml, "<ComicInfo>\n  <PageCount>2</PageCount>\n</ComicInfo>");
    }

    #[test]
    fn bloc_pages_garde_si_rien_ne_change() {
        let ci = "<ComicInfo><PageCount>1</PageCount><Pages/></ComicInfo>";
        let a = app(&["1.jpg"], Some(ci));
        let xml = String::from_utf8(a.build_cbz().unwrap().comicinfo.unwrap()).unwrap();
        assert!(xml.contains("<Pages/>"));
        assert_eq!(
            strip_pages_block(xml.as_bytes()),
            b"<ComicInfo><PageCount>1</PageCount></ComicInfo>"
        );
        // « <PagesX> » n'est pas un bloc Pages.
        assert_eq!(
            strip_pages_block(b"<PagesX>a</PagesX>"),
            b"<PagesX>a</PagesX>"
        );
    }

    #[test]
    fn reordre_renumerote() {
        let mut a = app(&["a.jpg", "b.png"], None);
        a.pages.swap(0, 1);
        a.dirty_order = true;
        assert_eq!(names(&a.build_cbz().unwrap()), ["p0000.png", "p0001.jpg"]);
    }

    #[test]
    fn couverture_existante_garde_son_extension_et_reste_unique() {
        let a = app(&["000_cover.png", "001.jpg", "x/000_cover.jpg"], None);
        let b = a.build_cbz().unwrap();
        // Une seule page sert de couverture ; l'autre « 000_cover » reste une page.
        assert_eq!(b.entries.len(), 3);
        assert_eq!(b.entries[0], ("000_cover.png".to_string(), Some(0)));
    }

    #[test]
    fn couverture_pas_en_tete_force_la_renumerotation() {
        // « 000.jpg » serait trié AVANT « 000_cover.jpg » : il faut renuméroter.
        let mut a = app(&["000.jpg", "001.jpg"], None);
        let tex_ctx = egui::Context::default();
        let tex = tex_ctx.load_texture(
            "t",
            ColorImage::new([1, 1], Color32::WHITE),
            TextureOptions::LINEAR,
        );
        a.cover = Some(Cover {
            ext: "jpg",
            bytes: png(1, 1, [0, 0, 0, 255]),
            tex,
            id: 99,
        });
        assert_eq!(
            names(&a.build_cbz().unwrap()),
            ["000_cover.jpg", "p0000.jpg", "p0001.jpg"]
        );
    }

    #[test]
    fn tout_supprime_refuse() {
        let mut a = app(&["1.jpg"], None);
        a.pages[0].delete = true;
        assert!(a.build_cbz().is_err());
    }

    #[test]
    fn defaire_refaire_et_modifie() {
        let mut a = app(&["1.jpg", "2.jpg"], None);
        assert!(!a.is_modified());
        a.toggle_delete(0);
        assert!(a.is_modified());
        a.undo();
        assert!(!a.is_modified() && !a.pages[0].delete);
        a.redo();
        assert!(a.pages[0].delete);
        // Recocher puis décocher = retour à l'état enregistré.
        a.toggle_delete(0);
        assert!(!a.is_modified());
    }

    #[test]
    fn deplacement_sur_place_ne_modifie_rien() {
        let mut a = app(&["1.jpg", "2.jpg"], None);
        a.move_page_to(0, 1); // lâchée juste après elle-même
        assert!(!a.dirty_order && a.undo.is_empty());
        a.move_page_to(0, 2);
        assert_eq!(a.pages[1].uid, 0);
        assert!(a.dirty_order);
    }

    #[test]
    fn synchro_apres_ecrasement() {
        let mut a = app(&["1.jpg", "2.jpg", "3.jpg"], None);
        a.pages[0].delete = true;
        a.pages.swap(1, 2);
        a.dirty_order = true;
        let b = a.build_cbz().unwrap();
        a.sync_after_overwrite(b);
        let n: Vec<_> = a.pages.iter().map(|p| (p.name.as_str(), p.uid)).collect();
        assert_eq!(n, [("p0000.jpg", 2), ("p0001.jpg", 1)]);
        assert!(!a.is_modified() && !a.dirty_order);
    }

    #[test]
    fn lecture_ignore_metadonnees_macos() {
        let dir = std::env::temp_dir().join(format!("cbz-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.cbz");
        {
            let mut zw = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            for n in [
                "p10.jpg",
                "p2.jpg",
                "__MACOSX/._p2.jpg",
                "._p3.jpg",
                "notes.txt",
            ] {
                zw.start_file(n, o).unwrap();
                zw.write_all(b"x").unwrap();
            }
            zw.finish().unwrap();
        }
        let (pages, ci) = CbzApp::read_cbz(&path, Lang::Fr, &|_, _| {}).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let n: Vec<_> = pages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(n, ["p2.jpg", "p10.jpg"]);
        assert!(ci.is_none());
    }

    #[test]
    fn jpeg_aplatit_la_transparence_sur_blanc() {
        let img = image::load_from_memory(&png(4, 4, [0, 0, 0, 0])).unwrap();
        let back = image::load_from_memory(&encode_jpeg(&img, 92).unwrap())
            .unwrap()
            .to_rgb8();
        assert!(back.get_pixel(1, 1).0.iter().all(|&c| c > 240));
    }

    fn jpeg(w: u32, h: u32) -> Arc<[u8]> {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            w,
            h,
            image::Rgb([9, 9, 9]),
        ));
        encode_jpeg(&img, 92).unwrap().into()
    }

    #[test]
    fn fusion_garde_le_png_sans_perte() {
        let mut a = app(&["1.png", "2.png"], None);
        a.merge_next(0);
        assert!(a.pages[0].name.ends_with(".png"));
        assert_eq!(
            image::guess_format(&a.pages[0].bytes).unwrap(),
            image::ImageFormat::Png
        );
    }

    #[test]
    fn fusion_jpeg_reste_jpeg() {
        let mut a = app(&["1.jpg", "2.jpg"], None);
        a.pages[0].bytes = jpeg(4, 4);
        a.merge_next(0);
        assert!(a.pages[0].name.ends_with(".jpg"));
        assert_eq!(
            image::guess_format(&a.pages[0].bytes).unwrap(),
            image::ImageFormat::Jpeg
        );
    }

    #[test]
    fn ouverture_en_arriere_plan() {
        let dir = std::env::temp_dir().join(format!("cbz-open-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.cbz");
        {
            let mut zw = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            for n in ["b.png", "a.png"] {
                zw.start_file(n, zip::write::SimpleFileOptions::default())
                    .unwrap();
                zw.write_all(&png(2, 2, [1, 2, 3, 255])).unwrap();
            }
            zw.finish().unwrap();
        }
        let mut a = CbzApp::default();
        a.open_cbz(path.clone());
        for _ in 0..500 {
            a.poll_opening();
            if a.opening.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(a.opening.is_none());
        assert_eq!(a.path.as_deref(), Some(path.as_path()));
        let n: Vec<_> = a.pages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(n, ["a.png", "b.png"]);
    }

    #[test]
    fn agrandissement_precharge_les_voisines() {
        let a = app(&["1.png", "2.png", "3.png", "4.png"], None);
        let ctx = egui::Context::default();
        let mut vs = ViewerState {
            idx: 1,
            tex: None,
            failed: false,
            zoom: false,
            cache: Vec::new(),
            jobs: Vec::new(),
        };
        for _ in 0..500 {
            vs.pump(&ctx, &a.pages);
            if vs.jobs.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(vs.tex.is_some() && !vs.failed);
        let mut uids: Vec<u64> = vs.cache.iter().map(|c| c.0).collect();
        uids.sort();
        assert_eq!(uids, [0, 1, 2]); // précédente, courante, suivante
                                     // Page suivante : déjà prête, et la page 0 sort du cache.
        vs.idx = 2;
        vs.pump(&ctx, &a.pages);
        assert!(vs.tex.is_some());
        assert!(!vs.cache.iter().any(|c| c.0 == 0));
    }

    #[test]
    fn table_de_traduction_complete() {
        // Chaque langue : aucune chaîne vide pour quelques clés témoins.
        for l in Lang::ALL {
            for k in [
                T::WindowTitle,
                T::DiscardTitle,
                T::SaveCopyFirst,
                T::Undo,
                T::KeyDel,
            ] {
                assert!(!tr(l, k).is_empty());
            }
        }
    }
}
