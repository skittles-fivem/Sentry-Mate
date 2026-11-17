// Stand-ins for the few browser objects the modules touch outside a page: <html> (lang, data-ready),
// localStorage, sessionStorage and history. Installed once per process and shared by every test file,
// so files that run together in one process see the same objects.
const g = globalThis;

function memoryStorage() {
  const m = new Map();
  return {
    getItem: (k) => (m.has(k) ? m.get(k) : null),
    setItem: (k, v) => { m.set(k, String(v)); },
    removeItem: (k) => { m.delete(k); },
    clear: () => m.clear(),
  };
}
const define = (k, v) => Object.defineProperty(g, k, { value: v, configurable: true, writable: true });

if (!g.__fakeBrowser) {
  const historyLog = [];
  define('__fakeBrowser', { historyLog, memoryStorage, define });
  define('document', { documentElement: { lang: '', dataset: {} }, title: '' });
  define('localStorage', memoryStorage());
  define('sessionStorage', memoryStorage());
  define('history', {
    pushState: (s, t, u) => historyLog.push(['push', u]),
    replaceState: (s, t, u) => historyLog.push(['replace', u]),
  });
}

export const { historyLog } = g.__fakeBrowser;
export { memoryStorage, define };
export const htmlEl = () => g.document.documentElement;
