use std::collections::HashMap;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{ensure, Context};
use gdk_pixbuf::Pixbuf;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::utils::Transform;

use crate::render_helpers::texture::TextureBuffer;

type AppIconTextureBuffer = TextureBuffer<GlesTexture>;

/// Per-thumbnail GPU texture cache for one app icon at one render scale.
/// It stores both positive and negative lookup results so missing icons do not hit the filesystem
/// every frame.
#[derive(Debug, Default)]
pub(super) struct AppIconTexture {
    app_id: Option<String>,
    physical_size: i32,
    scale: f64,
    texture: Option<Option<AppIconTextureBuffer>>,
}

impl AppIconTexture {
    /// Returns a renderer-local texture for the current app ID and requested logical size.
    /// The cache is invalidated when the app, physical icon size, or output scale changes.
    pub(super) fn get(
        &mut self,
        renderer: &mut GlesRenderer,
        app_id: Option<&str>,
        logical_size: f64,
        scale: f64,
    ) -> Option<AppIconTextureBuffer> {
        let physical_size = (logical_size * scale).round().max(1.) as i32;

        if self.app_id.as_deref() != app_id
            || self.physical_size != physical_size
            || self.scale != scale
        {
            self.texture = None;
            self.app_id = app_id.map(ToOwned::to_owned);
            self.physical_size = physical_size;
            self.scale = scale;
        }

        let app_id = self.app_id.as_deref()?;
        self.texture
            .get_or_insert_with(|| {
                // Pixel lookup is shared globally, but GPU textures are tied to this renderer.
                let pixels = app_icon_pixels(app_id, physical_size)?;
                TextureBuffer::from_memory(
                    renderer,
                    pixels.data.as_ref(),
                    Fourcc::Abgr8888,
                    (pixels.width, pixels.height),
                    false,
                    scale,
                    Transform::Normal,
                    Vec::new(),
                )
                .ok()
            })
            .clone()
    }

    /// Returns the previously rendered texture, if any, without forcing a new icon lookup.
    /// MRU hit testing uses this because it only needs an approximate current title-row size.
    pub(super) fn get_stale(&self) -> Option<&AppIconTextureBuffer> {
        if let Some(Some(texture)) = &self.texture {
            Some(texture)
        } else {
            None
        }
    }
}

/// CPU-side decoded app icon pixels ready to import as a renderer texture.
/// The pixel data is shared so all thumbnails for the same app and size can reuse one decode.
#[derive(Debug)]
struct AppIconPixels {
    data: Arc<[u8]>,
    width: i32,
    height: i32,
}

/// Source of an app icon discovered from a desktop entry or inferred from an app ID.
/// Absolute paths are loaded directly, while icon names are resolved through icon theme folders.
#[derive(Debug, Clone, PartialEq, Eq)]
enum IconSource {
    File(PathBuf),
    Name(String),
}

/// Minimal parsed desktop-entry data needed for app icon resolution.
/// The switcher uses `Icon` first, and `StartupWMClass` as an alternate key for Xwayland apps.
#[derive(Debug, Clone)]
struct DesktopEntry {
    icon: IconSource,
    startup_wm_class: Option<String>,
}

/// Lookup table from app IDs and desktop-file identifiers to their icon source.
/// It is built once from XDG application directories and intentionally ignores entries without icons.
#[derive(Debug, Default)]
struct DesktopEntryIndex {
    icons: HashMap<String, IconSource>,
}

/// Returns decoded pixels for an app ID at a physical icon size, sharing results globally.
/// Negative cache entries are retained too, preventing repeated directory scans for unknown apps.
fn app_icon_pixels(app_id: &str, physical_size: i32) -> Option<Arc<AppIconPixels>> {
    type PixelCache = HashMap<(String, i32), Option<Arc<AppIconPixels>>>;

    static CACHE: OnceLock<Mutex<PixelCache>> = OnceLock::new();

    let cache = CACHE.get_or_init(Mutex::default);
    let key = (app_id.to_owned(), physical_size);
    let mut cache = cache.lock().unwrap();

    if let Some(pixels) = cache.get(&key) {
        return pixels.clone();
    }

    let pixels = load_app_icon_pixels(app_id, physical_size).map(Arc::new);
    cache.insert(key, pixels.clone());
    pixels
}

