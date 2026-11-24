// Sentry Mate, "Logbook" UI pack: runs before the first paint.
//
// Sets the size and the theme on <html> before anything is drawn, so the page never shows at one size and
// then jumps to another. A classic script file, not inline, to suit the page's Content-Security-Policy.
//
// The size of everything is the root font size (app.css):
//   html font-size = 100% (the browser's standard size) x --auto x --ui-size
// The rule is the same in every Sentry Mate pack:
// - Auto follows the screen, not the window, from screen.width x screen.height in CSS pixels: 1 up to
//   1440 x 810, rising in a straight line to 1.45 at 3840 x 2160, capped there. The long and the short side
//   both count, so a portrait 4K screen grows like a landscape one and a 3440 x 1440 ultrawide grows like a
//   2560 x 1440 screen. Firefox reports the screen scaled by its page zoom, so there Auto is 1.
// - "Follow screen size" (on by default) turns Auto on; off, Auto is 1.
// - --ui-size is the person's Interface size, 75 to 200 % (default 100), whole numbers.
// Both are stored with the person's preferences on the server; this browser keeps a copy only so this
// first paint is right.
// - The Interface size on this device ('sentrymate.device-size', the same key in every pack; ui/shared/devicesize.js):
//   80, 90, 100, 110 or 125 (%) kept in this browser only. When set it wins here: --auto 1 and --ui-size that
//   size. Absent (Automatic, the default), the account's values above apply.
// core/display.js has the same formula and takes over once the page has started:
// change both together.
(function () {
  var html = document.documentElement;
  function read(key) {
    try { var v = localStorage.getItem(key); return v == null ? null : JSON.parse(v); } catch (e) { return null; }
  }
  function auto() {
    if (/\bFirefox\//.test(navigator.userAgent || '')) return 1;
    var s = window.screen || {}, w = s.width, h = s.height;
    if (!(w > 0) || !(h > 0)) return 1;
    var long = Math.max(w, h), short = Math.min(w, h);
    var f = Math.max(0, Math.min(1, Math.min((long - 1440) / 2400, (short - 810) / 1350)));
    return Math.round((1 + 0.45 * f) * 1000) / 1000;
  }
  var size = read('logbook:size') || {};
  var n = Math.round(Number(size.size));
  var pct = isFinite(n) && size.size != null ? Math.max(75, Math.min(200, n)) : 100;
  var follow = size.follow !== false;
  var device = Math.round(Number(read('sentrymate.device-size')));
  if ([80, 90, 100, 110, 125].indexOf(device) >= 0) { pct = device; follow = false; }
  html.style.setProperty('--auto', String(follow ? auto() : 1)); // CSSOM, which the CSP allows
  html.style.setProperty('--ui-size', String(pct / 100));
  var theme = read('logbook:theme');
  if (theme === 'light' || theme === 'dark') html.setAttribute('data-theme', theme);
}());
