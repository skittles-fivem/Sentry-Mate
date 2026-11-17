// Sentry Mate, "Logbook" UI pack: runs before the first paint.
//
// Sets the page's size and theme on <html> before anything is drawn, so it
// never shows at one size and then jumps to another. A classic script file,
// not inline, to suit the page's Content-Security-Policy.
//
// Size. app.css sets html font-size from one of three things:
//   1. Interface size, when the person chose one (data-ui-size): that share of
//      the standard size, 75% to 200%, on any screen. It is stored by the
//      server (api.getPrefs); this browser keeps a copy of the last value it
//      saw so the first paint is right, and app.js puts the server's value in
//      place as soon as it arrives.
//   2. Otherwise Auto (--auto, worked out here): the standard size on most
//      screens, larger on very wide windows. It is measured from the window's
//      width in device-independent pixels (window.outerWidth), which browser
//      zoom does not change. So zoom always multiplies the whole page: 150%
//      zoom is 1.5x the size at 100%, on a laptop and on a 4K wall alike.
//   3. Without this script: a CSS-only Auto from the viewport width.
(function () {
  var html = document.documentElement;

  // Auto: no growth up to 2400 device-independent px, then 1% more per 32 px,
  // up to 145% at 3840 (a 4K screen at 100% OS scaling). A 4K screen at 150%
  // or 200% OS scaling is 2560 or 1920 px wide here: the OS has already made
  // everything bigger, so it gets little or no growth.
  var AUTO_FROM = 2400, AUTO_SPAN = 3200, AUTO_MAX = 1.45;
  function windowWidth() {
    var w = window.outerWidth;
    if (!(w > 0) && window.screen) w = window.screen.width; // some embedded views report no outer size
    return w > 0 ? w : 0;
  }
  function applyAuto() {
    var w = windowWidth();
    if (!w) { html.removeAttribute('data-auto'); return; } // CSS falls back to the viewport width
    var f = Math.min(AUTO_MAX, Math.max(1, 1 + (w - AUTO_FROM) / AUTO_SPAN));
    var v = String(Math.round(f * 1000) / 1000);
    if (html.getAttribute('data-auto') === v) return;
    html.style.setProperty('--auto', v); // CSSOM, which the CSP allows (unlike style attributes in markup)
    html.setAttribute('data-auto', v);
  }
  applyAuto();
  window.addEventListener('resize', applyAuto);

  function read(key) {
    try { var v = localStorage.getItem('logbook:' + key); return v == null ? null : JSON.parse(v); } catch (e) { return null; }
  }
  var size = read('uiSize');
  if ([75, 90, 100, 110, 125, 150, 175, 200].indexOf(size) >= 0) html.setAttribute('data-ui-size', String(size));
  var theme = read('theme');
  if (theme === 'light' || theme === 'dark') html.setAttribute('data-theme', theme);
}());
