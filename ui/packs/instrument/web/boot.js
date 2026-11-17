// Sentry Mate · "Instrument" UI pack · runs in <head>, before the first paint.
//
// The root font size (see "root size" in app.css) is
//     browser default size (100%)  x  Auto factor  x  Interface size
// This file owns the two factors, so the first frame is already the right size:
// - Auto factor: 1 on ordinary screens, growing to 1.45 on a native 4K screen at 100% OS scaling. It is
//   worked out from the screen's size in device-independent pixels (screen.width / height), which page
//   zoom does not change in Chromium and WebKit, so browser zoom always works fully on top of it, and a
//   window that is resized does not change it. A 4K screen at 200% OS scaling is 1920 x 1080 DIPs and gets
//   no growth: the OS already enlarged everything.
//   Firefox reports the screen in CSS pixels, scaled by page zoom, so there the factor would undo zoom;
//   Firefox gets 1 (the person can still pick an Interface size).
// - Interface size: the person's setting, 'auto' (x1) or 75 to 200 (%). It multiplies the Auto size, so
//   on a 4K wall 150% is 1.5 x the Auto size. app.js loads the saved value from the server and applies it
//   live; this browser keeps a copy (localStorage) so the next first paint already uses it.
//
// app.js calls window.uiRoot.apply() and window.uiRoot.autoFactor(); nothing else touches these values.
(function () {
  var SIZES = ['auto', 75, 90, 100, 110, 125, 150, 175, 200];
  var root = document.documentElement;
  var GECKO_ZOOMED_SCREEN = /\bFirefox\//.test(navigator.userAgent || '');

  // 1 up to 2400 x 1350 DIPs, then linear to 1.45 at 3840 x 2160 (long and short side, so a portrait
  // 4K screen grows too and a 3840 x 1080 ultrawide does not), capped there.
  function autoFactor() {
    if (GECKO_ZOOMED_SCREEN) return 1;
    var w = screen.width || 0, h = screen.height || 0;
    var long = Math.max(w, h), short = Math.min(w, h);
    var t = Math.min((long - 2400) / 1440, (short - 1350) / 810);
    t = Math.max(0, Math.min(1, t));
    return Math.round((1 + 0.45 * t) * 1000) / 1000;
  }

  // size: 'auto' or a percentage from SIZES. Returns the size applied.
  function apply(size) {
    if (SIZES.indexOf(size) < 0) size = 'auto';
    root.style.setProperty('--ui-auto', String(autoFactor()));
    root.style.setProperty('--ui-scale', size === 'auto' ? '1' : String(size / 100));
    root.setAttribute('data-ui-size', String(size));
    return size;
  }

  var saved = 'auto';
  try { saved = JSON.parse(localStorage.getItem('sentrymate.ui-size')); } catch (e) { /* storage blocked: Auto */ }
  apply(saved);

  window.uiRoot = { SIZES: SIZES, autoFactor: autoFactor, apply: apply };
})();
