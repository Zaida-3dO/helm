// The script every sealed page loads before its own. It installs one frozen global, `helm`, and
// talks to the app only through postMessage on the channel the host wrote into the page.
//
// It runs in a frame with an opaque origin: it has no way to reach the app's page, its IPC or
// its storage. What a page may ask the app for is exactly the three requests below, each of
// which the app checks and, for messages and links, puts in front of the user first.
(function () {
  'use strict';

  var boot = window.__helmBootstrap;
  try {
    delete window.__helmBootstrap;
  } catch (e) {
    /* the channel is already read */
  }
  if (!boot || typeof boot.channel !== 'string') return;

  var MARK = 'helm-page-v1';
  var CHANNEL = boot.channel;
  var MAX_PAYLOAD = 300 * 1024;
  var MAX_STATE = 256 * 1024;
  var MAX_PENDING = 8;
  var STATE_TIMEOUT = 30 * 1000;
  var USER_TIMEOUT = 5 * 60 * 1000;
  var host = window.parent;

  var state = null;
  var revision = 0;
  var theme = { mode: 'light', variables: {}, reducedMotion: false };
  var listeners = { themechange: [], visibilitychange: [] };
  var pending = {};
  var pendingCount = 0;
  var nextId = 1;
  var saving = Promise.resolve();
  var resolveReady;
  var ready = new Promise(function (resolve) {
    resolveReady = resolve;
  });

  function post(type, fields) {
    var message = { mark: MARK, channel: CHANNEL, type: type };
    for (var k in fields) message[k] = fields[k];
    try {
      host.postMessage(message, '*');
    } catch (e) {
      /* the app is gone */
    }
  }

  function byteLength(text) {
    return typeof TextEncoder === 'function' ? new TextEncoder().encode(text).length : text.length;
  }

  function json(value, what) {
    var text;
    try {
      text = JSON.stringify(value);
    } catch (e) {
      throw new Error(what + ' must be JSON.');
    }
    if (text === undefined) throw new Error(what + ' must be JSON.');
    return text;
  }

  function request(method, payload, timeoutMs) {
    return new Promise(function (resolve, reject) {
      var text;
      try {
        text = json(payload, 'The request');
      } catch (e) {
        reject(e);
        return;
      }
      if (byteLength(text) > MAX_PAYLOAD) {
        reject(new Error('App request is too large.'));
        return;
      }
      if (pendingCount >= MAX_PENDING) {
        reject(new Error('Too many pending app requests.'));
        return;
      }
      var id = nextId++;
      pendingCount++;
      var timer = setTimeout(function () {
        if (!pending[id]) return;
        delete pending[id];
        pendingCount--;
        reject(new Error('The app request expired.'));
      }, timeoutMs);
      pending[id] = { resolve: resolve, reject: reject, timer: timer };
      post('request', { id: id, method: method, payload: JSON.parse(text) });
    });
  }

  function applyTheme(next) {
    if (!next || typeof next !== 'object') return;
    theme = {
      mode: next.mode === 'dark' ? 'dark' : 'light',
      variables: next.variables && typeof next.variables === 'object' ? next.variables : {},
      reducedMotion: next.reducedMotion === true,
    };
    var root = document.documentElement;
    root.style.colorScheme = theme.mode;
    for (var name in theme.variables) {
      if (/^--helm-[a-z-]+$/.test(name) && typeof theme.variables[name] === 'string') {
        root.style.setProperty(name, theme.variables[name]);
      }
    }
  }

  function emit(name, value) {
    var list = listeners[name].slice();
    for (var i = 0; i < list.length; i++) {
      try {
        list[i](value);
      } catch (e) {
        reportError(e);
      }
    }
  }

  function reportError(e) {
    var text = e && e.message ? e.message : String(e);
    post('error', { message: text.slice(0, 1000) });
  }

  window.addEventListener('message', function (event) {
    if (event.source !== host) return;
    var m = event.data;
    if (!m || m.mark !== MARK || m.channel !== CHANNEL) return;
    if (m.type === 'init') {
      state = m.state === undefined ? null : m.state;
      revision = typeof m.revision === 'number' ? m.revision : 0;
      applyTheme(m.theme);
      document.documentElement.removeAttribute('data-helm-loading');
      resolveReady();
    } else if (m.type === 'theme') {
      applyTheme(m.theme);
      emit('themechange', theme);
    } else if (m.type === 'visibility') {
      emit('visibilitychange', { visible: m.visible === true });
    } else if (m.type === 'response') {
      var entry = pending[m.id];
      if (!entry) return;
      delete pending[m.id];
      pendingCount--;
      clearTimeout(entry.timer);
      if (m.error) entry.reject(new Error(String(m.error)));
      else entry.resolve(m.result);
    }
  });

  // A page that never gets its init (the app is busy) still shows after a moment.
  setTimeout(function () {
    document.documentElement.removeAttribute('data-helm-loading');
  }, 4000);

  window.addEventListener('error', function (e) {
    reportError(e.error || e.message || 'Script error');
  });
  window.addEventListener('unhandledrejection', function (e) {
    reportError(e.reason || 'Unhandled rejection');
  });
  document.addEventListener('securitypolicyviolation', function (e) {
    if (/^(https?|wss?):/i.test(e.blockedURI || '')) {
      post('networkBlocked', { uri: String(e.blockedURI).slice(0, 200) });
    }
  });

  // A clicked link never navigates the frame. A web link is offered to the user through the app;
  // fragment links and download links are left alone (the sandbox allows no downloads).
  document.addEventListener(
    'click',
    function (e) {
      if (e.defaultPrevented || !e.isTrusted || !e.target || !e.target.closest) return;
      var a = e.target.closest('a[href]');
      if (!a || a.hasAttribute('download')) return;
      var href = a.getAttribute('href');
      if (!href || href.charAt(0) === '#') return;
      e.preventDefault();
      var url;
      try {
        url = new URL(a.href);
      } catch (err) {
        return;
      }
      if (url.host === location.host) return;
      if (url.protocol === 'http:' || url.protocol === 'https:') {
        api.openLink(url.href).catch(function () {});
      }
    },
    true,
  );

  var api = {
    get ready() {
      return ready;
    },
    get state() {
      return state;
    },
    get theme() {
      return theme;
    },
    setState: function (value) {
      var text;
      try {
        text = json(value, 'State');
      } catch (e) {
        return Promise.reject(new Error('Expected JSON state.'));
      }
      if (byteLength(text) > MAX_STATE) return Promise.reject(new Error('App state exceeds 256 KiB.'));
      state = JSON.parse(text);
      // One save at a time, each quoting the revision the one before it produced.
      var send = function () {
        return request('setState', { state: state, revision: revision }, STATE_TIMEOUT).then(function (r) {
          revision = r.revision;
          return r.revision;
        });
      };
      saving = saving.then(send, send);
      return saving;
    },
    on: function (name, callback) {
      if (!listeners[name] || typeof callback !== 'function') {
        throw new Error('Unknown event.');
      }
      listeners[name].push(callback);
      return function () {
        var at = listeners[name].indexOf(callback);
        if (at >= 0) listeners[name].splice(at, 1);
      };
    },
    sendMessage: function (message) {
      if (!message || typeof message !== 'object') return Promise.reject(new Error('Expected a message.'));
      var text = typeof message.text === 'string' ? message.text.trim() : '';
      if (!text || text.length > 16000) return Promise.reject(new Error('The message text is not valid.'));
      var out = { text: text };
      if (message.title !== undefined) {
        if (typeof message.title !== 'string' || message.title.length > 240) {
          return Promise.reject(new Error('The message title is not valid.'));
        }
        out.title = message.title;
      }
      if (message.data !== undefined) out.data = message.data;
      return request('sendMessage', out, USER_TIMEOUT);
    },
    openLink: function (url) {
      if (typeof url !== 'string') return Promise.reject(new Error('Expected a link.'));
      return request('openLink', { url: url }, USER_TIMEOUT);
    },
  };

  Object.freeze(api);
  Object.defineProperty(window, 'helm', { value: api, writable: false, configurable: false, enumerable: true });
  post('ready', {});
})();
