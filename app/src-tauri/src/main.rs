// Quire desktop app — GPL-3.0-or-later. The engine crates it calls are MIT OR Apache-2.0.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod pdf;
mod print;
mod scope;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use scope::{Remembered, Scope};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use tauri::menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, DragDropEvent, Emitter, Manager, State, WebviewWindow, WebviewWindowBuilder, Wry};
use tauri_plugin_dialog::DialogExt;

type CmdResult<T> = Result<T, String>;

fn err(e: quire_service::ServiceError) -> String {
    e.0
}

// ---------------------------------------------------------------------------
// The files the user chose (scope.rs): every path from the webview is checked against them
// ---------------------------------------------------------------------------

type Chosen = Mutex<Scope>;

/// The scope. A poisoned lock still holds a valid scope, so it is used as it is.
fn lock(chosen: &Chosen) -> MutexGuard<'_, Scope> {
    chosen.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Refuse a path the user didn't choose. Any check also ends the window in which the recent
/// lists of an earlier version may be adopted: from here on the webview may hold a document.
fn allowed(chosen: &Chosen, path: &Path) -> CmdResult<()> {
    let mut scope = lock(chosen);
    scope.seal();
    scope.check(path)
}

/// Where the recent documents and notebooks are kept between sessions.
fn remembered_file(app: &AppHandle) -> CmdResult<PathBuf> {
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("recent.json"))
}

/// The saved lists: `None` when there are none yet. A damaged file counts as empty lists, so
/// it never reopens the adoption of an earlier version's lists.
fn load_remembered(app: &AppHandle) -> Option<Remembered> {
    let text = std::fs::read_to_string(remembered_file(app).ok()?).ok()?;
    Some(serde_json::from_str(&text).unwrap_or_default())
}

fn save_remembered(app: &AppHandle, remembered: &Remembered) -> CmdResult<()> {
    let file = remembered_file(app)?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = file.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(remembered).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &file).map_err(|e| e.to_string())
}

/// What a native dialog is for. The kind, not the webview, decides the filters, the extension
/// a saved file gets and how much the choice grants (one file, or a whole notebook folder).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Pick {
    Open,
    Save,
    Pdf,
    Csv,
    Image,
    Notebook,
    NewNotebook,
}

/// `.quire` packages on macOS, `.quirez` single files elsewhere (see `DOC_EXT` in api.ts).
const DOC_EXT: &str = if cfg!(target_os = "macos") { "quire" } else { "quirez" };

/// Run a native dialog and add what the user picked to the scope. `suggested` is the file name a
/// save dialog starts with; only its last part is used.
#[tauri::command]
async fn pick(window: WebviewWindow, kind: Pick, suggested: Option<String>) -> CmdResult<Option<String>> {
    let mut dialog = window.dialog().file();
    #[cfg(any(windows, target_os = "macos"))]
    {
        dialog = dialog.set_parent(&window);
    }
    if let Some(name) = suggested.as_deref().and_then(|s| Path::new(s).file_name()) {
        dialog = dialog.set_file_name(name.to_string_lossy());
    }
    let mac = cfg!(target_os = "macos");
    let picked = match kind {
        Pick::Open => dialog
            .set_title("Open")
            .add_filter("Quire document", if mac { &["quire", "quirez"] } else { &["quirez", "json"] })
            .add_filter("SMath Studio worksheet", &["sm"])
            .blocking_pick_file(),
        Pick::Save if mac => dialog
            .add_filter("Quire document", &["quire"])
            .add_filter("Quire single file (for email and other systems)", &["quirez"])
            .blocking_save_file(),
        Pick::Save => dialog.add_filter("Quire document", &["quirez"]).blocking_save_file(),
        Pick::Pdf => dialog.add_filter("PDF", &["pdf"]).blocking_save_file(),
        Pick::Csv => dialog.add_filter("CSV", &["csv"]).blocking_save_file(),
        Pick::Image => dialog
            .set_title("Insert image")
            .add_filter("Images", &["png", "jpg", "jpeg", "gif", "svg", "webp"])
            .blocking_pick_file(),
        Pick::Notebook => dialog.set_title("Open Notebook").blocking_pick_folder(),
        Pick::NewNotebook => dialog.set_title("New Notebook").blocking_save_file(),
    };
    let Some(picked) = picked else { return Ok(None) };
    let path = picked.simplified().into_path().map_err(|e| e.to_string())?;
    let path = match kind {
        Pick::Open => scope::document_path(path),
        Pick::Save => scope::with_ext(path, &["quire", "quirez"], DOC_EXT),
        Pick::Pdf => scope::with_ext(path, &["pdf"], "pdf"),
        Pick::Csv => scope::with_ext(path, &["csv"], "csv"),
        Pick::Image | Pick::Notebook | Pick::NewNotebook => path,
    };
    let remembered = {
        let mut scope = lock(window.state::<Chosen>().inner());
        match kind {
            Pick::Notebook | Pick::NewNotebook => {
                scope.grant_notebook(&path);
                Some(scope.remembered().clone())
            }
            _ => {
                scope.grant_file(&path);
                None
            }
        }
    };
    if let Some(r) = remembered {
        save_remembered(window.app_handle(), &r)?;
    }
    Ok(Some(path.to_string_lossy().into_owned()))
}

