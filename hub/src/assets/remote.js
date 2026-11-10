// Helm Remote: a small page that reads and steers the PC's Helm through the hub.
//
// Everything on the page comes from the hub's /remote calls; the page keeps no copy of a chat.
// Chat text is only ever shown as text (textContent), never as markup. The token is kept for this
// tab only and goes in an Authorization header, never in an address.
'use strict';

(function () {
  var KEY = 'helm-remote-token';
  var MACHINE_KEY = 'helm-remote-machine';
  // One tick every five seconds while the page is visible. What the open view shows is read every
  // tick; the Waiting list, when it is not the open view, only every third tick.
  var POLL_MS = 5000;
  var WAITING_EVERY = 3;
  var token = '';
  var machine = '';
  try {
    token = sessionStorage.getItem(KEY) || '';
    machine = sessionStorage.getItem(MACHINE_KEY) || '';
  } catch (e) {
    token = '';
    machine = '';
  }

  var state = {
    tab: 'waiting', // waiting | chats | chat
    waiting: null, // the last app_list_waiting result
    waitingSig: '', // that result as text, so an unchanged list is not drawn again
    chats: [],
    chat: null, // { id, title, status }
    messages: {}, // messageId -> message, for the open chat
    older: null, // beforeMessageId for "load older"
    cursorSet: false, // whether the first page has set that cursor
    confirm: null, // the option waiting for a second tap: requestId + ':' + index
    drafts: {}, // typed answers by request id; they outlive every redraw and poll
    stopAsk: false,
    busy: false,
    query: '',
    machines: [], // the computers that are connected, from the hub
    pausedUntil: 0, // after "too many requests", no polling until then
  };
  // Calls in flight by view: a poll is skipped while the one before it has not come back.
  var inflight = { waiting: false, chats: false, chat: false };
  var tick = 0;

  function $(id) {
    return document.getElementById(id);
  }

  function el(tag, cls, text) {
    var n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text !== undefined && text !== null) n.textContent = String(text);
    return n;
  }

  function clear(n) {
    while (n.firstChild) n.removeChild(n.firstChild);
  }

  function notice(text, isError) {
    var n = $('notice');
    if (!text) {
      n.hidden = true;
      return;
    }
    n.textContent = text;
    n.className = isError ? 'error' : '';
    n.hidden = false;
  }

  function setLine(up) {
    var p = $('line');
    p.textContent = up ? 'Helm online' : 'Helm offline';
    p.className = 'pill ' + (up ? 'online' : 'offline');
    p.setAttribute('data-online', String(up));
  }

  // The computer picker shows only when there is a choice: more than one is connected, or one was
  // chosen earlier. An empty value means the one used most recently.
  function setMachines(list) {
    if (!Array.isArray(list)) return;
    state.machines = list;
    var sel = $('machine');
    var shown = list.length > 1 || machine !== '';
    sel.hidden = !shown;
    if (!shown) return;
    var names = list.slice();
    if (machine && names.indexOf(machine) < 0) names.push(machine);
    clear(sel);
    var auto = el('option', '', 'Latest computer');
    auto.value = '';
    sel.appendChild(auto);
    names.forEach(function (name) {
      var o = el('option', '', name);
      o.value = name;
      sel.appendChild(o);
    });
    sel.value = machine;
  }

  function ago(at) {
    if (!at) return '';
    var ms = at < 1e12 ? at * 1000 : at;
    var s = Math.max(0, Math.round((Date.now() - ms) / 1000));
    if (s < 60) return 'just now';
    if (s < 3600) return Math.round(s / 60) + ' min ago';
    if (s < 86400) return Math.round(s / 3600) + ' h ago';
    return Math.round(s / 86400) + ' d ago';
  }

  function signOut(why) {
    token = '';
    try {
      sessionStorage.removeItem(KEY);
    } catch (e) {
      /* the tab keeps no token either way */
    }
    $('top').hidden = true;
    $('app').hidden = true;
    $('signin').hidden = false;
    $('token').value = '';
    stopPolling();
    if (why) {
      var h = $('signin').querySelector('p');
      h.textContent = why;
    }
  }

  // One call to Helm through the hub. Resolves to the hub's body: {ok, result | error, line}.
  function call(name, args) {
    var body = { name: name, args: args || {} };
    if (machine) body.machine = machine;
    return fetch('/remote/api/call', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + token },
      body: JSON.stringify(body),
      cache: 'no-store',
      referrerPolicy: 'no-referrer',
    }).then(
      function (r) {
        if (r.status === 401 || r.status === 403) {
          signOut('That token was not accepted. Paste the token the hub gave this device.');
          return { ok: false, error: 'Not signed in.', auth: true };
        }
        if (r.status === 429) {
          var wait = parseInt(r.headers.get('Retry-After') || '30', 10) || 30;
          state.pausedUntil = Date.now() + wait * 1000;
          return { ok: false, error: 'Too many requests. Wait ' + wait + ' seconds.', limited: true };
        }
        return r.json().then(
          function (b) {
            if (typeof b.line === 'boolean') setLine(b.line);
            setMachines(b.machines);
            if (!b.ok && b.error === undefined) b.error = 'The hub refused the call.';
            return b;
          },
          function () {
            return { ok: false, error: 'The hub answered something unexpected.' };
          }
        );
      },
      function () {
        setLine(false);
        return { ok: false, error: 'Cannot reach the hub from here.', unreachable: true };
      }
    );
  }

  // ------------------------------------------------------------------------------ Waiting

  function loadWaiting() {
    if (inflight.waiting) return Promise.resolve({ ok: false, skipped: true });
    inflight.waiting = true;
    return call('app_list_waiting', {}).then(function (b) {
      inflight.waiting = false;
      if (b.ok) {
        var items = b.result.waiting || [];
        $('badge').textContent = String(items.length);
        $('badge').hidden = items.length === 0;
        // Requests that no longer wait take their drafts with them.
        var live = {};
        items.forEach(function (i) {
          live[i.requestId] = true;
        });
        Object.keys(state.drafts).forEach(function (id) {
          if (!live[id]) delete state.drafts[id];
        });
        // An unchanged list is not drawn again, so nothing being typed is touched.
        var sig = JSON.stringify(b.result);
        var changed = sig !== state.waitingSig;
        state.waiting = b.result;
        state.waitingSig = sig;
        if (changed && state.tab === 'waiting') renderWaiting();
      } else if (!b.auth && !b.limited && state.tab === 'waiting') {
        notice(b.error, true);
      }
      return b;
    });
  }

  // The option the page showed, by number and by its own text, so Helm can tell that the two agree.
  function answerWaiting(item, index, text) {
    var args = { chatId: item.chatId, requestId: item.requestId };
    if (text !== undefined) {
      args.text = text;
    } else {
      args.index = index;
      args.label = item.options[index].label;
    }
    state.busy = true;
    state.confirm = null;
    renderWaiting();
    call('app_answer_waiting', args).then(function (b) {
      state.busy = false;
      if (b.ok) {
        delete state.drafts[item.requestId];
        notice('Answered: ' + b.result.label, false);
      } else {
        // A typed answer that did not go is kept, so it can be sent again.
        notice(b.error, true);
      }
      state.waitingSig = '';
      loadWaiting().then(function () {
        if (state.tab === 'waiting') renderWaiting();
      });
    });
  }

  function renderWaiting() {
    var box = $('view-waiting');
    // Keep the focus and the cursor of an answer being typed across the redraw.
    var focus = null;
    var active = document.activeElement;
    if (active && active.getAttribute && active.getAttribute('data-request')) {
      focus = {
        id: active.getAttribute('data-request'),
        start: active.selectionStart,
        end: active.selectionEnd,
      };
    }
    clear(box);
    var w = state.waiting;
    if (!w) {
      box.appendChild(el('p', 'empty', 'Loading…'));
      return;
    }
    var items = w.waiting || [];
    if (!w.answering) {
      box.appendChild(
        el('p', 'hint', w.hint || 'Answering from this page is off. Turn it on in Helm: Settings, Hub.')
      );
    }
    if (!items.length) {
      box.appendChild(el('p', 'empty', 'Nothing is waiting for you.'));
      return;
    }
    items.forEach(function (item) {
      var card = el('div', 'card');
      card.setAttribute('data-testid', 'remote-card');
      card.setAttribute('data-request', item.requestId);
      card.appendChild(el('h3', '', item.chatTitle || 'A Helm chat'));
      var kind = { permission: 'Permission', plan: 'Plan to approve', question: 'Question' }[item.kind] || item.kind;
      card.appendChild(el('p', 'kind', kind));
      card.appendChild(el('p', 'text', item.text));
      if (item.tooLong) {
        card.appendChild(el('p', 'hint', 'Too long to read here, so only the options that allow nothing are offered.'));
      }
      var options = el('div', 'options');
      (item.options || []).forEach(function (o, i) {
        var key = item.requestId + ':' + i;
        var confirming = state.confirm === key;
        var b = el('button', o.allows ? 'allow' : '', confirming ? 'Tap again to confirm: ' + o.label : o.label);
        if (confirming) b.className += ' confirming';
        b.type = 'button';
        b.setAttribute('data-testid', 'remote-option');
        b.disabled = !item.answerable || state.busy;
        b.addEventListener('click', function () {
          if (o.allows && !confirming) {
            state.confirm = key;
            renderWaiting();
            return;
          }
          answerWaiting(item, i);
        });
        options.appendChild(b);
      });
      if ((item.options || []).length) card.appendChild(options);
      if (item.typed) {
        var ta = el('textarea');
        ta.rows = 2;
        ta.maxLength = 4000;
        ta.placeholder = 'Type an answer';
        ta.setAttribute('aria-label', 'Type an answer');
        ta.setAttribute('data-request', item.requestId);
        ta.setAttribute('data-testid', 'remote-answer-text');
        ta.value = state.drafts[item.requestId] || '';
        ta.addEventListener('input', function () {
          state.drafts[item.requestId] = ta.value;
        });
        ta.disabled = !item.answerable || state.busy;
        var go = el('button', 'primary', 'Send answer');
        go.type = 'button';
        go.setAttribute('data-testid', 'remote-answer-send');
        go.disabled = !item.answerable || state.busy;
        go.addEventListener('click', function () {
          var t = ta.value.trim();
          if (t) answerWaiting(item, 0, t);
        });
        card.appendChild(ta);
        card.appendChild(go);
      }
      box.appendChild(card);
    });
    if (focus) {
      var again = box.querySelector('textarea[data-request="' + focus.id + '"]');
      if (again && !again.disabled) {
        again.focus();
        try {
          again.setSelectionRange(focus.start, focus.end);
        } catch (e) {
          /* a cursor that cannot be restored is only a cursor */
        }
      }
    }
  }

  // --------------------------------------------------------------------------------- Chats

  function loadChats() {
    if (inflight.chats) return Promise.resolve({ ok: false, skipped: true });
    inflight.chats = true;
    var args = { limit: 30 };
    if (state.query) args.query = state.query;
    return call('app_list_chats', args).then(function (b) {
      inflight.chats = false;
      if (b.ok) {
        state.chats = b.result.chats || [];
        if (state.tab === 'chats') renderChats();
      } else if (!b.auth && !b.limited && state.tab === 'chats') {
        notice(b.error, true);
      }
      return b;
    });
  }

  function renderChats() {
    var ul = $('chat-list');
    clear(ul);
    if (!state.chats.length) {
      var li = el('li');
      li.appendChild(el('p', 'empty', state.query ? 'No chat matches.' : 'No chats.'));
      ul.appendChild(li);
      return;
    }
    state.chats.forEach(function (c) {
      var li = el('li');
      var b = el('button');
      b.type = 'button';
      b.setAttribute('data-testid', 'remote-chat');
      b.appendChild(el('span', 'title', c.title || 'Untitled'));
      b.appendChild(
        el('span', 'meta', [c.projectName, c.status, ago(c.updatedAt)].filter(Boolean).join(' · '))
      );
      b.addEventListener('click', function () {
        openChat(c.chatId, c.title, c.status);
      });
      li.appendChild(b);
      ul.appendChild(li);
    });
  }

  function openChat(id, title, status) {
    state.chat = { id: id, title: title || 'Untitled', status: status || '' };
    state.messages = {};
    state.older = null;
    state.cursorSet = false;
    state.stopAsk = false;
    $('stop').textContent = 'Stop';
    show('chat');
    $('chat-title').textContent = state.chat.title;
    renderStatus();
    refreshChat(true);
  }

  function renderStatus() {
    var s = $('chat-status');
    var busy = /^(running|starting|stopping)/.test(state.chat.status || '');
    s.textContent = state.chat.status || '';
    s.className = 'pill' + (busy ? ' busy' : '');
  }

  function absorb(result, scroll) {
    (result.messages || []).forEach(function (m) {
      state.messages[m.messageId] = m;
    });
    if (result.chat && result.chat.status) {
      state.chat.status = result.chat.status;
      renderStatus();
    }
    renderMessages(scroll);
  }

  function refreshChat(scroll) {
    if (!state.chat || inflight.chat) return;
    var id = state.chat.id;
    inflight.chat = true;
    call('app_read_chat', { chatId: id, limit: 30 }).then(function (b) {
      inflight.chat = false;
      if (!state.chat || state.chat.id !== id) return;
      if (b.ok) {
        // Later looks add to what is loaded; "older" learns its cursor from the first page only.
        if (!state.cursorSet) {
          state.cursorSet = true;
          state.older = b.result.beforeMessageId || null;
        }
        absorb(b.result, scroll);
      } else if (!b.auth && !b.limited) {
        notice(b.error, true);
      }
    });
  }

  function loadOlder() {
    if (!state.chat || state.older === null) return;
    var id = state.chat.id;
    call('app_read_chat', { chatId: id, beforeMessageId: state.older, limit: 30 }).then(function (b) {
      if (!state.chat || state.chat.id !== id) return;
      if (b.ok) {
        state.older = b.result.beforeMessageId || null;
        absorb(b.result, false);
      } else if (!b.auth) {
        notice(b.error, true);
      }
    });
  }

  function renderMessages(scroll) {
    var ol = $('messages');
    var nearEnd = scroll || window.innerHeight + window.scrollY >= document.body.scrollHeight - 80;
    clear(ol);
    Object.keys(state.messages)
      .map(Number)
      .sort(function (a, b) {
        return a - b;
      })
      .forEach(function (id) {
        var m = state.messages[id];
        var li = el('li', m.role === 'user' ? 'user' : 'assistant');
        li.appendChild(el('p', 'who', (m.role === 'user' ? 'You / another agent' : 'Helm chat') + ' · ' + ago(m.at)));
        li.appendChild(el('p', 'text', m.text + (m.truncated ? ' …' : '')));
        ol.appendChild(li);
      });
    $('older').hidden = state.older === null;
    if (nearEnd) window.scrollTo(0, document.body.scrollHeight);
  }

  function sendMessage(ev) {
    ev.preventDefault();
    if (!state.chat) return;
    var ta = $('draft');
    var text = ta.value.trim();
    if (!text) return;
    $('send').disabled = true;
    call('app_send_message', { chatId: state.chat.id, message: text, mode: 'steer' }).then(function (b) {
      $('send').disabled = false;
      if (b.ok) {
        ta.value = '';
        notice('Sent.', false);
        refreshChat(true);
      } else {
        notice(b.error, true);
      }
    });
  }

  function stopChat() {
    if (!state.chat) return;
    if (!state.stopAsk) {
      state.stopAsk = true;
      $('stop').textContent = 'Tap again to stop this chat';
      return;
    }
    state.stopAsk = false;
    $('stop').textContent = 'Stop';
    call('app_stop_chat', { chatId: state.chat.id }).then(function (b) {
      notice(b.ok ? 'Stop requested.' : b.error, !b.ok);
      refreshChat(false);
    });
  }

  // ------------------------------------------------------------------------------- Frame

  function show(tab) {
    state.tab = tab;
    notice('');
    $('view-waiting').hidden = tab !== 'waiting';
    $('view-chats').hidden = tab !== 'chats';
    $('view-chat').hidden = tab !== 'chat';
    $('tabs').hidden = tab === 'chat';
    $('tab-waiting').setAttribute('aria-pressed', String(tab === 'waiting'));
    $('tab-chats').setAttribute('aria-pressed', String(tab === 'chats'));
    if (tab === 'waiting') {
      renderWaiting();
      loadWaiting();
    }
    if (tab === 'chats') {
      renderChats();
      loadChats();
    }
  }

  var timer = null;
  function poll() {
    if (document.visibilityState !== 'visible' || !token) return;
    if (Date.now() < state.pausedUntil) return;
    tick += 1;
    // Waiting is read every tick while it is the open view, else every few ticks for the badge.
    if (state.tab === 'waiting' || tick % WAITING_EVERY === 0) loadWaiting();
    if (state.tab === 'chats') loadChats();
    if (state.tab === 'chat') refreshChat(false);
  }
  function startPolling() {
    stopPolling();
    timer = setInterval(poll, POLL_MS);
  }
  function stopPolling() {
    if (timer) clearInterval(timer);
    timer = null;
  }

  function start() {
    $('signin').hidden = true;
    $('top').hidden = false;
    $('app').hidden = false;
    show('waiting');
    // The first look decides which tab opens: Waiting when something waits, else Chats.
    loadWaiting().then(function (b) {
      if (b.ok && !(b.result.waiting || []).length && state.tab === 'waiting') show('chats');
    });
    startPolling();
  }

  $('signin-form').addEventListener('submit', function (ev) {
    ev.preventDefault();
    var t = $('token').value.trim();
    if (!t) return;
    token = t;
    try {
      sessionStorage.setItem(KEY, t);
    } catch (e) {
      /* the page still works for this load */
    }
    $('token').value = '';
    start();
  });
  $('signout').addEventListener('click', function () {
    signOut('Signed out.');
  });
  $('tab-waiting').addEventListener('click', function () {
    show('waiting');
  });
  $('tab-chats').addEventListener('click', function () {
    show('chats');
  });
  $('back').addEventListener('click', function () {
    state.chat = null;
    show('chats');
  });
  $('machine').addEventListener('change', function () {
    machine = $('machine').value;
    try {
      if (machine) sessionStorage.setItem(MACHINE_KEY, machine);
      else sessionStorage.removeItem(MACHINE_KEY);
    } catch (e) {
      /* the choice then lasts for this load only */
    }
    // What is shown belongs to the other computer: read it again.
    state.waiting = null;
    state.waitingSig = '';
    state.messages = {};
    state.cursorSet = false;
    show(state.tab === 'chat' ? 'chats' : state.tab);
    if (state.tab === 'chats') state.chat = null;
  });
  $('older').addEventListener('click', loadOlder);
  $('send-form').addEventListener('submit', sendMessage);
  $('stop').addEventListener('click', stopChat);
  var searchTimer = null;
  $('search').addEventListener('input', function () {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(function () {
      state.query = $('search').value.trim();
      loadChats();
    }, 350);
  });
  document.addEventListener('visibilitychange', function () {
    if (document.visibilityState === 'visible') poll();
  });

  if (token) start();
  else $('signin').hidden = false;
})();