/// Resolves and decodes the first usable icon for the given app ID.
/// Desktop-entry hints are preferred, then the raw app ID and common lowercase fallbacks are tried.
fn load_app_icon_pixels(app_id: &str, physical_size: i32) -> Option<AppIconPixels> {
    let _span = tracy_client::span!("load_app_icon_pixels");

    for source in icon_sources_for_app_id(app_id) {
        match source {
            IconSource::File(path) => {
                if let Ok(pixels) = load_icon_file(&path, physical_size) {
                    return Some(pixels);
                }
            }
            IconSource::Name(name) => {
                for path in themed_icon_candidates(&name, physical_size) {
                    if let Ok(pixels) = load_icon_file(&path, physical_size) {
                        return Some(pixels);
                    }
                }
            }
        }
    }

    None
}

/// Builds an ordered list of icon sources that are plausible for one Wayland app ID.
/// The order keeps explicit desktop-entry icons ahead of heuristics so specific app metadata wins.
fn icon_sources_for_app_id(app_id: &str) -> Vec<IconSource> {
    let mut sources = Vec::new();
    let index = desktop_entry_index();

    for key in app_id_lookup_keys(app_id) {
        if let Some(source) = index.icons.get(&key) {
            push_source(&mut sources, source.clone());
        }
    }

    let app_id = trim_desktop_suffix(app_id);
    push_source(&mut sources, IconSource::Name(app_id.to_owned()));

    let lowercase = app_id.to_lowercase();
    push_source(&mut sources, IconSource::Name(lowercase.clone()));

    if let Some(last_component) = lowercase.rsplit('.').next() {
        push_source(&mut sources, IconSource::Name(last_component.to_owned()));
    }

    sources
}

/// Returns desktop-entry lookup keys for an app ID with and without the `.desktop` suffix.
/// Lowercase lookup covers apps and desktop files that disagree only by case.
fn app_id_lookup_keys(app_id: &str) -> [String; 3] {
    let without_suffix = trim_desktop_suffix(app_id);
    [
        app_id.to_owned(),
        without_suffix.to_owned(),
        without_suffix.to_lowercase(),
    ]
}

/// Appends an icon source while preserving first-match priority.
/// This avoids trying the same path or icon name multiple times for one lookup.
fn push_source(sources: &mut Vec<IconSource>, source: IconSource) {
    if !sources.contains(&source) {
        sources.push(source);
    }
}

/// Returns the process-wide desktop-entry index, building it lazily on first use.
/// Keeping it immutable after startup keeps the MRU render path simple and conflict-resistant.
fn desktop_entry_index() -> &'static DesktopEntryIndex {
    static INDEX: OnceLock<DesktopEntryIndex> = OnceLock::new();

    INDEX.get_or_init(build_desktop_entry_index)
}

/// Scans XDG application directories into a desktop-entry lookup index.
/// Later duplicate keys do not overwrite earlier ones, matching the priority order of `data_dirs`.
fn build_desktop_entry_index() -> DesktopEntryIndex {
    let _span = tracy_client::span!("build_desktop_entry_index");

    let mut index = DesktopEntryIndex::default();
    for dir in application_dirs() {
        scan_application_dir(&dir, &mut index);
    }
    index
}