#[tauri::command]
fn report(doc: Json) -> CmdResult<quire_service::Report> {
    quire_service::report(&doc).map_err(err)
}

#[tauri::command]
fn open_document(chosen: State<'_, Chosen>, path: PathBuf) -> CmdResult<Json> {
    allowed(&chosen, &path)?;
    quire_service::open(&path).map_err(err)
}

/// `from` is the document's previous home, whose images a Save As copies: it is read, so it
/// is checked as well.
#[tauri::command]
fn save_document(
    chosen: State<'_, Chosen>,
    path: PathBuf,
    doc: Json,
    from: Option<PathBuf>,
) -> CmdResult<()> {
    allowed(&chosen, &path)?;
    if let Some(from) = &from {
        allowed(&chosen, from)?;
    }
    quire_service::save(&path, &doc, from.as_deref()).map_err(err)
}

/// Unsaved work, kept in the app's data folder so a crash or power cut loses little.
fn recovery_file(app: &AppHandle) -> CmdResult<PathBuf> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("recovery.json"))
}

/// The recovered document's path is granted again in the next session, so only a path the
/// user chose is kept; any other recovers as an untitled document.
#[tauri::command]
fn save_recovery(
    app: AppHandle,
    chosen: State<'_, Chosen>,
    doc: Json,
    path: Option<String>,
) -> CmdResult<()> {
    let path = path.filter(|p| lock(&chosen).allows(Path::new(p)));
    let file = recovery_file(&app)?;
    let tmp = file.with_extension("json.tmp");
    let body = json!({ "path": path, "doc": doc });
    std::fs::write(&tmp, body.to_string()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &file).map_err(|e| e.to_string())
}

/// The unsaved work left by the last session, if it ended without saving or discarding.
#[tauri::command]
fn take_recovery(app: AppHandle, chosen: State<'_, Chosen>) -> CmdResult<Option<Json>> {
    let file = recovery_file(&app)?;
    let Ok(text) = std::fs::read_to_string(&file) else { return Ok(None) };
    let recovered: Option<Json> = serde_json::from_str(&text).ok();
    // `save_recovery` kept the path only if it was in scope then
    if let Some(path) = recovered.as_ref().and_then(|r| r["path"].as_str()) {
        lock(&chosen).grant_file(Path::new(path));
    }
    Ok(recovered)
}

#[tauri::command]
fn clear_recovery(app: AppHandle) -> CmdResult<()> {
    let file = recovery_file(&app)?;
    match std::fs::remove_file(file) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        _ => Ok(()),
    }
}

fn templates_dir(app: &AppHandle) -> CmdResult<PathBuf> {
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("templates"))
}

#[tauri::command]
fn list_templates(app: AppHandle) -> CmdResult<Vec<quire_service::templates::TemplateInfo>> {
    Ok(quire_service::templates::list(&templates_dir(&app)?))
}

#[tauri::command]
fn open_template(app: AppHandle, id: String) -> CmdResult<quire_service::templates::TemplateDocument> {
    quire_service::templates::document(&templates_dir(&app)?, &id).map_err(err)
}

#[tauri::command]
fn save_template(
    app: AppHandle,
    chosen: State<'_, Chosen>,
    name: String,
    description: String,
    doc: Json,
    from: Option<PathBuf>,
) -> CmdResult<String> {
    if let Some(from) = &from {
        allowed(&chosen, from)?;
    }
    quire_service::templates::save_as(&templates_dir(&app)?, &name, &description, &doc, from.as_deref())
        .map_err(err)
}

