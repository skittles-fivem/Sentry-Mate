// Sentry Mate · "Console" UI pack · runs in <head>, before the first paint.
//
// The root size is worked out in app.css from three values set here, so the first frame is already the
// right size and theme:
//
//   root = round( clamp(13px, 0.55vw + 8px, 20px) x Interface size )     with Follow screen size on
//   root = round( 16px x Interface size )                                 with it off
//
// --ui-size is the Interface size (75 to 200 %, the person's interface_size preference) as a factor,
// --ui-follow is 1 or 0 (follow_screen). Every length in app.css is a whole number of px worked out from the
// root, so boxes and lines land on whole pixels at any size.
// The preferences live on the server; this browser keeps a copy only for the first paint.
// The Interface size on this device ('sentrymate.device-size', the same key in every pack; ui/shared/devicesize.js):
// 80, 90, 100, 110 or 125 (%) kept in this browser only. When set it wins here: --ui-size is that size and
// --ui-follow 0, so the root is 16 px x that size. Absent (Automatic, the default), the account's values apply.
// core/display.js owns these values after start-up (window.consoleRoot is its only link to this file).
(function () {
  var root = document.documentElement;
  var KEY = 'sentrymate.console.interface';
  var THEME_KEY = 'sentrymate.console.theme';
  var MIN = 75, MAX = 200;
  var DEVICE_KEY = 'sentrymate.device-size';
  var DEVICE_SIZES = [80, 90, 100, 110, 125];

  // this browser's own size (a step of DEVICE_SIZES) or null (Automatic); storage blocked: null
  function deviceSize(v) {
    if (arguments.length === 0) { try { v = JSON.parse(localStorage.getItem(DEVICE_KEY)); } catch (e) { v = null; } }
    v = Math.round(Number(v));
    return DEVICE_SIZES.indexOf(v) >= 0 ? v : null;
  }
  var device = deviceSize();
  // keep (or forget, with null) this browser's own size; the caller applies again
  function setDevice(v) {
    device = deviceSize(v);
    try { if (device) localStorage.setItem(DEVICE_KEY, JSON.stringify(device)); else localStorage.removeItem(DEVICE_KEY); } catch (e) { /* storage blocked: this page only */ }
    return device;
  }

  function clampSize(v) {
    v = Math.round(Number(v));
    if (!isFinite(v)) return 100;
    return Math.max(MIN, Math.min(MAX, v));
  }

  // { size: 75..200, follow: true | false } (the account's) -> what was applied: those, and device (this
  // browser's own size, or null for Automatic), which wins when set
  function apply(s) {
    s = s || {};
    var size = clampSize(s.size == null ? 100 : s.size);
    var follow = s.follow !== false;
    root.style.setProperty('--ui-size', String((device || size) / 100));
    root.style.setProperty('--ui-follow', follow && !device ? '1' : '0');
    root.setAttribute('data-ui-size', String(size));
    root.setAttribute('data-ui-follow', follow ? 'on' : 'off');
    if (device) root.setAttribute('data-ui-device', String(device)); else root.removeAttribute('data-ui-device');
    return { size: size, follow: follow, device: device };
  }

  // 'light' | 'dark' | 'device' (auto) -> the theme drawn now
  function effectiveTheme(choice) {
    if (choice === 'light' || choice === 'dark') return choice;
    try { return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark'; } catch (e) { return 'dark'; }
  }
  function applyTheme(choice) {
    var eff = effectiveTheme(choice);
    root.setAttribute('data-theme', eff);
    root.setAttribute('data-theme-choice', choice === 'light' || choice === 'dark' ? choice : 'device');
    return eff;
  }

  var saved = null, theme = 'device';
  try { saved = JSON.parse(localStorage.getItem(KEY)); } catch (e) { /* storage blocked: defaults */ }
  try { theme = JSON.parse(localStorage.getItem(THEME_KEY)) || 'device'; } catch (e) { /* storage blocked */ }
  apply(saved);
  applyTheme(theme);

  window.consoleRoot = {
    MIN: MIN, MAX: MAX, KEY: KEY, THEME_KEY: THEME_KEY,
    DEVICE_KEY: DEVICE_KEY, DEVICE_SIZES: DEVICE_SIZES, setDevice: setDevice,
    readDevice: function () { device = deviceSize(); return device; },
    apply: apply, clampSize: clampSize, effectiveTheme: effectiveTheme, applyTheme: applyTheme,
    saved: function () { return saved; },
  };
})();
