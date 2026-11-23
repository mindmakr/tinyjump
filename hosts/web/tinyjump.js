// tinyjump web host. Runs the runtime (WebAssembly) in the page, draws the view tree
// with real DOM elements, performs effects, persists objects in localStorage, and
// exposes the same automation surface as every other host on `window.tinyjump`.
// Plain ES2017, no build step, so it runs on old browsers.

const ALIGN = { start: 'flex-start', center: 'center', end: 'flex-end', stretch: 'stretch' };
const JUSTIFY = { start: 'flex-start', center: 'center', end: 'flex-end', between: 'space-between', around: 'space-around' };
const MAX_TIMEOUT = 2147483647;
const RTL_LANGUAGES = ['ar', 'he', 'fa', 'ur', 'ps', 'sd', 'ug', 'yi', 'dv', 'ku'];

const CSS = [
  '.tj-column,.tj-row,.tj-scroll,.tj-list,.tj-theme{display:flex;box-sizing:border-box;min-width:0}',
  '.tj-column,.tj-scroll,.tj-list,.tj-theme{flex-direction:column}',
  '.tj-row{flex-direction:row;align-items:center}',
  '.tj-scroll,.tj-list{overflow:auto}',
  '.tj-list-h{flex-direction:row}',
  '.tj-list-h>*{flex-shrink:0}',
  '.tj-list-vh>*{content-visibility:auto;contain-intrinsic-size:auto var(--tj-item-h)}',
  '.tj-theme{background:var(--tj-surface,transparent);color:var(--tj-fg,inherit)}',
  '.tj-tabs{display:flex;flex-direction:row;align-items:stretch;gap:4px;border-bottom:1px solid var(--tj-border,#e5e7eb);box-sizing:border-box;min-height:44px;overflow-x:auto}',
  '.tj-tab{min-height:44px;min-width:44px;padding:0 14px;border:none;border-bottom:2px solid transparent;background:transparent;color:var(--tj-muted,inherit);font:inherit;cursor:pointer;white-space:nowrap}',
  '.tj-tab[aria-selected=true]{border-bottom-color:var(--tj-accent,#2563eb);color:var(--tj-accent,#2563eb);font-weight:600}',
  '.tj-icon{display:inline-flex;align-items:center;justify-content:center;flex-shrink:0;line-height:0}',
  '.tj-native-missing{display:inline-flex;align-items:center;justify-content:center;min-height:44px;padding:8px;border:1px dashed #9ca3af;color:#6b7280;font:12px/1.3 ui-monospace,monospace;box-sizing:border-box}',
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
  '.tj-button,.tj-tab{unicode-bidi:plaintext}',
  '[dir=rtl] .tj-text,[dir=rtl] .tj-input,[dir=rtl] .tj-toggle span,[dir=rtl] .tj-input::placeholder{text-align:right}',
  '.tj-error{position:fixed;left:8px;right:8px;bottom:8px;padding:12px;background:#2b0d0d;color:#ffd7d7;',
  'font:13px/1.4 ui-monospace,monospace;white-space:pre-wrap;border-radius:8px;z-index:99999;max-height:40vh;overflow:auto}',
].join('');