/// Walks one application directory recursively and records usable desktop entries.
/// It indexes by desktop-file ID, file stem, and StartupWMClass to handle common Wayland and Xwayland IDs.
fn scan_application_dir(root: &Path, index: &mut DesktopEntryIndex) {
    let mut pending = vec![root.to_owned()];

    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };

            if file_type.is_dir() {
                pending.push(path);
            } else if path.extension() == Some(OsStr::new("desktop")) {
                let Some(entry) = parse_desktop_entry(&path) else {
                    continue;
                };

                if let Ok(relative) = path.strip_prefix(root) {
                    if let Some(desktop_id) = desktop_file_id(relative) {
                        index.insert(&desktop_id, entry.icon.clone());
                        index.insert(trim_desktop_suffix(&desktop_id), entry.icon.clone());
                    }
                }

                if let Some(stem) = path.file_stem().and_then(OsStr::to_str) {
                    index.insert(stem, entry.icon.clone());
                }

                if let Some(startup_wm_class) = entry.startup_wm_class {
                    index.insert(&startup_wm_class, entry.icon);
                }
            }
        }
    }
}

impl DesktopEntryIndex {
    /// Inserts a lookup key only if it has not already been provided by a higher-priority directory.
    /// Both original and lowercase forms are stored because app IDs and desktop files are not always consistent.
    fn insert(&mut self, key: &str, icon: IconSource) {
        let key = key.trim();
        if key.is_empty() {
            return;
        }

        self.icons
            .entry(key.to_owned())
            .or_insert_with(|| icon.clone());
        self.icons.entry(key.to_lowercase()).or_insert(icon);
    }
}

/// Parses the `[Desktop Entry]` group for the `Icon` and `StartupWMClass` keys.
/// The parser is intentionally small because this switcher only needs icon identity, not full desktop-file semantics.
fn parse_desktop_entry(path: &Path) -> Option<DesktopEntry> {
    let contents = fs::read_to_string(path).ok()?;
    let mut in_desktop_entry = false;
    let mut icon = None;
    let mut startup_wm_class = None;

    for line in contents.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }

        if !in_desktop_entry {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        match key {
            "Icon" => icon = Some(IconSource::from_icon_value(value)),
            "StartupWMClass" => startup_wm_class = Some(unescape_desktop_value(value)),
            _ => {}
        }
    }

    Some(DesktopEntry {
        icon: icon?,
        startup_wm_class,
    })
}

impl IconSource {
    /// Converts a desktop-entry `Icon` value into a direct file path or theme icon name.
    /// Desktop-entry escaping is decoded before checking whether the value is absolute.
    fn from_icon_value(value: &str) -> Self {
        let value = unescape_desktop_value(value);
        let path = Path::new(&value);

        if path.is_absolute() {
            Self::File(path.to_owned())
        } else {
            Self::Name(value)
        }
    }
}

/// Decodes the simple backslash escapes used in desktop-entry string values.
/// Unknown escapes are preserved so malformed-but-usable icon names are not discarded.
fn unescape_desktop_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();

    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }

        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(ch) => {
                out.push('\\');
                out.push(ch);
            }
            None => out.push('\\'),
        }
    }

    out
}

/// Converts a path relative to an applications directory into a desktop-file ID.
/// Nested directories are joined with `-`, matching the freedesktop desktop-entry ID convention.
fn desktop_file_id(relative: &Path) -> Option<String> {
    let mut id = String::new();

    for component in relative.components() {
        let Component::Normal(component) = component else {
            return None;
        };
        let component = component.to_str()?;

        if !id.is_empty() {
            id.push('-');
        }
        id.push_str(component);
    }

    Some(id)
}

/// Returns the XDG application directories that can contain `.desktop` files.
/// It delegates XDG environment handling to `data_dirs` so ordering stays consistent.
fn application_dirs() -> Vec<PathBuf> {
    data_dirs("applications")
}

/// Returns icon theme roots from XDG data dirs plus the legacy `~/.icons` directory.
/// The legacy path is still used by some custom icon themes.
fn icon_theme_dirs() -> Vec<PathBuf> {
    let mut dirs = data_dirs("icons");

    if let Some(home) = env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".icons"));
    }

    dirs
}

/// Returns XDG pixmap directories used as a final fallback for loose icon files.
/// Many older desktop entries still point at icon names that only exist in `pixmaps`.
fn pixmap_dirs() -> Vec<PathBuf> {
    data_dirs("pixmaps")
}