#[tauri::command]
fn delete_template(app: AppHandle, id: String) -> CmdResult<()> {
    quire_service::templates::delete(&templates_dir(&app)?, &id).map_err(err)
}

#[tauri::command]
fn library() -> quire_service::Library {
    quire_service::library()
}

/// A table's Export as CSV…: only ever writes a `.csv` file the user picked.
#[tauri::command]
fn export_csv(chosen: State<'_, Chosen>, path: PathBuf, contents: String) -> CmdResult<()> {
    allowed(&chosen, &path)?;
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("csv")) {
        return Err("a table can only be exported to a .csv file".into());
    }
    std::fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))
}

/// Notebooks: list, create, rename, move, trash and search notes in a folder, which must be a
/// notebook the user chose. Opening one (`list`) moves it to the front of the recent notebooks.
#[tauri::command]
fn notebook(app: AppHandle, chosen: State<'_, Chosen>, op: String, args: Json) -> CmdResult<Json> {
    let folder = quire_service::notebook::folder(&op, &args).map_err(err)?;
    let remembered = {
        let mut scope = lock(&chosen);
        scope.seal();
        scope.check_notebook(&folder)?;
        let before = scope.remembered().notebooks.first().cloned();
        if op == "list" {
            scope.grant_notebook(&folder);
        }
        let r = scope.remembered();
        (r.notebooks.first() != before.as_ref()).then(|| r.clone())
    };
    if let Some(r) = remembered {
        save_remembered(&app, &r)?;
    }
    quire_service::notebook::call(&op, &args).map_err(err)
}

#[tauri::command]
fn djot_html(src: String) -> String {
    quire_service::render_djot_editable(&src)
}

#[tauri::command]
fn import_image(chosen: State<'_, Chosen>, path: PathBuf) -> CmdResult<String> {
    allowed(&chosen, &path)?;
    quire_service::assets::stage_file(&path).map_err(err)
}

#[tauri::command]
fn stage_image(data: String, ext: String) -> CmdResult<String> {
    quire_service::assets::stage_base64(&data, &ext).map_err(err)
}

/// An untitled document (`bundle` empty) has only the images added since it was opened.
#[tauri::command]
fn read_asset(chosen: State<'_, Chosen>, bundle: PathBuf, src: String) -> CmdResult<String> {
    if bundle.as_os_str().is_empty() {
        if !quire_service::assets::is_staged(&src) {
            return Err(format!("{src} is missing from this unsaved document"));
        }
    } else {
        allowed(&chosen, &bundle)?;
    }
    quire_service::read_asset(&bundle, &src).map_err(err)
}

#[tauri::command]
fn new_document() -> Json {
    quire_service::new_document()
}

#[tauri::command]
async fn print_document(window: WebviewWindow, paper: print::Paper, title: String) -> CmdResult<()> {
    print::print(&window, paper, title)
}

#[tauri::command]
async fn export_pdf(
    window: WebviewWindow,
    path: PathBuf,
    paper: print::Paper,
    title: String,
    compress: bool,
    info: Option<print::PdfInfo>,
) -> CmdResult<()> {
    allowed(window.state::<Chosen>().inner(), &path)?;
    print::export_pdf(&window, path, paper, title, compress, info.unwrap_or_default())
}

/// End of a headless `--export-pdf` run.
#[tauri::command]
fn finish_export(app: AppHandle, error: Option<String>) {
    match error {
        None => app.exit(0),
        Some(e) => {
            eprintln!("quire: export failed: {e}");
            app.exit(1);
        }
    }
}

/// `quire-app --export-pdf <document.quire> <output.pdf>`: render without showing a window.
/// Returns the document and the PDF as absolute paths with no `..`, as the scope compares them.
fn export_job() -> Option<(PathBuf, PathBuf)> {
    let args: Vec<String> = std::env::args().collect();
    let i = args.iter().position(|a| a == "--export-pdf")?;
    let (input, output) = (args.get(i + 1)?, args.get(i + 2)?);
    let cwd = std::env::current_dir().ok()?;
    let input = std::fs::canonicalize(cwd.join(input)).ok()?;
    let output = cwd.join(output);
    let output = std::fs::canonicalize(output.parent()?).ok()?.join(output.file_name()?);
    Some((input, output))
}

