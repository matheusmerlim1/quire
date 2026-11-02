//! Quire's engine in the browser: the same calls the desktop app makes (`report`,
//! `djot_html`, …), compiled to WebAssembly so the web version needs no server.
//!
//! The interface is deliberately plain, with no binding generator: JavaScript writes a
//! command name and a JSON body into memory from [`alloc`], calls [`call`], and reads the
//! JSON answer at [`result_ptr`] (`result_len` bytes; [`result_ok`] says whether it is
//! an error message). `app/src/web.ts` is the other side.
//!
//! Files travel as base64: `open_bytes` takes a `.quirez` (or a `document.json`) the user
//! picked or a link pointed to, and `save_bytes` gives one back to download.

use std::cell::RefCell;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value as Json, json};

fn arg<'a>(body: &'a Json, key: &str) -> Result<&'a str, String> {
    body.get(key).and_then(Json::as_str).ok_or_else(|| format!("missing \"{key}\""))
}

fn err(e: quire_service::ServiceError) -> String {
    e.0
}

/// One editor call, as `quire serve` answers it, minus what needs a file system.
pub fn dispatch(command: &str, body: &Json) -> Result<Json, String> {
    let doc = || body.get("doc").ok_or("missing \"doc\"");
    match command {
        "report" => {
            serde_json::to_value(quire_service::report(doc()?).map_err(err)?).map_err(|e| e.to_string())
        }
        "new_document" => Ok(quire_service::new_document()),
        "library" => serde_json::to_value(quire_service::library()).map_err(|e| e.to_string()),
        "djot_html" => Ok(Json::String(quire_service::render_djot_editable(arg(body, "src")?))),
        "stage_image" => quire_service::assets::stage_base64(arg(body, "data")?, arg(body, "ext")?)
            .map(Json::String)
            .map_err(err),
        // images live in memory here: staged when inserted or when their document was opened
        "read_asset" => quire_service::read_asset(Path::new(""), arg(body, "src")?)
            .map(Json::String)
            .map_err(|_| "not included: links leave images out (open the .quirez file to see them)".into()),
        // only the built-in templates: there is nowhere to keep your own
        "list_templates" => {
            let builtin: Vec<_> =
                quire_service::templates::list(Path::new("")).into_iter().filter(|t| t.builtin).collect();
            serde_json::to_value(builtin).map_err(|e| e.to_string())
        }
        "open_template" => serde_json::to_value(
            quire_service::templates::document(Path::new(""), arg(body, "id")?).map_err(err)?,
        )
        .map_err(|e| e.to_string()),
        "open_bytes" => {
            let bytes =
                BASE64.decode(arg(body, "data")?).map_err(|e| format!("not a Quire document: {e}"))?;
            quire_service::open_bytes(&bytes).map_err(err)
        }
        "save_bytes" => {
            quire_service::save_bytes(doc()?).map(|b| Json::String(BASE64.encode(b))).map_err(err)
        }
        _ => Err(format!("{command} is not available in the browser")),
    }
}

thread_local! {
    static RESULT: RefCell<(bool, Vec<u8>)> = const { RefCell::new((true, Vec::new())) };
}

/// Room for `len` bytes that JavaScript fills; [`call`] frees it.
#[unsafe(no_mangle)]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len.max(1));
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Take back a buffer from [`alloc`] (with its length then) that wasn't passed to [`call`].
///
/// # Safety
/// `ptr` and `len` must come from one `alloc(len)` call, and the buffer not be used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dealloc(ptr: *mut u8, len: usize) {
    drop(unsafe { Vec::from_raw_parts(ptr, 0, len.max(1)) });
}

/// Run `command` with a JSON body; both are UTF-8 in buffers from [`alloc`], which are freed.
/// Returns the answer's length.
///
/// # Safety
/// Each pointer must come from `alloc` with the same length, filled with that many bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn call(cmd: *mut u8, cmd_len: usize, body: *mut u8, body_len: usize) -> usize {
    let take = |ptr, len: usize| unsafe { Vec::from_raw_parts(ptr, len, len.max(1)) };
    let (cmd, body) = (take(cmd, cmd_len), take(body, body_len));
    let answer = std::str::from_utf8(&cmd).map_err(|e| e.to_string()).and_then(|cmd| {
        let body: Json = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
        dispatch(cmd, &body)
    });
    let (ok, bytes) = match answer {
        Ok(v) => (true, v.to_string().into_bytes()),
        Err(message) => (false, json!(message).to_string().into_bytes()),
    };
    let len = bytes.len();
    RESULT.with(|r| *r.borrow_mut() = (ok, bytes));
    len
}

/// Where the last answer is (valid until the next [`call`]).
#[unsafe(no_mangle)]
pub extern "C" fn result_ptr() -> *const u8 {
    RESULT.with(|r| r.borrow().1.as_ptr())
}

