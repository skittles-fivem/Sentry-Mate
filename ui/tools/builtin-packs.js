// The built-in UI packs: every folder of ui/packs with a ui.json and a web/index.html, the same rule the server
// uses to embed them (crates/web/build.rs). Tests that run on every pack loop over this list, so a pack moved to
// ui/archived (not built, served or tested: ui/archived/README.md) drops out of them by itself. Today it is
// Instrument alone.
'use strict';

const fs = require('fs');
const path = require('path');

const PACKS_DIR = path.join(path.dirname(__dirname), 'packs');

const BUILTIN_PACKS = Object.freeze(fs.readdirSync(PACKS_DIR, { withFileTypes: true })
  .filter((d) => d.isDirectory() && fs.existsSync(path.join(PACKS_DIR, d.name, 'ui.json'))
    && fs.existsSync(path.join(PACKS_DIR, d.name, 'web', 'index.html')))
  .map((d) => d.name)
  .sort((a, b) => (a === 'instrument' ? -1 : b === 'instrument' ? 1 : a.localeCompare(b))));

module.exports = { BUILTIN_PACKS, PACKS_DIR };
