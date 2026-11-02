//! Notebooks: an ordinary folder of notes, as Obsidian keeps a vault. Sections are
//! subfolders (nested as deep as you like) and notes are ordinary `.quire` / `.quirez`
//! documents, so a notebook syncs through iCloud, Dropbox or Git and every note still
//! opens on its own.
//!
//! ```text
//! Physics 101/
//! ├── .quire-notebook.json     optional: manual order, section colours and icons
//! ├── Mechanics/
//! │   ├── Kinematics.quire     a note: its file name is its title
//! │   └── Newton's laws.quire
//! └── Lab reports/
//! ```
//!
//! Nothing here depends on `.quire-notebook.json`: without it items sort sections first,
//! then notes, by name (`Note 2` before `Note 10`). Everything is addressed by its path
//! relative to the notebook (`Mechanics/Kinematics.quire`, `/` on every system).

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::{Result, ServiceError};

/// The optional settings file at the notebook's root.
pub const META_FILE: &str = ".quire-notebook.json";
const NOTE_EXTS: [&str; 2] = ["quire", "quirez"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Section,
    Note,
}

/// A section or a note.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub kind: Kind,
    /// What the sidebar shows: a section's folder name, a note's file name without extension.
    pub name: String,
    /// Path from the notebook's root, `/`-separated.
    pub rel: String,
    /// Last change, in milliseconds since 1970 (notes: their document; sections: the folder).
    pub modified: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notebook {
    pub name: String,
    pub path: String,
    pub children: Vec<Node>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Style {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// `.quire-notebook.json`. Unknown keys are kept, so later versions can add their own.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Meta {
    /// Folder (`""` for the root) → its items' file names, in the order the user put them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    order: BTreeMap<String, Vec<String>>,
    /// Item → its colour and icon.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    style: BTreeMap<String, Style>,
    #[serde(flatten)]
    rest: BTreeMap<String, Json>,
}

fn err(msg: impl Into<String>) -> ServiceError {
    ServiceError(msg.into())
}

/// Set `meta.title` of a document. A document opened from disk may hold anything, so a
/// `meta` that is neither missing nor an object is reported instead of overwritten.
fn set_title(doc: &mut Json, title: String) -> Result<()> {
    let Some(root) = doc.as_object_mut() else {
        return Err(err("the document is not an object"));
    };
    let meta = root.entry("meta").or_insert(Json::Null);
    if meta.is_null() {
        *meta = Json::Object(serde_json::Map::new());
    }
    let Some(meta) = meta.as_object_mut() else {
        return Err(err("the document's meta is not an object"));
    };
    meta.insert("title".into(), Json::String(title));
    Ok(())
}

// ---------------------------------------------------------------------------
// Paths and names
// ---------------------------------------------------------------------------

/// Whether a path is a note (a `.quire` package or a `.quirez` file).
pub fn is_note(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| NOTE_EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// `root` joined with a relative path that must stay inside it.
fn inside(root: &Path, rel: &str) -> Result<PathBuf> {
    let mut out = root.to_path_buf();
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        let p = Path::new(part);
        if p.components().count() != 1 || !matches!(p.components().next(), Some(Component::Normal(_))) {
            return Err(err(format!("{rel} is not inside the notebook")));
        }
        out.push(part);
    }
    Ok(out)
}

fn rel_of(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"))
        .unwrap_or_default()
}

fn parent_rel(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(p, _)| p)
}

fn file_name(rel: &str) -> &str {
    rel.rsplit_once('/').map_or(rel, |(_, n)| n)
}

fn join_rel(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") }
}

/// A title as a file name: trimmed, and refused when it can't be one on every system.
pub fn clean_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(err("a name can't be empty"));
    }
    if name.starts_with('.') {
        return Err(err("a name can't start with a full stop"));
    }
    if let Some(c) = name
        .chars()
        .find(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control())
    {
        return Err(err(format!("a name can't contain {c}")));
    }
    if name.chars().count() > 120 {
        return Err(err("a name can be at most 120 characters long"));
    }
    Ok(name.to_string())
}

/// `name` + `ext` in `dir`, numbered (`Untitled 2`) if that is taken.
fn unique(dir: &Path, name: &str, ext: Option<&str>) -> PathBuf {
    let file = |n: &str| match ext {
        Some(e) => format!("{n}.{e}"),
        None => n.to_string(),
    };
    let taken = |p: &Path| {
        p.exists()
            // a note and its other format share a title: `Beam.quire` blocks `Beam.quirez`
            || (ext.is_some() && NOTE_EXTS.iter().any(|e| p.with_extension(e).exists()))
    };
    let mut path = dir.join(file(name));
    let mut n = 2;
    while taken(&path) {
        path = dir.join(file(&format!("{name} {n}")));
        n += 1;
    }
    path
}

