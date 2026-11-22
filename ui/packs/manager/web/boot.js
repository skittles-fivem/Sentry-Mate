// Sentry Mate · "Simple" UI pack · runs in <head>, before the first paint.
//
// Sets the root font size, so the first frame already has the right size and theme (no jump on load):
//
//     root size = browser default (100%) x Auto x Interface size
//
// - Auto follows the screen, not the window. It is worked out from the screen's size in CSS pixels
//   (screen.width x screen.height): 1 up to about 1440 x 810, rising smoothly to 1.45 at 3840 x 2160 and
//   capped there. Both the long and the short side count, so a portrait 4K screen grows and a
//   3840 x 1080 ultrawide does not grow more than a 1920 x 1080 one. Chromium and WebKit report the screen
//   in device-independent pixels that page zoom does not change, so browser zoom works fully on top.
//   Firefox reports the screen scaled by page zoom; there Auto is 1. core/display.js works Auto out again
//   when the window moves to another screen.
// - "Follow screen size" (the person's follow_screen preference, on by default) turns Auto on; off, Auto is 1.
// - Interface size: the person's interface_size preference, 75 to 200 (%), default 100.
//
// Both preferences live on the server (/api/v1/me/preferences); this browser keeps a copy in localStorage
// only so the first paint uses them. The theme copy works the same way. core/display.js owns these values
// after start-up (window.uiRoot is its only link to this file).
(function () {
  var root = document.documentElement;
  var KEY = 'sentrymate.interface';
  var THEME_KEY = 'sentrymate.theme';
  var MIN = 75, MAX = 200;
  var GECKO_ZOOMED_SCREEN = /\bFirefox\//.test(navigator.userAgent || '');

  function clampSize(v) {
    v = Math.round(Number(v));
    if (!isFinite(v)) return 100;
    return Math.max(MIN, Math.min(MAX, v));
  }

  // 1 up to 1440 x 810, then linear to 1.45 at 3840 x 2160 (long and short side), capped there.
  function autoFactor() {
    if (GECKO_ZOOMED_SCREEN) return 1;
    var w = screen.width || 0, h = screen.height || 0;
    var long = Math.max(w, h), short = Math.min(w, h);
    var t = Math.min((long - 1440) / 2400, (short - 810) / 1350);
    t = Math.max(0, Math.min(1, t));
    return Math.round((1 + 0.45 * t) * 1000) / 1000;
  }

  // { size: 75..200, follow: true | false }. Returns what was applied, with auto (the factor used).
  function apply(s) {
    s = s || {};
    var size = clampSize(s.size == null ? 100 : s.size);
    var follow = s.follow !== false;
    var auto = follow ? autoFactor() : 1;
    root.style.setProperty('--ui-auto', String(auto));
    root.style.setProperty('--ui-scale', String(size / 100));
    root.setAttribute('data-ui-size', String(size));
    root.setAttribute('data-ui-follow', follow ? 'on' : 'off');
    layout();
    return { size: size, follow: follow, auto: auto };
  }

  // The phone layout (a tab bar at the bottom instead of the sidebar), set on <html data-phone>:
  //   "tall"  a window up to 48rem wide (a phone held upright, a small tablet, a laptop at 200 % zoom)
  //   "short" a window up to 30rem high and 60rem wide (a phone on its side): the period stays in the top bar
  // In rem of the root size, like the tiers in app.css, so a larger Interface size reaches it sooner.
  // It follows the window, not the screen; app.css and core/frame.js read the attribute.
  function phoneLayout() {
    var rem = 16 * (parseFloat(root.style.getPropertyValue('--ui-auto')) || 1) * (parseFloat(root.style.getPropertyValue('--ui-scale')) || 1);
    var w = window.innerWidth || root.clientWidth || 0, hgt = window.innerHeight || root.clientHeight || 0;
    if (!w) return '';
    if (w <= 48 * rem) return 'tall';
    if (hgt <= 30 * rem && w <= 60 * rem) return 'short';
    return '';
  }
  function layout() {
    var v = phoneLayout();
    if ((root.getAttribute('data-phone') || '') === v) return v;
    if (v) root.setAttribute('data-phone', v); else root.removeAttribute('data-phone');
    try { window.dispatchEvent(new CustomEvent('phonelayout', { detail: v })); } catch (e) { /* old browser */ }
    return v;
  }
  window.addEventListener('resize', layout);

  // 'light' | 'dark' | 'device' -> the theme drawn now
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

  window.uiRoot = {
    MIN: MIN, MAX: MAX, KEY: KEY, THEME_KEY: THEME_KEY,
    autoFactor: autoFactor, apply: apply, clampSize: clampSize, layout: layout,
    phone: function () { return root.getAttribute('data-phone') || ''; },
    effectiveTheme: effectiveTheme, applyTheme: applyTheme,
    saved: function () { return saved; },
  };
})();
