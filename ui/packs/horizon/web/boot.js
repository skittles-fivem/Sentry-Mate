// Sentry Mate · Horizon UI pack · runs in <head>, before the first paint, so the first frame already has the
// right size, look and background (no jump on load).
//
// Root size = browser default (100%) x Auto x Interface size. The rule is the same in every pack:
// - Auto follows the screen (screen.width x screen.height in CSS px), not the window: 1 up to 1440 x 810,
//   then a straight line to 1.45 at 3840 x 2160, capped there. The long and the short side both count. Firefox
//   reports the screen scaled by its page zoom, so there Auto is 1. Follow screen size off makes Auto 1.
// - Interface size is the person's interface_size, 75 to 200 (%), default 100.
// Both are the person's preferences on the server; this browser keeps a copy only for the first paint.
// core/display.js owns them afterwards; window.hzBoot is its link to this file.
//
// Also set here from this browser's copy: the theme (data-theme: dark | light) and the Background choice
// (data-sky-choice: auto | full | calm | off), which is kept per browser only.
(function () {
  var html = document.documentElement;
  var SIZE_KEY = 'sentrymate.interface';
  var THEME_KEY = 'sentrymate.theme';
  var SKY_KEY = 'sentrymate.horizon.sky';
  var LOW = 75, HIGH = 200;
  var firefox = /\bFirefox\//.test(navigator.userAgent || '');

  function read(key) { try { return JSON.parse(localStorage.getItem(key)); } catch (e) { return null; } }

  function clampSize(v) {
    v = Math.round(Number(v));
    return isFinite(v) ? Math.min(HIGH, Math.max(LOW, v)) : 100;
  }

  function autoFactor() {
    if (firefox) return 1;
    var w = screen.width || 0, h = screen.height || 0;
    var big = Math.max(w, h), small = Math.min(w, h);
    var t = Math.max(0, Math.min(1, Math.min((big - 1440) / 2400, (small - 810) / 1350)));
    return Math.round((1 + 0.45 * t) * 1000) / 1000;
  }

  function applySize(s) {
    s = s || {};
    var size = clampSize(s.size == null ? 100 : s.size);
    var follow = s.follow !== false;
    var auto = follow ? autoFactor() : 1;
    html.style.setProperty('--ui-auto', String(auto));
    html.style.setProperty('--ui-scale', String(size / 100));
    html.setAttribute('data-ui-size', String(size));
    return { size: size, follow: follow, auto: auto };
  }

  function themeFor(choice) {
    if (choice === 'light' || choice === 'dark') return choice;
    try { return matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark'; } catch (e) { return 'dark'; }
  }
  function applyTheme(choice) {
    var c = choice === 'light' || choice === 'dark' ? choice : 'device';
    var t = themeFor(c);
    html.setAttribute('data-theme', t);
    html.setAttribute('data-theme-choice', c);
    return t;
  }

  function skyChoice(v) { return v === 'full' || v === 'calm' || v === 'off' ? v : 'auto'; }

  var size = read(SIZE_KEY);
  var theme = read(THEME_KEY) || 'device';
  var sky = skyChoice(read(SKY_KEY));
  applySize(size);
  applyTheme(theme);
  html.setAttribute('data-sky-choice', sky);

  window.hzBoot = {
    LOW: LOW, HIGH: HIGH, SIZE_KEY: SIZE_KEY, THEME_KEY: THEME_KEY, SKY_KEY: SKY_KEY,
    autoFactor: autoFactor, applySize: applySize, clampSize: clampSize, themeFor: themeFor, applyTheme: applyTheme,
    skyChoice: skyChoice, savedSize: function () { return size; }, savedSky: function () { return sky; },
  };
})();