fn item(app: &AppHandle, id: &str, label: &str, accel: Option<&str>) -> tauri::Result<MenuItem<Wry>> {
    MenuItem::with_id(app, id, label, true, accel)
}

/// File ▸ Open Recent, refilled whenever the UI's list changes.
struct RecentMenu(Submenu<Wry>);

fn fill_recent(app: &AppHandle, menu: &Submenu<Wry>, paths: &[String]) -> tauri::Result<()> {
    for item in menu.items()? {
        menu.remove(&item)?;
    }
    if paths.is_empty() {
        menu.append(&MenuItem::with_id(app, "recent_none", "No Recent Documents", false, None::<&str>)?)?;
        return Ok(());
    }
    for (i, p) in paths.iter().enumerate() {
        let path = std::path::Path::new(p.trim_end_matches(['/', '\\']));
        let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.clone());
        // the folder disambiguates two documents with the same name
        let folder = path.parent().and_then(|d| d.file_name()).map(|d| d.to_string_lossy().into_owned());
        let label = match folder {
            Some(f) => format!("{name}  —  {f}"),
            None => name,
        };
        menu.append(&MenuItem::with_id(app, format!("recent:{i}"), label, true, None::<&str>)?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "recent_clear", "Clear Menu", true, None::<&str>)?)?;
    Ok(())
}

/// The UI's File ▸ Open Recent. Only documents the user chose are kept; they are remembered
/// for the next session and returned, so the UI's list matches the menu.
#[tauri::command]
fn set_recent(app: AppHandle, chosen: State<'_, Chosen>, paths: Vec<String>) -> CmdResult<Vec<String>> {
    let (kept, remembered) = {
        let mut scope = lock(&chosen);
        let kept = scope.set_recent(&paths);
        (kept, scope.remembered().clone())
    };
    save_remembered(&app, &remembered)?;
    let kept: Vec<String> = kept.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let menu = app.state::<RecentMenu>();
    fill_recent(&app, &menu.0, &kept).map_err(|e| e.to_string())?;
    Ok(kept)
}

/// Versions up to 0.2.1 kept the recent lists only in the webview. They are taken over once,
/// at the first start of a later version and before any document is open (`Scope::adopt`).
#[tauri::command]
fn adopt_recent(
    app: AppHandle,
    chosen: State<'_, Chosen>,
    documents: Vec<String>,
    notebooks: Vec<String>,
) -> CmdResult<()> {
    let remembered = {
        let mut scope = lock(&chosen);
        scope.adopt(&documents, &notebooks).then(|| scope.remembered().clone())
    };
    match remembered {
        Some(r) => save_remembered(&app, &r),
        None => Ok(()),
    }
}