fn modified(path: &Path) -> Option<u64> {
    // a package's document.json changes when the note does; the folder itself may not
    let p = if path.is_dir() && is_note(path) { path.join("document.json") } else { path.to_path_buf() };
    let t = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(t.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64)
}

/// Natural order, ignoring case: `Note 2` before `Note 10`, `apple` beside `Apple`.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let num = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        s.push(c);
                        it.next();
                    }
                    s
                };
                let (na, nb) = (num(&mut a), num(&mut b));
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                let o = fold(x).cmp(&fold(y));
                if o != Ordering::Equal {
                    return o;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// A character without case or accents, for sorting and search (`É` → `e`).
fn fold(c: char) -> char {
    let c = c.to_lowercase().next().unwrap_or(c);
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        _ => c,
    }
}

fn folded(s: &str) -> String {
    s.chars().map(fold).collect()
}

// ---------------------------------------------------------------------------
// Meta
// ---------------------------------------------------------------------------

fn read_meta(root: &Path) -> Meta {
    std::fs::read_to_string(root.join(META_FILE))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_meta(root: &Path, meta: &Meta) -> Result<()> {
    let path = root.join(META_FILE);
    if meta.order.is_empty() && meta.style.is_empty() && meta.rest.is_empty() {
        // nothing to keep: no file, so a plain folder stays a plain folder
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        return Ok(());
    }
    let tmp = root.join(".quire-notebook.json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(meta)? + "\n")?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// After `from` became `to` (renamed or moved), keys that named it or lay under it follow.
fn rekey(meta: &mut Meta, from: &str, to: &str) {
    let follow = |k: &str| -> Option<String> {
        if k == from {
            Some(to.to_string())
        } else {
            k.strip_prefix(from).filter(|r| r.starts_with('/')).map(|r| format!("{to}{r}"))
        }
    };
    meta.order =
        std::mem::take(&mut meta.order).into_iter().map(|(k, v)| (follow(&k).unwrap_or(k), v)).collect();
    meta.style =
        std::mem::take(&mut meta.style).into_iter().map(|(k, v)| (follow(&k).unwrap_or(k), v)).collect();
}

/// Forget what lay at or under `rel`.
fn forget(meta: &mut Meta, rel: &str) {
    let gone = |k: &String| k == rel || k.strip_prefix(rel).is_some_and(|r| r.starts_with('/'));
    meta.order.retain(|k, _| !gone(k));
    meta.style.retain(|k, _| !gone(k));
    if let Some(list) = meta.order.get_mut(parent_rel(rel)) {
        list.retain(|n| n != file_name(rel));
    }
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

/// The notebook at `root`: its sections and notes, in order.
pub fn list(root: &Path) -> Result<Notebook> {
    if !root.is_dir() {
        return Err(err(format!("{} is not a folder", root.display())));
    }
    if is_note(root) {
        return Err(err("a document can't be opened as a notebook"));
    }
    let meta = read_meta(root);
    let children = list_dir(root, root, &meta, 0)?;
    Ok(Notebook {
        name: root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        path: root.to_string_lossy().into_owned(),
        children,
    })
}

fn list_dir(root: &Path, dir: &Path, meta: &Meta, depth: usize) -> Result<Vec<Node>> {
    if depth > 16 {
        return Ok(Vec::new()); // a link loop, or a folder nobody means as a notebook
    }
    let mut nodes = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let rel = rel_of(root, &path);
        let style = meta.style.get(&rel).cloned().unwrap_or_default();
        if is_note(&path) {
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(name);
            nodes.push(Node {
                kind: Kind::Note,
                name: stem,
                modified: modified(&path),
                rel,
                color: style.color,
                icon: style.icon,
                children: vec![],
            });
        } else if path.is_dir() {
            let children = list_dir(root, &path, meta, depth + 1)?;
            nodes.push(Node {
                kind: Kind::Section,
                name,
                modified: modified(&path),
                rel,
                color: style.color,
                icon: style.icon,
                children,
            });
        }
    }
    let order = meta.order.get(&rel_of(root, dir));
    let rank = |n: &Node| order.and_then(|o| o.iter().position(|x| x == file_name(&n.rel)));
    nodes.sort_by(|a, b| match (rank(a), rank(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        // what the order doesn't know yet comes after what it does
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => {
            (a.kind == Kind::Note).cmp(&(b.kind == Kind::Note)).then_with(|| natural_cmp(&a.name, &b.name))
        }
    });
    Ok(nodes)
}

/// The file names in `dir`, as the sidebar shows them.
fn ordered_names(root: &Path, dir_rel: &str) -> Result<Vec<String>> {
    let meta = read_meta(root);
    let dir = inside(root, dir_rel)?;
    Ok(list_dir(root, &dir, &meta, 0)?.into_iter().map(|n| file_name(&n.rel).to_string()).collect())
}

// ---------------------------------------------------------------------------
// Changes
// ---------------------------------------------------------------------------

/// Make a notebook: the folder, and a first note to write in. Returns the note.
pub fn create_notebook(path: &Path, ext: &str) -> Result<String> {
    if path.exists() && std::fs::read_dir(path)?.next().is_some() {
        return Err(err(format!("{} already exists and isn't empty", path.display())));
    }
    std::fs::create_dir_all(path)?;
    create_note(path, "", "Untitled", ext, None)
}

/// A new note in section `dir`, titled `title` (numbered if taken), at `index` among its
/// neighbours (the end by default). Returns its path from the root.
pub fn create_note(root: &Path, dir: &str, title: &str, ext: &str, index: Option<usize>) -> Result<String> {
    let title = clean_name(title)?;
    if !NOTE_EXTS.contains(&ext) {
        return Err(err(format!("a note is a .quire or .quirez document, not .{ext}")));
    }
    let folder = inside(root, dir)?;
    if !folder.is_dir() {
        return Err(err(format!("there is no section {dir}")));
    }
    let path = unique(&folder, &title, Some(ext));
    let mut doc = crate::new_document();
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(title);
    set_title(&mut doc, stem)?;
    crate::save(&path, &doc, None)?;
    let rel = rel_of(root, &path);
    place(root, &rel, index)?;
    Ok(rel)
}

/// A new section (folder) in `dir`. Returns its path from the root.
pub fn create_section(root: &Path, dir: &str, name: &str, index: Option<usize>) -> Result<String> {
    let name = clean_name(name)?;
    let folder = inside(root, dir)?;
    let path = unique(&folder, &name, None);
    std::fs::create_dir(&path)?;
    let rel = rel_of(root, &path);
    place(root, &rel, index)?;
    Ok(rel)
}

/// Put `rel` at `index` among the items of its folder, remembering the order.
fn place(root: &Path, rel: &str, index: Option<usize>) -> Result<()> {
    let Some(index) = index else {
        // the end of an unordered folder is where sorting puts it anyway
        if !read_meta(root).order.contains_key(parent_rel(rel)) {
            return Ok(());
        }
        return place(root, rel, Some(usize::MAX));
    };
    let dir = parent_rel(rel);
    let name = file_name(rel).to_string();
    let mut names = ordered_names(root, dir)?;
    names.retain(|n| *n != name);
    names.insert(index.min(names.len()), name);
    let mut meta = read_meta(root);
    meta.order.insert(dir.to_string(), names);
    write_meta(root, &meta)
}

/// Rename a section or a note (its title; a note keeps its format). Returns the new path.
pub fn rename(root: &Path, rel: &str, name: &str) -> Result<String> {
    let name = clean_name(name)?;
    let from = inside(root, rel)?;
    if !from.exists() || rel.is_empty() {
        return Err(err(format!("there is no {rel} in the notebook")));
    }
    let ext = is_note(&from).then(|| from.extension().unwrap_or_default().to_string_lossy().into_owned());
    let file = match &ext {
        Some(e) => format!("{name}.{e}"),
        None => name.clone(),
    };
    let to = from.with_file_name(&file);
    let same = file == file_name(rel);
    // a change of case only ("beam" → "Beam") is a rename on systems that ignore case
    let case_only = !same && to.exists() && file.eq_ignore_ascii_case(file_name(rel));
    if same {
        return Ok(rel.to_string());
    }
    let clash = if ext.is_some() {
        NOTE_EXTS.iter().any(|e| from.with_file_name(format!("{name}.{e}")).exists())
    } else {
        to.exists()
    };
    if clash && !case_only {
        return Err(err(format!("there is already something called {name} here")));
    }
    if case_only {
        let tmp = from.with_file_name(format!(".{file}.renaming"));
        std::fs::rename(&from, &tmp)?;
        std::fs::rename(&tmp, &to)?;
    } else {
        std::fs::rename(&from, &to)?;
    }
    let new_rel = join_rel(parent_rel(rel), &file);
    let mut meta = read_meta(root);
    if let Some(list) = meta.order.get_mut(parent_rel(rel)) {
        for n in list.iter_mut().filter(|n| **n == file_name(rel)) {
            *n = file.clone();
        }
    }
    rekey(&mut meta, rel, &new_rel);
    write_meta(root, &meta)?;
    Ok(new_rel)
}

/// Move a section or note into section `to` (the root is `""`), at `index` there
/// (the end by default). Also reorders within one section. Returns the new path.
pub fn move_item(root: &Path, rel: &str, to: &str, index: Option<usize>) -> Result<String> {
    let from = inside(root, rel)?;
    let dest = inside(root, to)?;
    if rel.is_empty() || !from.exists() {
        return Err(err(format!("there is no {rel} in the notebook")));
    }
    if !dest.is_dir() || is_note(&dest) {
        return Err(err(format!("there is no section {to}")));
    }
    if to == rel || to.starts_with(&format!("{rel}/")) {
        return Err(err("a section can't go inside itself"));
    }
    if parent_rel(rel) == to {
        place(root, rel, index)?;
        return Ok(rel.to_string());
    }
    let name = file_name(rel);
    let target = dest.join(name);
    if target.exists() {
        return Err(err(format!(
            "there is already something called {name} in {}",
            if to.is_empty() { "the notebook" } else { to }
        )));
    }
    std::fs::rename(&from, &target)?;
    let new_rel = join_rel(to, name);
    let mut meta = read_meta(root);
    if let Some(list) = meta.order.get_mut(parent_rel(rel)) {
        list.retain(|n| n != name);
    }
    rekey(&mut meta, rel, &new_rel);
    write_meta(root, &meta)?;
    place(root, &new_rel, index)?;
    Ok(new_rel)
}

/// Copy a note beside itself ("Kinematics copy"). Returns the copy.
pub fn duplicate(root: &Path, rel: &str) -> Result<String> {
    let from = inside(root, rel)?;
    if !is_note(&from) {
        return Err(err("only notes can be duplicated"));
    }
    let ext = from.extension().unwrap_or_default().to_string_lossy().into_owned();
    let stem = from.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let to = unique(from.parent().unwrap_or(root), &format!("{stem} copy"), Some(&ext));
    let mut doc = crate::open(&from)?;
    set_title(&mut doc, to.file_stem().unwrap_or_default().to_string_lossy().into_owned())?;
    crate::save(&to, &doc, Some(&from))?;
    let new_rel = rel_of(root, &to);
    let at = ordered_names(root, parent_rel(rel))?.iter().position(|n| n == file_name(rel)).map(|i| i + 1);
    place(root, &new_rel, at)?;
    Ok(new_rel)
}

#[cfg(not(target_arch = "wasm32"))]
fn to_trash(path: &Path) -> std::result::Result<(), trash::Error> {
    trash::delete(path)
}

#[cfg(target_arch = "wasm32")]
fn to_trash(_: &Path) -> std::result::Result<(), &'static str> {
    Err("there is no Trash in a browser")
}

/// Move a section or note to the system's Trash, where it can be put back.
pub fn trash(root: &Path, rel: &str) -> Result<()> {
    let path = inside(root, rel)?;
    if rel.is_empty() || !path.exists() {
        return Err(err(format!("there is no {rel} in the notebook")));
    }
    to_trash(&path).map_err(|e| err(format!("could not move {} to the Trash: {e}", file_name(rel))))?;
    let mut meta = read_meta(root);
    forget(&mut meta, rel);
    write_meta(root, &meta)
}

/// Give a section or note a colour (`#3b82f6`) and/or an icon (an emoji); `None` clears.
pub fn set_style(root: &Path, rel: &str, style: Style) -> Result<()> {
    if !inside(root, rel)?.exists() {
        return Err(err(format!("there is no {rel} in the notebook")));
    }
    if let Some(c) = &style.color {
        let hex = c.strip_prefix('#').unwrap_or("");
        if !(matches!(hex.len(), 3 | 6) && hex.chars().all(|c| c.is_ascii_hexdigit())) {
            return Err(err(format!("{c} is not a colour")));
        }
    }
    if style.icon.as_ref().is_some_and(|i| i.chars().count() > 8) {
        return Err(err("an icon is a single emoji"));
    }
    let mut meta = read_meta(root);
    if style == Style::default() {
        meta.style.remove(rel);
    } else {
        meta.style.insert(rel.to_string(), style);
    }
    write_meta(root, &meta)
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/// A note that matches a search.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub rel: String,
    pub title: String,
    /// The section it is in, for showing beside the title ("Mechanics › Forces").
    pub section: String,
    /// A line around the first match in the note's content; empty when only the title matched.
    pub snippet: String,
    /// Where the match starts and ends in `snippet`, in characters.
    pub mark: Option<(usize, usize)>,
    pub title_match: bool,
    /// The region the snippet comes from, to go to it.
    pub region: Option<String>,
}

/// Notes whose title or content contains every word of `query`, ignoring case and accents
/// (`calculo` finds *Cálculo*). Title matches first, then the most recently changed.
pub fn search(root: &Path, query: &str, limit: usize) -> Result<Vec<Hit>> {
    let words: Vec<String> = folded(query).split_whitespace().map(str::to_string).collect();
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let book = list(root)?;
    let mut notes = Vec::new();
    fn walk<'a>(nodes: &'a [Node], out: &mut Vec<&'a Node>) {
        for n in nodes {
            match n.kind {
                Kind::Note => out.push(n),
                Kind::Section => walk(&n.children, out),
            }
        }
    }
    walk(&book.children, &mut notes);
    let mut hits: Vec<(Hit, u64)> = Vec::new();
    for note in notes {
        let title_f = folded(&note.name);
        let lines = crate::open(&inside(root, &note.rel)?).map(|d| text_lines(&d)).unwrap_or_default();
        let all = std::iter::once(title_f.clone())
            .chain(lines.iter().map(|(_, l)| folded(l)))
            .collect::<Vec<_>>()
            .join("\n");
        if !words.iter().all(|w| all.contains(w)) {
            continue;
        }
        let title_match = words.iter().all(|w| title_f.contains(w));
        // the first line holding any of the words, cut to a readable window around it
        let (mut snippet, mut mark, mut region) = (String::new(), None, None);
        for (id, line) in &lines {
            let f = folded(line);
            if let Some((at, w)) = words.iter().filter_map(|w| f.find(w.as_str()).map(|i| (i, w))).min() {
                let start_c = f.get(..at).map_or(0, |before| before.chars().count());
                let len_c = w.chars().count();
                let chars: Vec<char> = line.chars().collect();
                let from = start_c.saturating_sub(40);
                let to = (start_c + len_c + 80).min(chars.len());
                let mut s: String = chars.get(from..to).unwrap_or_default().iter().collect();
                let mut offset = start_c - from;
                if from > 0 {
                    s.insert(0, '…');
                    offset += 1;
                }
                if to < chars.len() {
                    s.push('…');
                }
                snippet = s;
                mark = Some((offset, offset + len_c));
                region = Some(id.clone());
                break;
            }
        }
        let section =
            parent_rel(&note.rel).split('/').filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" › ");
        hits.push((
            Hit {
                rel: note.rel.clone(),
                title: note.name.clone(),
                section,
                snippet,
                mark,
                title_match,
                region,
            },
            note.modified.unwrap_or(0),
        ));
    }
    hits.sort_by(|(a, ta), (b, tb)| b.title_match.cmp(&a.title_match).then(tb.cmp(ta)));
    Ok(hits.into_iter().take(limit).map(|(h, _)| h).collect())
}

/// A document's searchable text, line by line, with the region each line comes from.
fn text_lines(doc: &Json) -> Vec<(String, String)> {
    let mut out = Vec::new();
    fn text_of(v: &Json) -> String {
        match v {
            Json::String(s) => s.clone(),
            Json::Object(m) => m.values().filter_map(Json::as_str).collect::<Vec<_>>().join("\n"),
            _ => String::new(),
        }
    }
    fn walk(regions: &[Json], out: &mut Vec<(String, String)>) {
        for r in regions {
            let id = r["id"].as_str().unwrap_or_default().to_string();
            let mut push = |s: &str| {
                for line in s.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    out.push((id.clone(), line.to_string()));
                }
            };
            if let Some(e) = r["expr"].as_str() {
                push(&e.replace('\'', ""));
            }
            push(&plain_djot(&text_of(&r["text"])));
            push(&text_of(&r["title"]));
            push(&text_of(&r["caption"]["text"]));
            if let Some(note) = r["note"].as_str() {
                push(note);
            }
            if let Some(rows) = r["rows"].as_array() {
                for row in rows.iter().filter_map(Json::as_array) {
                    push(&row.iter().filter_map(Json::as_str).collect::<Vec<_>>().join("  "));
                }
            }
            if let Some(children) = r["regions"].as_array() {
                walk(children, out);
            }
        }
    }
    if let Some(regions) = doc["regions"].as_array() {
        walk(regions, &mut out);
    }
    out
}

