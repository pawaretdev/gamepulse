//! Library page (launcher-style game cards) and each game's detail page
//!
//! Cards swap the "Play button" for an alerts on/off switch: watch several games at once by switching on several cards.
//! Covers are not embedded in the exe (publisher-copyrighted art); users pick their own (right-click a card, or the icon on the cover)
//! and the app copies it to `covers/<id>.<ext>`. No file = plain placeholder cover with the first letter of the game's name.
//!
//! Never use `ui.put` to place widgets over a card: it moves the layout cursor past that widget,
//! so whatever follows overlaps. Use `overlay` (a child Ui that doesn't move the parent's cursor) instead.
use super::*;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Filter {
    All,
    Watching,
    Running,
}

/// What the user asked for on the hub page, for App to handle (navigate, choose/remove cover)
pub(super) enum Action {
    Open(&'static GameProfile),
    Back,
    ChooseCover(&'static GameProfile),
    RemoveCover(&'static GameProfile),
}

pub(super) struct Cover {
    art: egui::TextureHandle,
    /// Same image shrunk and blurred, used as the banner background (upscaling a tiny image with LINEAR = free soft blur)
    backdrop: egui::TextureHandle,
}

pub(super) type Covers = HashMap<&'static str, Cover>;

const TILE_WIDTH: f32 = 180.0;
/// 3:4, the usual game cover ratio
const COVER_HEIGHT: f32 = 240.0;
const BANNER_HEIGHT: f32 = 180.0;
const COVER_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];
/// Shrink oversized covers on load: no need to keep a 4K image on the GPU to show a 180 px card
const MAX_COVER_SIDE: u32 = 1280;
/// Cover files larger than this are rejected, so picking the wrong file (e.g. a RAW photo) can't eat RAM while decoding
const MAX_COVER_FILE: u64 = 20 * 1024 * 1024;

fn covers_dir() -> PathBuf {
    root_path("covers")
}

fn cover_file(game: &GameProfile) -> Option<PathBuf> {
    COVER_EXTENSIONS
        .iter()
        .map(|ext| covers_dir().join(format!("{}.{ext}", game.id)))
        .find(|path| path.is_file())
}

fn texture(context: &egui::Context, name: String, image: &image::RgbaImage) -> egui::TextureHandle {
    let size = [image.width() as usize, image.height() as usize];
    let pixels = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
    context.load_texture(name, pixels, egui::TextureOptions::LINEAR)
}

pub(super) fn load_cover(
    context: &egui::Context,
    game: &GameProfile,
) -> Result<Option<Cover>, String> {
    let Some(path) = cover_file(game) else {
        return Ok(None);
    };
    let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let image = image::load_from_memory(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let image = if image.width().max(image.height()) > MAX_COVER_SIDE {
        image.thumbnail(MAX_COVER_SIDE, MAX_COVER_SIDE)
    } else {
        image
    };
    let backdrop = image::imageops::blur(&image.thumbnail(96, 96).to_rgba8(), 2.5);
    Ok(Some(Cover {
        art: texture(context, format!("cover-{}", game.id), &image.to_rgba8()),
        backdrop: texture(context, format!("backdrop-{}", game.id), &backdrop),
    }))
}

/// Load every game's cover at startup; broken files fall back to the placeholder and are reported in Events
pub(super) fn load_covers(context: &egui::Context, engine: &mut Engine) -> Covers {
    let mut covers = HashMap::new();
    for game in GAMES {
        match load_cover(context, game) {
            Ok(Some(cover)) => {
                covers.insert(game.id, cover);
            }
            Ok(None) => {}
            Err(error) => engine.push(Level::Warn, format!("Cannot load cover {error}")),
        }
    }
    covers
}

/// The Windows file dialog (its own modal loop) must be called off the UI thread,
/// otherwise the frame freezes while holding the Engine lock, which would also stop monitoring
fn pick_image() -> Option<PathBuf> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    let filter: Vec<u16> = "Images (PNG, JPG, WebP)\0*.png;*.jpg;*.jpeg;*.webp\0\0"
        .encode_utf16()
        .collect();
    let mut file = vec![0u16; 32768];
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrTitle: w!("Choose cover image"),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: every buffer the dialog points to lives until this blocking call returns
    if !unsafe { GetOpenFileNameW(&mut dialog) }.as_bool() {
        return None;
    }
    let end = file.iter().position(|c| *c == 0)?;
    Some(PathBuf::from(String::from_utf16_lossy(&file[..end])))
}

/// Verify it is a real image first, then copy it to `covers/<id>.<ext>`, replacing the old cover (the source file is untouched)
fn install_cover(game: &GameProfile, source: &Path) -> Result<(), String> {
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|e| COVER_EXTENSIONS.contains(&e.as_str()))
        .ok_or("use a PNG, JPG or WebP file")?;
    let size = fs::metadata(source).map_err(|e| e.to_string())?.len();
    if size > MAX_COVER_FILE {
        return Err(format!("file is over {} MiB", MAX_COVER_FILE / 1024 / 1024));
    }
    let bytes = fs::read(source).map_err(|e| e.to_string())?;
    image::load_from_memory(&bytes).map_err(|e| format!("not a readable image: {e}"))?;
    fs::create_dir_all(covers_dir()).map_err(|e| e.to_string())?;
    remove_cover_files(game).map_err(|e| e.to_string())?;
    fs::write(covers_dir().join(format!("{}.{ext}", game.id)), bytes).map_err(|e| e.to_string())
}

pub(super) fn remove_cover_files(game: &GameProfile) -> std::io::Result<()> {
    for ext in COVER_EXTENSIONS {
        let path = covers_dir().join(format!("{}.{ext}", game.id));
        if path.is_file() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

/// Result: Ok(true) = cover changed, Ok(false) = user cancelled
pub(super) type CoverJob = Receiver<(&'static GameProfile, Result<bool, String>)>;

pub(super) fn choose_cover(game: &'static GameProfile) -> CoverJob {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = match pick_image() {
            None => Ok(false),
            Some(path) => install_cover(game, &path).map(|()| true),
        };
        let _ = sender.send((game, result));
    });
    receiver
}

/// Crop the image to fill the frame without distortion (like CSS object-fit: cover)
/// `anchor_y` = vertical position kept when the image is too tall (0 top, 0.5 center)
fn cover_uv(texture: egui::Vec2, target: egui::Vec2, anchor_y: f32) -> egui::Rect {
    let (source, wanted) = (texture.x / texture.y, target.x / target.y);
    if source > wanted {
        let width = wanted / source;
        egui::Rect::from_min_max(
            egui::pos2((1.0 - width) / 2.0, 0.0),
            egui::pos2((1.0 + width) / 2.0, 1.0),
        )
    } else {
        let height = source / wanted;
        let top = (1.0 - height) * anchor_y;
        egui::Rect::from_min_max(egui::pos2(0.0, top), egui::pos2(1.0, top + height))
    }
}

/// "Ragnarok Origin Classic" -> "R"
fn monogram(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default()
}

fn paint_texture(ui: &Ui, rect: egui::Rect, texture: &egui::TextureHandle, anchor_y: f32) {
    egui::Image::new(texture)
        .uv(cover_uv(texture.size_vec2(), rect.size(), anchor_y))
        .corner_radius(CornerRadius::same(12))
        .paint_at(ui, rect);
}

fn paint_cover(ui: &Ui, rect: egui::Rect, game: &GameProfile, cover: Option<&Cover>) {
    match cover {
        Some(cover) => paint_texture(ui, rect, &cover.art, 0.5),
        None => {
            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same(12), p().card);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                monogram(game.name),
                rounded((rect.height() * 0.32).clamp(32.0, 80.0)),
                p().muted,
            );
        }
    }
}