/// Native menu bar. Custom items are forwarded to the UI as `menu` events carrying the item id;
/// clipboard items are native so they work in text fields.
fn menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let sep = || PredefinedMenuItem::separator(app);
    let about = AboutMetadata {
        comments: Some("Free-form math notebook".into()),
        license: Some("GPL-3.0-or-later (engine: MIT OR Apache-2.0)".into()),
        ..Default::default()
    };
    // macOS keeps Settings and Quit in the app menu; elsewhere they live in File
    let mac = cfg!(target_os = "macos");
    let settings = item(app, "settings", "Settings…", Some("CmdOrCtrl+,"))?;
    let recent = Submenu::with_id(app, "recent", "Open Recent", true)?;
    fill_recent(app, &recent, &[])?;
    app.manage(RecentMenu(recent.clone()));
    let file = Submenu::with_items(
        app,
        "File",
        true,
        &[
            &item(app, "new", "New Note", Some("CmdOrCtrl+N"))?,
            &item(app, "new_section", "New Section", Some("CmdOrCtrl+Alt+N"))?,
            &item(app, "new_from_template", "New from Template…", Some("CmdOrCtrl+Shift+N"))?,
            &sep()?,
            &item(app, "new_notebook", "New Notebook…", None)?,
            &item(app, "open_notebook", "Open Notebook…", Some("CmdOrCtrl+Shift+O"))?,
            &sep()?,
            &item(app, "open", "Open…", Some("CmdOrCtrl+O"))?,
            &recent,
            &sep()?,
            &item(app, "save", "Save", Some("CmdOrCtrl+S"))?,
            &item(app, "save_as", "Save As…", Some("CmdOrCtrl+Shift+S"))?,
            &item(app, "save_template", "Save as Template…", None)?,
            &sep()?,
            &item(app, "export_pdf", "Export as PDF…", Some("CmdOrCtrl+E"))?,
            &item(app, "share_link", "Share Link…", None)?,
            &item(app, "print", "Print…", Some("CmdOrCtrl+P"))?,
            &sep()?,
            &item(app, "document_settings", "Document Settings…", Some("CmdOrCtrl+Alt+,"))?,
            &item(app, "page_setup", "Page Setup…", None)?,
            &sep()?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;
    if !mac {
        file.append_items(&[&sep()?, &settings, &sep()?, &PredefinedMenuItem::quit(app, Some("Quit"))?])?;
    }
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &item(app, "undo", "Undo", Some("CmdOrCtrl+Z"))?,
            &item(app, "redo", "Redo", Some("CmdOrCtrl+Shift+Z"))?,
            &sep()?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &item(app, "duplicate", "Duplicate", Some("CmdOrCtrl+D"))?,
            &item(app, "delete", "Delete", None)?,
            &item(app, "select_all", "Select All", Some("CmdOrCtrl+A"))?,
            &sep()?,
            &item(app, "find", "Find…", Some("CmdOrCtrl+F"))?,
            &item(app, "find_replace", "Find and Replace…", Some("CmdOrCtrl+Alt+F"))?,
            // Ctrl+G turns letters into Greek in math, so Windows and Linux use F3
            &item(app, "find_next", "Find Next", Some(if mac { "Cmd+G" } else { "F3" }))?,
            &item(app, "find_prev", "Find Previous", Some(if mac { "Cmd+Shift+G" } else { "Shift+F3" }))?,
        ],
    )?;
    let insert = Submenu::with_items(
        app,
        "Insert",
        true,
        &[
            &item(app, "insert_math", "Math", Some("CmdOrCtrl+Shift+M"))?,
            &item(app, "insert_text", "Text Region", Some("CmdOrCtrl+Shift+T"))?,
            &item(app, "insert_image", "Image…", Some("CmdOrCtrl+Shift+I"))?,
            &item(app, "insert_table", "Table", Some("CmdOrCtrl+Alt+T"))?,
            &item(app, "import_csv", "Table from CSV…", None)?,
            &item(app, "insert_plot", "Plot", Some("CmdOrCtrl+Alt+P"))?,
            &item(app, "insert_area", "Area", None)?,
            &sep()?,
            &item(app, "insert_toc", "Table of Contents", None)?,
            &item(app, "insert_figures", "List of Figures", None)?,
            &item(app, "insert_tables", "List of Tables", None)?,
            &item(app, "insert_checks", "Summary of Checks", None)?,
            &sep()?,
            &item(app, "insert_ref", "Cross-Reference…", Some("CmdOrCtrl+Shift+R"))?,
        ],
    )?;
    let format = Submenu::with_items(
        app,
        "Format",
        true,
        &[
            &item(app, "fmt_bold", "Bold", Some("CmdOrCtrl+B"))?,
            &item(app, "fmt_italic", "Italic", Some("CmdOrCtrl+I"))?,
            &item(app, "fmt_underline", "Underline", Some("CmdOrCtrl+U"))?,
            &item(app, "fmt_strike", "Strikethrough", None)?,
            &sep()?,
            &item(app, "fmt_align_left", "Align Left", None)?,
            &item(app, "fmt_align_center", "Center", None)?,
            &item(app, "fmt_align_right", "Align Right", None)?,
            &item(app, "fmt_align_justify", "Justify", None)?,
            &sep()?,
            &item(app, "fmt_bullets", "Bulleted List", None)?,
            &item(app, "fmt_numbers", "Numbered List", None)?,
        ],
    )?;
    let arrange = Submenu::with_items(
        app,
        "Arrange",
        true,
        &[
            &item(app, "arrange_left", "Align Left Edges", None)?,
            &item(app, "arrange_hcenter", "Align Centres", None)?,
            &item(app, "arrange_right", "Align Right Edges", None)?,
            &sep()?,
            &item(app, "arrange_top", "Align Tops", None)?,
            &item(app, "arrange_vcenter", "Align Middles", None)?,
            &item(app, "arrange_bottom", "Align Bottoms", None)?,
            &sep()?,
            &item(app, "distribute_x", "Distribute Horizontally", None)?,
            &item(app, "distribute_y", "Distribute Vertically", None)?,
        ],
    )?;
    let view = Submenu::with_items(
        app,
        "View",
        true,
        &[
            &item(app, "command_palette", "Command Palette…", Some("CmdOrCtrl+K"))?,
            &item(app, "search_notebook", "Search Notebook…", Some("CmdOrCtrl+Shift+F"))?,
            &sep()?,
            &item(app, "toggle_sidebar", "Show Sidebar", Some("CmdOrCtrl+Backslash"))?,
            &item(app, "page_view", "Page View", Some("CmdOrCtrl+Shift+P"))?,
            &item(app, "toggle_inspector", "Show Inspector", Some("CmdOrCtrl+Alt+I"))?,
            &sep()?,
            &item(app, "zoom_in", "Zoom In", Some("CmdOrCtrl+="))?,
            &item(app, "zoom_out", "Zoom Out", Some("CmdOrCtrl+-"))?,
            &item(app, "zoom_reset", "Actual Size", Some("CmdOrCtrl+0"))?,
            &item(app, "zoom_fit_width", "Zoom to Width", None)?,
            &item(app, "zoom_fit_page", "Zoom to Page", None)?,
            &sep()?,
            &item(app, "next_error", "Next Error", Some("CmdOrCtrl+'"))?,
            &sep()?,
            &PredefinedMenuItem::fullscreen(app, None)?,
        ],
    )?;
    let window = Submenu::with_items(
        app,
        "Window",
        true,
        &[&PredefinedMenuItem::minimize(app, None)?, &PredefinedMenuItem::maximize(app, None)?],
    )?;
    let help_item = item(app, "help", "Quire Help", Some(if mac { "Cmd+Shift+/" } else { "F1" }))?;
    let updates = item(app, "check_updates", "Check for Updates…", None)?;
    if mac {
        let app_menu = Submenu::with_items(
            app,
            "Quire",
            true,
            &[
                &PredefinedMenuItem::about(app, Some("About Quire"), Some(about))?,
                &updates,
                &sep()?,
                &settings,
                &sep()?,
                &PredefinedMenuItem::services(app, None)?,
                &sep()?,
                &PredefinedMenuItem::hide(app, None)?,
                &PredefinedMenuItem::hide_others(app, None)?,
                &PredefinedMenuItem::show_all(app, None)?,
                &sep()?,
                &PredefinedMenuItem::quit(app, None)?,
            ],
        )?;
        let help = Submenu::with_items(app, "Help", true, &[&help_item])?;
        return Menu::with_items(
            app,
            &[&app_menu, &file, &edit, &insert, &format, &arrange, &view, &window, &help],
        );
    }
    let help = Submenu::with_items(
        app,
        "Help",
        true,
        &[&help_item, &updates, &sep()?, &PredefinedMenuItem::about(app, Some("About Quire"), Some(about))?],
    )?;
    Menu::with_items(app, &[&file, &edit, &insert, &format, &arrange, &view, &window, &help])
}

