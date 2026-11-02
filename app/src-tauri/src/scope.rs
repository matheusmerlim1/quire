//! The files and folders the app may touch: only those the user chose. A path enters the scope
//! from a native dialog, an OS open request (Finder, Explorer, the command line), a dropped file,
//! or the recent lists, which this module keeps rather than the webview. Every command that
//! reads or writes a path the webview passes checks it here first, so a document that managed to
//! run script in the webview still couldn't reach the rest of the disk.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Documents in File ▸ Open Recent (as many as the UI shows), and recent notebooks: twice the
/// UI's eight, so one the UI still lists is never forgotten here first.
const MAX_DOCUMENTS: usize = 10;
const MAX_NOTEBOOKS: usize = 16;

/// What outlives a session: the recent documents and notebooks, in order, newest first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Remembered {
    #[serde(default)]
    pub documents: Vec<PathBuf>,
    #[serde(default)]
    pub notebooks: Vec<PathBuf>,
}

#[derive(Debug, Default)]
pub struct Scope {
    /// Single files chosen this session: documents, PDF and CSV targets, images.
    files: HashSet<PathBuf>,
    /// Folders chosen this session, with everything in them: notebooks, the templates folder.
    folders: Vec<PathBuf>,
    remembered: Remembered,
    /// The recent lists of versions that kept them only in the webview may be adopted once,
    /// at startup, before any document is open (see `adopt`).
    adoptable: bool,
}

/// `path` as the scope compares it: absolute, with no `.` or `..` parts. `..` is refused rather
/// than resolved, because a folder link would make the lexical answer differ from the real one.
pub fn normal(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => return None,
            c => out.push(c.as_os_str()),
        }
    }
    Some(out)
}

