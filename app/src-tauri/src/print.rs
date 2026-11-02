//! Printing and PDF export through WebKit's own print engine.
//!
//! The editor's page view already lays the document out as paper-sized sheets, so the
//! print operation uses the exact paper size with zero margins and no scaling: what is
//! on screen is what lands in the PDF. macOS (AppKit) and Linux (GTK's "Print to File"
//! backend) write the PDF directly, with no dialog; Windows still goes through Print.

use std::collections::HashMap;
#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

use serde::Deserialize;
use tauri::WebviewWindow;

/// How long a PDF export may take before it is reported as failed.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const EXPORT_TIMEOUT: Duration = Duration::from_secs(120);

// The front end sends these on every system, but only macOS and Linux write the PDF and read
// them; Windows prints through the system dialog.

/// Paper size in PostScript points (1/72 in), already rotated for landscape.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Paper {
    pub width_pt: f64,
    pub height_pt: f64,
}

/// A place on a page: page index from 0, and points from the top of the paper.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Spot {
    pub page: usize,
    pub top: f64,
}

/// A bookmark for a heading; `title` already carries its number ("2.1 Bending").
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Clone, Deserialize)]
pub struct Heading {
    pub title: String,
    pub level: u8,
    #[serde(flatten)]
    pub at: Spot,
}

/// A clickable box the print engine does not make itself (a contents-list row), in points
/// from the paper's top-left corner, that jumps to the region `to`.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Clone, Deserialize)]
pub struct LinkBox {
    pub page: usize,
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    pub to: String,
}

/// What the page view knows and the print engine does not: the document's details, its
/// headings and where every cross-reference target landed.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PdfInfo {
    pub title: String,
    pub author: String,
    pub subject: String,
    pub keywords: String,
    pub company: String,
    pub lang: String,
    /// The number printed on the first page.
    pub first_page: Option<i64>,
    pub outline: Vec<Heading>,
    /// Region id → where it starts.
    pub targets: HashMap<String, Spot>,
    pub links: Vec<LinkBox>,
}

