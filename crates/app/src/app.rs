//! Application state and the map screen.

use crate::icons::{self, Icon};
use crate::platform::{self, Drive};
use crate::theme::{self, Palette};
use crate::treemap::{self, tile_rect};
use crate::worker::{LayoutWorker, Request};
use egui::{
    pos2, vec2, Align, Color32, Context, CornerRadius, FontId, Id, Key, Layout as UiLayout, Margin,
    Pos2, Rect, RichText, Sense, Shape, Stroke, StrokeKind, Ui,
};
use serde::{Deserialize, Serialize};
use spacescope_core::fmt::{self, human_count, human_size};
use spacescope_core::{
    flags, kind, rescan_subtree, scan_path, Filter, Layout, LayoutOptions, Metric, NodeId,
    Progress, ScanOptions, Shared, TileKind, Tree, NO_NODE,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

const ANIM_SECS: f64 = 0.26;
/// `(min_tile, min_dir)` per detail level.
const DETAIL: [(f32, f32); 5] = [
    (12.0, 96.0),
    (8.0, 64.0),
    (5.0, 40.0),
    (3.5, 27.0),
    (2.5, 17.0),
];
const FILTER_HELP: &str = "Separate terms with ;\n\n\
    *.mp4          name pattern\n\
    steam          name contains\n\
    >100mb  <2gb   size limits\n\
    >2years        older than\n\
    <30days        newer than\n\
    |*.tmp         exclude";

// ---------------------------------------------------------------- arguments

#[derive(Clone, Default)]
pub struct Args {
    pub path: Option<PathBuf>,
    pub theme: Option<String>,
    pub filter: Option<String>,
    // Test hooks: render one frame to a PNG and exit.
    pub screenshot: Option<PathBuf>,
    pub shot_after: f32,
    pub size: Option<(f32, f32)>,
    pub debug_hover: Option<(f32, f32)>,
    pub zoom: Option<String>,
    pub script: Option<String>,
    pub free: bool,
    pub detail: Option<u8>,
    pub write_assets: Option<PathBuf>,
}

impl Args {
    pub fn parse() -> Result<Args, String> {
        let mut a = Args {
            shot_after: 1.0,
            ..Default::default()
        };
        let mut it = std::env::args().skip(1);
        let pair = |v: &str, sep: char| -> Option<(f32, f32)> {
            let (x, y) = v.split_once(sep)?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        };
        while let Some(arg) = it.next() {
            let mut value = || it.next().ok_or(format!("{arg} needs a value"));
            match arg.as_str() {
                "-h" | "--help" => {
                    return Err(format!(
                        "{} - disk space treemap\n\nUSAGE:\n    spacescope [PATH] [--theme dark|light|system] [--filter EXPR]",
                        crate::APP_NAME
                    ))
                }
                "--theme" => a.theme = Some(value()?),
                "--filter" => a.filter = Some(value()?),
                "--screenshot" => a.screenshot = Some(PathBuf::from(value()?)),
                "--shot-after" => a.shot_after = value()?.parse().map_err(|_| "bad --shot-after")?,
                "--size" => a.size = Some(pair(&value()?, 'x').ok_or("bad --size, expected WxH")?),
                "--debug-hover" => {
                    a.debug_hover = Some(pair(&value()?, ',').ok_or("bad --debug-hover")?)
                }
                "--zoom" => a.zoom = Some(value()?),
                "-V" | "--version" => {
                    return Err(format!("{} {}", crate::APP_NAME, env!("CARGO_PKG_VERSION")))
                }
                "--script" => a.script = Some(value()?),
                "--free" => a.free = true,
                "--detail" => a.detail = value()?.parse().ok(),
                "--write-assets" => a.write_assets = Some(PathBuf::from(value()?)),
                s if s.starts_with("--") => return Err(format!("unknown option {s}")),
                _ => a.path = Some(PathBuf::from(arg)),
            }
        }
        Ok(a)
    }
}

// ----------------------------------------------------------------- settings

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub logical: bool,
    pub detail: u8,
    pub show_free: bool,
    /// 0 = follow the system, 1 = dark, 2 = light.
    pub theme: u8,
    /// Follow changes on disk after a scan.
    pub live: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            logical: false,
            detail: 2,
            show_free: false,
            theme: 0,
            live: true,
        }
    }
}

// ------------------------------------------------------------------ session

#[derive(Clone)]
pub struct HoverInfo {
    tile: usize,
    layout: usize,
    generation: u64,
    at: f64,
    pub node: NodeId,
    pub kind: TileKind,
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    pub alloc: u64,
    pub weight: u64,
    pub parent_weight: u64,
    pub files: u32,
    pub count: u32,
    pub mtime: i64,
    pub is_dir: bool,
    pub flags: u8,
    pub cat: u8,
}

impl HoverInfo {
    fn new(tree: &Tree, layout: &Arc<Layout>, idx: usize, generation: u64, at: f64) -> Self {
        let t = layout.tiles[idx];
        let n = *tree.node(t.node);
        let parent_weight = layout
            .tiles
            .get(t.parent as usize)
            .map_or(t.weight, |p| p.weight);
        let name = match t.kind {
            TileKind::More => format!("{} smaller items", human_count(t.count as u64)),
            TileKind::Free => "Free space".to_string(),
            _ => tree.name_lossy(t.node).into_owned(),
        };
        HoverInfo {
            tile: idx,
            layout: Arc::as_ptr(layout) as usize,
            generation,
            at,
            node: t.node,
            kind: t.kind,
            name,
            path: tree.path(t.node),
            size: n.size,
            alloc: n.alloc,
            weight: t.weight,
            parent_weight,
            files: n.files,
            count: t.count,
            mtime: n.mtime,
            is_dir: n.is_dir(),
            flags: n.flags,
            cat: n.kind,
        }
    }

    fn is_item(&self) -> bool {
        matches!(
            self.kind,
            TileKind::File | TileKind::Dir | TileKind::DirCollapsed
        )
    }
}

#[derive(Clone, Copy, PartialEq)]
struct ReqKey {
    root: NodeId,
    w: u32,
    h: u32,
    detail: u8,
    logical: bool,
    filter: u64,
    free: u64,
    generation: u64,
}

#[derive(Clone, Copy, PartialEq)]
struct MeshKey {
    layout: usize,
    dark: bool,
    origin: (u32, u32),
    ppp: u32,
}

enum AnimKind {
    /// Zooming into a tile of the old layout (rect in layout coordinates).
    In {
        from: Rect,
    },
    /// Zooming out; `to` is where the old root sits in the new layout.
    Out {
        old_root: NodeId,
        to: Option<Rect>,
    },
    Fade,
}

struct Anim {
    kind: AnimKind,
    old_mesh: Arc<egui::Mesh>,
    /// `None` until the new layout has arrived (zoom-out and fades).
    start: Option<f64>,
}

#[derive(Clone, Copy)]
enum Nav {
    To(NodeId),
    Back,
    Forward,
    Up,
}