/// Gradient from transparent to `to` going down, so text over the image stays readable in every theme
fn fade(painter: &egui::Painter, rect: egui::Rect, to: Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), Color32::TRANSPARENT);
    mesh.colored_vertex(rect.right_top(), Color32::TRANSPARENT);
    mesh.colored_vertex(rect.right_bottom(), to);
    mesh.colored_vertex(rect.left_bottom(), to);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(mesh);
}

/// Pill painted directly on the painter, so it can sit on top of a cover
fn paint_pill(painter: &egui::Painter, left_center: egui::Pos2, text: &str, color: Color32) {
    let galley = painter.layout_no_wrap(text.into(), semibold(12.0), color);
    let size = egui::vec2(galley.size().x + 28.0, 22.0);
    let rect = egui::Rect::from_min_size(left_center - egui::vec2(0.0, size.y / 2.0), size);
    painter.rect_filled(rect, CornerRadius::same(99), tint(p().panel, 235));
    painter.rect_filled(rect, CornerRadius::same(99), tint(color, 34));
    painter.rect_stroke(
        rect,
        CornerRadius::same(99),
        Stroke::new(1.0_f32, tint(color, 110)),
        egui::StrokeKind::Inside,
    );
    painter.circle_filled(rect.left_center() + egui::vec2(12.0, 0.0), 3.5, color);
    painter.galley(
        rect.left_center() + egui::vec2(20.0, -galley.size().y / 2.0),
        galley,
        color,
    );
}