fn ext_is(path: &Path, exts: &[&str]) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// A path with `ext` added unless it already ends in one of `allowed` (a save dialog may return
/// what was typed without the extension).
pub fn with_ext(path: PathBuf, allowed: &[&str], ext: &str) -> PathBuf {
    if ext_is(&path, allowed) {
        return path;
    }
    let mut s = path.into_os_string();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

/// `…/Beam.quire/document.json` (picked inside a bundle) means the bundle `…/Beam.quire`.
pub fn document_path(path: PathBuf) -> PathBuf {
    let in_bundle = path.file_name().is_some_and(|n| n.eq_ignore_ascii_case("document.json"));
    match path.parent() {
        Some(bundle) if in_bundle => bundle.to_path_buf(),
        _ => path,
    }
}

/// Newest first, without repeats, at most `max`.
fn push_front(list: &mut Vec<PathBuf>, path: PathBuf, max: usize) {
    list.retain(|p| *p != path);
    list.insert(0, path);
    list.truncate(max);
}

impl Scope {
    /// The scope at startup. `remembered` is `None` when this machine has no saved lists yet.
    #[cfg(test)]
    pub fn new(remembered: Option<Remembered>) -> Scope {
        let mut scope = Scope::default();
        scope.restore(remembered);
        scope
    }

    /// Take in the lists saved by the last session (`None`: this machine has none yet). What was
    /// granted before, from the command line, stays.
    pub fn restore(&mut self, remembered: Option<Remembered>) {
        self.adoptable = remembered.is_none();
        let mut remembered = remembered.unwrap_or_default();
        // a saved list is data from disk: keep only what could have been granted
        remembered.documents.retain(|p| normal(p).as_ref() == Some(p));
        remembered.notebooks.retain(|p| normal(p).as_ref() == Some(p));
        self.remembered = remembered;
    }

    /// A single file the user chose.
    pub fn grant_file(&mut self, path: &Path) {
        if let Some(p) = normal(path) {
            self.files.insert(p);
        }
    }

    /// A folder the user chose, with everything in it.
    pub fn grant_folder(&mut self, path: &Path) {
        if let Some(p) = normal(path)
            && !self.folders.contains(&p)
        {
            self.folders.push(p);
        }
    }

    /// A notebook the user chose: granted, and kept among the recent notebooks.
    pub fn grant_notebook(&mut self, path: &Path) {
        if let Some(p) = normal(path) {
            self.grant_folder(&p);
            push_front(&mut self.remembered.notebooks, p, MAX_NOTEBOOKS);
        }
    }

    pub fn allows(&self, path: &Path) -> bool {
        let Some(p) = normal(path) else { return false };
        self.files.contains(&p)
            || self.remembered.documents.contains(&p)
            || self.folders.iter().chain(&self.remembered.notebooks).any(|f| p.starts_with(f))
    }

    /// `Ok` when the app may touch `path`; otherwise an error that says why it won't.
    pub fn check(&self, path: &Path) -> Result<(), String> {
        if self.allows(path) {
            Ok(())
        } else {
            Err(format!(
                "Quire only opens and saves files you chose; choose {} with File ▸ Open or Save As",
                path.file_name()
                    .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
            ))
        }
    }

    /// Whether `path` is one of the notebooks the user chose, or inside one (notebook operations
    /// act on a whole folder, so a single granted file doesn't make one).
    pub fn check_notebook(&self, path: &Path) -> Result<(), String> {
        let inside = normal(path)
            .is_some_and(|p| self.folders.iter().chain(&self.remembered.notebooks).any(|f| p.starts_with(f)));
        if inside {
            Ok(())
        } else {
            Err(format!("Quire only opens notebooks you chose; choose {} with Open Notebook", path.display()))
        }
    }

    /// File ▸ Open Recent as the webview would have it. Only documents the user chose survive;
    /// they become the remembered list, which is returned for the menu.
    pub fn set_recent(&mut self, paths: &[String]) -> Vec<PathBuf> {
        let mut kept: Vec<PathBuf> = Vec::new();
        for p in paths.iter().map(PathBuf::from) {
            if let Some(p) = normal(&p).filter(|p| self.allows(p))
                && !kept.contains(&p)
            {
                kept.push(p);
            }
        }
        kept.truncate(MAX_DOCUMENTS);
        self.remembered.documents = kept.clone();
        kept
    }

    /// Take over the recent lists an earlier version kept in the webview. That is allowed once,
    /// only on a machine with no saved lists, and only before the first document is opened: until
    /// then the webview has run nothing but the app's own code. Returns whether it was adopted.
    pub fn adopt(&mut self, documents: &[String], notebooks: &[String]) -> bool {
        if !std::mem::take(&mut self.adoptable) {
            return false;
        }
        for p in documents.iter().filter_map(|p| normal(Path::new(p))).take(MAX_DOCUMENTS) {
            if !self.remembered.documents.contains(&p) {
                self.remembered.documents.push(p);
            }
        }
        for p in notebooks.iter().filter_map(|p| normal(Path::new(p))).take(MAX_NOTEBOOKS) {
            if !self.remembered.notebooks.contains(&p) {
                self.remembered.notebooks.push(p);
            }
        }
        true
    }

    /// From now on nothing is adopted: a document is about to be read.
    pub fn seal(&mut self) {
        self.adoptable = false;
    }

    pub fn remembered(&self) -> &Remembered {
        &self.remembered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absolute path on every system (`C:\x` on Windows, `/x` elsewhere).
    fn abs(p: &str) -> PathBuf {
        let root = if cfg!(windows) { "C:\\" } else { "/" };
        Path::new(root).join(p)
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn nothing_is_allowed_until_the_user_chooses_it() {
        let scope = Scope::new(Some(Remembered::default()));
        assert!(!scope.allows(&abs("Users/me/.ssh/id_ed25519")));
        let e = scope.check(&abs("Users/me/Beam.quire")).err().unwrap_or_default();
        assert!(e.contains("Beam.quire") && e.contains("chose"), "{e}");
    }

    #[test]
    fn a_chosen_file_is_allowed_but_not_its_neighbours() {
        let mut scope = Scope::new(None);
        scope.grant_file(&abs("docs/Beam.quirez"));
        assert!(scope.allows(&abs("docs/Beam.quirez")));
        assert!(scope.allows(&abs("docs/./Beam.quirez")), "the same file, spelled differently");
        assert!(!scope.allows(&abs("docs/Other.quirez")));
        assert!(!scope.allows(&abs("docs")));
        assert!(!scope.allows(&abs("docs/Beam.quirez/../../etc/passwd")));
    }

    #[test]
    fn a_chosen_folder_allows_what_is_inside_it() {
        let mut scope = Scope::new(None);
        scope.grant_folder(&abs("Physics 101"));
        assert!(scope.allows(&abs("Physics 101")));
        assert!(scope.allows(&abs("Physics 101/Mechanics/Kinematics.quire")));
        assert!(!scope.allows(&abs("Physics 101 (old)/x.quire")), "a name that only starts the same");
        assert!(!scope.allows(&abs("Physics 101/../secrets.txt")), "climbing out is refused");
        assert!(scope.check_notebook(&abs("Physics 101")).is_ok());
    }

    #[test]
    fn relative_paths_are_never_allowed() {
        let mut scope = Scope::new(None);
        scope.grant_file(Path::new("Beam.quire"));
        scope.grant_folder(Path::new(""));
        assert!(!scope.allows(Path::new("Beam.quire")));
        assert!(!scope.allows(Path::new("")));
        assert!(!scope.allows(Path::new("assets/x.png")));
    }

    #[test]
    fn a_single_file_is_not_a_notebook() {
        let mut scope = Scope::new(None);
        scope.grant_file(&abs("docs/Beam.quirez"));
        assert!(scope.check_notebook(&abs("docs/Beam.quirez")).is_err());
        assert!(scope.check_notebook(&abs("docs")).is_err());
    }

    #[test]
    fn the_recent_list_keeps_only_documents_the_user_chose() {
        let mut scope = Scope::new(Some(Remembered::default()));
        scope.grant_file(&abs("a.quire"));
        scope.grant_folder(&abs("Book"));
        let kept = scope.set_recent(&[s(&abs("a.quire")), s(&abs("planted.quire")), s(&abs("Book/n.quire"))]);
        assert_eq!(kept, [abs("a.quire"), abs("Book/n.quire")]);
        assert_eq!(scope.remembered().documents, kept);
    }

    #[test]
    fn remembered_documents_and_notebooks_are_allowed_in_the_next_session() {
        let mut first = Scope::new(Some(Remembered::default()));
        first.grant_file(&abs("a.quire"));
        first.set_recent(&[s(&abs("a.quire"))]);
        first.grant_notebook(&abs("Book"));
        let next = Scope::new(Some(first.remembered().clone()));
        assert!(next.allows(&abs("a.quire")));
        assert!(next.allows(&abs("Book/Note.quire")));
        assert!(next.check_notebook(&abs("Book")).is_ok());
        assert!(!next.allows(&abs("b.quire")));
    }

    #[test]
    fn a_document_dropped_from_the_recent_list_is_forgotten() {
        let mut first = Scope::new(Some(Remembered::default()));
        first.grant_file(&abs("a.quire"));
        first.set_recent(&[s(&abs("a.quire"))]);
        let mut next = Scope::new(Some(first.remembered().clone()));
        assert!(next.allows(&abs("a.quire")));
        next.set_recent(&[]);
        assert!(!next.allows(&abs("a.quire")), "Clear Menu forgets it");
    }

    #[test]
    fn saved_lists_with_relative_or_climbing_paths_are_ignored() {
        let scope = Scope::new(Some(Remembered {
            documents: vec![PathBuf::from("rel.quire"), abs("x/../y.quire")],
            notebooks: vec![PathBuf::from("Book")],
        }));
        assert!(scope.remembered().documents.is_empty());
        assert!(scope.remembered().notebooks.is_empty());
    }

    #[test]
    fn recent_notebooks_are_newest_first_without_repeats() {
        let mut scope = Scope::new(Some(Remembered::default()));
        for b in ["A", "B", "A"] {
            scope.grant_notebook(&abs(b));
        }
        assert_eq!(scope.remembered().notebooks, [abs("A"), abs("B")]);
        for i in 0..20 {
            scope.grant_notebook(&abs(&format!("N{i}")));
        }
        assert_eq!(scope.remembered().notebooks.len(), MAX_NOTEBOOKS);
    }

    #[test]
    fn old_recent_lists_are_adopted_once_on_a_machine_without_saved_lists() {
        let mut scope = Scope::new(None);
        assert!(scope.adopt(&[s(&abs("a.quire")), "relative.quire".into()], &[s(&abs("Book"))]));
        assert!(scope.allows(&abs("a.quire")));
        assert!(scope.allows(&abs("Book/n.quire")));
        assert!(!scope.allows(Path::new("relative.quire")));
        assert!(!scope.adopt(&[s(&abs("b.quire"))], &[]), "only once");
        assert!(!scope.allows(&abs("b.quire")));
    }

    #[test]
    fn nothing_is_adopted_once_lists_are_saved_or_a_document_was_read() {
        let mut saved = Scope::new(Some(Remembered::default()));
        assert!(!saved.adopt(&[s(&abs("a.quire"))], &[]));
        let mut sealed = Scope::new(None);
        sealed.seal();
        assert!(!sealed.adopt(&[s(&abs("a.quire"))], &[]));
        assert!(!sealed.allows(&abs("a.quire")));
    }

    #[test]
    fn save_dialogs_add_the_missing_extension() {
        let exts = ["quire", "quirez"];
        assert_eq!(with_ext(abs("Beam"), &exts, "quire"), abs("Beam.quire"));
        assert_eq!(with_ext(abs("Beam.QUIREZ"), &exts, "quire"), abs("Beam.QUIREZ"));
        assert_eq!(with_ext(abs("v1.2"), &["pdf"], "pdf"), abs("v1.2.pdf"));
    }

    #[test]
    fn a_document_json_picked_in_a_bundle_means_the_bundle() {
        assert_eq!(document_path(abs("Beam.quire/document.json")), abs("Beam.quire"));
        assert_eq!(document_path(abs("Beam.quirez")), abs("Beam.quirez"));
    }
}