pub struct Session {
    pub shared: Arc<Shared>,
    pub progress: Arc<Progress>,
    pub volume: Option<(u64, u64)>,
    root_path: PathBuf,
    zoom: NodeId,
    history: Vec<NodeId>,
    hist_pos: usize,
    crumbs: Vec<(NodeId, String)>,
    pub layout: Option<Arc<Layout>>,
    mesh: Option<(Arc<egui::Mesh>, MeshKey)>,
    anim: Option<Anim>,
    labels_at: f64,
    pub inflight: Option<u64>,
    sent_key: Option<ReqKey>,
    sent_at: f64,
    hovered: Option<usize>,
    hover_info: Option<HoverInfo>,
    selected: Option<NodeId>,
    ctx_target: Option<HoverInfo>,
    filter_text: String,
    filter: Option<(u64, Arc<Filter>)>,
    filter_err: Option<String>,
    filter_serial: u64,
    scroll_acc: f32,
    tip_size: egui::Vec2,
    nav_cooldown: f64,
    pending_nav: Option<Nav>,
    totals: (u32, u64, u64),
    totals_gen: u64,
    zoom_applied: bool,
    watch: Option<crate::watch::LiveWatch>,
    /// Why live updates could not start, if they could not.
    watch_off: Option<String>,
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn crumb_names(tree: &Tree, node: NodeId) -> Vec<(NodeId, String)> {
    tree.chain(node)
        .into_iter()
        .map(|id| {
            let mut name = tree.name_lossy(id).into_owned();
            if id == 0 && name.len() > 3 {
                name = name.trim_end_matches(['\\', '/']).to_string();
            }
            (id, name)
        })
        .collect()
}

impl Session {
    fn new(
        shared: Arc<Shared>,
        progress: Arc<Progress>,
        volume: Option<(u64, u64)>,
        root_path: PathBuf,
    ) -> Self {
        let crumbs = crumb_names(&shared.tree.read(), 0);
        Session {
            shared,
            progress,
            volume,
            root_path,
            zoom: 0,
            history: vec![0],
            hist_pos: 0,
            crumbs,
            layout: None,
            mesh: None,
            anim: None,
            labels_at: 0.0,
            inflight: None,
            sent_key: None,
            sent_at: -10.0,
            hovered: None,
            hover_info: None,
            selected: None,
            ctx_target: None,
            filter_text: String::new(),
            filter: None,
            filter_err: None,
            filter_serial: 0,
            scroll_acc: 0.0,
            tip_size: vec2(380.0, 190.0),
            nav_cooldown: 0.0,
            pending_nav: None,
            totals: (0, 0, 0),
            totals_gen: 0,
            zoom_applied: false,
            watch: None,
            watch_off: None,
        }
    }

    fn apply_filter(&mut self) {
        match Filter::parse(&self.filter_text, unix_now()) {
            Ok(f) => {
                self.filter_err = None;
                self.filter_serial += 1;
                self.filter = if f.is_empty() {
                    None
                } else {
                    Some((self.filter_serial, Arc::new(f)))
                };
            }
            // Keep showing the last valid filter while the text is half-typed.
            Err(e) => self.filter_err = Some(e),
        }
    }

    /// Performs a navigation. Returns false if the tree was busy; retry later.
    fn navigate(&mut self, nav: Nav, now: f64) -> bool {
        let Some(tree) = self.shared.tree.try_read_for(Duration::from_millis(4)) else {
            return false;
        };
        let (mut target, record, new_pos) = match nav {
            Nav::To(n) => (n, true, 0),
            Nav::Up => (tree.node(self.zoom).parent, true, 0),
            Nav::Back if self.hist_pos > 0 => {
                (self.history[self.hist_pos - 1], false, self.hist_pos - 1)
            }
            Nav::Forward if self.hist_pos + 1 < self.history.len() => {
                (self.history[self.hist_pos + 1], false, self.hist_pos + 1)
            }
            _ => return true,
        };
        if target == NO_NODE || target as usize >= tree.len() {
            target = 0;
        }
        // Rescans and deletions can retire nodes: fall back to a live folder.
        while target != 0 && (tree.is_detached(target) || !tree.node(target).is_dir()) {
            target = tree.node(target).parent;
        }
        if !record {
            self.hist_pos = new_pos;
        }
        if target == self.zoom {
            return true;
        }
        let old = self.zoom;
        let kind = match &self.layout {
            Some(layout) if tree.is_ancestor(old, target) => match layout.find(target) {
                Some(i) => {
                    let t = &layout.tiles[i];
                    AnimKind::In {
                        from: Rect::from_min_size(pos2(t.x, t.y), vec2(t.w, t.h)),
                    }
                }
                None => AnimKind::Fade,
            },
            Some(_) if tree.is_ancestor(target, old) => AnimKind::Out {
                old_root: old,
                to: None,
            },
            _ => AnimKind::Fade,
        };
        self.crumbs = crumb_names(&tree, target);
        drop(tree);

        match (self.layout.take(), self.mesh.take()) {
            (Some(_), Some((mesh, _))) => {
                let start = matches!(kind, AnimKind::In { .. }).then_some(now);
                self.anim = Some(Anim {
                    kind,
                    old_mesh: mesh,
                    start,
                });
            }
            _ => {
                // Already mid-transition: keep the outgoing picture, cross-fade.
                if let Some(a) = &mut self.anim {
                    a.kind = AnimKind::Fade;
                    a.start = None;
                }
            }
        }
        self.zoom = target;
        if record {
            self.history.truncate(self.hist_pos + 1);
            self.history.push(target);
            self.hist_pos = self.history.len() - 1;
        }
        self.hovered = None;
        self.hover_info = None;
        self.sent_key = None;
        self.selected = None;
        self.scroll_acc = 0.0;
        self.nav_cooldown = now + 0.4;
        true
    }

