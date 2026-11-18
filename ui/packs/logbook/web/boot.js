// Sentry Mate, "Logbook" UI pack: runs before the first paint.
//
// Sets the size and the theme on <html> before anything is drawn, so the page never shows at one size and
// then jumps to another. A classic script file, not inline, to suit the page's Content-Security-Policy.
//
// The size of everything is the root font size (app.css):
//   html font-size = 100% (the browser's standard size) x --auto x --ui-size
// --auto follows the screen: 1 up to a 1440 x 900 screen, rising in a straight line to 1.45 at
// 3840 x 2160 (whichever of width and height is nearer its start), from screen.width x screen.height in
// CSS pixels. "Follow screen size" off makes it 1. --ui-size is the person's Interface size, 75 to 200 %.
// Both are stored with the person's preferences on the server; this browser keeps a copy only so this
// first paint is right. core/display.js has the same formula and takes over once the page has started:
// change both together.
(function () {
  var html = document.documentElement;
  function read(key) {
    try { var v = localStorage.getItem(key); return v == null ? null : JSON.parse(v); } catch (e) { return null; }
  }
  function auto() {
    var s = window.screen || {}, w = s.width, h = s.height;
    if (!(w > 0) || !(h > 0)) return 1;
    var f = Math.max(0, Math.min(1, Math.min((w - 1440) / 2400, (h - 900) / 1260)));
    return Math.round((1 + 0.45 * f) * 1000) / 1000;
  }
  var size = read('logbook:size') || {};
  var pct = typeof size.size === 'number' && size.size >= 75 && size.size <= 200 ? size.size : 100;
  var follow = size.follow !== false;
  html.style.setProperty('--auto', String(follow ? auto() : 1)); // CSSOM, which the CSP allows
  html.style.setProperty('--ui-size', String(pct / 100));
  var theme = read('logbook:theme');
  if (theme === 'light' || theme === 'dark') html.setAttribute('data-theme', theme);
}());