/// Add a widget over a given rect without moving the parent's layout
/// (widgets added later get the click before the card underneath)
fn overlay<R>(ui: &mut Ui, rect: egui::Rect, add: impl FnOnce(&mut Ui) -> R) -> R {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::centered_and_justified(egui::Direction::TopDown)),
    );
    add(&mut child)
}

/// Alerts on/off switch placed over the cover
fn watch_toggle(ui: &mut Ui, rect: egui::Rect, engine: &mut Engine, game: &'static GameProfile) {
    let on = engine.config.watches(game.id);
    let (label, color) = if on {
        ("Alerts on", p().green)
    } else {
        ("Alerts off", p().muted)
    };
    // Leave room on the left to paint the dot ourselves: egui's default font has no bold ● glyph
    let button = egui::Button::new(
        RichText::new(format!("    {label}"))
            .small()
            .semibold()
            .color(color),
    )
    .fill(tint(p().panel, 235))
    .stroke(Stroke::new(1.0_f32, tint(color, 110)))
    .corner_radius(CornerRadius::same(99));
    let hover = if on {
        "Stop alerting for this game. Its status still shows here."
    } else {
        "Alert me when this game disconnects"
    };
    let response = overlay(ui, rect, |ui| ui.add(button)).on_hover_text(hover);
    let center = egui::pos2(response.rect.left() + 14.0, response.rect.center().y);
    if on {
        ui.painter().circle_filled(center, 3.5, color);
    } else {
        ui.painter()
            .circle_stroke(center, 3.5, Stroke::new(1.5_f32, color));
    }
    if response.clicked() {
        engine.set_watched(game, !on);
    }
}

/// Image icon button (frame + mountain + sun) for changing the cover, painted by hand because the font has no such icon
fn cover_button(ui: &mut Ui, rect: egui::Rect) -> bool {
    let button = egui::Button::new("")
        .fill(tint(p().panel, 235))
        .stroke(Stroke::new(1.0_f32, p().border))
        .corner_radius(CornerRadius::same(8));
    let response = overlay(ui, rect, |ui| ui.add(button)).on_hover_text("Change cover image");
    let color = if response.hovered() {
        p().blue
    } else {
        p().text
    };
    let icon = egui::Rect::from_center_size(response.rect.center(), egui::vec2(14.0, 11.0));
    let painter = ui.painter();
    painter.rect_stroke(
        icon,
        CornerRadius::same(2),
        Stroke::new(1.3_f32, color),
        egui::StrokeKind::Middle,
    );
    painter.add(egui::Shape::convex_polygon(
        vec![
            icon.left_bottom() + egui::vec2(1.5, -1.5),
            icon.left_bottom() + egui::vec2(5.5, -6.5),
            icon.left_bottom() + egui::vec2(9.0, -1.5),
        ],
        color,
        Stroke::NONE,
    ));
    painter.circle_filled(icon.right_top() + egui::vec2(-3.5, 3.5), 1.6, color);
    response.clicked()
}

/// "Library" button with a 2×2 grid icon (painted by hand, the font has no ← arrow)
fn library_button(ui: &mut Ui) -> bool {
    let response = ui.button("       Library");
    let color = if response.hovered() {
        p().blue
    } else {
        p().text
    };
    let origin = egui::pos2(response.rect.left() + 13.0, response.rect.center().y - 6.0);
    for (x, y) in [(0.0, 0.0), (7.0, 0.0), (0.0, 7.0), (7.0, 7.0)] {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(origin + egui::vec2(x, y), egui::vec2(5.0, 5.0)),
            CornerRadius::same(1),
            color,
        );
    }
    response.clicked()
}

