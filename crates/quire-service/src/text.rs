//! Djot → HTML for text regions. Documents are untrusted input, so raw HTML
//! passthrough is dropped and only http(s)/mailto/fragment links survive.

use std::borrow::Cow;

use jotdown::{AttributeKind, Attributes, Container, Event};

/// Earlier versions wrote text colours unquoted (`[x]{color=#c00}`), which Djot does not
/// accept as an attribute value; quote them so those documents keep their colours.
fn quote_colors(src: &str) -> Cow<'_, str> {
    quote_colors_at(src, &mut Vec::new())
}

/// [`quote_colors`], noting where each added quote landed in the result, so that offsets
/// into it can be mapped back to the source.
fn quote_colors_at<'a>(src: &'a str, added: &mut Vec<usize>) -> Cow<'a, str> {
    if !src.contains("color=#") {
        return Cow::Borrowed(src);
    }
    let mut out = String::with_capacity(src.len() + 8);
    let mut rest = src;
    while let Some(i) = rest.find("color=#") {
        // `i + 6` is the '#', an ASCII byte, so this split is always on a character boundary
        let Some((head, value)) = rest.split_at_checked(i + 6) else { break };
        let bytes = value.as_bytes();
        let hex = bytes.iter().skip(1).take_while(|b| b.is_ascii_hexdigit()).count();
        let ends = matches!(bytes.get(1 + hex), Some(b' ' | b'}'));
        out.push_str(head);
        match value.split_at_checked(1 + hex) {
            Some((color, tail)) if (3..=8).contains(&hex) && ends => {
                added.push(out.len());
                out.push('"');
                out.push_str(color);
                added.push(out.len());
                out.push('"');
                rest = tail;
            }
            _ => rest = value,
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// Djot copies any `{key=value}` onto the element, so a document could add `style`, `id` or an
/// event handler. Only what the editor and the SMath importer write is kept: classes (`.center`,
/// `.underline`) and the `color`, `size` and `font` that `applyDjotAttributes` turns into styles.
fn safe_attrs(attrs: Attributes<'_>) -> Attributes<'_> {
    attrs
        .into_iter()
        .filter(|(k, _)| match k {
            AttributeKind::Class => true,
            AttributeKind::Pair { key } => matches!(key.as_ref(), "color" | "size" | "font"),
            AttributeKind::Id | AttributeKind::Comment => false,
        })
        .collect()
}

fn safe_link(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || lower.starts_with('#')
}

/// Inline math in a text region (`` $`expr` ``), in reading order. These are live:
/// they are evaluated with the worksheet as `<region id>:m<n>`.
pub fn inline_math(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    for e in jotdown::Parser::new(&quote_colors(src)) {
        match e {
            Event::Start(Container::Math { .. }, _) => current = Some(String::new()),
            Event::End(Container::Math { .. }) => out.extend(current.take()),
            Event::Str(s) => {
                if let Some(c) = current.as_mut() {
                    c.push_str(&s);
                }
            }
            _ => {}
        }
    }
    out
}

pub fn render_djot(src: &str) -> String {
    render(src, None)
}

/// HTML for the rich-text editor (`app/src/richtext.ts`). Each top-level block comes in a
/// `<div data-djot="…">` that holds its exact source, blank lines after it included, so the
/// editor writes untouched blocks back byte for byte. Blocks the editor cannot represent
/// (tables, code, footnotes, attributes it does not know…) are also marked `data-keep`: the
/// editor shows them as rendered and keeps their source. Inline math keeps its source as
/// `<span class="qmath" data-expr="…">` so the editor can show and edit it.
pub fn render_djot_editable(src: &str) -> String {
    let src = without_nul(src);
    let mut added = Vec::new();
    let prepared = quote_colors_at(&src, &mut added);
    // an offset into `prepared`, in `src`: without the quotes added to legacy colours
    let original = |at: usize| at - added.partition_point(|&q| q < at);
    let blocks = top_level_blocks(&prepared);
    let mut neutral = Neutralize::new(true);
    let mut html = String::with_capacity(prepared.len() * 2);
    for (i, block) in blocks.iter().enumerate() {
        let from = if i == 0 { 0 } else { original(block.start) };
        let to = blocks.get(i + 1).map_or(src.len(), |next| original(next.start));
        html.push_str("<div data-djot=\"");
        push_attr(&mut html, src.get(from..to).unwrap_or_default());
        html.push_str(if editable(&block.events) { "\">" } else { "\" data-keep=\"\">" });
        let events = block.events.iter().cloned().filter_map(|e| neutral.apply(e));
        html.push_str(&jotdown::html::render_to_string(events));
        html.push_str("</div>\n");
    }
    let exprs = inline_math(&src);
    let html = replace_sentinels(&html, "");
    let mut out = String::with_capacity(html.len());
    let mut rest = html.as_str();
    let marker = "<span class=\"qmath\" data-math=\":m";
    while let Some((head, after)) = rest.split_once(marker) {
        out.push_str(head);
        let end = after.find('"').unwrap_or(0);
        let n: usize = after.get(..end).and_then(|d| d.parse().ok()).unwrap_or(0);
        out.push_str("<span class=\"qmath\" data-expr=\"");
        push_attr(&mut out, exprs.get(n).map(String::as_str).unwrap_or_default());
        out.push('"');
        rest = after.get(end + 1..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

/// Escape text for a double-quoted HTML attribute. Carriage returns become references, since
/// an HTML parser would otherwise turn `\r\n` into `\n` and the source would change.
fn push_attr(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
}

/// A top-level block of a Djot document, and where its source starts.
struct Block<'s> {
    start: usize,
    events: Vec<Event<'s>>,
}

/// The top-level blocks of a document. Sections are not blocks: a heading carries the section
/// it opens (which holds its attributes), and the rest of that section follows as blocks.
fn top_level_blocks(src: &str) -> Vec<Block<'_>> {
    let mut blocks = Vec::new();
    let mut section = None;
    let mut current: Option<Block<'_>> = None;
    let mut depth = 0usize;
    for (e, range) in jotdown::Parser::new(src).into_offset_iter() {
        if depth == 0 {
            match &e {
                Event::Start(Container::Document, _)
                | Event::End(Container::Document | Container::Section { .. })
                | Event::Blankline => continue,
                Event::Start(Container::Section { .. }, _) => {
                    section = Some((e, range.start));
                    continue;
                }
                _ => {}
            }
        }
        let block = current.get_or_insert_with(|| match (&e, section.take()) {
            (Event::Start(Container::Heading { .. }, _), Some((open, start))) => {
                Block { start, events: vec![open] }
            }
            _ => Block { start: range.start, events: Vec::new() },
        });
        match &e {
            Event::Start(..) => depth += 1,
            Event::End(..) => depth = depth.saturating_sub(1),
            _ => {}
        }
        block.events.push(e);
        if depth == 0
            && let Some(mut done) = current.take()
        {
            // the section ends further down the document; here it closes around the heading
            if let Some(Event::Start(open @ Container::Section { .. }, _)) = done.events.first() {
                let close = Event::End(open.clone());
                done.events.push(close);
            }
            blocks.push(done);
        }
    }
    blocks
}

/// Whether the rich-text editor can represent a block exactly. It writes back what it holds,
/// so a block it would change must be kept as written instead.
fn editable(events: &[Event]) -> bool {
    let mut events = events.iter().peekable();
    while let Some(e) = events.next() {
        let ok = match e {
            // a cross-reference is an empty link; one with text would lose it
            Event::Start(Container::Link(url, _), _) if url.starts_with("#ref:") => {
                container_editable(e) && matches!(events.peek(), Some(Event::End(_)))
            }
            Event::Start(..) => container_editable(e),
            Event::FootnoteReference(_) | Event::ThematicBreak(_) | Event::Attributes(_) => false,
            _ => true,
        };
        if !ok {
            return false;
        }
    }
    true
}

fn container_editable(e: &Event) -> bool {
    use jotdown::{LinkType, ListKind, OrderedListNumbering, OrderedListStyle, SpanLinkType};
    let Event::Start(container, attrs) = e else { return false };
    match container {
        Container::Section { .. } => {
            attrs_within(attrs, &["title", "center", "right", "justify"], &["size", "font"])
        }
        Container::Paragraph => attrs_within(attrs, &["center", "right", "justify"], &["size", "font"]),
        Container::Span => attrs_within(attrs, &["underline", "strike"], &["color", "size"]),
        Container::Link(url, kind) => {
            let classes: &[&str] = if url.starts_with("#ref:") { &["num", "page", "title"] } else { &[] };
            matches!(
                kind,
                LinkType::AutoLink | LinkType::Span(SpanLinkType::Inline | SpanLinkType::Reference)
            ) && safe_link(url)
                && attrs_within(attrs, classes, &[])
        }
        Container::List { kind: ListKind::Unordered(_), .. }
        | Container::List {
            kind:
                ListKind::Ordered {
                    numbering: OrderedListNumbering::Decimal,
                    style: OrderedListStyle::Period,
                    ..
                },
            ..
        } => attrs.is_empty(),
        Container::Math { display } => !display && attrs.is_empty(),
        Container::Heading { .. }
        | Container::ListItem
        | Container::Blockquote
        | Container::Strong
        | Container::Emphasis
        | Container::Delete
        | Container::Insert
        | Container::Mark
        | Container::Superscript
        | Container::Subscript
        | Container::Verbatim => attrs.is_empty(),
        _ => false,
    }
}

/// Attributes made only of these classes and of these keys, with values the editor reads.
fn attrs_within(attrs: &jotdown::Attributes, classes: &[&str], keys: &[&str]) -> bool {
    attrs.iter().all(|(kind, value)| {
        let value = value.to_string();
        match kind {
            jotdown::AttributeKind::Class => value.split_whitespace().all(|c| classes.contains(&c)),
            jotdown::AttributeKind::Pair { key } if keys.contains(&key.as_ref()) => match key.as_ref() {
                "size" => {
                    value.bytes().all(|b| b.is_ascii_digit() || b == b'.') && value.parse::<f64>().is_ok()
                }
                "color" => {
                    let hex = value.strip_prefix('#').unwrap_or_default();
                    (3..=8).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit())
                }
                _ => !value.contains('"'),
            },
            _ => false,
        }
    })
}

/// Like [`render_djot`], but inline math becomes `<span class="qmath" data-math="<id>:m<n>">`
/// placeholders that the editor fills with the evaluated, typeset expression.
pub fn render_djot_with_math(src: &str, region_id: &str) -> String {
    render(src, Some(region_id))
}

fn without_nul(src: &str) -> Cow<'_, str> {
    if src.contains('\u{0}') { Cow::Owned(src.replace('\u{0}', "")) } else { Cow::Borrowed(src) }
}

fn render(src: &str, math_prefix: Option<&str>) -> String {
    let src = without_nul(src);
    let src = quote_colors(&src);
    let mut neutral = Neutralize::new(math_prefix.is_some());
    let events = jotdown::Parser::new(&src).filter_map(|e| neutral.apply(e));
    let html = jotdown::html::render_to_string(events);
    match math_prefix {
        Some(id) => replace_sentinels(&html, id),
        None => html,
    }
}

/// Makes untrusted events safe to render: raw HTML is dropped, unsafe links lead nowhere and
/// only formatting attributes survive ([`safe_attrs`]). The editor's `data-djot` source keeps
/// a block's attributes as written, so filtering what is rendered loses nothing on save.
/// With `math`, inline math becomes numbered sentinels that [`replace_sentinels`] swaps for
/// placeholders.
struct Neutralize {
    math: bool,
    raw_depth: usize,
    in_math: bool,
    math_index: usize,
}

impl Neutralize {
    fn new(math: bool) -> Self {
        Neutralize { math, raw_depth: 0, in_math: false, math_index: 0 }
    }

    fn apply<'s>(&mut self, e: Event<'s>) -> Option<Event<'s>> {
        match &e {
            Event::Start(Container::RawBlock { .. } | Container::RawInline { .. }, _) => {
                self.raw_depth += 1;
                return None;
            }
            Event::End(Container::RawBlock { .. } | Container::RawInline { .. }) => {
                self.raw_depth = self.raw_depth.saturating_sub(1);
                return None;
            }
            _ if self.raw_depth > 0 => return None,
            Event::Start(Container::Math { .. }, _) if self.math => {
                // a sentinel survives HTML escaping; it is swapped for the placeholder later
                self.in_math = true;
                let sentinel = format!("\u{0}{}\u{0}", self.math_index);
                self.math_index += 1;
                return Some(Event::Str(sentinel.into()));
            }
            Event::End(Container::Math { .. }) if self.math => {
                self.in_math = false;
                return None;
            }
            Event::Str(_) if self.in_math => return None,
            Event::Start(Container::Link(url, ty), attrs) if !safe_link(url) => {
                return Some(Event::Start(Container::Link("#".into(), *ty), safe_attrs(attrs.clone())));
            }
            Event::End(Container::Link(url, ty)) if !safe_link(url) => {
                return Some(Event::End(Container::Link("#".into(), *ty)));
            }
            _ => {}
        }
        Some(match e {
            Event::Start(c, attrs) => Event::Start(c, safe_attrs(attrs)),
            e => e,
        })
    }
}

