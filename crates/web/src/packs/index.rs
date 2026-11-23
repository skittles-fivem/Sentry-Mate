//! The `index.html` a pack serves, rewritten once when the pack is loaded so its pages load fast and update at once:
//!
//! - Every `src="/..."` and `href="/..."` that names a file of the pack points to it under
//!   `/_v/<version>/` instead ([`PREFIX`]; the version is a hash of all of the pack's files), which is cached for good:
//!   a new build or pack gives new addresses. The files it loads use relative addresses (`./x.js`, `fonts/x.woff2`),
//!   so they are under the same prefix; `index.html` itself is checked again on every load.
//! - The modules the first page needs (the static `import`s of each `<script type="module">`, all the way down) are
//!   named with `<link rel="modulepreload">` after it, so the browser asks for them all at once instead of one
//!   level after another.
//!
//! Nothing else changes: a page that works served as it is works rewritten.

use bytes::Bytes;

/// Where a pack's files are under their version: `/_v/<version>/<path>`.
pub const PREFIX: &str = "/_v/";

/// Modules preloaded at most (a pack with more still works; they just load as the browser finds them).
const MAX_PRELOADS: usize = 200;

/// `html` with the pack's files under `/_v/<version>/` and its first modules preloaded. `has` says whether the pack
/// has a file (`core/app.js`), `read` gives its bytes.
pub fn rewrite(html: &str, version: &str, has: &dyn Fn(&str) -> bool, read: &dyn Fn(&str) -> Option<Bytes>) -> String {
    let prefix = format!("{PREFIX}{version}/");
    let mut out = String::with_capacity(html.len() + 4096);
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let tag_end = match rest[start..].find('>') {
            Some(e) => start + e + 1,
            None => break,
        };
        let tag = &rest[start..tag_end];
        out.push_str(&rewrite_tag(tag, &prefix, has));
        if let Some(entry) = module_entry(tag).filter(|p| has(p)) {
            for m in module_graph(&entry, has, read) {
                out.push_str(&format!("\n  <link rel=\"modulepreload\" href=\"{prefix}{m}\">"));
            }
        }
        rest = &rest[tag_end..];
    }
    out.push_str(rest);
    out
}

/// A tag with its `src` and `href` values that name a pack file put under `prefix`. Comments and the text between
/// tags are left alone.
fn rewrite_tag(tag: &str, prefix: &str, has: &dyn Fn(&str) -> bool) -> String {
    if tag.starts_with("<!") {
        return tag.to_string();
    }
    let mut out = String::with_capacity(tag.len() + 32);
    let mut rest = tag;
    loop {
        let found = ["src=", "href="].iter().filter_map(|a| rest.find(a).map(|i| (i, a.len()))).min();
        let Some((i, len)) = found else { break };
        // an attribute name starts after a space
        let attr_start = i == 0 || rest.as_bytes()[i - 1].is_ascii_whitespace();
        let after = &rest[i + len..];
        let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'');
        let (Some(q), true) = (quote, attr_start) else {
            out.push_str(&rest[..i + len]);
            rest = after;
            continue;
        };
        let value_end = after[1..].find(q).map(|e| e + 1);
        let Some(value_end) = value_end else { break };
        let value = &after[1..value_end];
        out.push_str(&rest[..i + len + 1]);
        let path = value.split(['?', '#']).next().unwrap_or("");
        match path.strip_prefix('/') {
            Some(p) if !p.starts_with('/') && !value.starts_with(PREFIX) && !p.is_empty() && has(p) => {
                out.push_str(prefix);
                out.push_str(&value[1..]);
            }
            _ => out.push_str(value),
        }
        out.push(q);
        rest = &after[value_end + 1..];
    }
    out.push_str(rest);
    out
}

/// The pack path of a `<script type="module" src="/...">`.
fn module_entry(tag: &str) -> Option<String> {
    if !tag.starts_with("<script") || !(tag.contains("type=\"module\"") || tag.contains("type='module'")) {
        return None;
    }
    let i = tag.find(" src=")? + 5;
    let q = tag[i..].chars().next()?;
    let v = &tag[i + 1..];
    let v = &v[..v.find(q)?];
    v.strip_prefix('/').filter(|p| !p.starts_with('/')).map(|p| p.split(['?', '#']).next().unwrap_or(p).to_string())
}

/// Every module `entry` imports statically, all the way down (not `entry` itself), in the order they are found.
fn module_graph(entry: &str, has: &dyn Fn(&str) -> bool, read: &dyn Fn(&str) -> Option<Bytes>) -> Vec<String> {
    let mut seen = vec![entry.to_string()];
    let mut i = 0;
    while i < seen.len() && seen.len() <= MAX_PRELOADS {
        let file = seen[i].clone();
        i += 1;
        let Some(src) = read(&file) else { continue };
        for spec in static_imports(&String::from_utf8_lossy(&src)) {
            if let Some(p) = resolve(&file, &spec).filter(|p| has(p))
                && !seen.contains(&p)
            {
                seen.push(p);
            }
        }
    }
    seen.into_iter().skip(1).take(MAX_PRELOADS).collect()
}