    fn scanning(&self) -> bool {
        !self.progress.is_done()
    }
}

// ---------------------------------------------------------------------- app

pub enum Action {
    Scan(PathBuf),
    Browse,
    Home,
    Zoom(NodeId),
    Back,
    Forward,
    Up,
    Rescan,
    Stop,
    Open(PathBuf),
    Reveal(PathBuf),
    Copy(String),
    AskDelete(HoverInfo),
    Export,
    Theme(u8),
    Live(bool),
    Elevate,
}

enum BgMsg {
    Toast(String, bool),
    Picked(PathBuf),
    Drives(Vec<Drive>),
}

pub struct App {
    pub(crate) args: Args,
    pub(crate) settings: Settings,
    worker: LayoutWorker,
    pub(crate) drives: Vec<Drive>,
    pub(crate) session: Option<Session>,
    pub(crate) path_input: String,
    toast: Option<(String, bool, Instant)>,
    confirm_delete: Option<HoverInfo>,
    bg_tx: Sender<BgMsg>,
    bg_rx: Receiver<BgMsg>,
    script: Option<crate::script::Script>,
    serial: u64,
    pub(crate) drives_loaded: bool,
    elevated: bool,
    fonts: crate::fonts::Fonts,
}

fn apply_theme(ctx: &Context, pref: u8) {
    ctx.set_theme(match pref {
        1 => egui::ThemePreference::Dark,
        2 => egui::ThemePreference::Light,
        _ => egui::ThemePreference::System,
    });
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, args: Args) -> Self {
        let ctx = &cc.egui_ctx;
        theme::install(ctx);
        let fonts = crate::fonts::Fonts::install(ctx);
        let mut settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, "settings"))
            .unwrap_or_default();
        if args.free {
            settings.show_free = true;
        }
        if let Some(d) = args.detail {
            settings.detail = d;
        }
        settings.detail = settings.detail.min(DETAIL.len() as u8 - 1);
        match args.theme.as_deref() {
            Some("dark") => settings.theme = 1,
            Some("light") => settings.theme = 2,
            Some("system") => settings.theme = 0,
            _ => {}
        }
        apply_theme(ctx, settings.theme);
        let (bg_tx, bg_rx) = channel();
        let mut app = App {
            worker: LayoutWorker::new(ctx.clone()),
            drives: Vec::new(),
            session: None,
            path_input: String::new(),
            toast: None,
            confirm_delete: None,
            bg_tx,
            bg_rx,
            serial: 0,
            drives_loaded: false,
            elevated: platform::is_elevated(),
            fonts,
            script: crate::script::Script::from_args(&args).unwrap_or_else(|e| {
                eprintln!("bad --script: {e}");
                None
            }),
            settings,
            args,
        };
        app.refresh_drives(ctx);
        if let Some(path) = app.args.path.clone() {
            app.start_scan(ctx, &path);
        }
        app
    }

    fn toast(&mut self, msg: impl Into<String>, error: bool) {
        self.toast = Some((msg.into(), error, Instant::now()));
    }

    fn scan_options(ctx: &Context) -> ScanOptions {
        let c = ctx.clone();
        ScanOptions {
            threads: 0,
            on_done: Some(Arc::new(move || c.request_repaint())),
            no_mft: false,
        }
    }

    /// Re-reads the drive list in the background (network drives can stall).
    pub(crate) fn refresh_drives(&self, ctx: &Context) {
        let (tx, ctx) = (self.bg_tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(BgMsg::Drives(platform::drives()));
            ctx.request_repaint();
        });
    }

    pub(crate) fn start_scan(&mut self, ctx: &Context, path: &Path) {
        match scan_path(path, &Self::scan_options(ctx)) {
            Ok((shared, progress)) => {
                if let Some(old) = self.session.take() {
                    old.progress.cancel();
                }
                self.refresh_drives(ctx);
                let root = shared.tree.read().root_path.clone();
                let volume = platform::volume_of(&root, &self.drives);
                let mut s = Session::new(shared, progress, volume, root);
                if let Some(f) = &self.args.filter {
                    s.filter_text = f.clone();
                    s.apply_filter();
                }
                self.session = Some(s);
            }
            Err(e) => self.toast(format!("Cannot scan {}: {e}", path.display()), true),
        }
    }

    fn browse(&mut self, ctx: &Context) {
        let tx = self.bg_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            if let Some(dir) = rfd::FileDialog::new()
                .set_title("Choose a folder to scan")
                .pick_folder()
            {
                let _ = tx.send(BgMsg::Picked(dir));
                ctx.request_repaint();
            }
        });
    }

    fn export(&mut self, ctx: &Context) {
        let Some(s) = &self.session else { return };
        let (shared, root, tx, ctx) = (s.shared.clone(), s.zoom, self.bg_tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let Some(file) = rfd::FileDialog::new()
                .set_title("Export as CSV")
                .set_file_name("disk-usage.csv")
                .add_filter("CSV", &["csv"])
                .save_file()
            else {
                return;
            };
            let result = std::fs::File::create(&file)
                .map(std::io::BufWriter::new)
                .and_then(|w| {
                    let tree = shared.tree.read();
                    spacescope_core::export::write_csv(&tree, root, w)
                });
            let msg = match result {
                Ok(rows) => BgMsg::Toast(
                    format!("Exported {} rows to {}", human_count(rows), file.display()),
                    false,
                ),
                Err(e) => BgMsg::Toast(format!("Export failed: {e}"), true),
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    fn delete(&mut self, ctx: &Context, target: HoverInfo) {
        let Some(s) = &self.session else { return };
        let (shared, tx, ctx) = (s.shared.clone(), self.bg_tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let msg = match platform::trash(&target.path) {
                Ok(()) => {
                    shared.tree.write().remove(target.node);
                    shared.bump();
                    BgMsg::Toast(
                        format!(
                            "Moved {} to the {} ({} freed)",
                            target.name,
                            platform::TRASH,
                            human_size(target.alloc)
                        ),
                        false,
                    )
                }
                Err(e) => BgMsg::Toast(e, true),
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    fn poll_background(&mut self, ctx: &Context) {
        while let Ok(msg) = self.bg_rx.try_recv() {
            match msg {
                BgMsg::Toast(m, err) => self.toast(m, err),
                BgMsg::Picked(p) => self.start_scan(ctx, &p),
                BgMsg::Drives(d) => {
                    self.drives = d;
                    self.drives_loaded = true;
                    if let Some(s) = &mut self.session {
                        s.volume = platform::volume_of(&s.root_path, &self.drives);
                    }
                }
            }
        }
    }

    fn apply(&mut self, ctx: &Context, actions: Vec<Action>) {
        let now = ctx.input(|i| i.time);
        for a in actions {
            match a {
                Action::Scan(p) => self.start_scan(ctx, &p),
                Action::Browse => self.browse(ctx),
                Action::Home => {
                    if let Some(s) = self.session.take() {
                        s.progress.cancel();
                    }
                    self.refresh_drives(ctx);
                }
                Action::Zoom(n) => self.nav(Nav::To(n)),
                Action::Back => self.nav(Nav::Back),
                Action::Forward => self.nav(Nav::Forward),
                Action::Up => self.nav(Nav::Up),
                Action::Rescan => {
                    self.refresh_drives(ctx);
                    if let Some(s) = &mut self.session {
                        if !s.scanning() {
                            s.progress =
                                rescan_subtree(&s.shared, s.zoom, &Self::scan_options(ctx));
                            s.selected = None;
                            if let (0, Some(w)) = (s.zoom, &s.watch) {
                                w.clear_missed();
                            }
                        }
                    }
                }
                Action::Stop => {
                    if let Some(s) = &self.session {
                        s.progress.cancel();
                    }
                }
                Action::Open(p) => {
                    if let Err(e) = platform::open_path(&p) {
                        self.toast(e, true);
                    }
                }
                Action::Reveal(p) => {
                    if let Err(e) = platform::reveal(&p) {
                        self.toast(e, true);
                    }
                }
                Action::Copy(text) => {
                    ctx.copy_text(text);
                    self.toast("Path copied", false);
                }
                Action::AskDelete(info) => self.confirm_delete = Some(info),
                Action::Export => self.export(ctx),
                Action::Theme(t) => {
                    self.settings.theme = t;
                    apply_theme(ctx, t);
                }
                Action::Live(on) => {
                    self.settings.live = on;
                    if let Some(s) = &mut self.session {
                        // Dropping the watcher stops it; it restarts lazily.
                        s.watch = None;
                        s.watch_off = None;
                    }
                }
                Action::Elevate => {
                    let path = self.session.as_ref().map(|s| s.root_path.clone());
                    match platform::relaunch_elevated(path.as_deref()) {
                        Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                        Err(e) => self.toast(e, true),
                    }
                }
            }
        }
        if let Some(s) = &mut self.session {
            if let Some(nav) = s.pending_nav.take() {
                if !s.navigate(nav, now) {
                    s.pending_nav = Some(nav);
                    ctx.request_repaint();
                }
            }
        }
    }

    fn nav(&mut self, nav: Nav) {
        if let Some(s) = &mut self.session {
            s.pending_nav = Some(nav);
        }
    }

    // ----------------------------------------------------------- test hooks

    fn test_hooks(&mut self, ctx: &Context) {
        // `--zoom a/b/c`: descend by child names once the scan is complete.
        if let (Some(spec), Some(s)) = (self.args.zoom.clone(), &mut self.session) {
            if !s.zoom_applied && !s.scanning() {
                let tree = s.shared.tree.read();
                let mut cur = 0;
                for part in spec.split('/').filter(|p| !p.is_empty()) {
                    match tree
                        .children(cur)
                        .find(|&c| tree.name_lossy(c).eq_ignore_ascii_case(part))
                    {
                        Some(c) => cur = c,
                        None => break,
                    }
                }
                drop(tree);
                s.zoom_applied = true;
                s.pending_nav = Some(Nav::To(cur));
            }
        }
        let settled = self.session.as_ref().is_none_or(|s| {
            !s.scanning()
                && s.layout.is_some()
                && s.anim.is_none()
                && s.inflight.is_none()
                && s.pending_nav.is_none()
                && (self.args.zoom.is_none() || s.zoom_applied)
                && s.watch.as_ref().is_none_or(|w| w.idle())
        });
        if let Some(script) = &mut self.script {
            script.drive(ctx, settled);
        }
    }

    // ----------------------------------------------------------- map screen

    fn map_screen(&mut self, ctx: &Context, pal: &Palette, s: &mut Session, out: &mut Vec<Action>) {
        // Cached totals for the status bar.
        let generation = s.shared.generation();
        if s.totals_gen != generation {
            if let Some(tree) = s.shared.tree.try_read_for(Duration::from_millis(2)) {
                let r = tree.node(0);
                s.totals = (r.files, r.size, r.alloc);
                s.totals_gen = generation;
                // The folder being viewed was deleted or replaced: climb out.
                if tree.is_detached(s.zoom) && s.pending_nav.is_none() {
                    s.pending_nav = Some(Nav::To(s.zoom));
                }
            }
        }
        // Live updates start once the first scan is complete.
        if self.settings.live && !s.scanning() && s.watch.is_none() && s.watch_off.is_none() {
            let dirs = s.progress.dirs.load(Ordering::Relaxed) as usize;
            match crate::watch::LiveWatch::start(&s.shared, &s.root_path, dirs, ctx) {
                Ok(w) => s.watch = Some(w),
                Err(e) => s.watch_off = Some(e.message()),
            }
        }
        let scanning = s.scanning();
        if let Some(w) = &mut s.watch {
            w.tick(&s.shared, scanning, ctx);
        }
        self.shortcuts(ctx, s, out);

        egui::TopBottomPanel::top("bar")
            .frame(
                egui::Frame::new()
                    .fill(pal.panel)
                    .inner_margin(Margin::symmetric(8, 6)),
            )
            .show_separator_line(false)
            .show(ctx, |ui| self.top_bar(ui, pal, s, out));
        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(pal.panel)
                    .inner_margin(Margin::symmetric(12, 5)),
            )
            .show_separator_line(false)
            .show(ctx, |ui| self.status_bar(ui, pal, s));
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(pal.bg)
                    .inner_margin(Margin::same(6)),
            )
            .show(ctx, |ui| self.treemap(ui, pal, s, out));
    }

    fn shortcuts(&mut self, ctx: &Context, s: &mut Session, out: &mut Vec<Action>) {
        let typing = ctx.wants_keyboard_input();
        let filter_id = Id::new("filter-box");
        // Never touch `ctx` inside the input closure: it would self-deadlock.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::F)) {
            ctx.memory_mut(|m| m.request_focus(filter_id));
        }
        ctx.input_mut(|i| {
            if i.pointer.button_pressed(egui::PointerButton::Extra1) {
                out.push(Action::Back);
            }
            if i.pointer.button_pressed(egui::PointerButton::Extra2) {
                out.push(Action::Forward);
            }
            if i.consume_key(egui::Modifiers::ALT, Key::ArrowLeft) {
                out.push(Action::Back);
            }
            if i.consume_key(egui::Modifiers::ALT, Key::ArrowRight) {
                out.push(Action::Forward);
            }
            if i.consume_key(egui::Modifiers::ALT, Key::ArrowUp) {
                out.push(Action::Up);
            }
            if i.key_pressed(Key::F5) {
                out.push(Action::Rescan);
            }
            if i.key_pressed(Key::Escape)
                && !s.filter_text.is_empty()
                && self.confirm_delete.is_none()
            {
                s.filter_text.clear();
                s.apply_filter();
            }
            if !typing {
                if i.key_pressed(Key::Backspace) {
                    out.push(Action::Back);
                }
                if i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals) {
                    self.settings.detail = (self.settings.detail + 1).min(DETAIL.len() as u8 - 1);
                }
                if i.key_pressed(Key::Minus) {
                    self.settings.detail = self.settings.detail.saturating_sub(1);
                }
            }
        });
    }

    fn top_bar(&mut self, ui: &mut Ui, pal: &Palette, s: &mut Session, out: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if icons::button(ui, Icon::Back, "Back  (Alt+Left)", s.hist_pos > 0).clicked() {
                out.push(Action::Back);
            }
            let can_fwd = s.hist_pos + 1 < s.history.len();
            if icons::button(ui, Icon::Forward, "Forward  (Alt+Right)", can_fwd).clicked() {
                out.push(Action::Forward);
            }
            if icons::button(ui, Icon::Up, "Up one folder  (Alt+Up)", s.zoom != 0).clicked() {
                out.push(Action::Up);
            }
            if icons::button(ui, Icon::Home, "Scan another drive or folder", true).clicked() {
                out.push(Action::Home);
            }
            ui.add_space(6.0);

            ui.with_layout(UiLayout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.menu_button("More", |ui| {
                    ui.set_min_width(170.0);
                    if ui.button("Export as CSV…").clicked() {
                        out.push(Action::Export);
                        ui.close_menu();
                    }
                    let mut live = self.settings.live;
                    if ui
                        .checkbox(&mut live, "Live updates")
                        .on_hover_text("Follow changes on disk after the scan")
                        .changed()
                    {
                        out.push(Action::Live(live));
                        ui.close_menu();
                    }
                    if cfg!(windows) && !self.elevated {
                        ui.separator();
                        if ui
                            .button("Restart as administrator")
                            .on_hover_text(
                                "Reads the NTFS file table directly: several times faster,\n\
                                 hard links counted once, and no unreadable folders.",
                            )
                            .clicked()
                        {
                            out.push(Action::Elevate);
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    ui.label(RichText::new("Theme").small().color(pal.text_dim));
                    for (i, name) in ["Match system", "Dark", "Light"].iter().enumerate() {
                        if ui.radio(self.settings.theme == i as u8, *name).clicked() {
                            out.push(Action::Theme(i as u8));
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    ui.label(
                        RichText::new(format!(
                            "{} {}{}",
                            crate::APP_NAME,
                            env!("CARGO_PKG_VERSION"),
                            if self.elevated {
                                "  ·  administrator"
                            } else {
                                ""
                            }
                        ))
                        .small()
                        .color(pal.text_dim),
                    );
                });
                if s.scanning() {
                    if icons::button(ui, Icon::Stop, "Stop scanning", true).clicked() {
                        out.push(Action::Stop);
                    }
                } else if icons::button(ui, Icon::Refresh, "Rescan this folder  (F5)", true)
                    .clicked()
                {
                    out.push(Action::Rescan);
                }
                if s.volume.is_some() {
                    let tip = if self.settings.show_free {
                        "Hide free space"
                    } else {
                        "Show free space"
                    };
                    if icons::toggle(ui, Icon::Disk, tip, self.settings.show_free).clicked() {
                        self.settings.show_free = !self.settings.show_free;
                    }
                }
                let max = DETAIL.len() as u8 - 1;
                if icons::button(
                    ui,
                    Icon::Plus,
                    "More detail  (+)",
                    self.settings.detail < max,
                )
                .clicked()
                {
                    self.settings.detail += 1;
                }
                if icons::button(
                    ui,
                    Icon::Minus,
                    "Less detail  (-)",
                    self.settings.detail > 0,
                )
                .clicked()
                {
                    self.settings.detail -= 1;
                }
                ui.add_space(4.0);
                if ui
                    .selectable_label(self.settings.logical, "Size")
                    .on_hover_text("Rank by the size of the file contents")
                    .clicked()
                {
                    self.settings.logical = true;
                }
                if ui
                    .selectable_label(!self.settings.logical, "On disk")
                    .on_hover_text("Rank by the space actually used on disk")
                    .clicked()
                {
                    self.settings.logical = false;
                }
                ui.add_space(4.0);

                if !s.filter_text.is_empty()
                    && icons::button(ui, Icon::Close, "Clear filter  (Esc)", true).clicked()
                {
                    s.filter_text.clear();
                    s.apply_filter();
                }
                let mut edit = egui::TextEdit::singleline(&mut s.filter_text)
                    .id(Id::new("filter-box"))
                    .hint_text("Filter   *.mp4; >100mb")
                    .desired_width(210.0)
                    .margin(Margin::symmetric(8, 5));
                if s.filter_err.is_some() {
                    edit = edit.text_color(pal.danger);
                }
                let resp = ui.add(edit);
                if resp.changed() {
                    s.apply_filter();
                }
                match &s.filter_err {
                    Some(e) => resp.on_hover_text(e.as_str()),
                    None => resp.on_hover_text(
                        RichText::new(FILTER_HELP)
                            .family(egui::FontFamily::Monospace)
                            .size(12.0),
                    ),
                };
                ui.add_space(8.0);

                ui.with_layout(UiLayout::left_to_right(Align::Center), |ui| {
                    breadcrumb(ui, pal, s, out);
                });
            });
        });
    }

    fn status_bar(&mut self, ui: &mut Ui, pal: &Palette, s: &mut Session) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let p = &s.progress;
            let (files, size, alloc) = s.totals;
            let shown = if self.settings.logical { size } else { alloc };
            let dim = |t: String| RichText::new(t).size(12.0).color(pal.text_dim);
            let strong = |t: String| RichText::new(t).size(12.0).color(pal.text);
            if s.scanning() {
                ui.add(egui::Spinner::new().size(13.0).color(pal.accent));
                ui.label(strong("Scanning".into()));
                ui.label(dim(format!(
                    "{} files  ·  {}  ·  {:.0} s",
                    human_count(p.files.load(Ordering::Relaxed)),
                    human_size(p.bytes.load(Ordering::Relaxed)),
                    p.elapsed().as_secs_f32()
                )));
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            } else {
                ui.label(strong(human_size(shown)));
                ui.label(dim(format!(
                    "{}  ·  {} files  ·  scanned in {}{}",
                    if self.settings.logical {
                        "in total"
                    } else {
                        "on disk"
                    },
                    human_count(files as u64),
                    human_duration(p.elapsed()),
                    if p.is_cancelled() {
                        " (stopped early)"
                    } else if p.used_mft() {
                        " from the NTFS file table"
                    } else {
                        ""
                    }
                )));
            }
            // Live-update indicator.
            if let Some(w) = &s.watch {
                let (color, tip) = if w.missed() {
                    (
                        pal.warn,
                        "Live updates: some changes came too fast to track.\n\
                         Press F5 to make the picture exact again."
                            .to_string(),
                    )
                } else {
                    (
                        pal.free_text,
                        format!(
                            "Live: the map follows changes on disk.\n{} folder updates applied.",
                            human_count(w.applied() as u64)
                        ),
                    )
                };
                let (r, resp) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 3.5, color);
                resp.on_hover_text(&tip);
                ui.label(RichText::new("Live").size(12.0).color(color))
                    .on_hover_text(tip);
            } else if let Some(why) = &s.watch_off {
                ui.label(RichText::new("Not live").size(12.0).color(pal.text_dim))
                    .on_hover_text(why.as_str());
            }
            let errors = p.errors.load(Ordering::Relaxed);
            if errors > 0 {
                ui.label(
                    RichText::new(format!("{} unreadable", human_count(errors)))
                        .size(12.0)
                        .color(pal.warn),
                )
                .on_hover_text(
                    "Some folders could not be read, usually because of permissions.\n\
                         Run as administrator / root to include them.",
                );
            }
            let mounts = p.mounts.load(Ordering::Relaxed);
            if mounts > 0 {
                ui.label(
                    RichText::new(format!(
                        "{mounts} other filesystem{} skipped",
                        if mounts == 1 { "" } else { "s" }
                    ))
                    .size(12.0)
                    .color(pal.text_dim),
                )
                .on_hover_text(
                    "Folders that are mount points of another filesystem are not counted.\n\
                     Scan them separately from the start screen.",
                );
            }
            if let (Some(_), Some(l)) = (&s.filter, &s.layout) {
                ui.label(
                    RichText::new(format!("Filter matches {}", human_size(l.root_weight)))
                        .size(12.0)
                        .color(pal.accent),
                );
            }
            ui.with_layout(UiLayout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                let names = ui.available_width() > 620.0;
                for cat in (0..kind::COUNT).rev() {
                    if names {
                        ui.label(dim(kind::NAMES[cat].to_string()));
                    }
                    let (r, resp) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().rect_filled(r, 2.5, pal.cats[cat]);
                    resp.on_hover_text(kind::NAMES[cat]);
                    if names {
                        ui.add_space(5.0);
                    }
                }
            });
        });
    }

    fn treemap(&mut self, ui: &mut Ui, pal: &Palette, s: &mut Session, out: &mut Vec<Action>) {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click());
        if rect.width() < 8.0 || rect.height() < 8.0 {
            return;
        }
        let ppp = ctx.pixels_per_point();
        let origin = pos2(
            (rect.min.x * ppp).round() / ppp,
            (rect.min.y * ppp).round() / ppp,
        );
        let local = |r: Rect| r.translate(origin.to_vec2());

        // Collect finished layouts.
        while let Ok(reply) = self.worker.rx.try_recv() {
            if s.inflight != Some(reply.serial) {
                continue;
            }
            s.inflight = None;
            if reply.layout.root != s.zoom {
                continue;
            }
            for (_, text) in &reply.layout.labels {
                self.fonts.ensure(&ctx, text);
            }
            let layout = Arc::new(reply.layout);
            if let Some(anim) = &mut s.anim {
                if anim.start.is_none() {
                    if let AnimKind::Out { old_root, to } = &mut anim.kind {
                        *to = layout.find(*old_root).map(|i| {
                            let t = &layout.tiles[i];
                            Rect::from_min_size(pos2(t.x, t.y), vec2(t.w, t.h))
                        });
                    }
                    anim.start = Some(now);
                }
            }
            s.layout = Some(layout);
            s.mesh = None;
            s.hovered = None;
        }

        // Ask for a new layout whenever anything it depends on has changed.
        let (min_tile, min_dir) = DETAIL[self.settings.detail as usize];
        let free = if self.settings.show_free && s.zoom == 0 && s.filter.is_none() {
            s.volume.map_or(0, |v| v.1)
        } else {
            0
        };
        let key = ReqKey {
            root: s.zoom,
            w: rect.width().round() as u32,
            h: rect.height().round() as u32,
            detail: self.settings.detail,
            logical: self.settings.logical,
            filter: s.filter.as_ref().map_or(0, |f| f.0),
            free,
            generation: s.shared.generation(),
        };
        if s.inflight.is_none() && s.sent_key != Some(key) {
            let live_only = s.sent_key.is_some_and(|k| {
                ReqKey {
                    generation: key.generation,
                    ..k
                } == key
            });
            let gap = match (live_only, s.filter.is_some()) {
                (false, _) => 0.0,
                (true, false) => 0.12,
                (true, true) => 1.0,
            };
            let since = now - s.sent_at;
            if since >= gap {
                self.serial += 1;
                self.worker.send(Request {
                    serial: self.serial,
                    shared: s.shared.clone(),
                    root: s.zoom,
                    opts: LayoutOptions {
                        width: rect.width(),
                        height: rect.height(),
                        metric: if self.settings.logical {
                            Metric::Logical
                        } else {
                            Metric::Allocated
                        },
                        min_tile,
                        min_dir,
                        header_h: treemap::HEADER_H,
                        pad: 3.0,
                        free_space: free,
                        ..Default::default()
                    },
                    filter: s.filter.clone(),
                });
                s.inflight = Some(self.serial);
                s.sent_key = Some(key);
                s.sent_at = now;
            } else {
                ctx.request_repaint_after(Duration::from_secs_f64(gap - since));
            }
        }

        let painter = ui.painter_at(rect);
        if let Some(layout) = &s.layout {
            let key = MeshKey {
                layout: Arc::as_ptr(layout) as usize,
                dark: pal.dark,
                origin: (origin.x.to_bits(), origin.y.to_bits()),
                ppp: ppp.to_bits(),
            };
            if s.mesh.as_ref().map(|m| m.1) != Some(key) {
                s.mesh = Some((Arc::new(treemap::build_mesh(layout, origin, ppp, pal)), key));
            }
        }

        // Transition between zoom levels.
        let mut animating = false;
        let mut finished = false;
        if let Some(anim) = &s.anim {
            match anim.start {
                None => {
                    painter.add(Shape::mesh(anim.old_mesh.clone()));
                    animating = true;
                }
                Some(start) => {
                    let p = ((now - start) / ANIM_SECS).clamp(0.0, 1.0) as f32;
                    if p >= 1.0 && s.mesh.is_some() {
                        finished = true;
                    } else {
                        animating = true;
                        let e = 1.0 - (1.0 - p).powi(3);
                        let lerp = |a: Rect, b: Rect| {
                            Rect::from_min_max(
                                a.min + (b.min - a.min) * e,
                                a.max + (b.max - a.max) * e,
                            )
                        };
                        let (old_src, new_src, cur) = match &anim.kind {
                            AnimKind::In { from } => {
                                let from = local(*from);
                                (from, rect, lerp(from, rect))
                            }
                            AnimKind::Out { to: Some(to), .. } => {
                                let to = local(*to);
                                (rect, to, lerp(rect, to))
                            }
                            _ => (rect, rect, rect),
                        };
                        let k = ((e - 0.25) / 0.7).clamp(0.0, 1.0);
                        let fade = 1.0 - k * k * (3.0 - 2.0 * k);
                        match &s.mesh {
                            Some((mesh, _)) => {
                                painter.add(Shape::mesh(treemap::remap(mesh, new_src, cur, 1.0)));
                                painter.add(Shape::mesh(treemap::remap(
                                    &anim.old_mesh,
                                    old_src,
                                    cur,
                                    fade,
                                )));
                            }
                            None => {
                                painter.add(Shape::mesh(treemap::remap(
                                    &anim.old_mesh,
                                    old_src,
                                    cur,
                                    1.0,
                                )));
                            }
                        }
                        ctx.request_repaint();
                    }
                }
            }
        }
        if finished {
            s.anim = None;
            s.labels_at = now;
        }
        if animating {
            return;
        }

        let (Some(layout), Some((mesh, _))) = (s.layout.clone(), s.mesh.clone()) else {
            centered_note(&painter, rect, pal, "Reading folders…");
            return;
        };
        painter.add(Shape::mesh(mesh));
        if layout.root_weight == 0 {
            let msg = if s.scanning() {
                "Reading folders…"
            } else if s.filter.is_some() {
                "Nothing here matches the filter"
            } else {
                "This folder is empty"
            };
            centered_note(&painter, rect, pal, msg);
            return;
        }
        let alpha = ((now - s.labels_at) / 0.14).clamp(0.0, 1.0) as f32;
        if alpha < 1.0 {
            ctx.request_repaint();
        }
        treemap::paint_labels(&painter, &layout, origin, pal, alpha);

        // Hover.
        let menu_open = resp.context_menu_opened() || ctx.memory(|m| m.any_popup_open());
        let pointer = resp.hover_pos();
        if !menu_open {
            s.hovered = pointer.and_then(|p| layout.hit(p.x - origin.x, p.y - origin.y));
            match s.hovered {
                Some(i) => {
                    let lp = Arc::as_ptr(&layout) as usize;
                    let generation = s.shared.generation();
                    let same = s
                        .hover_info
                        .as_ref()
                        .is_some_and(|h| h.tile == i && h.layout == lp);
                    let fresh = same
                        && s.hover_info
                            .as_ref()
                            .is_some_and(|h| h.generation == generation || now - h.at < 0.25);
                    if !fresh {
                        match s.shared.tree.try_read_for(Duration::from_millis(2)) {
                            Some(tree) => {
                                let info = HoverInfo::new(&tree, &layout, i, generation, now);
                                drop(tree);
                                self.fonts.ensure(&ctx, &info.name);
                                s.hover_info = Some(info);
                            }
                            None if !same => s.hover_info = None,
                            None => {}
                        }
                    }
                }
                None => s.hover_info = None,
            }
        }

        // Highlights.
        if let Some(i) = s.hovered.filter(|&i| i < layout.tiles.len()) {
            let t = &layout.tiles[i];
            if let Some(parent) = layout.tiles.get(t.parent as usize) {
                painter.rect_stroke(
                    tile_rect(parent, origin),
                    0.0,
                    Stroke::new(1.0_f32, pal.hover.gamma_multiply(0.38)),
                    StrokeKind::Inside,
                );
            }
            let r = tile_rect(t, origin);
            let wash = if t.kind == TileKind::Dir { 0.05 } else { 0.13 };
            painter.rect_filled(r, 0.0, pal.hover.gamma_multiply(wash));
            painter.rect_stroke(
                r,
                0.0,
                Stroke::new(1.5_f32, pal.hover.gamma_multiply(0.92)),
                StrokeKind::Inside,
            );
        }
        if let Some(i) = s.selected.and_then(|n| layout.find(n)) {
            painter.rect_stroke(
                tile_rect(&layout.tiles[i], origin),
                0.0,
                Stroke::new(2.0_f32, pal.accent),
                StrokeKind::Inside,
            );
        }

        // Clicks.
        let zoom_target = |i: usize| -> Option<NodeId> {
            let t = &layout.tiles[i];
            let own = matches!(
                t.kind,
                TileKind::Dir | TileKind::DirCollapsed | TileKind::More
            );
            if own && t.node != layout.root {
                return Some(t.node);
            }
            layout
                .tiles
                .get(t.parent as usize)
                .map(|p| p.node)
                .filter(|&n| n != layout.root)
        };
        if resp.double_clicked() {
            if let Some(n) = s.hovered.and_then(zoom_target) {
                out.push(Action::Zoom(n));
            }
        } else if resp.clicked() {
            let node = s
                .hover_info
                .as_ref()
                .filter(|h| h.is_item())
                .map(|h| h.node);
            s.selected = if s.selected == node { None } else { node };
        }
        if resp.secondary_clicked() {
            s.ctx_target = s.hover_info.clone();
            s.selected = s
                .ctx_target
                .as_ref()
                .filter(|h| h.is_item())
                .map(|h| h.node);
        }
        let zoom = s.zoom;
        let target = s.ctx_target.clone();
        let show_free = &mut self.settings.show_free;
        resp.context_menu(|ui| {
            let Some(t) = &target else {
                ui.close_menu();
                return;
            };
            ui.set_min_width(210.0);
            ui.label(
                RichText::new(&t.name)
                    .family(theme::semibold())
                    .color(pal.text),
            );
            ui.label(
                RichText::new(human_size(t.weight))
                    .size(11.5)
                    .color(pal.text_dim),
            );
            ui.separator();
            match t.kind {
                TileKind::Free => {
                    if ui.button("Hide free space").clicked() {
                        *show_free = false;
                        ui.close_menu();
                    }
                }
                TileKind::More => {
                    if t.node != zoom && ui.button("Zoom into this folder").clicked() {
                        out.push(Action::Zoom(t.node));
                        ui.close_menu();
                    }
                    if ui
                        .button(format!("Open folder in {}", platform::FILE_MANAGER))
                        .clicked()
                    {
                        out.push(Action::Open(t.path.clone()));
                        ui.close_menu();
                    }
                }
                _ => {
                    if t.is_dir && t.node != zoom && ui.button("Zoom in").clicked() {
                        out.push(Action::Zoom(t.node));
                        ui.close_menu();
                    }
                    if ui.button("Open").clicked() {
                        out.push(Action::Open(t.path.clone()));
                        ui.close_menu();
                    }
                    if ui
                        .button(format!("Show in {}", platform::FILE_MANAGER))
                        .clicked()
                    {
                        out.push(Action::Reveal(t.path.clone()));
                        ui.close_menu();
                    }
                    if ui.button("Copy path").clicked() {
                        out.push(Action::Copy(t.path.display().to_string()));
                        ui.close_menu();
                    }
                    if t.node != 0 {
                        ui.separator();
                        let label = RichText::new(format!("Move to {}…", platform::TRASH))
                            .color(pal.danger);
                        if ui.button(label).clicked() {
                            out.push(Action::AskDelete(t.clone()));
                            ui.close_menu();
                        }
                    }
                }
            }
        });

        // Wheel: up dives one level towards the pointer, down climbs out.
        if resp.hovered() && !menu_open {
            let dy = ctx.input(|i| i.smooth_scroll_delta.y);
            if now < s.nav_cooldown {
                s.scroll_acc = 0.0;
            } else {
                s.scroll_acc += dy;
            }
            if s.scroll_acc > 60.0 {
                s.scroll_acc = 0.0;
                let mut i = s.hovered;
                while let Some(idx) = i {
                    let t = &layout.tiles[idx];
                    if t.parent == 0 || t.parent == u32::MAX {
                        break;
                    }
                    i = Some(t.parent as usize);
                }
                if let Some(t) = i.map(|i| &layout.tiles[i]) {
                    let is_dir = matches!(t.kind, TileKind::Dir | TileKind::DirCollapsed);
                    if is_dir && t.node != layout.root {
                        out.push(Action::Zoom(t.node));
                    }
                }
            } else if s.scroll_acc < -60.0 {
                s.scroll_acc = 0.0;
                if s.zoom != 0 {
                    out.push(Action::Up);
                }
            }
        }

        if let (Some(info), Some(p), false) = (&s.hover_info, pointer, menu_open) {
            // Keep the tooltip clear of the pointer: flip it to the other
            // side near the window edges instead of sliding over the cursor,
            // where it would sit between the user and what they are clicking.
            let screen = ctx.screen_rect();
            let size = s.tip_size;
            let mut pos = p + vec2(16.0, 18.0);
            if pos.x + size.x > screen.right() - 6.0 {
                pos.x = p.x - 14.0 - size.x;
            }
            if pos.y + size.y > screen.bottom() - 6.0 {
                pos.y = p.y - 12.0 - size.y;
            }
            pos.x = pos.x.max(screen.left() + 6.0);
            pos.y = pos.y.max(screen.top() + 6.0);
            let filtered = s.filter.is_some();
            s.tip_size = tooltip(&ctx, pal, pos, info, layout.root_weight, filtered);
        }
    }

    // --------------------------------------------------------------- chrome

    fn modals(&mut self, ctx: &Context, pal: &Palette) {
        let Some(t) = self.confirm_delete.clone() else {
            return;
        };
        let (mut go, mut cancel) = (false, false);
        let frame = egui::Frame::popup(&ctx.style()).inner_margin(Margin::same(18));
        let modal = egui::Modal::new(Id::new("confirm-delete"))
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width(400.0);
                ui.label(
                    RichText::new(format!("Move to the {}?", platform::TRASH))
                        .family(theme::semibold())
                        .size(17.0)
                        .color(pal.text),
                );
                ui.add_space(6.0);
                ui.label(RichText::new(&t.name).color(pal.text));
                ui.label(
                    RichText::new(t.path.display().to_string())
                        .size(11.5)
                        .color(pal.text_dim),
                );
                ui.add_space(2.0);
                let detail = if t.is_dir {
                    format!(
                        "{}  ·  {} files",
                        human_size(t.alloc),
                        human_count(t.files as u64)
                    )
                } else {
                    human_size(t.alloc)
                };
                ui.label(RichText::new(detail).color(pal.text_dim));
                ui.add_space(12.0);
                ui.with_layout(UiLayout::right_to_left(Align::Center), |ui| {
                    let label =
                        RichText::new(format!("Move to {}", platform::TRASH)).color(Color32::WHITE);
                    if ui.add(egui::Button::new(label).fill(pal.danger)).clicked() {
                        go = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if go {
            self.confirm_delete = None;
            self.delete(ctx, t);
        } else if cancel || modal.should_close() {
            self.confirm_delete = None;
        }
    }

    fn paint_toast(&mut self, ctx: &Context, pal: &Palette) {
        let Some((msg, error, at)) = &self.toast else {
            return;
        };
        let age = at.elapsed().as_secs_f32();
        let life = if *error { 6.0 } else { 3.5 };
        if age > life {
            self.toast = None;
            return;
        }
        let alpha = ((life - age) / 0.4).clamp(0.0, 1.0);
        let screen = ctx.screen_rect();
        egui::Area::new(Id::new("toast"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::CENTER_BOTTOM)
            .fixed_pos(pos2(screen.center().x, screen.bottom() - 48.0))
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_opacity(alpha);
                egui::Frame::new()
                    .fill(pal.raised)
                    .stroke(Stroke::new(
                        1.0_f32,
                        if *error { pal.danger } else { pal.border },
                    ))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(14, 9))
                    .shadow(egui::Shadow {
                        offset: [0, 4],
                        blur: 16,
                        spread: 0,
                        color: Color32::from_black_alpha(80),
                    })
                    .show(ui, |ui| {
                        ui.set_max_width(520.0);
                        ui.label(RichText::new(msg).color(pal.text));
                    });
            });
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

fn centered_note(p: &egui::Painter, rect: Rect, pal: &Palette, msg: &str) {
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        msg,
        FontId::proportional(15.0),
        pal.text_dim,
    );
}

fn breadcrumb(ui: &mut Ui, pal: &Palette, s: &Session, out: &mut Vec<Action>) {
    ui.spacing_mut().item_spacing.x = 0.0;
    let font = FontId::proportional(13.5);
    let avail = ui.available_width();
    const SEP: f32 = 16.0;
    let widths: Vec<f32> = s
        .crumbs
        .iter()
        .map(|(_, name)| {
            ui.fonts(|f| {
                f.layout_no_wrap(name.clone(), font.clone(), Color32::WHITE)
                    .size()
                    .x
            }) + 12.0
        })
        .collect();
    let n = widths.len();
    if n == 0 {
        return;
    }
    // Keep as many trailing segments as fit; the current folder always shows.
    let mut start = n - 1;
    let mut used = widths[start].min(avail * 0.6);
    while start > 0 && used + widths[start - 1] + SEP <= avail - 26.0 {
        start -= 1;
        used += widths[start] + SEP;
    }
    let chevron = |ui: &mut Ui| {
        let (r, _) = ui.allocate_exact_size(vec2(SEP, 20.0), Sense::hover());
        icons::paint(
            ui.painter(),
            Rect::from_center_size(r.center(), vec2(9.0, 9.0)),
            Icon::Forward,
            pal.text_dim.gamma_multiply(0.7),
        );
    };
    if start > 0 {
        ui.label(RichText::new("…").color(pal.text_dim));
        chevron(ui);
    }
    for (i, (id, name)) in s.crumbs.iter().enumerate().skip(start) {
        if i + 1 == n {
            ui.add(
                egui::Label::new(
                    RichText::new(name)
                        .family(theme::semibold())
                        .size(13.5)
                        .color(pal.text),
                )
                .truncate(),
            );
        } else {
            let b =
                egui::Button::new(RichText::new(name).size(13.5).color(pal.text_dim)).frame(false);
            if ui
                .add(b)
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                out.push(Action::Zoom(*id));
            }
            chevron(ui);
        }
    }
}

