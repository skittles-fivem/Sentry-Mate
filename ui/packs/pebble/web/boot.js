// Sentry Mate, "Pebble" UI pack: runs before the first paint.
//
// Sets the size, the device pixel step and the theme on <html> before anything is drawn, so the page never
// shows at one size and then jumps. A classic script file, not inline, to suit the page's Content-Security-Policy.
//
//   html font-size = 100% (the browser's standard size) x --auto x --ui-size
//
// - Auto follows the screen, not the window (screen.width x screen.height in CSS px): 1 up to 1440 x 810,
//   rising in a straight line to 1.45 at 3840 x 2160, capped there; the long and the short side both count.
//   Firefox reports the screen scaled by its page zoom, so there Auto is 1. The rule is the same in every pack.
// - "Follow screen size" (on by default) turns Auto on; off, Auto is 1.
// - --ui-size is the person's Interface size, 50 to 200 % (default 100).
// - --dp is one device pixel in CSS px (1px at scale 1, 0.5px at scale 2). app.css rounds every length to it,
//   so edges land on whole device pixels at any browser zoom.
// - The Interface size on this device ('sentrymate.device-size', the same key in every pack; ui/shared/devicesize.js):
//   80, 90, 100, 110 or 125 (%) kept in this browser only. When set it wins here: --auto 1 and --ui-size that
//   size. Absent (Automatic, the default), the account's values above apply.
// core/display.js has the same formula and takes over once the page has started: change both together.
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
  var size = read('pebble:size') || {};
  var n = Math.round(Number(size.size));
  var pct = isFinite(n) && size.size != null ? Math.max(50, Math.min(200, n)) : 100;
  var follow = size.follow !== false;
  var device = Math.round(Number(read('sentrymate.device-size')));
  if ([80, 90, 100, 110, 125].indexOf(device) >= 0) { pct = device; follow = false; }
  var dpr = window.devicePixelRatio || 1;
  html.style.setProperty('--auto', String(follow ? auto() : 1)); // CSSOM, which the CSP allows
  html.style.setProperty('--ui-size', String(pct / 100));
  html.style.setProperty('--dp', (1 / dpr) + 'px');
  var theme = read('pebble:theme');
  if (theme === 'light' || theme === 'dark') html.setAttribute('data-theme', theme);
}());