/// Right-click menu of a cover (both library cards and the cover on the game page)
fn cover_menu(
    response: &egui::Response,
    engine: &mut Engine,
    game: &'static GameProfile,
    has_cover: bool,
    action: &mut Option<Action>,
) {
    response.context_menu(|ui| {
        if ui.button("Choose cover image...").clicked() {
            *action = Some(Action::ChooseCover(game));
            ui.close_menu();
        }
        if has_cover && ui.button("Remove cover").clicked() {
            *action = Some(Action::RemoveCover(game));
            ui.close_menu();
        }
        ui.separator();
        let on = engine.config.watches(game.id);
        if ui
            .button(if on {
                "Turn alerts off"
            } else {
                "Turn alerts on"
            })
            .clicked()
        {
            engine.set_watched(game, !on);
            ui.close_menu();
        }
    });
}

fn client_summary(engine: &Engine, game: &GameProfile) -> String {
    match engine.snapshot.as_ref().map(|s| s.counts(game.id)) {
        None => "Checking...".into(),
        Some((_, 0)) => "Not running".into(),
        Some((online, total)) => format!("{online} of {total} clients online"),
    }
}

fn game_tile(
    ui: &mut Ui,
    engine: &mut Engine,
    game: &'static GameProfile,
    cover: Option<&Cover>,
) -> Option<Action> {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(TILE_WIDTH, COVER_HEIGHT + 50.0),
        egui::Sense::click(),
    );
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let cover_rect = egui::Rect::from_min_size(rect.min, egui::vec2(TILE_WIDTH, COVER_HEIGHT));
    let watched = engine.config.watches(game.id);
    let (status, color) = engine.game_status(game);

    paint_cover(ui, cover_rect, game, cover);
    let painter = ui.painter();
    if !watched {
        // Games with alerts off are dimmed, but the status below stays readable
        painter.rect_filled(cover_rect, CornerRadius::same(12), tint(p().bg, 110));
    }
    let strip = egui::Rect::from_min_max(
        egui::pos2(cover_rect.left(), cover_rect.bottom() - 64.0),
        cover_rect.max,
    );
    fade(painter, strip, tint(p().bg, 220));
    paint_pill(
        painter,
        egui::pos2(cover_rect.left() + 10.0, cover_rect.bottom() - 20.0),
        &status,
        color,
    );
    let hovered = ui.rect_contains_pointer(cover_rect);
    let border = if hovered {
        Stroke::new(2.0_f32, p().blue)
    } else if watched && color == p().red {
        // Dropped/closed game that is being watched: red border visible from across the room
        Stroke::new(2.0_f32, p().red)
    } else {
        Stroke::new(1.0_f32, p().border)
    };
    painter.rect_stroke(
        cover_rect,
        CornerRadius::same(12),
        border,
        egui::StrokeKind::Inside,
    );

    let name = painter.layout(game.name.to_string(), semibold(14.0), p().text, TILE_WIDTH);
    let name_height = name.size().y;
    let name_pos = egui::pos2(rect.left() + 2.0, cover_rect.bottom() + 8.0);
    painter.galley(name_pos, name, p().text);
    painter.text(
        name_pos + egui::vec2(0.0, name_height + 2.0),
        egui::Align2::LEFT_TOP,
        client_summary(engine, game),
        FontId::proportional(12.0),
        p().muted,
    );

    let mut action = response.clicked().then_some(Action::Open(game));
    watch_toggle(
        ui,
        egui::Rect::from_min_size(
            cover_rect.right_top() + egui::vec2(-112.0, 8.0),
            egui::vec2(104.0, 24.0),
        ),
        engine,
        game,
    );
    // The change-cover icon appears only on hover, to keep the overview clean
    if hovered
        && cover_button(
            ui,
            egui::Rect::from_min_size(
                cover_rect.min + egui::vec2(8.0, 8.0),
                egui::vec2(28.0, 24.0),
            ),
        )
    {
        action = Some(Action::ChooseCover(game));
    }
    cover_menu(&response, engine, game, cover.is_some(), &mut action);
    action
}