/// Draws the hover card and returns its size, used to place it next frame.
fn tooltip(
    ctx: &Context,
    pal: &Palette,
    pos: Pos2,
    h: &HoverInfo,
    view: u64,
    filtered: bool,
) -> egui::Vec2 {
    egui::Area::new(Id::new("map-tooltip"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos)
        .constrain(true)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(pal.raised)
                .stroke(Stroke::new(1.0_f32, pal.border))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(12, 10))
                .shadow(egui::Shadow {
                    offset: [0, 6],
                    blur: 22,
                    spread: 0,
                    color: Color32::from_black_alpha(if pal.dark { 110 } else { 45 }),
                })
                .show(ui, |ui| {
                    ui.set_max_width(400.0);
                    ui.spacing_mut().item_spacing.y = 3.0;
                    ui.label(
                        RichText::new(&h.name)
                            .family(theme::semibold())
                            .size(14.0)
                            .color(pal.text),
                    );
                    ui.label(
                        RichText::new(h.path.display().to_string())
                            .size(11.5)
                            .color(pal.text_dim),
                    );
                    ui.add_space(5.0);
                    let pct = |part: u64, whole: u64| {
                        if whole == 0 {
                            "0%".to_string()
                        } else {
                            let p = part as f64 * 100.0 / whole as f64;
                            if p < 0.1 {
                                "<0.1%".to_string()
                            } else {
                                format!("{p:.1}%")
                            }
                        }
                    };
                    let local_time = |unix: i64| -> String {
                        use chrono::TimeZone;
                        chrono::Local
                            .timestamp_opt(unix, 0)
                            .single()
                            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                            .unwrap_or_else(|| fmt::date(unix))
                    };
                    egui::Grid::new("tip")
                        .num_columns(2)
                        .spacing([16.0, 4.0])
                        .min_row_height(0.0)
                        .show(ui, |ui| {
                            let mut row = |k: &str, v: String| {
                                ui.label(RichText::new(k).size(12.0).color(pal.text_dim));
                                ui.label(RichText::new(v).size(12.0).color(pal.text));
                                ui.end_row();
                            };
                            match h.kind {
                                TileKind::Free => {
                                    row("Free", human_size(h.weight));
                                }
                                TileKind::More => {
                                    row("Total", human_size(h.weight));
                                    row("Items", human_count(h.count as u64));
                                    row("Share of view", pct(h.weight, view));
                                }
                                _ => {
                                    if filtered {
                                        row("Matching", human_size(h.weight));
                                    }
                                    row("On disk", human_size(h.alloc));
                                    row("Size", human_size(h.size));
                                    row(
                                        "Share",
                                        format!(
                                            "{} of view  ·  {} of parent",
                                            pct(h.weight, view),
                                            pct(h.weight, h.parent_weight)
                                        ),
                                    );
                                    if h.is_dir {
                                        row(
                                            "Contains",
                                            format!("{} files", human_count(h.files as u64)),
                                        );
                                    }
                                    if h.mtime > 0 {
                                        row(
                                            "Modified",
                                            format!(
                                                "{}  ·  {}",
                                                local_time(h.mtime),
                                                fmt::age(h.mtime, unix_now())
                                            ),
                                        );
                                    }
                                    row(
                                        "Type",
                                        if h.is_dir {
                                            "Folder".to_string()
                                        } else {
                                            kind::NAMES[h.cat as usize % kind::COUNT].to_string()
                                        },
                                    );
                                }
                            }
                        });
                    let note = if h.flags & flags::ERROR != 0 {
                        Some("Could not be read (permission denied?)")
                    } else if h.flags & flags::LINK != 0 {
                        Some("Link: not followed")
                    } else if h.flags & flags::MOUNT != 0 {
                        Some("Another filesystem: not scanned")
                    } else if h.flags & flags::HARDLINK != 0 {
                        Some("Hard link: counted once elsewhere")
                    } else {
                        None
                    };
                    if let Some(n) = note {
                        ui.add_space(3.0);
                        ui.label(RichText::new(n).size(11.5).color(pal.warn));
                    }
                });
        })
        .response
        .rect
        .size()
}

