// Classic-script entry: <script src=".../global.js"></script> defines
// window.nukemichi. Every method returns a Promise and loads the ES module on
// first use, so calls made before it loads simply wait (in call order).
(function () {
  var script = document.currentScript;
  var src = script && script.src ? script.src : location.href;
  // A bare package URL (cdn.jsdelivr.net/npm/nukemichi@0.1) has no file path to resolve against.
  var index = /\/global(\.min)?\.js([?#].*)?$/.test(src)
    ? new URL('index.js', src).href
    : src.replace(/[?#].*$/, '').replace(/\/$/, '') + '/js/src/index.js';
  var mod = import(index).then(function (m) { return m.default; });
  function later(name) {
    return function () {
      var args = arguments;
      return mod.then(function (n) { return n[name].apply(n, args); });
    };
  }
  window.nukemichi = {
    configure: later('configure'),
    fetch: later('fetch'),
    connect: later('connect'),
    ready: later('ready'),
    createClient: later('createClient'),
    loaded: mod,
  };
})();