/// Djot without its markup, near enough for searching: `*bold* [x]{.u}` → `bold x`.
fn plain_djot(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        // attribute lines and heading/list markers
        if line.starts_with('{') && line.ends_with('}') {
            continue;
        }
        let line = line.trim_start_matches(['#', '-', ' ']);
        let mut depth = 0;
        let mut prev = ' ';
        for c in line.chars() {
            match c {
                '{' => depth += 1,
                '}' if depth > 0 => depth -= 1,
                _ if depth > 0 => {}
                '\\' if prev != '\\' => {}
                '*' | '_' | '[' | ']' | '`' | '^' | '~' | '$' if prev != '\\' => {}
                _ => out.push(c),
            }
            prev = c;
        }
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// One entry point for the app and the dev server
// ---------------------------------------------------------------------------

/// The folder operation `op` works in: the notebook's root, or the folder `create_notebook`
/// makes. Every other path an operation takes is relative and stays inside it (`inside`), so the
/// desktop app checks only this one against the folders the user chose.
pub fn folder(op: &str, args: &Json) -> Result<PathBuf> {
    let key = if op == "create_notebook" { "path" } else { "root" };
    args.get(key).and_then(Json::as_str).map(PathBuf::from).ok_or_else(|| err(format!("missing \"{key}\"")))
}

/// Run notebook operation `op` with JSON arguments, as the desktop app and `quire serve`
/// receive them: `{"root": "/…/Physics 101", "rel": "Mechanics/Kinematics.quire", …}`.
pub fn call(op: &str, args: &Json) -> Result<Json> {
    let s = |k: &str| args.get(k).and_then(Json::as_str).ok_or_else(|| err(format!("missing \"{k}\"")));
    let opt_s = |k: &str| args.get(k).and_then(Json::as_str).map(str::to_string);
    let index = args.get("index").and_then(Json::as_u64).map(|i| i as usize);
    let root = || folder(op, args);
    let text = |v: String| Ok(Json::String(v));
    match op {
        "list" => Ok(serde_json::to_value(list(&root()?)?)?),
        "create_notebook" => text(create_notebook(&root()?, s("ext")?)?),
        "create_note" => text(create_note(
            &root()?,
            s("dir")?,
            &opt_s("title").unwrap_or_else(|| "Untitled".into()),
            s("ext")?,
            index,
        )?),
        "create_section" => text(create_section(
            &root()?,
            s("dir")?,
            &opt_s("name").unwrap_or_else(|| "New section".into()),
            index,
        )?),
        "rename" => text(rename(&root()?, s("rel")?, s("name")?)?),
        "move" => text(move_item(&root()?, s("rel")?, s("to")?, index)?),
        "duplicate" => text(duplicate(&root()?, s("rel")?)?),
        "trash" => trash(&root()?, s("rel")?).map(|_| Json::Null),
        "set_style" => set_style(&root()?, s("rel")?, Style { color: opt_s("color"), icon: opt_s("icon") })
            .map(|_| Json::Null),
        "search" => {
            let limit = args.get("limit").and_then(Json::as_u64).unwrap_or(50) as usize;
            Ok(serde_json::to_value(search(&root()?, s("query")?, limit)?)?)
        }
        _ => Err(err(format!("unknown notebook operation {op}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    /// A scratch notebook that removes itself.
    struct Book(PathBuf);
    impl Book {
        fn new(tag: &str) -> std::result::Result<Book, Box<dyn std::error::Error>> {
            let p = std::env::temp_dir().join(format!("quire-nb-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p)?;
            Ok(Book(p))
        }
        fn names(&self, nodes: &[Node]) -> Vec<String> {
            nodes.iter().map(|n| n.name.clone()).collect()
        }
        fn top(&self) -> std::result::Result<Vec<String>, Box<dyn std::error::Error>> {
            Ok(self.names(&list(&self.0)?.children))
        }
    }
    impl Drop for Book {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn lists_sections_first_then_notes_in_natural_order() -> TestResult {
        let b = Book::new("list")?;
        for n in ["Note 10", "note 2", "Ábaco"] {
            create_note(&b.0, "", n, "quire", None)?;
        }
        create_section(&b.0, "", "Mechanics", None)?;
        std::fs::write(b.0.join("readme.txt"), "not a note")?;
        std::fs::create_dir(b.0.join(".git"))?;
        assert_eq!(b.top()?, ["Mechanics", "Ábaco", "note 2", "Note 10"]);
        let book = list(&b.0)?;
        let abaco = book.children.get(1).ok_or("missing the note Ábaco")?;
        assert_eq!(abaco.rel, "Ábaco.quire");
        assert_eq!(abaco.kind, Kind::Note);
        assert!(abaco.modified.is_some());
        // a fresh notebook needs no settings file
        assert!(!b.0.join(META_FILE).exists());
        Ok(())
    }

    #[test]
    fn a_new_note_is_a_document_titled_after_its_file() -> TestResult {
        let b = Book::new("new")?;
        let a = create_note(&b.0, "", "Untitled", "quire", None)?;
        let c = create_note(&b.0, "", "Untitled", "quirez", None)?;
        assert_eq!(a, "Untitled.quire");
        assert_eq!(c, "Untitled 2.quirez", "the other format of a title counts as taken");
        let doc = crate::open(&b.0.join(&c))?;
        assert_eq!(doc["meta"]["title"], "Untitled 2");
        assert!(create_note(&b.0, "", "a/b", "quire", None).is_err());
        assert!(create_note(&b.0, "", "  ", "quire", None).is_err());
        assert!(create_note(&b.0, "nowhere", "x", "quire", None).is_err());
        Ok(())
    }

    #[test]
    fn notebooks_start_with_a_note_and_refuse_full_folders() -> TestResult {
        let b = Book::new("create")?;
        let path = b.0.join("Physics 101");
        assert_eq!(create_notebook(&path, "quire")?, "Untitled.quire");
        assert!(create_notebook(&path, "quire").is_err());
        assert!(list(&path.join("Untitled.quire")).is_err(), "a document is not a notebook");
        Ok(())
    }

    #[test]
    fn keeps_a_manual_order_and_new_items_go_where_asked() -> TestResult {
        let b = Book::new("order")?;
        for n in ["A", "B", "C"] {
            create_note(&b.0, "", n, "quire", None)?;
        }
        move_item(&b.0, "C.quire", "", Some(0))?;
        assert_eq!(b.top()?, ["C", "A", "B"]);
        create_note(&b.0, "", "D", "quire", Some(1))?;
        assert_eq!(b.top()?, ["C", "D", "A", "B"]);
        // an ordered folder adds new items at the end, not by name
        create_note(&b.0, "", "0 first by name", "quire", None)?;
        assert_eq!(b.top()?.last().ok_or("empty listing")?, "0 first by name");
        // a file added outside Quire shows after the ordered ones
        crate::save(&b.0.join("Z.quire"), &crate::new_document(), None)?;
        assert_eq!(b.top()?.last().ok_or("empty listing")?, "Z");
        Ok(())
    }

    #[test]
    fn rename_keeps_the_format_order_and_style() -> TestResult {
        let b = Book::new("rename")?;
        create_section(&b.0, "", "Mech", None)?;
        let k = create_note(&b.0, "Mech", "Kin", "quirez", None)?;
        create_note(&b.0, "Mech", "Dyn", "quire", Some(0))?;
        set_style(&b.0, "Mech", Style { color: Some("#3b82f6".into()), icon: Some("⚙️".into()) })?;
        let k2 = rename(&b.0, &k, "Kinematics")?;
        assert_eq!(k2, "Mech/Kinematics.quirez");
        let m = rename(&b.0, "Mech", "Mechanics")?;
        assert_eq!(m, "Mechanics");
        let book = list(&b.0)?;
        let mech = book.children.first().ok_or("no sections")?;
        assert_eq!(mech.color.as_deref(), Some("#3b82f6"));
        assert_eq!(b.names(&mech.children), ["Dyn", "Kinematics"], "order follows the rename");
        assert!(rename(&b.0, "Mechanics/Dyn.quire", "Kinematics").is_err(), "taken");
        assert_eq!(rename(&b.0, "Mechanics/Dyn.quire", "dyn")?, "Mechanics/dyn.quire", "case only");
        assert!(rename(&b.0, "../x", "y").is_err());
        Ok(())
    }

    #[test]
    fn moves_between_sections_but_not_into_themselves() -> TestResult {
        let b = Book::new("move")?;
        create_section(&b.0, "", "A", None)?;
        create_section(&b.0, "A", "Inner", None)?;
        create_section(&b.0, "", "B", None)?;
        let n = create_note(&b.0, "A", "Note", "quire", None)?;
        set_style(&b.0, "A/Inner", Style { color: Some("#f00".into()), icon: None })?;
        assert_eq!(move_item(&b.0, &n, "B", None)?, "B/Note.quire");
        assert_eq!(move_item(&b.0, "A/Inner", "B", Some(0))?, "B/Inner");
        let book = list(&b.0)?;
        let bsec = book.children.iter().find(|s| s.name == "B").ok_or("no section B")?;
        assert_eq!(b.names(&bsec.children), ["Inner", "Note"]);
        assert_eq!(
            bsec.children.first().and_then(|n| n.color.as_deref()),
            Some("#f00"),
            "style follows the move"
        );
        assert!(move_item(&b.0, "B", "B/Inner", None).is_err());
        create_note(&b.0, "", "Note", "quire", None)?;
        assert!(move_item(&b.0, "Note.quire", "B", None).is_err(), "name taken there");
        Ok(())
    }

    #[test]
    fn duplicates_beside_the_original() -> TestResult {
        let b = Book::new("dup")?;
        for n in ["A", "B"] {
            create_note(&b.0, "", n, "quire", None)?;
        }
        move_item(&b.0, "B.quire", "", Some(0))?;
        let c = duplicate(&b.0, "B.quire")?;
        assert_eq!(c, "B copy.quire");
        assert_eq!(b.top()?, ["B", "B copy", "A"]);
        assert_eq!(crate::open(&b.0.join(c))?["meta"]["title"], "B copy");
        Ok(())
    }

    #[test]
    fn styles_are_checked() -> TestResult {
        let b = Book::new("style")?;
        create_note(&b.0, "", "A", "quire", None)?;
        assert!(set_style(&b.0, "A.quire", Style { color: Some("red".into()), icon: None }).is_err());
        set_style(&b.0, "A.quire", Style { color: None, icon: Some("📐".into()) })?;
        assert_eq!(list(&b.0)?.children.first().and_then(|n| n.icon.as_deref()), Some("📐"));
        set_style(&b.0, "A.quire", Style::default())?;
        assert!(!b.0.join(META_FILE).exists(), "nothing left to keep");
        Ok(())
    }

    #[test]
    fn searches_titles_and_content_ignoring_case_and_accents() -> TestResult {
        let b = Book::new("search")?;
        create_section(&b.0, "", "Mecânica", None)?;
        let n = create_note(&b.0, "Mecânica", "Cálculo da viga", "quire", None)?;
        let path = b.0.join(&n);
        let mut doc = crate::open(&path)?;
        doc["regions"] = serde_json::json!([
            { "id": "t1", "kind": "text", "at": [0, 0], "text": "The *bending moment* of a simply supported beam." },
            { "id": "m1", "kind": "math", "at": [0, 40], "expr": "M := q*L^2/8" },
        ]);
        crate::save(&path, &doc, None)?;
        create_note(&b.0, "", "Beam notes", "quire", None)?;

        let hits = search(&b.0, "calculo", 10)?;
        assert_eq!(hits.len(), 1);
        assert!(hits.first().ok_or("no hits")?.title_match);
        assert_eq!(hits.first().ok_or("no hits")?.section, "Mecânica");

        let hits = search(&b.0, "BENDING moment", 10)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits.first().ok_or("no hits")?.snippet, "The bending moment of a simply supported beam.");
        assert_eq!(hits.first().ok_or("no hits")?.mark, Some((4, 11)));
        assert_eq!(hits.first().ok_or("no hits")?.region.as_deref(), Some("t1"));

        // a title match comes before a content match
        let hits = search(&b.0, "beam", 10)?;
        assert_eq!(
            hits.iter().map(|h| h.title.as_str()).collect::<Vec<_>>(),
            ["Beam notes", "Cálculo da viga"]
        );
        assert!(search(&b.0, "q*L^2", 10)?.first().and_then(|h| h.region.as_deref()) == Some("m1"));
        assert!(search(&b.0, "nothing like this", 10)?.is_empty());
        assert!(search(&b.0, "   ", 10)?.is_empty());
        Ok(())
    }

    #[test]
    fn plain_djot_drops_markup() -> TestResult {
        assert_eq!(plain_djot("{.center}\n# *Title* [u]{.underline} \\* x"), "Title u * x\n");
        Ok(())
    }

    #[test]
    fn calls_by_name_as_the_app_does() -> TestResult {
        let b = Book::new("call")?;
        let root = b.0.to_string_lossy().into_owned();
        let sec = call("create_section", &serde_json::json!({ "root": root, "dir": "", "name": "Labs" }))?;
        assert_eq!(sec, "Labs");
        let note = call("create_note", &serde_json::json!({ "root": root, "dir": "Labs", "ext": "quire" }))?;
        assert_eq!(note, "Labs/Untitled.quire");
        let book = call("list", &serde_json::json!({ "root": root }))?;
        assert_eq!(book["children"][0]["children"][0]["name"], "Untitled");
        assert_eq!(book["children"][0]["kind"], "section");
        assert!(call("rename", &serde_json::json!({ "root": root, "rel": "Labs" })).is_err(), "missing name");
        assert!(call("explode", &serde_json::json!({})).is_err());
        Ok(())
    }

    #[test]
    fn an_operation_changes_only_the_folder_it_names() -> TestResult {
        let b = Book::new("folder")?;
        let (made, elsewhere) = (b.0.join("Made"), b.0.join("Elsewhere"));
        std::fs::create_dir(&elsewhere)?;
        // a root beside the path must not decide where a notebook is made
        let args = serde_json::json!({ "path": made, "root": elsewhere, "ext": "quire" });
        assert_eq!(folder("create_notebook", &args)?, made);
        call("create_notebook", &args)?;
        assert!(made.join("Untitled.quire").exists());
        assert_eq!(std::fs::read_dir(&elsewhere)?.count(), 0);
        for op in ["list", "create_note", "create_section", "rename", "move", "duplicate", "trash", "search"]
        {
            assert_eq!(folder(op, &args)?, elsewhere, "{op} works in its root");
        }
        assert!(folder("list", &serde_json::json!({ "path": made })).is_err(), "no root, no folder");
        // relative paths stay inside the root, so the root is all there is to check
        let made_s = made.to_string_lossy();
        for (op, extra) in [
            ("create_note", serde_json::json!({ "dir": "..", "ext": "quire" })),
            ("create_section", serde_json::json!({ "dir": "../Elsewhere", "name": "x" })),
            ("rename", serde_json::json!({ "rel": "../Elsewhere", "name": "x" })),
            ("move", serde_json::json!({ "rel": "Untitled.quire", "to": ".." })),
            ("trash", serde_json::json!({ "rel": "../Elsewhere" })),
        ] {
            let mut args = extra;
            args["root"] = made_s.as_ref().into();
            assert!(call(op, &args).is_err(), "{op} must not leave the notebook");
        }
        assert!(elsewhere.is_dir() && made.join("Untitled.quire").exists());
        Ok(())
    }

    #[test]
    fn natural_order() -> TestResult {
        let mut v = vec!["b10", "B2", "a", "Á", "b1"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a", "Á", "b1", "B2", "b10"]);
        Ok(())
    }
}