fn chip(ui: &mut Ui, filter: &mut Filter, value: Filter, label: &str) {
    let selected = *filter == value;
    let text = RichText::new(label)
        .semibold()
        .color(if selected { p().bg } else { p().muted });
    let button = egui::Button::new(text)
        .fill(if selected { p().blue } else { p().card })
        .stroke(Stroke::new(
            1.0_f32,
            if selected { p().blue } else { p().border },
        ))
        .corner_radius(CornerRadius::same(99))
        .min_size(egui::vec2(0.0, 30.0));
    if ui.add(button).clicked() {
        *filter = value;
    }
}

fn snapshot_notes(ui: &mut Ui, engine: &Engine) {
    if let Some(snapshot) = &engine.snapshot {
        for error in &snapshot.errors {
            ui.label(RichText::new(format!("⚠ {error}")).color(p().amber));
        }
    }
    if !engine.watching {
        ui.add_space(8.0);
        hint(
            ui,
            "Paused — status still refreshes on Check now, but no alerts are sent.",
            p().amber,
        );
    }
}

pub(super) fn library(
    ui: &mut Ui,
    engine: &mut Engine,
    covers: &Covers,
    filter: &mut Filter,
) -> Option<Action> {
    ui.horizontal(|ui| {
        chip(ui, filter, Filter::All, "All");
        chip(ui, filter, Filter::Watching, "Alerts on");
        chip(ui, filter, Filter::Running, "Running");
    });
    ui.add_space(10.0);
    let games: Vec<&'static GameProfile> = GAMES
        .iter()
        .filter(|game| match filter {
            Filter::All => true,
            Filter::Watching => engine.config.watches(game.id),
            Filter::Running => engine
                .snapshot
                .as_ref()
                .is_some_and(|s| s.counts(game.id).1 > 0),
        })
        .collect();
    ui.label(
        RichText::new(format!("LIBRARY   {}", games.len()))
            .small()
            .semibold()
            .color(p().muted),
    );
    ui.add_space(6.0);
    let mut action = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if games.is_empty() {
                hint(ui, "No games match this filter.", p().muted);
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(18.0, 18.0);
                for game in games {
                    if let Some(clicked) = game_tile(ui, engine, game, covers.get(game.id)) {
                        action = Some(clicked);
                    }
                }
            });
            ui.add_space(8.0);
            hint(ui, "Right-click a card to change its cover.", p().muted);
            snapshot_notes(ui, engine);
        });
    action
}

/// Steam-style game header: sharp portrait cover on the left, the same cover blurred and darkened as background.
/// The portrait cover is not stretched to full width (a 640 px image stretched turns blurry and only shows hair)
fn game_header(
    ui: &mut Ui,
    engine: &mut Engine,
    game: &'static GameProfile,
    cover: Option<&Cover>,
) -> Option<Action> {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), BANNER_HEIGHT),
        egui::Sense::hover(),
    );
    match cover {
        Some(cover) => {
            paint_texture(ui, rect, &cover.backdrop, 0.35);
            ui.painter()
                .rect_filled(rect, CornerRadius::same(12), tint(p().bg, 150));
        }
        None => {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(12), p().card);
        }
    }
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(12),
        Stroke::new(1.0_f32, p().border),
        egui::StrokeKind::Inside,
    );

    let art_height = BANNER_HEIGHT - 32.0;
    let art = egui::Rect::from_min_size(
        rect.min + egui::vec2(16.0, 16.0),
        egui::vec2(art_height * 0.75, art_height),
    );
    let art_response = ui.interact(art, ui.id().with(("cover", game.id)), egui::Sense::click());
    paint_cover(ui, art, game, cover);
    ui.painter().rect_stroke(
        art,
        CornerRadius::same(12),
        Stroke::new(1.0_f32, p().border),
        egui::StrokeKind::Inside,
    );

    let left = art.right() + 20.0;
    let painter = ui.painter();
    painter.text(
        egui::pos2(left, rect.top() + 22.0),
        egui::Align2::LEFT_TOP,
        game.name,
        semibold(24.0),
        p().text,
    );
    let (status, color) = engine.game_status(game);
    paint_pill(painter, egui::pos2(left, rect.top() + 76.0), &status, color);
    painter.text(
        egui::pos2(left, rect.top() + 98.0),
        egui::Align2::LEFT_TOP,
        client_summary(engine, game),
        FontId::proportional(13.0),
        p().muted,
    );
    painter.text(
        egui::pos2(left, rect.top() + 118.0),
        egui::Align2::LEFT_TOP,
        format!("Process: {}", game.exe),
        FontId::monospace(12.0),
        p().muted,
    );

    watch_toggle(
        ui,
        egui::Rect::from_min_size(
            rect.right_top() + egui::vec2(-124.0, 16.0),
            egui::vec2(108.0, 26.0),
        ),
        engine,
        game,
    );
    let mut action = None;
    if ui.rect_contains_pointer(art)
        && cover_button(
            ui,
            egui::Rect::from_min_size(art.min + egui::vec2(8.0, 8.0), egui::vec2(28.0, 24.0)),
        )
    {
        action = Some(Action::ChooseCover(game));
    }
    cover_menu(&art_response, engine, game, cover.is_some(), &mut action);
    action
}

