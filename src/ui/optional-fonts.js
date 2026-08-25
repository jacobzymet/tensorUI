/** Load Google Fonts after first paint. A blocking stylesheet to
 *  fonts.googleapis.com hangs WKWebView indefinitely on networks that
 *  drop those hosts (China without a VPN), leaving a blank window. */
(function loadOptionalUiFonts() {
  var href =
    'https://fonts.googleapis.com/css2?family=Atkinson+Hyperlegible:wght@400;700&family=DM+Sans:wght@400;500;600&family=Fraunces:opsz,wght@9..144,500;600&family=IBM+Plex+Sans:wght@400;500;600&family=Instrument+Sans:wght@500;600&family=Inter:wght@400;500;600&family=Literata:opsz,wght@7..72,400;500;600&family=Source+Sans+3:wght@400;500;600&family=Space+Grotesk:wght@500;600&family=Syne:wght@600;700&display=swap';
  var link = document.createElement('link');
  link.rel = 'stylesheet';
  link.href = href;
  link.media = 'print';
  var settled = false;
  function abandon() {
    if (settled) return;
    settled = true;
    link.onload = null;
    link.onerror = null;
    if (link.parentNode) link.parentNode.removeChild(link);
  }
  var timer = window.setTimeout(abandon, 3000);
  link.onload = function () {
    if (settled) return;
    settled = true;
    window.clearTimeout(timer);
    link.media = 'all';
  };
  link.onerror = function () {
    window.clearTimeout(timer);
    abandon();
  };
  document.head.appendChild(link);
})();