/// Builds ordered XDG data directories for a child folder such as `applications` or `icons`.
/// User data comes first, then `$XDG_DATA_DIRS`, falling back to the standard system paths.
fn data_dirs(child: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(data_home) = env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(data_home).join(child));
    } else if let Some(home) = env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share").join(child));
    }

    if let Some(data_dirs) = env::var_os("XDG_DATA_DIRS") {
        dirs.extend(env::split_paths(&data_dirs).map(|dir| dir.join(child)));
    } else {
        dirs.push(PathBuf::from("/usr/local/share").join(child));
        dirs.push(PathBuf::from("/usr/share").join(child));
    }

    dirs
}

/// Returns existing icon files for a theme icon name, ordered by likely visual quality.
/// It prefers configured/hicolor/Adwaita themes, closer sizes, app-context directories, and scalable files.
fn themed_icon_candidates(icon_name: &str, desired_size: i32) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    for (root_order, root) in icon_theme_dirs().into_iter().enumerate() {
        for theme in sorted_child_dirs(&root) {
            let Some(theme_name) = theme.file_name().and_then(OsStr::to_str) else {
                continue;
            };
            let theme_priority = theme_priority(theme_name);

            for size_dir in sorted_child_dirs(&theme) {
                let Some(size_name) = size_dir.file_name().and_then(OsStr::to_str) else {
                    continue;
                };
                let size_score = icon_size_score(size_name, desired_size);

                // The icon-theme spec stores app icons under context directories, but some themes
                // place files directly in the size directory, so keep both forms.
                for (context_priority, context) in ["apps", ""].into_iter().enumerate() {
                    let dir = if context.is_empty() {
                        size_dir.clone()
                    } else {
                        size_dir.join(context)
                    };

                    add_icon_file_candidates(
                        &mut candidates,
                        &dir,
                        icon_name,
                        (
                            theme_priority,
                            size_score,
                            context_priority as u16,
                            root_order as u16,
                        ),
                    );
                }
            }
        }
    }

    for (root_order, dir) in pixmap_dirs().into_iter().enumerate() {
        add_icon_file_candidates(
            &mut candidates,
            &dir,
            icon_name,
            (50, 0, 0, root_order as u16),
        );
    }

    candidates.sort_by_key(|(score, _)| *score);
    candidates.dedup_by(|(_, a), (_, b)| a == b);
    candidates.into_iter().map(|(_, path)| path).collect()
}

/// Adds concrete icon files from one directory to the scored candidate list.
/// The score tuple is kept sortable so callers can combine theme, size, context, format, and root priority.
fn add_icon_file_candidates(
    candidates: &mut Vec<((u16, u16, u16, u16, u16), PathBuf)>,
    dir: &Path,
    icon_name: &str,
    base_score: (u16, u16, u16, u16),
) {
    for (extension_priority, file_name) in icon_file_names(icon_name) {
        let path = dir.join(file_name);
        if path.is_file() {
            candidates.push((
                (
                    base_score.0,
                    base_score.1,
                    base_score.2,
                    extension_priority,
                    base_score.3,
                ),
                path,
            ));
        }
    }
}

/// Returns possible file names for an icon name, preserving explicit known extensions.
/// SVG is tried before raster formats because it scales cleanly to fractional output sizes.
fn icon_file_names(icon_name: &str) -> Vec<(u16, String)> {
    if has_known_icon_extension(icon_name) {
        return vec![(0, icon_name.to_owned())];
    }

    ["svg", "png", "xpm"]
        .into_iter()
        .enumerate()
        .map(|(idx, ext)| (idx as u16, format!("{icon_name}.{ext}")))
        .collect()
}

/// Checks whether an icon name already includes a file extension this loader understands.
/// Dotted reverse-DNS app IDs are not treated as file names unless the final component is an icon format.
fn has_known_icon_extension(icon_name: &str) -> bool {
    Path::new(icon_name)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| matches!(ext, "svg" | "png" | "xpm"))
}

