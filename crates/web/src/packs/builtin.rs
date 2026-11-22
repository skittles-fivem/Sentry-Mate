//! The built-in UI packs, embedded at build time (see `build.rs`): `ui/packs/<id>/ui.json` and every file of its
//! `web/` folder except the developers' own (`README.md`, mock and test files).

/// A built-in pack: its id (the folder name), `ui.json`, and its `web/` files by path (`core/app.js`).
pub struct BuiltinPack {
    pub id: &'static str,
    pub ui_json: &'static [u8],
    pub files: &'static [(&'static str, &'static [u8])],
}

include!(concat!(env!("OUT_DIR"), "/builtin_packs.rs"));