#[cfg(target_os = "macos")]
unsafe fn start(
    webview: *mut std::ffi::c_void,
    window: *mut std::ffi::c_void,
    paper: Paper,
    save_to: Option<&Path>,
    title: &str,
) -> Result<(), String> {
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{
        NSPrintInfo, NSPrintJobDisposition, NSPrintJobSavingURL, NSPrintSaveJob, NSPrintingPaginationMode,
        NSWindow,
    };
    use objc2_foundation::{NSCopying, NSSize, NSString, NSURL};
    use objc2_web_kit::WKWebView;

    // SAFETY: Tauri hands us the live WKWebView and its NSWindow on the main thread.
    let (webview, window) = unsafe { (&*(webview as *const WKWebView), &*(window as *const NSWindow)) };

    let info = NSPrintInfo::sharedPrintInfo().copy();
    info.setPaperSize(NSSize::new(paper.width_pt, paper.height_pt));
    info.setTopMargin(0.0);
    info.setBottomMargin(0.0);
    info.setLeftMargin(0.0);
    info.setRightMargin(0.0);
    info.setHorizontallyCentered(false);
    info.setVerticallyCentered(false);
    info.setHorizontalPagination(NSPrintingPaginationMode::Clip);
    info.setVerticalPagination(NSPrintingPaginationMode::Automatic);

    if let Some(path) = save_to {
        let path = path.to_str().ok_or("the PDF path is not valid UTF-8")?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        // SAFETY: the print-info dictionary takes NSString keys and object values.
        unsafe {
            let dict = info.dictionary();
            let disposition: &AnyObject = NSPrintSaveJob.as_ref();
            let url_obj: &AnyObject = url.as_ref();
            dict.insert(NSPrintJobDisposition, disposition);
            dict.insert(NSPrintJobSavingURL, url_obj);
        }
    }

    // SAFETY: printOperationWithPrintInfo: exists on macOS 11+, the app's minimum is 12.
    let op = unsafe { webview.printOperationWithPrintInfo(&info) };
    op.setShowsPrintPanel(save_to.is_none());
    op.setShowsProgressPanel(save_to.is_none());
    op.setJobTitle(Some(&NSString::from_str(title)));
    // WebKit prints blank pages unless the print view has a real frame.
    if let Some(view) = op.view() {
        view.setFrame(webview.frame());
    }
    // SAFETY: no delegate and no context pointer.
    unsafe {
        op.runOperationModalForWindow_delegate_didRunSelector_contextInfo(
            window,
            None,
            None,
            std::ptr::null_mut(),
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn run(window: &WebviewWindow, paper: Paper, save_to: Option<PathBuf>, title: String) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    window
        .with_webview(move |wv| {
            // SAFETY: with_webview runs on the main thread with valid native handles.
            let r = unsafe { start(wv.inner(), wv.ns_window(), paper, save_to.as_deref(), &title) };
            let _ = tx.send(r);
        })
        .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?
}

/// The macOS print job finishes asynchronously; wait until the file exists and stops growing.
#[cfg(target_os = "macos")]
fn wait_for_file(path: &Path, timeout: Duration) -> Result<(), String> {
    let start = Instant::now();
    let (mut last, mut stable_since) = (0u64, Instant::now());
    while start.elapsed() < timeout {
        std::thread::sleep(Duration::from_millis(100));
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if size == 0 || size != last {
            last = size;
            stable_since = Instant::now();
        } else if stable_since.elapsed() > Duration::from_millis(500) {
            return Ok(());
        }
    }
    Err(format!("timed out writing {}", path.display()))
}

/// Linux: WebKitGTK prints through GTK, and GTK's "Print to File" backend writes the file
/// named by `output-uri` in the format `output-file-format`, with no dialog.
#[cfg(target_os = "linux")]
mod gtk_pdf {
    use std::cell::RefCell;
    use std::ffi::{CStr, CString, c_char};
    use std::path::Path;
    use std::rc::Rc;
    use std::sync::mpsc::Sender;

    use gtk::glib;
    use gtk::{PageOrientation, PageSetup, PaperSize, PrintSettings, Unit};
    use webkit2gtk::{PrintOperation, PrintOperationExt, WebView};

    use super::Paper;

    /// GTK's file printer is named "Print to File", translated into the user's language.
    const PRINT_TO_FILE: &str = "Print to File";

    unsafe extern "C" {
        fn dgettext(domain: *const c_char, msgid: *const c_char) -> *const c_char;
    }

    /// The file printer's name in the user's language, as GTK itself looks it up.
    fn file_printer_name() -> String {
        let (Ok(domain), Ok(msgid)) = (CString::new("gtk30"), CString::new(PRINT_TO_FILE)) else {
            return PRINT_TO_FILE.to_owned();
        };
        // SAFETY: both arguments are valid NUL-terminated strings that outlive the call; gettext
        // keeps the returned string until the locale changes, and it is copied out right below.
        let text = unsafe { dgettext(domain.as_ptr(), msgid.as_ptr()) };
        if text.is_null() {
            return PRINT_TO_FILE.to_owned();
        }
        // SAFETY: a non-null result of dgettext is a valid NUL-terminated string (see above).
        match unsafe { CStr::from_ptr(text) }.to_str() {
            Ok(name) => name.to_owned(),
            Err(_) => PRINT_TO_FILE.to_owned(),
        }
    }

    /// Start writing `path` as a PDF of the page view. `done` gets the outcome once the job
    /// has finished or failed; it runs on the GTK main thread, so it must not block.
    pub fn start(
        view: &WebView,
        paper: Paper,
        path: &Path,
        done: Sender<Result<(), String>>,
    ) -> Result<(), String> {
        let uri =
            glib::filename_to_uri(path, None).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        // paper and page setup must agree: GTK would otherwise scale or re-orient the sheets
        let size =
            PaperSize::new_custom("quire-page", "Quire page", paper.width_pt, paper.height_pt, Unit::Points);
        let page = PageSetup::new();
        page.set_orientation(PageOrientation::Portrait);
        page.set_paper_size(&size);
        page.set_top_margin(0.0, Unit::Points);
        page.set_bottom_margin(0.0, Unit::Points);
        page.set_left_margin(0.0, Unit::Points);
        page.set_right_margin(0.0, Unit::Points);

        let settings = PrintSettings::new();
        settings.set_printer(&file_printer_name());
        settings.set(gtk::PRINT_SETTINGS_OUTPUT_URI.as_str(), Some(uri.as_str()));
        settings.set(gtk::PRINT_SETTINGS_OUTPUT_FILE_FORMAT.as_str(), Some("pdf"));
        settings.set_orientation(PageOrientation::Portrait);
        settings.set_paper_size(&size);
        settings.set_n_copies(1);

        let op = PrintOperation::new(view);
        op.set_print_settings(&settings);
        op.set_page_setup(&page);

        // WebKit may still be using the operation after this function returns; keep it
        // until the job ends, then let go (the signal handlers below hold this slot).
        let slot = Rc::new(RefCell::new(Some(op.clone())));
        let (ok, failed) = (done.clone(), done);
        let kept = slot.clone();
        op.connect_finished(move |_| {
            kept.borrow_mut().take();
            let _ = ok.send(Ok(()));
        });
        let kept = slot.clone();
        op.connect_failed(move |_, e| {
            kept.borrow_mut().take();
            let _ = failed.send(Err(format!("could not write the PDF: {}", e.message())));
        });
        op.print();
        Ok(())
    }
}

pub fn print(window: &WebviewWindow, paper: Paper, title: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return run(window, paper, None, title);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (paper, title);
        window.print().map_err(|e| e.to_string())
    }
}

/// Linux: start the GTK job on the main thread and wait for its outcome here, off the UI thread.
#[cfg(target_os = "linux")]
fn run_gtk(window: &WebviewWindow, paper: Paper, path: PathBuf) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let failed = tx.clone();
    window
        .with_webview(move |wv| {
            if let Err(e) = gtk_pdf::start(&wv.inner(), paper, &path, tx) {
                let _ = failed.send(Err(e));
            }
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(EXPORT_TIMEOUT).map_err(|_| "timed out writing the PDF".to_string())?
}

pub fn export_pdf(
    window: &WebviewWindow,
    path: PathBuf,
    paper: Paper,
    title: String,
    compress: bool,
    info: PdfInfo,
) -> Result<(), String> {
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("cannot replace {}: {e}", path.display()))?;
    }
    #[cfg(target_os = "macos")]
    {
        run(window, paper, Some(path.clone()), title)?;
        wait_for_file(&path, EXPORT_TIMEOUT)?;
    }
    #[cfg(target_os = "linux")]
    {
        let _ = title;
        run_gtk(window, paper, path.clone())?;
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        // bookmarks, links and smaller images are a bonus; the PDF is already complete without them
        if let Err(e) = crate::pdf::finish(&path, &info, compress) {
            eprintln!("quire: could not finish {}: {e}", path.display());
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (window, paper, title, compress, info);
        Err("direct PDF export is macOS and Linux only; use Print and choose a PDF printer".into())
    }
}