/// The specifiers of a module's static imports and re-exports (`import x from './a.js'`, `import './b.js'`,
/// `export { y } from '../c.js'`): the string after `from` or a bare `import`, never `import(`.
fn static_imports(js: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = js.as_bytes();
    for kw in ["from", "import"] {
        let mut at = 0;
        while let Some(i) = js[at..].find(kw) {
            let i = at + i;
            at = i + kw.len();
            let before_ok = i == 0
                || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'$' || b[i - 1] == b'.');
            if !before_ok {
                continue;
            }
            let after = js[at..].trim_start();
            let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
            let Some(end) = after[1..].find(q) else { continue };
            let spec = &after[1..end + 1];
            if spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') {
                out.push(spec.to_string());
            }
        }
    }
    out
}

/// `spec` imported from the pack file `from`, as a pack path (`None`: outside the pack).
fn resolve(from: &str, spec: &str) -> Option<String> {
    let spec = spec.split(['?', '#']).next().unwrap_or(spec);
    let mut parts: Vec<&str> = if spec.starts_with('/') {
        Vec::new()
    } else {
        let mut d: Vec<&str> = from.split('/').collect();
        d.pop();
        d
    };
    for seg in spec.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn pack() -> HashMap<&'static str, &'static str> {
        HashMap::from([
            ("core/app.js", "import { a } from './a.js';\nimport './side.js';\nconst p = import('../pages/x.js');"),
            ("core/a.js", "export { b } from \"../shared/b.js\"; // import nothing from 'here'"),
            ("core/side.js", "import { a } from './a.js';"),
            ("shared/b.js", "export const b = 1; const s = 'transform';"),
            ("pages/x.js", ""),
            ("app.css", ""),
            ("boot.js", ""),
        ])
    }

    fn run(html: &str) -> String {
        let p = pack();
        let has = |x: &str| p.contains_key(x);
        let read = |x: &str| p.get(x).map(|s| Bytes::from_static(s.as_bytes()));
        rewrite(html, "abc", &has, &read)
    }

    #[test]
    fn pack_files_move_under_the_version_and_the_rest_stays() {
        let html = r#"<!doctype html><!-- <script src="/boot.js"> --><head><script src="/boot.js"></script>
<link rel="stylesheet" href='/app.css?x=1'><link rel="icon" href="/missing.svg"><a href="/cars/c1">car</a>
<a href="//example.org/app.css">x</a><img data-src="/app.css"></head>"#;
        let out = run(html);
        assert!(out.contains(r#"<script src="/_v/abc/boot.js">"#), "{out}");
        assert!(out.contains(r#"href='/_v/abc/app.css?x=1'"#), "{out}");
        assert!(out.contains(r#"href="/missing.svg""#), "no such file: as it was");
        assert!(out.contains(r#"href="/cars/c1""#), "a page route: as it was");
        assert!(out.contains(r#"href="//example.org/app.css""#));
        assert!(out.contains(r#"data-src="/app.css""#), "only src and href");
        assert!(out.contains(r#"<!-- <script src="/boot.js"> -->"#), "comments stay");
        assert_eq!(run(&out), out, "rewriting twice changes nothing more");
    }

    #[test]
    fn the_first_modules_are_preloaded_once_each() {
        let out = run(r#"<head><script src="/core/app.js" type="module"></script></head>"#);
        let preloads: Vec<&str> = out.match_indices("modulepreload\" href=\"").map(|(i, _)| &out[i + 21..]).collect();
        let names: Vec<&str> = preloads.iter().map(|p| &p[..p.find('"').unwrap_or(0)]).collect();
        assert_eq!(names, ["/_v/abc/core/a.js", "/_v/abc/core/side.js", "/_v/abc/shared/b.js"], "{out}");
        assert!(!out.contains("pages/x.js"), "a dynamic import loads when it is needed");
    }

    #[test]
    fn imports_resolve_inside_the_pack_only() {
        assert_eq!(resolve("core/app.js", "./a.js").as_deref(), Some("core/a.js"));
        assert_eq!(resolve("core/app.js", "../shared/b.js").as_deref(), Some("shared/b.js"));
        assert_eq!(resolve("core/app.js", "/vendor/x.mjs").as_deref(), Some("vendor/x.mjs"));
        assert_eq!(resolve("app.js", "../../x.js"), None);
    }
}