/// 1 when the last answer is a value, 0 when it is an error message.
#[unsafe(no_mangle)]
pub extern "C" fn result_ok() -> u32 {
    RESULT.with(|r| r.borrow().0 as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_a_document_as_the_app_asks() -> Result<(), Box<dyn std::error::Error>> {
        let mut doc = dispatch("new_document", &json!({}))?;
        doc["regions"] = json!([
            { "id": "a", "kind": "math", "at": [0, 0], "expr": "q:=12 kN/m" },
            { "id": "b", "kind": "math", "at": [0, 30], "expr": "L:=6 m" },
            { "id": "c", "kind": "math", "at": [0, 60], "expr": "M:=q*L^2/8", "show": "value" },
        ]);
        let report = dispatch("report", &json!({ "doc": doc }))?;
        let shown = report.to_string();
        assert!(shown.contains("54") && shown.contains("kN"), "{shown}");
        Ok(())
    }

    /// What a shared link can hold at the limits: the report (parsing, evaluating, `→`, and
    /// the parsed trees sent back as JSON) fits WebAssembly's 1 MB stack, with room to spare in
    /// an optimised build (unoptimised builds need several times more, so they get 8 MB).
    #[test]
    fn a_document_at_the_engine_limits_reports_on_a_small_stack() {
        let size = if cfg!(debug_assertions) { 8 << 20 } else { 1 << 20 };
        let run = std::thread::Builder::new().stack_size(size).spawn(|| -> Result<String, String> {
            let n = 199;
            let parens = format!("{}x{}", "(".repeat(n), ")".repeat(n));
            let calls = format!("{}x{}", "abs(".repeat(n), ")".repeat(n));
            let mut doc = dispatch("new_document", &json!({}))?;
            doc["regions"] = json!([
                { "id": "a", "kind": "math", "at": [0, 0], "expr": "x:=2" },
                { "id": "b", "kind": "math", "at": [0, 30], "expr": parens, "show": "value" },
                { "id": "c", "kind": "math", "at": [0, 60], "expr": calls, "show": "symbolic" },
                { "id": "d", "kind": "math", "at": [0, 90], "expr": "g(n):=if(n<1, 0, 1 + g(n-1))" },
                { "id": "e", "kind": "math", "at": [0, 120], "expr": "g(250)", "show": "value" },
                { "id": "f", "kind": "math", "at": [0, 150], "expr": "(".repeat(5000), "show": "value" },
            ]);
            Ok(dispatch("report", &json!({ "doc": doc }))?.to_string())
        });
        let Ok(Ok(Ok(report))) = run.map(|h| h.join()) else { std::process::abort() };
        assert!(report.contains("\"resultText\":\"250\""), "{report:.300}");
        assert!(report.contains("too deeply nested"), "{report:.300}");
    }

    #[test]
    fn a_document_survives_the_trip_through_bytes() -> Result<(), Box<dyn std::error::Error>> {
        let doc = dispatch("open_template", &json!({ "id": "builtin:beam" }))?["doc"].clone();
        let data = dispatch("save_bytes", &json!({ "doc": doc }))?;
        let back = dispatch("open_bytes", &json!({ "data": data }))?;
        let count = |d: &Json| d.get("regions").and_then(Json::as_array).map(Vec::len);
        assert!(count(&doc).is_some_and(|n| n > 0));
        assert_eq!(count(&back), count(&doc));
        assert_eq!(back["regions"].as_array().map(Vec::len), doc["regions"].as_array().map(Vec::len));
        Ok(())
    }

    #[test]
    fn says_what_the_browser_cannot_do() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            dispatch("notebook", &json!({})).err().as_deref(),
            Some("notebook is not available in the browser")
        );
        let templates = dispatch("list_templates", &json!({}))?;
        assert!(templates.as_array().is_some_and(|all| all.iter().all(|t| t["builtin"] == true)));
        Ok(())
    }

    #[test]
    fn calls_through_memory_as_javascript_does() -> Result<(), Box<dyn std::error::Error>> {
        let put = |s: &str| {
            let p = alloc(s.len());
            unsafe { std::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len()) };
            (p, s.len())
        };
        let (c, cl) = put("djot_html");
        let (b, bl) = put(r#"{"src":"*bold*"}"#);
        let len = unsafe { call(c, cl, b, bl) };
        let answer = unsafe { std::slice::from_raw_parts(result_ptr(), len) };
        assert_eq!(result_ok(), 1);
        assert!(std::str::from_utf8(answer).is_ok_and(|s| s.contains("<strong>bold</strong>")));
        let (c, cl) = put("nope");
        let (b, bl) = put("{}");
        unsafe { call(c, cl, b, bl) };
        assert_eq!(result_ok(), 0);
        Ok(())
    }
}
