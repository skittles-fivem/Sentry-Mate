// Entry point for `node --test ui/shared/tests/`: Node 22 runs a directory given to --test as a module,
// which resolves to this file. It loads every *.test.js next to it; each registers its own tests.
import { readdirSync } from 'node:fs';
import { dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
for (const name of readdirSync(here).filter((n) => n.endsWith('.test.js')).sort()) {
  await import(pathToFileURL(`${here}/${name}`).href);
}
