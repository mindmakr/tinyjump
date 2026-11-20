// tinyjump web host. Runs the runtime (WebAssembly) in the page, draws the view tree
// with real DOM elements, performs effects, persists objects in localStorage, and
// exposes the same automation surface as every other host on `window.tinyjump`.
// Plain ES2017, no build step, so it runs on old browsers.

const ALIGN = { start: 'flex-start', center: 'center', end: 'flex-end', stretch: 'stretch' };
const JUSTIFY = { start: 'flex-start', center: 'center', end: 'flex-end', between: 'space-between', around: 'space-around' };
const MAX_TIMEOUT = 2147483647;
const RTL_LANGUAGES = ['ar', 'he', 'fa', 'ur', 'ps', 'sd', 'ug', 'yi', 'dv', 'ku'];

const CSS = [
  '.tj-column,.tj-row,.tj-scroll{display:flex;box-sizing:border-box;min-width:0}',
  '.tj-column,.tj-scroll{flex-direction:column}',
  '.tj-row{flex-direction:row;align-items:center}',
  '.tj-scroll{overflow:auto}',
  '.tj-spacer{flex:1}',
  '.tj-text{white-space:pre-wrap}',
  '.tj-toggle{display:inline-flex;gap:8px;align-items:center}',
  // Touch targets of at least 44px, as on Android and iOS (and as `tj review` checks).
  '.tj-button,.tj-input,.tj-toggle{min-height:44px;box-sizing:border-box;font:inherit}',
  '.tj-button{min-width:44px;padding:0 14px}',
  '.tj-input{padding:0 10px}',
  // Each text reads in its own script's direction (English stays "3 left" in an Arabic
  // screen) while aligning to the layout's start, like Android and iOS.
  '.tj-text,.tj-input,.tj-toggle span,.tj-input::placeholder{unicode-bidi:plaintext;text-align:left}',
  '.tj-button{unicode-bidi:plaintext}',
  '[dir=rtl] .tj-text,[dir=rtl] .tj-input,[dir=rtl] .tj-toggle span,[dir=rtl] .tj-input::placeholder{text-align:right}',
  '.tj-error{position:fixed;left:8px;right:8px;bottom:8px;padding:12px;background:#2b0d0d;color:#ffd7d7;',
  'font:13px/1.4 ui-monospace,monospace;white-space:pre-wrap;border-radius:8px;z-index:99999;max-height:40vh;overflow:auto}',
].join('');

function px(v) {
  return typeof v === 'number' ? v + 'px' : v;
}

function localStore(prefix) {
  return {
    load(key) {
      try { return localStorage.getItem(prefix + key); } catch (e) { return null; }
    },
    save(key, value) {
      localStorage.setItem(prefix + key, value);
    },
    keys(p) {
      const out = [];
      try {
        for (let i = 0; i < localStorage.length; i++) {
          const k = localStorage.key(i);
          if (k && k.indexOf(prefix + p) === 0) out.push(k.slice(prefix.length));
        }
      } catch (e) { /* storage blocked: behave as empty */ }
      return out;
    },
  };
}

/**
 * Boots an app.
 * @param {{wasm: string, source: string, root: HTMLElement, server?: string, store?: object, seed?: number, devOverlay?: boolean, token?: string, user?: string}} opts
 *   server: base URL of a `tj serve` backend for `@server` objects ('' = same origin).
 *   token, user: the signed-in user's token for the server and their id (see setToken).
 *   seed: for replaying a run; by default it comes from crypto.getRandomValues.
 *   dir: 'ltr' or 'rtl'; defaults to the page's dir, then the browser language.
 */