// 24x24 stroke icons for `<Icon name="..." />` (no icon font or asset pipeline needed).
const ICONS = {
  check: 'M5 13l4 4L19 7',
  plus: 'M12 5v14M5 12h14',
  close: 'M6 6l12 12M18 6L6 18',
  search: 'M11 4a7 7 0 1 0 0 14a7 7 0 1 0 0-14M20 20l-4.2-4.2',
  home: 'M3 11l9-7 9 7M5 10v10h14V10',
  user: 'M12 4a4 4 0 1 0 0 8a4 4 0 1 0 0-8M4 20c1.5-4 14.5-4 16 0',
  settings: 'M12 9a3 3 0 1 0 0 6a3 3 0 1 0 0-6M12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.1 2.1M16.3 16.3l2.1 2.1M18.4 5.6l-2.1 2.1M7.7 16.3l-2.1 2.1',
  back: 'M15 6l-6 6 6 6',
  next: 'M9 6l6 6-6 6',
  edit: 'M4 20h4l11-11-4-4L4 16v4',
  trash: 'M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13',
  calendar: 'M4 6h16v14H4zM8 3v4M16 3v4M4 10h16',
  clock: 'M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18M12 7v5l3 2',
  bell: 'M18 16H6l1-2v-4a5 5 0 0 1 10 0v4l1 2M10 19a2 2 0 0 0 4 0',
  mail: 'M4 6h16v12H4zM4 7l8 6 8-6',
  star: 'M12 3l2.9 5.9 6.5.9-4.7 4.6 1.1 6.5L12 17.8 6.2 20.9l1.1-6.5L2.6 9.8l6.5-.9z',
  heart: 'M12 20s-7-4.5-8.5-9A4.8 4.8 0 0 1 12 6.8 4.8 4.8 0 0 1 20.5 11C19 15.5 12 20 12 20z',
  info: 'M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18M12 11v5M12 8h.01',
  warning: 'M12 4L3 20h18L12 4zM12 10v4M12 17h.01',
  lock: 'M6 11h12v9H6zM8 11V8a4 4 0 0 1 8 0v3',
  menu: 'M4 7h16M4 12h16M4 17h16',
  share: 'M12 4v11M8 8l4-4 4 4M5 14v6h14v-6',
  refresh: 'M20 12a8 8 0 1 1-2.3-5.7M20 4v5h-5',
  download: 'M12 4v11M8 11l4 4 4-4M5 19h14',
};

const BUILTIN_KINDS = ['Column', 'Row', 'Scroll', 'Spacer', 'Text', 'Button', 'Input', 'Toggle', 'Image', 'Time', 'Format', 'Tabs', 'Icon', 'List', 'Theme'];
const REGISTRY = new Map();
const PLUGINS = new Map();

/**
 * Registers a custom native web component (`<Native name="MapView" ... />`) globally before `start()`.
 * @param {string} name
 * @param {{create: (props: object, emit: (event: string, value?: any) => void) => HTMLElement, update?: (el: HTMLElement, props: object, changed: object) => void, container?: (el: HTMLElement) => HTMLElement, destroy?: (el: HTMLElement) => void}} spec
 */
export function registerComponent(name, spec) {
  if (spec == null) REGISTRY.delete(name); else REGISTRY.set(name, spec);
}

/**
 * Registers a device capability plugin (`plugin(name, method, args)`) globally before `start()`.
 * Handler returns a JSON-serializable value or a Promise (docs/device-plugins.md).
 * @param {string} name
 * @param {((method: string, args: object) => any) | null} handler
 */