/// Documents the system asked us to open (command line, Finder, Explorer) before the
/// UI was ready to take them; afterwards they go straight to the UI as `open-file` events.
#[derive(Default)]
struct OpenRequests {
    ready: bool,
    pending: Vec<String>,
}

type Requests = std::sync::Mutex<OpenRequests>;

/// What the system may hand us to open. An SMath `.sm` opens as an untitled document (the UI
/// does the import), so the file itself is never written.
fn is_document(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.ends_with(".quire")
        || p.ends_with(".quirez")
        || p.ends_with(".quire/")
        || p.ends_with("document.json")
        || p.ends_with(".sm")
}

/// The open requests. A poisoned lock still holds a valid list, so it is used as it is.
fn requests_lock(state: &Requests) -> std::sync::MutexGuard<'_, OpenRequests> {
    state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// An OS open request is the user's choice: the document joins the scope before the UI sees it.
fn request_open(app: &AppHandle, path: PathBuf) {
    let path = scope::document_path(path);
    lock(&app.state::<Chosen>()).grant_file(&path);
    let path = path.to_string_lossy().into_owned();
    let state = app.state::<Requests>();
    let mut r = requests_lock(&state);
    if r.ready {
        let _ = app.emit("open-file", path);
    } else {
        r.pending.push(path);
    }
}