/// MVP pictures, user-supplied like covers: `mvps/<name>.png` next to config.json
/// (e.g. `mvps/golden-thief-bug.png`). Game art is never bundled; no file = a letter placeholder
pub(super) type MvpArt = HashMap<&'static str, egui::TextureHandle>;

const MVP_ART_DIR: &str = "mvps";
const MVP_ICON: f32 = 30.0;

fn mvp_file_name(name: &str) -> String {
    name.to_ascii_lowercase().replace(' ', "-")
}

pub(super) fn load_mvp_art(context: &egui::Context, engine: &mut Engine) -> MvpArt {
    let mut art = HashMap::new();
    for mvp in GAMES.iter().flat_map(|g| g.mvps) {
        let path = root_path(MVP_ART_DIR).join(format!("{}.png", mvp_file_name(mvp.name)));
        if !path.is_file() {
            continue;
        }
        match image::open(&path) {
            Ok(image) => {
                let small = image.thumbnail(96, 96).to_rgba8();
                art.insert(
                    mvp.name,
                    texture(context, format!("mvp-{}", mvp.name), &small),
                );
            }
            Err(error) => engine.push(
                Level::Warn,
                format!("Cannot load MVP picture {}: {error}", path.display()),
            ),
        }
    }
    art
}

/// Picture (or first letter) in a rounded square edged with the status color
fn mvp_icon(ui: &mut Ui, art: Option<&egui::TextureHandle>, name: &str, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(MVP_ICON, MVP_ICON), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(7), p().panel);
    match art {
        Some(texture) => {
            egui::Image::new(texture)
                .uv(cover_uv(texture.size_vec2(), rect.size(), 0.5))
                .corner_radius(CornerRadius::same(7))
                .paint_at(ui, rect.shrink(1.0));
        }
        None => {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                monogram(name),
                rounded(15.0),
                p().muted,
            );
        }
    }
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(7),
        Stroke::new(1.5_f32, tint(color, 200)),
        egui::StrokeKind::Inside,
    );
}

/// Rounded chip like the library filters; `active` = already in that state (shown filled)
fn toolbar_chip(ui: &mut Ui, label: &str, active: bool) -> bool {
    let text =
        RichText::new(label)
            .small()
            .semibold()
            .color(if active { p().bg } else { p().text });
    let button = egui::Button::new(text)
        .fill(if active { p().blue } else { p().panel })
        .stroke(Stroke::new(
            1.0_f32,
            if active { p().blue } else { p().border },
        ))
        .corner_radius(CornerRadius::same(99))
        .min_size(egui::vec2(0.0, 24.0));
    ui.add(button).clicked()
}

/// "Clear timers" asks for a second click within 4 s, so one stray click can't wipe them
fn clear_button(ui: &mut Ui) -> bool {
    let id = egui::Id::new("clear-mvp-timers");
    let now = ui.input(|input| input.time);
    let armed = ui
        .data(|data| data.get_temp::<f64>(id))
        .is_some_and(|at| now - at < 4.0);
    let (label, color) = if armed {
        ("Click again to clear", p().amber)
    } else {
        ("Clear timers", p().muted)
    };
    let button = egui::Button::new(RichText::new(label).small().semibold().color(color))
        .fill(if armed {
            tint(p().amber, 30)
        } else {
            p().panel
        })
        .stroke(Stroke::new(
            1.0_f32,
            if armed { p().amber } else { p().border },
        ))
        .corner_radius(CornerRadius::same(99))
        .min_size(egui::vec2(0.0, 24.0));
    let response = ui.add(button).on_hover_text(
        "Forget every MVP timer and start counting again from the next announcements. \
         Use it after Gamepulse or the tracker was off for a while.",
    );
    if !response.clicked() {
        return false;
    }
    if armed {
        ui.data_mut(|data| data.remove::<f64>(id));
        true
    } else {
        ui.data_mut(|data| data.insert_temp(id, now));
        false
    }
}

