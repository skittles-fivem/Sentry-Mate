//! The built-in UI packs, embedded at build time (see `build.rs`): `ui/packs/<id>/ui.json` and every file of its
//! `web/` folder except the developers' own (`README.md`, mock and test files). A release build also embeds the
//! pack's bundled and minified scripts and style sheets (`ui/tools/bundle`, `SENTRY_MATE_UI_BUNDLE`), served in place
//! of the sources' files with the same path.

/// A built-in pack: its id (the folder name), `ui.json`, and its `web/` files by path (`core/app.js`).
pub struct BuiltinPack {
    pub id: &'static str,
    pub ui_json: &'static [u8],
    pub files: &'static [(&'static str, &'static [u8])],
    /// The bundled files (`core/app.js`, `pages/car.js`, the shared `core/c-<hash>.js` chunks, minified style
    /// sheets): served in place of the file with the same path. Empty in a build without a bundle.
    pub bundle: &'static [(&'static str, &'static [u8])],
}

include!(concat!(env!("OUT_DIR"), "/builtin_packs.rs"));