/// Lists child directories in stable order, returning an empty list if the root is missing.
/// Stable ordering makes first-match behavior deterministic across filesystems.
fn sorted_child_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };

    let mut dirs: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            file_type.is_dir().then(|| entry.path())
        })
        .collect();
    dirs.sort();
    dirs
}

/// Scores an icon theme by personal-fork preference.
/// `NIRI_APP_ICON_THEME` allows overriding the default search priority without adding config plumbing.
fn theme_priority(theme_name: &str) -> u16 {
    if env::var("NIRI_APP_ICON_THEME").as_deref() == Ok(theme_name) {
        0
    } else if theme_name == "hicolor" {
        10
    } else if theme_name == "Adwaita" {
        20
    } else {
        100
    }
}

/// Scores an icon size directory by closeness to the requested physical size.
/// Scalable directories are ideal, unknown directory names are neutral, and symbolic icons are de-prioritized.
fn icon_size_score(size_name: &str, desired_size: i32) -> u16 {
    let lower = size_name.to_ascii_lowercase();

    if lower.contains("symbolic") {
        return 2000;
    }
    if lower.contains("scalable") {
        return 0;
    }

    parse_icon_dir_size(&lower)
        .map(|size| (size - desired_size).unsigned_abs().min(1999) as u16)
        .unwrap_or(1000)
}

/// Parses icon-theme size directory names like `48x48` or `32x32@2`.
/// The returned value is the effective physical size, using the larger dimension for safety.
fn parse_icon_dir_size(size_name: &str) -> Option<i32> {
    let (base, scale) = size_name.split_once('@').unwrap_or((size_name, "1"));
    let (width, height) = base.split_once('x')?;

    let width: i32 = width.parse().ok()?;
    let height: i32 = height.parse().ok()?;
    let scale: i32 = scale.parse().ok()?;

    Some(width.max(height) * scale.max(1))
}

/// Loads an icon file at the requested physical size using gdk-pixbuf.
/// gdk-pixbuf handles PNG, SVG, and XPM here, letting this module avoid format-specific decoders.
fn load_icon_file(path: &Path, physical_size: i32) -> anyhow::Result<AppIconPixels> {
    let pixbuf = Pixbuf::from_file_at_scale(path, physical_size, physical_size, true)
        .with_context(|| format!("error loading icon {}", path.display()))?;

    pixels_from_pixbuf(&pixbuf)
}

/// Copies a pixbuf into tightly packed RGBA bytes for Smithay texture import.
/// Rowstride padding is stripped and RGB icons get an opaque alpha channel.
fn pixels_from_pixbuf(pixbuf: &Pixbuf) -> anyhow::Result<AppIconPixels> {
    ensure!(pixbuf.bits_per_sample() == 8, "expected 8-bit app icon");

    let width = pixbuf.width();
    let height = pixbuf.height();
    let channels = pixbuf.n_channels();
    let rowstride = pixbuf.rowstride();

    ensure!(width > 0 && height > 0, "empty app icon");
    ensure!(
        channels == 3 || channels == 4,
        "unexpected app icon channels"
    );

    let bytes = pixbuf.read_pixel_bytes();
    let bytes: &[u8] = bytes.as_ref();
    let last_row_len = width as usize * channels as usize;
    let min_len = (height as usize - 1) * rowstride as usize + last_row_len;
    ensure!(bytes.len() >= min_len, "truncated app icon pixel data");

    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);

    for y in 0..height as usize {
        let row_start = y * rowstride as usize;
        let row = &bytes[row_start..];

        for x in 0..width as usize {
            let offset = x * channels as usize;
            rgba.push(row[offset]);
            rgba.push(row[offset + 1]);
            rgba.push(row[offset + 2]);
            rgba.push(if channels == 4 { row[offset + 3] } else { 255 });
        }
    }

    Ok(AppIconPixels {
        data: rgba.into(),
        width,
        height,
    })
}

/// Removes a `.desktop` suffix when present.
/// Some clients expose the full desktop-file name as app ID while others expose the bare ID.
fn trim_desktop_suffix(value: &str) -> &str {
    value.strip_suffix(".desktop").unwrap_or(value)
}