/// MVP tracker: on/off, and every MVP with its next spawn and an alert checkbox
fn mvp_section(ui: &mut Ui, engine: &mut Engine, art: &MvpArt) {
    ui.add_space(16.0);
    let on = engine.boss_watch.is_some();
    ui.horizontal(|ui| {
        ui.label(RichText::new("MVP tracker").size(16.0).semibold());
        ui.add_space(4.0);
        let (status, color) = engine.boss_status();
        pill(ui, &status, color);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (label, fill, text) = if on {
                ("Turn off", p().card, p().text)
            } else {
                ("Turn on", p().green, p().on_green)
            };
            let button = egui::Button::new(RichText::new(label).semibold().color(text))
                .fill(fill)
                .min_size(egui::vec2(96.0, 28.0));
            let hover = if on {
                "Stop reading the game window (no CPU used while off)"
            } else {
                "Read the game's MVP announcements from its window once a second"
            };
            if ui.add(button).on_hover_text(hover).clicked() {
                engine.set_boss_tracker(!on);
            }
            let route = engine.route_open();
            let label = if route {
                "Close route window"
            } else {
                "Route window"
            };
            if ui
                .add(egui::Button::new(label).min_size(egui::vec2(0.0, 28.0)))
                .on_hover_text(
                    "A small window that stays on top of the game: which MVP to go for first, \
                     then next, by spawn time. Right-click it to show ticked or all MVPs",
                )
                .clicked()
            {
                engine.toggle_route();
            }
        });
    });
    match (&engine.boss_problem, on) {
        (_, false) => hint(
            ui,
            "Off: nothing is captured. Timers below are from the last time it ran.",
            p().muted,
        ),
        (Some(problem), true) if problem != "Starting..." => hint(ui, problem, p().amber),
        _ => hint(
            ui,
            "Every MVP respawns 1 hour after it dies. Ticked MVPs alert as set in Settings › MVP tracker.",
            p().muted,
        ),
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        for (color, label) in [
            (p().green, "up"),
            (p().amber, "30% HP"),
            (p().blue, "spawns soon"),
            (p().red, "dead"),
            (p().muted, "not seen yet"),
        ] {
            dot(ui, color, 3.5);
            ui.label(RichText::new(label).small().color(p().muted));
            ui.add_space(6.0);
        }
    });
    ui.add_space(4.0);
    let rows = engine.boss_rows();
    let mut toggled: Option<(&'static str, bool)> = None;
    let mut all: Option<bool> = None;
    let mut clear = false;
    let ticked = rows.iter().filter(|r| r.tracked).count();
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Alerts for {ticked} of {} MVPs", rows.len()))
                    .color(p().muted),
            );
            ui.add_space(6.0);
            if toolbar_chip(ui, "Tick all", ticked == rows.len()) {
                all = Some(true);
            }
            if toolbar_chip(ui, "Untick all", ticked == 0) {
                all = Some(false);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                clear = clear_button(ui);
            });
        });
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        egui::Grid::new("mvps")
            .num_columns(3)
            .spacing([18.0, 7.0])
            .show(ui, |ui| {
                for row in &rows {
                    let mut tracked = row.tracked;
                    ui.horizontal(|ui| {
                        mvp_icon(ui, art.get(row.name), row.name, row.color);
                        if ui
                            .checkbox(&mut tracked, RichText::new(row.name).semibold())
                            .on_hover_text("Send alerts for this MVP")
                            .changed()
                        {
                            toggled = Some((row.name, tracked));
                        }
                    });
                    ui.label(RichText::new(row.map).color(p().muted));
                    ui.horizontal(|ui| {
                        dot(ui, row.color, 3.5);
                        let color = if row.color == p().muted {
                            p().muted
                        } else {
                            p().text
                        };
                        ui.label(RichText::new(&row.status).color(color));
                    });
                    ui.end_row();
                }
            });
    });
    if clear {
        engine.clear_boss_timers();
    }
    if let Some((name, on)) = toggled {
        engine.set_boss_tracked(name, on);
    }
    if let Some(on) = all {
        engine.set_all_bosses_tracked(on);
    }
}