impl eframe::App for App {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.test_hooks(ctx);
        self.poll_background(ctx);

        let dropped = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));
        if let Some(p) = dropped {
            let dir = if p.is_dir() {
                p
            } else {
                p.parent().map(Path::to_path_buf).unwrap_or(p)
            };
            self.start_scan(ctx, &dir);
        }

        let pal = Palette::of(ctx);
        let mut actions = Vec::new();
        match self.session.take() {
            None => self.start_screen(ctx, &pal, &mut actions),
            Some(mut s) => {
                self.map_screen(ctx, &pal, &mut s, &mut actions);
                self.session = Some(s);
            }
        }
        self.apply(ctx, actions);
        self.modals(ctx, &pal);
        self.paint_toast(ctx, &pal);
    }

    fn raw_input_hook(&mut self, _ctx: &Context, raw: &mut egui::RawInput) {
        if let Some(script) = &mut self.script {
            script.feed(raw);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        // Scripted test runs must not overwrite the user's preferences.
        if self.script.is_none() {
            eframe::set_value(storage, "settings", &self.settings);
        }
    }
}

fn human_duration(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 1.0 {
        format!("{:.0} ms", (s * 1000.0).max(1.0))
    } else if s < 60.0 {
        format!("{s:.1} s")
    } else {
        format!("{}m {:02}s", s as u64 / 60, s as u64 % 60)
    }
}
