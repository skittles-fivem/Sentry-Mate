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
// core/display.js owns these values after start-up (window.consoleRoot is its only link to this file).
(function () {
  var root = document.documentElement;
  var KEY = 'sentrymate.console.interface';
  var THEME_KEY = 'sentrymate.console.theme';
  var MIN = 75, MAX = 200;

  function clampSize(v) {
    v = Math.round(Number(v));
    if (!isFinite(v)) return 100;
    return Math.max(MIN, Math.min(MAX, v));
  }

  // { size: 75..200, follow: true | false } -> what was applied
  function apply(s) {
    s = s || {};
    var size = clampSize(s.size == null ? 100 : s.size);
    var follow = s.follow !== false;
    root.style.setProperty('--ui-size', String(size / 100));
    root.style.setProperty('--ui-follow', follow ? '1' : '0');
    root.setAttribute('data-ui-size', String(size));
    root.setAttribute('data-ui-follow', follow ? 'on' : 'off');
    return { size: size, follow: follow };
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
    apply: apply, clampSize: clampSize, effectiveTheme: effectiveTheme, applyTheme: applyTheme,
    saved: function () { return saved; },
  };
})();