pub(super) fn game_page(
    ui: &mut Ui,
    engine: &mut Engine,
    game: &'static GameProfile,
    cover: Option<&Cover>,
    art: &MvpArt,
) -> Option<Action> {
    let mut action = library_button(ui).then_some(Action::Back);
    ui.add_space(6.0);
    let cards = engine.cards(game);
    let mut clicked = None;
    // The banner scrolls with the page: scrolling down to the MVP tracker pushes it off the top
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if let Some(header) = game_header(ui, engine, game, cover) {
                action = Some(header);
            }
            ui.add_space(12.0);
            ui.label(RichText::new("Clients").size(16.0).semibold());
            ui.add_space(4.0);
            match &engine.snapshot {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new(format!("Looking for {}...", game.name)).color(p().muted),
                        );
                    });
                }
                Some(_) if cards.is_empty() => empty_state(
                    ui,
                    "Not running",
                    &format!("Start {} — clients appear here automatically.", game.name),
                    if engine.watching && engine.config.watches(game.id) {
                        p().red
                    } else {
                        p().muted
                    },
                ),
                Some(_) => {
                    let columns = if ui.available_width() > 640.0 { 2 } else { 1 };
                    ui.columns(columns, |columns| {
                        for (index, card) in cards.iter().enumerate() {
                            if let Some(card_action) =
                                client_card(&mut columns[index % columns.len()], card)
                            {
                                clicked = Some((card.identity, card_action));
                            }
                        }
                    });
                }
            }
            if !engine.config.watches(game.id) {
                ui.add_space(8.0);
                hint(
                    ui,
                    "Alerts are off for this game. Turn them on with the switch in the header.",
                    p().amber,
                );
            }
            snapshot_notes(ui, engine);
            if !game.mvps.is_empty() {
                mvp_section(ui, engine, art);
            }
        });
    match clicked {
        Some((identity, CardAction::Pin(on))) => engine.set_pinned(identity, on),
        Some((identity, CardAction::Pip(on))) => engine.set_pip(identity, on),
        None => {}
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_crop_keeps_aspect_and_anchor() {
        // 3:4 cover into a 3:4 frame = use the whole image
        let full = cover_uv(egui::vec2(640.0, 860.0), egui::vec2(640.0, 860.0), 0.5);
        assert!((full.width() - 1.0).abs() < 1e-4 && (full.height() - 1.0).abs() < 1e-4);
        // Portrait cover into a landscape frame: crop top/bottom around the anchor
        let top = cover_uv(egui::vec2(640.0, 860.0), egui::vec2(800.0, 160.0), 0.0);
        assert_eq!(top.min.y, 0.0);
        assert!((top.width() - 1.0).abs() < 1e-4 && top.height() < 0.3);
        // Wide image into a narrow frame: crop left/right equally
        let wide = cover_uv(egui::vec2(1920.0, 1080.0), egui::vec2(180.0, 240.0), 0.5);
        assert!((wide.min.x - (1.0 - wide.max.x)).abs() < 1e-4 && wide.height() == 1.0);
    }

    #[test]
    fn mvp_picture_names_match_rooc_kit() {
        assert_eq!(mvp_file_name("Golden Thief Bug"), "golden-thief-bug");
        assert_eq!(mvp_file_name("Moonlight Flower"), "moonlight-flower");
    }

    #[test]
    fn placeholder_monogram() {
        assert_eq!(monogram("Ragnarok Origin Classic"), "R");
        assert_eq!(monogram("  ever planet"), "E");
        assert_eq!(monogram(""), "");
    }

    #[test]
    fn install_cover_rejects_non_images_without_touching_covers() {
        let dir = std::env::temp_dir().join(format!("gamepulse-cover-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let text = dir.join("notes.txt");
        fs::write(&text, b"hello").unwrap();
        assert!(install_cover(&GAMES[0], &text).is_err());
        let fake = dir.join("fake.png");
        fs::write(&fake, b"not a png").unwrap();
        assert!(install_cover(&GAMES[0], &fake)
            .unwrap_err()
            .contains("not a readable image"));
        let _ = fs::remove_dir_all(dir);
    }
}