/// Called once by the UI when it can open documents: hands over what arrived before.
#[tauri::command]
fn take_open_requests(state: State<'_, Requests>, chosen: State<'_, Chosen>) -> Vec<String> {
    lock(&chosen).seal();
    let mut r = requests_lock(&state);
    r.ready = true;
    std::mem::take(&mut r.pending)
}

/// Images dropped on the window were chosen by the user, like a picked file.
fn grant_dropped(app: &AppHandle, event: &DragDropEvent) {
    if let DragDropEvent::Drop { paths, .. } = event {
        let mut scope = lock(app.state::<Chosen>().inner());
        for p in paths.iter().filter(|p| quire_service::assets::is_image(p)) {
            scope.grant_file(p);
        }
    }
}

fn main() {
    let job = export_job();
    // Windows and Linux pass a double-clicked document as an argument, maybe a relative one
    let cwd = std::env::current_dir().unwrap_or_default();
    let launch_docs: Vec<PathBuf> = if job.is_some() {
        Vec::new()
    } else {
        std::env::args()
            .skip(1)
            .filter(|a| !a.starts_with('-') && is_document(a))
            .filter_map(|a| std::fs::canonicalize(cwd.join(a)).ok())
            .map(scope::document_path)
            .collect()
    };
    if std::env::args().any(|a| a == "--export-pdf") && job.is_none() {
        eprintln!("usage: quire-app --export-pdf <document.quire> <output.pdf>");
        std::process::exit(2);
    }
    // what the command line names was chosen by the user; the saved lists join in `setup`
    let mut chosen = Scope::default();
    for p in &launch_docs {
        chosen.grant_file(p);
    }
    if let Some((input, output)) = &job {
        chosen.grant_file(input);
        chosen.grant_file(output);
    }
    let pending = launch_docs.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let built = tauri::Builder::default()
        .manage::<Requests>(Mutex::new(OpenRequests { ready: false, pending }))
        .manage::<Chosen>(Mutex::new(chosen))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .menu(menu)
        .on_menu_event(|app, event| {
            let _ = app.emit("menu", event.id().0.as_str());
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::DragDrop(drop) = event {
                grant_dropped(window.app_handle(), drop);
            }
        })
        .on_webview_event(|webview, event| {
            if let tauri::WebviewEvent::DragDrop(drop) = event {
                grant_dropped(webview.app_handle(), drop);
            }
        })
        .setup(move |app| {
            {
                let remembered = load_remembered(app.handle());
                let mut scope = lock(app.state::<Chosen>().inner());
                scope.restore(remembered);
                // your templates, and the images they hold until a new document is saved
                scope.grant_folder(&templates_dir(app.handle())?);
            }
            let config = app.config().app.windows.first().cloned().ok_or("tauri.conf.json has no window")?;
            let mut window = WebviewWindowBuilder::from_config(app.handle(), &config)?;
            if let Some((input, output)) = &job {
                let job = json!({ "open": input, "pdf": output });
                window =
                    window.visible(false).initialization_script(format!("window.__QUIRE_EXPORT__ = {job};"));
            }
            window.build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            report,
            open_document,
            save_document,
            read_asset,
            new_document,
            list_templates,
            open_template,
            save_template,
            delete_template,
            djot_html,
            library,
            import_image,
            stage_image,
            print_document,
            export_pdf,
            finish_export,
            export_csv,
            notebook,
            take_open_requests,
            set_recent,
            adopt_recent,
            pick,
            save_recovery,
            take_recovery,
            clear_recovery
        ])
        .build(tauri::generate_context!());
    let app = match built {
        Ok(app) => app,
        Err(e) => {
            eprintln!("Quire could not start: {e}");
            std::process::exit(1);
        }
    };
    app.run(|handle, event| {
        // macOS delivers Finder double-clicks and "Open With" as events, also at launch
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Opened { urls } = event {
            for url in urls {
                if let Ok(path) = url.to_file_path() {
                    request_open(handle, path);
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (handle, event, request_open);
    });
}