export async function start(opts) {
  const store = opts.store || localStore('tj:');
  const enc = new TextEncoder();
  const dec = new TextDecoder();
  let ex = null;

  const view = (ptr, len) => new Uint8Array(ex.memory.buffer, ptr, len);
  const readStr = (ptr, len) => dec.decode(view(ptr, len).slice());
  const writeStr = (s) => {
    const b = enc.encode(s);
    const p = ex.tj_alloc(b.length);
    view(p, b.length).set(b);
    return [p, b.length];
  };
  // Hands a string to Rust, which takes ownership of the allocation.
  const giveStr = (s, outPtr) => {
    const [p, n] = writeStr(s);
    new DataView(ex.memory.buffer).setUint32(outPtr, p, true);
    return n;
  };

  const imports = {
    env: {
      tj_host_load(kp, kl, out) {
        const v = store.load(readStr(kp, kl));
        return v === null || v === undefined ? -1 : giveStr(v, out);
      },
      tj_host_save(kp, kl, vp, vl) {
        try { store.save(readStr(kp, kl), readStr(vp, vl)); return 0; } catch (e) { return 1; }
      },
      tj_host_keys(pp, pl, out) {
        return giveStr(JSON.stringify(store.keys(readStr(pp, pl))), out);
      },
    },
  };
  const bytes = await (await fetch(opts.wasm)).arrayBuffer();
  ex = (await WebAssembly.instantiate(bytes, imports)).instance.exports;

  const result = () => JSON.parse(readStr(ex.tj_out_ptr(), ex.tj_out_len()));
  const root = opts.root;
  // Right-to-left locales mirror the layout; the browser's flexbox follows `dir`.
  root.setAttribute('dir', opts.dir || document.documentElement.dir || (RTL_LANGUAGES.indexOf((navigator.language || '').split('-')[0]) >= 0 ? 'rtl' : 'ltr'));
  const nodes = new Map([[0, root]]);
  const pending = new Map();
  let nextReply = 1;
  let wakeTimer = null;
  let overlay = null;

  if (!document.getElementById('tj-style')) {
    const style = document.createElement('style');
    style.id = 'tj-style';
    style.textContent = CSS;
    document.head.appendChild(style);
  }

  function showError(message, trace) {
    console.error('[tinyjump]', message, (trace || []).join('\n'));
    if (!opts.devOverlay) return;
    if (!overlay) {
      overlay = document.createElement('div');
      overlay.className = 'tj-error';
      overlay.title = 'click to dismiss';
      overlay.onclick = () => { overlay.remove(); overlay = null; };
      document.body.appendChild(overlay);
    }
    overlay.textContent = message + (trace && trace.length ? '\n  ' + trace.join('\n  ') : '');
  }

  function send(input) {
    const [p, n] = writeStr(JSON.stringify(input));
    const code = ex.tj_handle(Date.now(), p, n);
    ex.tj_free(p, n);
    const out = result();
    if (code !== 0) { showError(out.message); return; }
    apply(out);
  }

  function request(input) {
    const id = nextReply++;
    input.reply = id;
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject });
      send(input);
    });
  }

  function apply(outputs) {
    for (const o of outputs) {
      switch (o.type) {
        case 'patch': o.ops.forEach(patch); break;
        case 'effect': runEffect(o.id, o.effect); break;
        case 'subscribe': subscribe(o.object, o.seq); break;
        case 'wake': wake(o.at); break;
        case 'reply': {
          const p = pending.get(o.reply);
          if (p) { pending.delete(o.reply); if (o.error) p.reject(new Error(o.error)); else p.resolve(o.ok); }
          break;
        }
        case 'log': (o.level === 'error' ? console.error : o.level === 'warn' ? console.warn : console.log)('[app]', o.message); break;
        case 'error': showError(o.message, o.trace); break;
        case 'emit': case 'changed': window.dispatchEvent(new CustomEvent('tinyjump:' + o.type, { detail: o })); break;
      }
    }
  }

  function wake(at) {
    clearTimeout(wakeTimer);
    if (at === null || at === undefined) return;
    wakeTimer = setTimeout(() => send({ type: 'tick' }), Math.min(MAX_TIMEOUT, Math.max(0, at - Date.now())));
  }

  // ---------- server objects ----------

  const server = opts.server; // base URL of `tj serve`/`tj dev`; '' = same origin; undefined = none
  const replicas = new Map(); // object key -> change number of the state this device has
  let events = null;
  let reconnect = null;
  let token = opts.token || null; // sent as `Authorization: Bearer` (docs/access-control.md)
  let user = opts.user || null;
  let roles = opts.roles || []; // as the server verified them: for showing the right screen
  const auth = (headers) => (token ? Object.assign({ authorization: 'Bearer ' + token }, headers) : headers);
  // A 401: the embedding app should refresh the token and call setToken.
  const tokenExpired = () => window.dispatchEvent(new CustomEvent('tinyjump:tokenExpired'));

  const objectUrl = (key) => server + '/api/' + key.split('/').map(encodeURIComponent).join('/');

  // One realtime stream for all replicas, reopened when the set changes. It resumes from the
  // oldest replica's change number, so the server sends only what this device missed. Without
  // a token, the browser reconnects by itself from the last event id. With one, EventSource
  // cannot send it, so the page gets a one-time ticket first, and reconnects itself (a used
  // ticket cannot be reused).
  function connectRealtime() {
    if (events) events.close();
    events = null;
    if (!replicas.size) return;
    let since = Infinity;
    const keys = [];
    replicas.forEach((seq, k) => { since = Math.min(since, seq); keys.push(k); });
    if (!token) {
      openStream(server + '/api/events?' + keys.map((k) => 'object=' + encodeURIComponent(k)).join('&') + '&since=' + since);
      return;
    }
    fetch(server + '/api/events/ticket', { method: 'POST', headers: auth({ 'content-type': 'application/json' }), body: JSON.stringify({ objects: keys }) })
      .then((r) => {
        if (r.status === 401) tokenExpired();
        return r.json();
      })
      .then((j) => {
        if (!j.ok) throw new Error(j.error);
        openStream(server + '/api/events?ticket=' + encodeURIComponent(j.ok.ticket) + '&since=' + since);
      })
      .catch(() => { clearTimeout(reconnect); reconnect = setTimeout(connectRealtime, 2000); });
  }

  function openStream(url) {
    const stream = new EventSource(url);
    events = stream;
    // The server is reachable again: queued calls go now rather than after their backoff.
    stream.addEventListener('open', () => send({ type: 'online' }));
    stream.addEventListener('error', () => {
      if (!token || events !== stream) return;
      stream.close();
      clearTimeout(reconnect);
      reconnect = setTimeout(connectRealtime, 2000);
    });
    stream.addEventListener('changed', (e) => {
      const d = JSON.parse(e.data);
      if (!replicas.has(d.object)) return;
      replicas.set(d.object, Math.max(replicas.get(d.object), d.seq));
      send({ type: 'sync', object: d.object, state: d.state, seq: d.seq });
    });
    // Server-side events (emit, read) reach the page like local ones.
    stream.addEventListener('emit', (e) => window.dispatchEvent(new CustomEvent('tinyjump:emit', { detail: JSON.parse(e.data) })));
  }

  // The signed-in user changed: calls queued by the previous one are dropped and replicas
  // start over, so nothing one user could see or send carries over to the next. `newRoles`
  // are the roles the server reports for them (`GET /api/whoami`), for the UI only.
  function setToken(newToken, newUser, newRoles) {
    token = newToken || null;
    const sameRoles = (newRoles || []).join(',') === roles.join(',');
    if ((newUser || null) !== user || !sameRoles) {
      const changed = (newUser || null) !== user;
      user = newUser || null;
      roles = newRoles || [];
      if (changed) {
        replicas.clear();
        if (events) events.close();
        events = null;
      }
      send({ type: 'identity', user, roles });
    } else {
      clearTimeout(reconnect);
      reconnect = setTimeout(connectRealtime, 0);
    }
  }

  function subscribe(key, seq) {
    if (server === undefined || replicas.has(key)) return;
    replicas.set(key, seq || 0);
    clearTimeout(reconnect);
    reconnect = setTimeout(connectRealtime, 0);
  }

  function serverCall(id, effect) {
    if (server === undefined) {
      send({ type: 'effectDone', id, error: 'no server configured for ' + effect.object }); // permanent: not retried
      return;
    }
    // The call id makes retries safe: the server runs each call once. No answer (offline, a
    // 5xx, an unreadable reply) is `unreachable`, and the runtime retries the call later.
    // A 401 is retried too, once the embedding app has refreshed the token.
    fetch(objectUrl(effect.object) + '/' + encodeURIComponent(effect.method), {
      method: 'POST',
      headers: auth({ 'content-type': 'application/json', 'idempotency-key': effect.call }),
      body: JSON.stringify(effect.args),
    })
      .then((r) => {
        if (r.status === 401) tokenExpired();
        return r.status >= 500 || r.status === 401 ? Promise.reject(new Error('server answered ' + r.status)) : r.json();
      })
      .then((j) => send(j.error ? { type: 'effectDone', id, error: j.error } : { type: 'effectDone', id, ok: j.ok }))
      .catch((e) => send({ type: 'effectDone', id, error: String(e && e.message || e), unreachable: true }));
  }

  function runEffect(id, effect) {
    if (effect.kind === 'call') {
      serverCall(id, effect);
      return;
    }
    if (effect.kind !== 'http') {
      send({ type: 'effectDone', id, error: 'unsupported effect ' + effect.kind });
      return;
    }
    fetch(effect.url, { method: effect.method, headers: effect.headers, body: effect.body })
      .then((res) => res.text().then((body) => {
        const headers = {};
        res.headers.forEach((v, k) => { headers[k] = v; });
        send({ type: 'effectDone', id, ok: { status: res.status, headers, body } });
      }))
      .catch((e) => send({ type: 'effectDone', id, error: String(e && e.message || e) }));
  }

  // ---------- native widgets ----------

  function create(kind, props) {
    let el;
    switch (kind) {
      case 'Button': el = document.createElement('button'); el.type = 'button'; el.className = 'tj-button'; break;
      case 'Input': el = document.createElement(props.multiline ? 'textarea' : 'input'); el.className = 'tj-input'; break;
      case 'Toggle': {
        el = document.createElement('label');
        el.className = 'tj-toggle';
        el.appendChild(document.createElement('input')).type = 'checkbox';
        el.appendChild(document.createElement('span'));
        break;
      }
      case 'Image': el = document.createElement('img'); break;
      default: el = document.createElement('div'); el.className = 'tj-' + kind.toLowerCase();
    }
    return el;
  }

  function setProp(el, kind, k, v) {
    const s = el.style;
    const none = v === null || v === undefined;
    switch (k) {
      case 'text':
        if (kind === 'Toggle') el.lastChild.textContent = none ? '' : v;
        else el.textContent = none ? '' : v;
        break;
      case 'value':
        if (kind === 'Toggle') el.firstChild.checked = !!v;
        else if (el.value !== (none ? '' : String(v))) el.value = none ? '' : String(v);
        break;
      case 'placeholder': el.placeholder = none ? '' : v; break;
      case 'disabled': el.disabled = !!v; break;
      case 'secure': if (el.tagName === 'INPUT') el.type = v ? 'password' : 'text'; break;
      case 'src': el.src = none ? '' : v; break;
      case 'fit': s.objectFit = none ? '' : v; break;
      case 'lines':
        s.display = none ? '' : '-webkit-box';
        s.webkitLineClamp = none ? '' : String(v);
        s.webkitBoxOrient = none ? '' : 'vertical';
        s.overflow = none ? '' : 'hidden';
        break;
      case 'id': if (none) delete el.dataset.tjId; else el.dataset.tjId = v; break;
      case 'label': if (none) el.removeAttribute('aria-label'); else el.setAttribute('aria-label', v); break;
      case 'padding': s.padding = none ? '' : px(v); break;
      case 'margin': s.margin = none ? '' : px(v); break;
      case 'gap': s.gap = none ? '' : px(v); break;
      case 'align': s.alignItems = none ? '' : (ALIGN[v] || v); break;
      case 'justify': s.justifyContent = none ? '' : (JUSTIFY[v] || v); break;
      case 'grow': s.flexGrow = none ? '' : String(v); break;
      case 'width': s.width = none ? '' : px(v); break;
      case 'height': s.height = none ? '' : px(v); break;
      case 'color': s.color = none ? '' : v; break;
      case 'background': s.background = none ? '' : v; break;
      case 'radius': s.borderRadius = none ? '' : px(v); break;
      case 'size': s.fontSize = none ? '' : px(v); break;
      case 'weight': s.fontWeight = none ? '' : String(v); break;
      case 'border': s.border = none ? '' : (typeof v === 'number' ? v + 'px solid currentColor' : v); break;
    }
  }

  function listen(el, id, kind) {
    const fire = (event, value) => {
      if (el.__tj.events.indexOf(event) >= 0) send({ type: 'ui', target: { node: id }, event, value });
    };
    if (kind === 'Button') el.addEventListener('click', () => fire('onPress', null));
    if (kind === 'Input') {
      el.addEventListener('input', () => fire('onChange', el.value));
      el.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' && (el.tagName === 'INPUT' || e.ctrlKey || e.metaKey)) fire('onSubmit', el.value);
      });
    }
    if (kind === 'Toggle') el.firstChild.addEventListener('change', () => fire('onChange', el.firstChild.checked));
  }

  function patch(op) {
    switch (op.op) {
      case 'create': {
        const el = create(op.kind, op.props);
        el.__tj = { id: op.id, kind: op.kind, events: op.events };
        Object.keys(op.props).forEach((k) => setProp(el, op.kind, k, op.props[k]));
        listen(el, op.id, op.kind);
        nodes.set(op.id, el);
        break;
      }
      case 'update': {
        const el = nodes.get(op.id);
        Object.keys(op.props).forEach((k) => setProp(el, el.__tj.kind, k, op.props[k]));
        if (op.events) el.__tj.events = op.events;
        break;
      }
      case 'children': {
        const parent = nodes.get(op.id);
        op.ids.forEach((cid, i) => {
          const child = nodes.get(cid);
          if (parent.childNodes[i] !== child) parent.insertBefore(child, parent.childNodes[i] || null);
        });
        while (parent.childNodes.length > op.ids.length) parent.removeChild(parent.lastChild);
        break;
      }
      case 'remove': {
        const el = nodes.get(op.id);
        if (el && el.parentNode) el.parentNode.removeChild(el);
        nodes.delete(op.id);
        break;
      }
    }
  }

  // ---------- automation ----------

  function withBounds(list) {
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    return list.map((n) => {
      const el = nodes.get(n.id);
      const r = el ? el.getBoundingClientRect() : { x: 0, y: 0, width: 0, height: 0 };
      const visible = !!el && r.width > 0 && r.height > 0 && r.bottom > 0 && r.right > 0 && r.top < vh && r.left < vw;
      return Object.assign({}, n, {
        bounds: { x: Math.round(r.left), y: Math.round(r.top), w: Math.round(r.width), h: Math.round(r.height) },
        visible,
        children: withBounds(n.children),
      });
    });
  }

  const api = {
    /** Semantic tree with bounds and viewport visibility, plus object states. */
    inspect: () => request({ type: 'inspect' }).then((s) => Object.assign(s, { tree: withBounds(s.tree), viewport: { w: window.innerWidth, h: window.innerHeight } })),
    /** Fire a UI event. target: {id} | {text} | {node} */
    act: (target, event, value) => request({ type: 'ui', target, event, value: value === undefined ? null : value }),
    call: (object, method, args) => request({ type: 'call', object, method, args: args || [] }),
    get: (object) => request({ type: 'get', object }),
    /** Any protocol input (docs/host-protocol.md): those that take a reply resolve with it. */
    input: (i) => {
      if (['call', 'get', 'ui', 'inspect'].indexOf(i.type) >= 0) return request(Object.assign({}, i));
      send(Object.assign({}, i));
      return Promise.resolve(null);
    },
    tick: () => send({ type: 'tick' }),
    /** The signed-in user's token and id; a new id drops what the previous user queued or saw. */
    setToken,
    /** Forgets the token, the replicas and every queued server call. */
    signOut: () => setToken(null, null, []),
  };

  // ---------- boot ----------

  const [p, n] = writeStr(opts.source);
  // The seed drives Math.random (set it to replay a run); ids and crypto.randomId() use the
  // secure bytes sent as `entropy` below.
  const words = crypto.getRandomValues(new Uint32Array(2));
  const seed = opts.seed || (words[0] & 0x1fffff) * 4294967296 + words[1];
  const code = ex.tj_boot(p, n, Date.now(), seed, 1);
  ex.tj_free(p, n);
  const out = result();
  if (code === 1) {
    showError(out.map((e) => e.text).join('\n\n'));
    throw new Error('compile failed');
  }
  if (code === 2) {
    showError('boot failed: ' + out.message, out.trace);
    throw new Error(out.message);
  }
  apply(out);
  const entropy = crypto.getRandomValues(new Uint8Array(32));
  send({ type: 'entropy', bytes: Array.from(entropy, (b) => ('0' + b.toString(16)).slice(-2)).join('') });
  // Before any other input, so calls held from the last session are checked against it.
  if (opts.user !== undefined) send({ type: 'identity', user: opts.user || null, roles });
  window.tinyjump = api;
  return api;
}