fn replace_sentinels(html: &str, id: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut parts = html.split('\u{0}');
    out.push_str(parts.next().unwrap_or_default());
    while let (Some(n), Some(rest)) = (parts.next(), parts.next()) {
        out.push_str(&format!("<span class=\"qmath\" data-math=\"{id}:m{n}\"></span>"));
        out.push_str(rest);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{render_djot, render_djot_editable};

    /// Shared with `app/src/richtext.test.ts`: Djot, the editor HTML for it, and what the
    /// editor writes when it rewrites those blocks (the same Djot, for blocks it keeps).
    #[derive(serde::Deserialize)]
    struct Case {
        djot: String,
        html: String,
        written: String,
    }

    #[test]
    fn editor_cases_render_as_the_app_expects() -> serde_json::Result<()> {
        let cases: Vec<Case> = serde_json::from_str(include_str!("../../../app/src/richtext.cases.json"))?;
        assert!(cases.len() > 40);
        for c in cases {
            assert_eq!(render_djot_editable(&c.djot), c.html, "{:?}", c.djot);
            // what the editor writes reads the same as what it was given (a soft line break
            // becomes a space)
            let words = |s: &str| render_djot(s).split_whitespace().collect::<Vec<_>>().join(" ");
            assert_eq!(words(&c.written), words(&c.djot), "{:?} → {:?}", c.djot, c.written);
            if c.html.matches("data-keep").count() == c.html.matches("<div data-djot").count() {
                assert_eq!(c.written, c.djot.trim_end(), "kept blocks are written as they were");
            }
        }
        Ok(())
    }

    #[test]
    fn editor_blocks_hold_their_exact_source() {
        let src = "{.title}\n# Report\r\n\r\nText [x]{color=#c00} with a [link](https://a.b).\n\n\n\
                   | a | b |\n|---|---|\n\n- one\n- two\n\n> quote\n\n``` rust\ncode\n```\n";
        let html = render_djot_editable(src);
        let doc = sources(&html);
        assert_eq!(doc.concat(), src);
        assert_eq!(doc.len(), 6, "{doc:?}");
        // the legacy unquoted colour is shown quoted but kept as written
        assert!(html.contains(r##"<span color="#c00">x</span>"##), "{html}");
        assert_eq!(html.matches("data-keep").count(), 2, "the table and the code: {html}");
    }

    /// The `data-djot` sources in editor HTML, unescaped.
    fn sources(html: &str) -> Vec<String> {
        html.split("<div data-djot=\"")
            .skip(1)
            .filter_map(|s| s.split_once('"'))
            .map(|(src, _)| {
                src.replace("&#13;", "\r")
                    .replace("&quot;", "\"")
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&amp;", "&")
            })
            .collect()
    }

    #[test]
    fn formatting() {
        assert_eq!(render_djot("*bold* and _it_"), "<p><strong>bold</strong> and <em>it</em></p>\n");
        assert!(render_djot("[Carga:]{.underline} x").contains("<span class=\"underline\">Carga:</span>"));
        assert!(render_djot("{.center}\nTítulo").contains("<p class=\"center\">Título</p>"));
        assert_eq!(
            render_djot("# Assumptions"),
            "<section id=\"Assumptions\">\n<h1>Assumptions</h1>\n</section>\n"
        );
    }

    #[test]
    fn colours_quoted_or_not() {
        let want = "<p><span color=\"#c00\" size=\"9\">red</span></p>\n";
        assert_eq!(render_djot("[red]{color=\"#c00\" size=9}"), want);
        assert_eq!(render_djot("[red]{color=#c00 size=9}"), want);
        assert!(render_djot("[u]{.underline color=#1a2b3c}").contains("color=\"#1a2b3c\""));
        assert_eq!(render_djot("a color=#zz b"), "<p>a color=#zz b</p>\n");
    }

    #[test]
    fn cross_references_are_empty_fragment_links() {
        assert_eq!(render_djot("See [](#ref:b12b)."), "<p>See <a href=\"#ref:b12b\"></a>.</p>\n");
        assert_eq!(
            render_djot("on [](#ref:t1){.page}"),
            "<p>on <a href=\"#ref:t1\" class=\"page\"></a></p>\n"
        );
    }

    #[test]
    fn inline_math_becomes_live_placeholders() {
        let src = "*$`C := 5.1 'm`  X $`L := 2 'm =`*";
        assert_eq!(super::inline_math(src), ["C := 5.1 'm", "L := 2 'm ="]);
        let h = super::render_djot_with_math(src, "abc");
        assert!(h.contains(r#"<span class="qmath" data-math="abc:m0"></span>  X <span class="qmath" data-math="abc:m1"></span>"#), "{h}");
    }

    #[test]
    fn editable_html_keeps_inline_math_source() {
        let h = super::render_djot_editable("Area $`A := 2 m*3 m =` ok");
        assert!(h.contains(r#"<span class="qmath" data-expr="A := 2 m*3 m ="></span>"#), "{h}");
    }

    #[test]
    fn untrusted_content_is_neutralized() {
        let h = render_djot(
            "`<script>alert(1)</script>`{=html} ok\n\n```=html\n<img src=x onerror=alert(1)>\n```\n",
        );
        assert!(!h.contains("script") && !h.contains("onerror"), "{h}");
        assert!(h.contains("ok"));
        let l = render_djot("[x](javascript:alert(1))");
        assert!(!l.contains("javascript"), "{l}");
        assert!(render_djot("<b>hi</b>").contains("&lt;b&gt;"));
    }

    #[test]
    fn only_formatting_attributes_reach_the_html() {
        let src = "[x]{onmouseover=\"alert(1)\" style=\"position:fixed\" #login .u color=\"#c00\"}\n\n\
                   {onclick=\"alert(1)\" .center size=12 font=\"Arial\"}\npara\n\n\
                   [y](javascript:void0){onfocus=\"alert(1)\" .page}\n";
        let h = render_djot(src);
        assert!(h.contains(r##"<span class="u" color="#c00">x</span>"##), "{h}");
        assert!(h.contains(r#"<p class="center" size="12" font="Arial">para</p>"#), "{h}");
        assert!(h.contains(r##"<a href="#" class="page">y</a>"##), "{h}");
        // the editor's render too; its data-djot source keeps them, escaped, for saving
        let editable = super::render_djot_editable(src);
        for html in [&h, &editable] {
            for bad in ["onmouseover=\"", "style=\"", "id=\"login\"", "onclick=\"", "onfocus=\""] {
                assert!(!html.contains(bad), "{bad} in {html}");
            }
        }
        assert!(editable.contains(r##"<span class="u" color="#c00">x</span>"##), "{editable}");
        assert_eq!(sources(&editable).concat(), src, "the source survives untouched");
    }
}