export function registerPlugin(name, handler) {
  if (handler == null) PLUGINS.delete(name); else PLUGINS.set(name, handler);
}

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
    remove(key) {
      localStorage.removeItem(prefix + key);
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
 * @param {{wasm: string, source: string, root: HTMLElement, server?: string, store?: object, seed?: number, devOverlay?: boolean, token?: string, user?: string, components?: object, plugins?: object}} opts
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
      tj_host_remove(kp, kl) {
        // A store passed in without `remove` keeps a blank record instead.
        if (!store.remove) return 1;
        try { store.remove(readStr(kp, kl)); return 0; } catch (e) { return 1; }
      },
    },
  };
  const bytes = await (await fetch(opts.wasm)).arrayBuffer();
  ex = (await WebAssembly.instantiate(bytes, imports)).instance.exports;

  const result = () => JSON.parse(readStr(ex.tj_out_ptr(), ex.tj_out_len()));
  const root = opts.root;
  let activeLocale = opts.locale || (typeof navigator !== 'undefined' && navigator.language) || 'en';
  const dirForLocale = (loc) => (RTL_LANGUAGES.indexOf(String(loc || '').split(/[-_]/)[0].toLowerCase()) >= 0 ? 'rtl' : 'ltr');
  // Right-to-left locales mirror the layout; the browser's flexbox follows `dir`.
  root.setAttribute('dir', opts.dir || document.documentElement.dir || dirForLocale(activeLocale));
  const nodes = new Map([[0, root]]);
  const components = new Map(REGISTRY);
  if (opts.components) {
    Object.keys(opts.components).forEach((k) => components.set(k, opts.components[k]));
  }
  // Per-instance plugin overrides only (storing `null` explicitly disables a global plugin on this instance).
  const plugins = new Map();
  if (opts.plugins) {
    Object.keys(opts.plugins).forEach((k) => {
      plugins.set(k, opts.plugins[k] == null ? null : opts.plugins[k]);
    });
  }
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
      // An object of a class streamed whole (`Slot/*`): the class's entry keeps the change
      // number, so a reconnect resumes and the stream address stays short.
      const whole = d.object.split('/')[0] + '/*';
      const key = replicas.has(whole) ? whole : d.object;
      if (!replicas.has(key)) return;
      replicas.set(key, Math.max(replicas.get(key), d.seq));
      send({ type: 'sync', object: d.object, state: d.state, seq: d.seq });
    });
    // Objects this user may not read (a replica kept from before they signed in as someone
    // else): stop asking for them.
    stream.addEventListener('refused', (e) => JSON.parse(e.data).objects.forEach((o) => replicas.delete(o)));
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
      user = newUser || null;
      roles = newRoles || [];
      // Another user or other roles: replicas start over, as the runtime's do.
      replicas.clear();
      if (events) events.close();
      events = null;
      send({ type: 'identity', user, roles });
    } else {
      clearTimeout(reconnect);
      reconnect = setTimeout(connectRealtime, 0);
    }
  }

  function subscribe(key, seq) {
    if (server === undefined || replicas.has(key)) return;
    // Already in a class stream: nothing new to ask for.
    if (key.slice(-2) !== '/*' && replicas.has(key.split('/')[0] + '/*')) return;
    if (key.slice(-2) === '/*') {
      // The class stream covers the objects of the class asked for one by one.
      const prefix = key.slice(0, -1);
      Array.from(replicas.keys()).forEach((k) => { if (k.indexOf(prefix) === 0) replicas.delete(k); });
    }
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
        if (r.status >= 502 || r.status === 401 || r.status === 429) return Promise.reject(new Error('server answered ' + r.status));
        return r.json().then((j) => {
          if (j && typeof j.error === 'string') return { error: j.error, field: j.field || undefined };
          if (r.status >= 200 && r.status < 300) return { ok: j ? j.ok : null };
          return Promise.reject(new Error('server answered ' + r.status));
        });
      })
      .then((j) => send(j.error !== undefined ? { type: 'effectDone', id, error: j.error, field: j.field } : { type: 'effectDone', id, ok: j.ok }))
      .catch((e) => send({ type: 'effectDone', id, error: String(e && e.message || e), unreachable: true }));
  }

  function runEffect(id, effect) {
    if (effect.kind === 'call') {
      serverCall(id, effect);
      return;
    }
    if (effect.kind === 'plugin') {
      const name = String(effect.plugin || '');
      const handler = plugins.has(name) ? plugins.get(name) : PLUGINS.get(name);
      if (typeof handler !== 'function') {
        Promise.resolve().then(() => send({ type: 'effectDone', id, error: 'no device plugin registered for ' + name }));
        return;
      }
      const rawArgs = effect.args;
      const argsCopy = rawArgs && typeof rawArgs === 'object' && !Array.isArray(rawArgs) ? Object.assign({}, rawArgs) : {};
      Promise.resolve()
        .then(() => handler(String(effect.method || ''), argsCopy))
        .then(
          (ok) => {
            let safeOk = ok === undefined ? null : ok;
            try {
              JSON.stringify(safeOk);
            } catch (e) {
              send({ type: 'effectDone', id, error: 'plugin returned non-JSON value: ' + String(e && e.message || e) });
              return;
            }
            send({ type: 'effectDone', id, ok: safeOk });
          },
          (e) => send({ type: 'effectDone', id, error: String(e && e.message || e) })
        );
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

  function renderTabs(el) {
    const items = Array.isArray(el.__tabs.items) ? el.__tabs.items : [];
    const current = el.__tabs.value === null || el.__tabs.value === undefined ? null : String(el.__tabs.value);
    while (el.firstChild) el.removeChild(el.firstChild);
    items.forEach((raw) => {
      const val = raw && typeof raw === 'object' ? String(raw.value !== undefined && raw.value !== null ? raw.value : (raw.label !== undefined && raw.label !== null ? raw.label : '')) : String(raw === null || raw === undefined ? '' : raw);
      const label = raw && typeof raw === 'object' ? String(raw.label !== undefined && raw.label !== null ? raw.label : val) : val;
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'tj-tab';
      btn.setAttribute('role', 'tab');
      btn.dataset.tjVal = val;
      btn.setAttribute('aria-selected', current !== null && val === current ? 'true' : 'false');
      btn.textContent = label;
      btn.addEventListener('click', () => {
        if (el.__tj && !el.__quiet && el.__tj.events.indexOf('onChange') >= 0) {
          send({ type: 'ui', target: { node: el.__tj.id }, event: 'onChange', value: val });
        }
      });
      el.appendChild(btn);
    });
  }

  function updateTabSelection(el) {
    const current = el.__tabs.value === null || el.__tabs.value === undefined ? null : String(el.__tabs.value);
    for (let i = 0; i < el.childNodes.length; i++) {
      const btn = el.childNodes[i];
      if (btn && btn.dataset) {
        btn.setAttribute('aria-selected', current !== null && btn.dataset.tjVal === current ? 'true' : 'false');
      }
    }
  }

  function renderIcon(el) {
    const name = el.__icon.name || '';
    const size = typeof el.__icon.size === 'number' && el.__icon.size > 0 ? el.__icon.size : 20;
    const d = Object.prototype.hasOwnProperty.call(ICONS, name) ? ICONS[name] : 'M12 5a7 7 0 1 0 0 14a7 7 0 1 0 0-14';
    const ns = 'http://www.w3.org/2000/svg';
    let svg = el.firstChild;
    if (!svg) {
      svg = document.createElementNS(ns, 'svg');
      svg.setAttribute('viewBox', '0 0 24 24');
      svg.setAttribute('fill', 'none');
      svg.setAttribute('stroke', 'currentColor');
      svg.setAttribute('stroke-width', '2');
      svg.setAttribute('stroke-linecap', 'round');
      svg.setAttribute('stroke-linejoin', 'round');
      svg.appendChild(document.createElementNS(ns, 'path'));
      el.appendChild(svg);
    }
    svg.setAttribute('width', String(size));
    svg.setAttribute('height', String(size));
    svg.firstChild.setAttribute('d', d);
    el.style.width = size + 'px';
    el.style.height = size + 'px';
  }

  function applyTheme(el) {
    const t = el.__theme;
    const dark = t.mode === 'dark' || (t.mode === 'system' && window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches);
    const mode = dark ? 'dark' : 'light';
    el.setAttribute('data-tj-theme', mode);
    el.style.colorScheme = t.mode ? mode : '';
    const surface = t.surface || (t.mode ? (dark ? '#111827' : '#ffffff') : '');
    const fg = t.color || (t.mode ? (dark ? '#f9fafb' : '#111827') : '');
    const muted = t.muted || (t.mode ? (dark ? '#9ca3af' : '#6b7280') : '');
    const border = t.mode ? (dark ? '#374151' : '#e5e7eb') : '';
    const setVar = (name, val) => { if (val) el.style.setProperty(name, val); else el.style.removeProperty(name); };
    setVar('--tj-surface', surface);
    setVar('--tj-fg', fg);
    setVar('--tj-accent', t.accent || '');
    setVar('--tj-muted', muted);
    setVar('--tj-border', border);
    setVar('--tj-radius', t.radius !== undefined ? px(t.radius) : '');
  }

  function create(id, kind, props) {
    let el;
    if (BUILTIN_KINDS.indexOf(kind) < 0) {
      const spec = components.get(kind);
      let creating = true;
      const emit = (event, value) => {
        if (!creating && el && el.__tj && !el.__quiet && el.__tj.events.indexOf(event) >= 0) {
          send({ type: 'ui', target: { node: id }, event, value: value === undefined ? null : value });
        }
      };
      if (spec && typeof spec.create === 'function') {
        el = spec.create(Object.assign({}, props), emit);
        creating = false;
        const container = typeof spec.container === 'function' ? spec.container(el) : null;
        if (container) {
          container.style.display = 'flex';
          container.style.flexDirection = 'column';
          container.style.boxSizing = 'border-box';
          container.style.minWidth = '0';
        }
        el.__native = { spec, props: Object.assign({}, props), container };
      } else {
        creating = false;
        el = document.createElement('div');
        el.className = 'tj-native-missing';
        el.setAttribute('data-tj-native', kind);
        el.textContent = '<' + kind + '>';
        el.__native = { spec: null, props: Object.assign({}, props), container: null };
      }
      return el;
    }
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
      case 'Time': el = document.createElement('span'); el.className = 'tj-text'; el.__time = {}; break;
      case 'Format': el = document.createElement('span'); el.className = 'tj-text'; el.__format = {}; break;
      case 'Tabs': el = document.createElement('div'); el.className = 'tj-tabs'; el.setAttribute('role', 'tablist'); el.__tabs = { items: [], value: null }; break;
      case 'Icon': el = document.createElement('span'); el.className = 'tj-icon'; el.setAttribute('aria-hidden', 'true'); el.__icon = { name: '', size: 20 }; break;
      case 'List': el = document.createElement('div'); el.className = 'tj-list'; el.__list = { horizontal: false, lastExtent: -1 }; break;
      case 'Theme': {
        el = document.createElement('div');
        el.className = 'tj-theme';
        el.__theme = {};
        if (window.matchMedia) {
          const mq = window.matchMedia('(prefers-color-scheme: dark)');
          const onChange = () => { if (el.__theme.mode === 'system') applyTheme(el); };
          if (mq.addEventListener) mq.addEventListener('change', onChange);
          else if (mq.addListener) mq.addListener(onChange);
          el.__themeCleanup = () => {
            if (mq.removeEventListener) mq.removeEventListener('change', onChange);
            else if (mq.removeListener) mq.removeListener(onChange);
          };
        }
        break;
      }
      default: el = document.createElement('div'); el.className = 'tj-' + kind.toLowerCase();
    }
    return el;
  }

  // ---------- times: the platform formats and parses them (docs/language.md, Time) ----------

  // A time as `YYYY-MM-DD HH:MM` in `zone` ("UTC", or the viewer's when absent), from the
  // platform's own calendar (no locale data needed).
  function isoParts(ms, zone) {
    const d = new Date(ms);
    const utc = zone === 'UTC';
    const two = (n) => (n < 10 ? '0' : '') + n;
    const year = utc ? d.getUTCFullYear() : d.getFullYear();
    const month = (utc ? d.getUTCMonth() : d.getMonth()) + 1;
    const day = utc ? d.getUTCDate() : d.getDate();
    const hour = utc ? d.getUTCHours() : d.getHours();
    const minute = utc ? d.getUTCMinutes() : d.getMinutes();
    return year + '-' + two(month) + '-' + two(day) + ' ' + two(hour) + ':' + two(minute);
  }

  // What a `<Time>` shows.
  function showTime(t) {
    if (typeof t.value !== 'number' || !isFinite(t.value)) return '';
    const show = t.show || 'datetime';
    if (t.style === 'iso') {
      const iso = isoParts(t.value, t.zone);
      return show === 'date' ? iso.slice(0, 10) : show === 'time' ? iso.slice(11) : iso;
    }
    // Component options, which browsers have had since 2015 (dateStyle came in 2020-21).
    const opts = { timeZone: t.zone || undefined };
    if (show !== 'time') { opts.year = 'numeric'; opts.month = 'short'; opts.day = 'numeric'; }
    if (show !== 'date') { opts.hour = 'numeric'; opts.minute = '2-digit'; }
    const loc = activeLocale && activeLocale.toLowerCase() !== 'pseudo' ? activeLocale : undefined;
    try {
      return new Intl.DateTimeFormat(loc, opts).format(new Date(t.value));
    } catch (e) {
      return new Intl.DateTimeFormat(undefined, opts).format(new Date(t.value));
    }
  }

  // What a `<Format>` shows (number, currency or percent in the active locale).
  function formatNumber(f) {
    if (typeof f.value !== 'number' || !isFinite(f.value)) return '';
    const rawLoc = f.locale || activeLocale || 'en';
    const loc = String(rawLoc).toLowerCase() === 'pseudo' ? 'en' : rawLoc;
    const kind = f.kind || 'number';
    const o = { style: kind === 'currency' ? 'currency' : kind === 'percent' ? 'percent' : 'decimal' };
    if (o.style === 'currency') o.currency = f.currency || 'USD';
    if (typeof f.decimals === 'number' && isFinite(f.decimals) && f.decimals >= 0) {
      const d = Math.min(20, Math.floor(f.decimals));
      o.minimumFractionDigits = d;
      o.maximumFractionDigits = d;
    }
    try {
      return new Intl.NumberFormat(loc, o).format(f.value);
    } catch (e) {
      return String(f.value);
    }
  }

  // A typed input's text as a value: milliseconds for dates and times (a time of day is
  // milliseconds since midnight), a number for numbers; null when it is not one. Agents
  // may send `2026-12-01 14:30` as well as the picker's `2026-12-01T14:30`.
  function typedValue(type, zone, text) {
    if (typeof text !== 'string') return text;
    const v = text.trim().replace(' ', 'T');
    if (v === '') return null;
    let ms = NaN;
    if (type === 'number') {
      ms = Number(v);
    } else if (type === 'time' || type === 'date' || type === 'datetime') {
      // Exactly the shape, as on Android and iOS: no seconds, offsets or trailing text.
      const length = { time: 5, date: 10, datetime: 16 }[type];
      if (v.length !== length) return null;
      ms = type === 'time' ? new Date('1970-01-01T' + v + 'Z').getTime() : new Date((type === 'date' ? v + 'T00:00' : v) + (zone === 'UTC' ? 'Z' : '')).getTime();
      // The platform reads 24:00 as the next midnight and rolls 2026-02-30 into March: a
      // value that does not show again as what was typed does not exist.
      if (isFinite(ms) && typedText(type, zone, ms) !== v) ms = NaN;
    } else {
      return text;
    }
    return isFinite(ms) ? ms : null;
  }

  // A value as a typed input's text.
  function typedText(type, zone, v) {
    if (typeof v !== 'number' || !isFinite(v)) return '';
    if (type === 'number') return String(v);
    if (type === 'time') return new Date(v).toISOString().slice(11, 16);
    const iso = isoParts(v, zone);
    return type === 'date' ? iso.slice(0, 10) : iso.replace(' ', 'T');
  }

  function setProp(el, kind, k, v) {
    const s = el.style;
    const none = v === null || v === undefined;
    if (el.__native) {
      // Custom native components own their custom props via spec.create/update; only apply shared layout/style/a11y props.
      const cs = (el.__native.container || el).style;
      switch (k) {
        case 'id': if (none) delete el.dataset.tjId; else el.dataset.tjId = v; break;
        case 'label': if (none) el.removeAttribute('aria-label'); else el.setAttribute('aria-label', v); break;
        case 'padding': cs.padding = none ? '' : px(v); break;
        case 'margin': s.margin = none ? '' : px(v); break;
        case 'gap': cs.gap = none ? '' : px(v); break;
        case 'align': cs.alignItems = none ? '' : (ALIGN[v] || v); break;
        case 'justify': cs.justifyContent = none ? '' : (JUSTIFY[v] || v); break;
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
      return;
    }
    if (kind === 'Input' && k === 'value') el.__value = v;
    if (kind === 'Time' && (k === 'value' || k === 'show' || k === 'style' || k === 'zone')) {
      el.__time[k] = none ? undefined : v;
      el.textContent = showTime(el.__time);
      return;
    }
    if (kind === 'Format' && (k === 'value' || k === 'kind' || k === 'currency' || k === 'decimals' || k === 'locale')) {
      el.__format[k] = none ? undefined : v;
      el.textContent = formatNumber(el.__format);
      return;
    }
    if (kind === 'Input' && (k === 'type' || k === 'zone')) {
      el['__' + k] = none ? undefined : v;
      if (k === 'type' && el.tagName === 'INPUT') el.type = { number: 'number', date: 'date', time: 'time', datetime: 'datetime-local' }[v] || 'text';
      if (el.__value !== undefined) el.value = typedText(el.__type, el.__zone, el.__value);
      return;
    }
    if (kind === 'Input' && k === 'value' && el.__type && el.__type !== 'text') {
      const text = typedText(el.__type, el.__zone, v);
      if (el.value !== text) el.value = text;
      return;
    }
    if (kind === 'Tabs' && (k === 'items' || k === 'value')) {
      el.__tabs[k] = none ? (k === 'items' ? [] : null) : v;
      return;
    }
    if (kind === 'Icon') {
      if (k === 'name' || k === 'size') {
        el.__icon[k] = none ? (k === 'size' ? 20 : '') : v;
        renderIcon(el);
        return;
      }
      if (k === 'label') {
        if (none) { el.removeAttribute('aria-label'); el.removeAttribute('role'); el.setAttribute('aria-hidden', 'true'); }
        else { el.setAttribute('aria-label', v); el.setAttribute('role', 'img'); el.removeAttribute('aria-hidden'); }
        return;
      }
    }
    if (kind === 'List') {
      if (k === 'horizontal') {
        el.__list.horizontal = !none && !!v;
        el.classList.toggle('tj-list-h', el.__list.horizontal);
        el.__list.lastExtent = -1;
        return;
      }
      if (k === 'itemHeight') {
        if (!none && typeof v === 'number' && v > 0) {
          el.style.setProperty('--tj-item-h', v + 'px');
          el.classList.add('tj-list-vh');
        } else {
          el.style.removeProperty('--tj-item-h');
          el.classList.remove('tj-list-vh');
        }
        return;
      }
    }
    if (kind === 'Theme' && ['mode', 'accent', 'surface', 'muted', 'color', 'radius'].indexOf(k) >= 0) {
      if (none) delete el.__theme[k]; else el.__theme[k] = v;
      applyTheme(el);
      if (k === 'mode' || k === 'accent' || k === 'surface' || k === 'muted') return;
    }
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
      const value = () => (el.__type && el.__type !== 'text' ? typedValue(el.__type, el.__zone, el.value) : el.value);
      el.addEventListener('input', () => fire('onChange', value()));
      el.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' && (el.tagName === 'INPUT' || e.ctrlKey || e.metaKey)) fire('onSubmit', value());
      });
    }
    if (kind === 'Toggle') el.firstChild.addEventListener('change', () => fire('onChange', el.firstChild.checked));
    if (kind === 'List') {
      el.addEventListener('scroll', () => {
        if (el.__tj.events.indexOf('onEndReached') < 0) return;
        const horiz = el.__list && el.__list.horizontal;
        const extent = horiz ? el.scrollWidth : el.scrollHeight;
        const view = horiz ? el.clientWidth : el.clientHeight;
        const offset = horiz ? Math.abs(el.scrollLeft) : el.scrollTop;
        if (extent < el.__list.lastExtent) el.__list.lastExtent = -1;
        if (extent > view && extent - offset - view <= 120 && el.__list.lastExtent !== extent) {
          el.__list.lastExtent = extent;
          fire('onEndReached', null);
        }
      });
    }
  }

  function applyProps(el, kind, props, isCreate) {
    const keys = Object.keys(props);
    el.__quiet = true;
    try {
      keys.forEach((k) => setProp(el, kind, k, props[k]));
      if (kind === 'Tabs') {
        if (isCreate || Object.prototype.hasOwnProperty.call(props, 'items')) renderTabs(el);
        else if (Object.prototype.hasOwnProperty.call(props, 'value')) updateTabSelection(el);
      }
      if (kind === 'Icon' && isCreate && !Object.prototype.hasOwnProperty.call(props, 'name') && !Object.prototype.hasOwnProperty.call(props, 'size')) {
        renderIcon(el);
      }
      if (!isCreate && el.__native) {
        keys.forEach((k) => {
          if (props[k] === null) delete el.__native.props[k];
          else el.__native.props[k] = props[k];
        });
        if (el.__native.spec && typeof el.__native.spec.update === 'function') {
          el.__native.spec.update(el, Object.assign({}, el.__native.props), Object.assign({}, props));
        }
      }
    } finally {
      el.__quiet = false;
    }
  }

  function patch(op) {
    switch (op.op) {
      case 'create': {
        const el = create(op.id, op.kind, op.props);
        el.__tj = { id: op.id, kind: op.kind, events: op.events };
        applyProps(el, op.kind, op.props, true);
        listen(el, op.id, op.kind);
        nodes.set(op.id, el);
        break;
      }
      case 'update': {
        const el = nodes.get(op.id);
        if (!el) break;
        if (op.events) el.__tj.events = op.events;
        applyProps(el, el.__tj.kind, op.props, false);
        break;
      }
      case 'children': {
        const rawParent = nodes.get(op.id);
        if (!rawParent) break;
        if (rawParent.__native && !rawParent.__native.container) break;
        const parent = rawParent.__native ? rawParent.__native.container : rawParent;
        if (rawParent.__list && op.ids.length < parent.childNodes.length) rawParent.__list.lastExtent = -1;
        op.ids.forEach((cid, i) => {
          const child = nodes.get(cid);
          if (parent.childNodes[i] !== child) parent.insertBefore(child, parent.childNodes[i] || null);
        });
        while (parent.childNodes.length > op.ids.length) parent.removeChild(parent.lastChild);
        break;
      }
      case 'remove': {
        const el = nodes.get(op.id);
        if (el && el.__themeCleanup) el.__themeCleanup();
        if (el && el.__native && el.__native.spec && typeof el.__native.spec.destroy === 'function') {
          el.__native.spec.destroy(el);
        }
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
        // A time or formatted number shows what the platform made of it, so agents read what people see.
        props: el && (el.__time || el.__format) ? Object.assign({}, n.props, { text: el.textContent }) : n.props,
        bounds: { x: Math.round(r.left), y: Math.round(r.top), w: Math.round(r.width), h: Math.round(r.height) },
        visible,
        children: withBounds(n.children),
      });
    });
  }

  // An agent types text into a date, time or number input as a person would: the input
  // turns it into its value, as it does for the picker.
  function forAgent(input) {
    if (input.type !== 'ui' || typeof input.value !== 'string' || !input.target) return input;
    let el = null;
    if (input.target.node !== undefined) el = nodes.get(input.target.node);
    else if (input.target.id !== undefined) nodes.forEach((n) => { if (n.dataset && n.dataset.tjId === input.target.id) el = n; });
    // Or the text an agent sees on it: its label or placeholder.
    else if (input.target.text !== undefined) nodes.forEach((n) => { if (n.getAttribute && (n.getAttribute('aria-label') === input.target.text || n.placeholder === input.target.text)) el = n; });
    if (el && el.__tj && el.__tj.kind === 'Input' && el.__type && el.__type !== 'text') input.value = typedValue(el.__type, el.__zone, input.value);
    return input;
  }

  function setLocale(locale) {
    activeLocale = locale || (typeof navigator !== 'undefined' && navigator.language) || 'en';
    if (!opts.dir) root.setAttribute('dir', dirForLocale(activeLocale));
    nodes.forEach((el) => {
      if (el.__format) el.textContent = formatNumber(el.__format);
      if (el.__time) el.textContent = showTime(el.__time);
    });
    send({ type: 'locale', locale: activeLocale });
  }

  const api = {
    /** Semantic tree with bounds and viewport visibility, plus object states. */
    inspect: () => request({ type: 'inspect' }).then((s) => Object.assign(s, { tree: withBounds(s.tree), viewport: { w: window.innerWidth, h: window.innerHeight } })),
    /** Fire a UI event. target: {id} | {text} | {node} */
    act: (target, event, value) => request(forAgent({ type: 'ui', target, event, value: value === undefined ? null : value })),
    call: (object, method, args) => request({ type: 'call', object, method, args: args || [] }),
    get: (object) => request({ type: 'get', object }),
    /** Any protocol input (docs/host-protocol.md): those that take a reply resolve with it. */
    input: (i) => {
      if (i && i.type === 'locale') { setLocale(i.locale); return Promise.resolve(null); }
      if (['call', 'get', 'ui', 'inspect'].indexOf(i.type) >= 0) return request(forAgent(Object.assign({}, i)));
      send(Object.assign({}, i));
      return Promise.resolve(null);
    },
    tick: () => send({ type: 'tick' }),
    /** The signed-in user's token and id; a new id drops what the previous user queued or saw. */
    setToken,
    /** Forgets the token, the replicas and every queued server call. */
    signOut: () => setToken(null, null, []),
    /** Switches the active BCP-47 locale (or 'pseudo') and re-renders translations & formatters. */
    setLocale,
    /** Registers a custom native component (`<Native name="..." />`) for this app instance. */
    registerComponent: (name, spec) => (spec == null ? components.delete(name) : components.set(name, spec)),
    /** Registers (or with null disables) a device capability plugin (`plugin(name, method, args)`) for this app instance. */
    registerPlugin: (name, handler) => plugins.set(name, handler == null ? null : handler),
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
  if (activeLocale && activeLocale.toLowerCase() !== 'en') send({ type: 'locale', locale: activeLocale });
  window.tinyjump = api;
  return api;
}
