// Opt-in TinyJump Web device capability plugins (plain ES2017, zero dependencies).
// Bundled by `tj build --web` and loaded by `tj dev` only when an app declares
// matching device permissions (`files`, `notifications`, `location`, `camera`).

const DEFAULT_MAX_FILE_BYTES = 10 * 1024 * 1024;
const HARD_MAX_FILE_BYTES = 10 * 1024 * 1024;
const MAX_RAW_CAMERA_BYTES = 50 * 1024 * 1024;
const MAX_BASE64_CHARS = Math.ceil(HARD_MAX_FILE_BYTES / 3) * 4 + 4;
let nextNotifId = 1;
let activeFileInput = false;

function bytesToBase64(bytes) {
  let binary = '';
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    const slice = bytes.subarray(i, Math.min(i + chunk, bytes.length));
    let part = '';
    for (let j = 0; j < slice.length; j++) part += String.fromCharCode(slice[j]);
    binary += part;
  }
  return btoa(binary);
}

function base64ToBytes(b64) {
  const clean = String(b64).replace(/\s+/g, '');
  if (clean.length > MAX_BASE64_CHARS) {
    throw new Error('file exceeds maximum size limit');
  }
  let binary;
  try {
    binary = atob(clean);
  } catch (e) {
    throw new Error('invalid base64 payload');
  }
  if (binary.length > HARD_MAX_FILE_BYTES) {
    throw new Error('file exceeds maximum size limit');
  }
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}

function sanitizeFilename(name) {
  if (typeof name !== 'string') throw new Error('files.save requires a string "name"');
  const trimmed = name.trim();
  if (!trimmed || trimmed === '.' || trimmed === '..') {
    throw new Error('files.save requires a valid filename');
  }
  const leaf = trimmed
    .replace(/[\\/]+/g, '_')
    .replace(/\.\.+/g, '_')
    .replace(/[\x00-\x1f\x7f]/g, '')
    .trim();
  if (!leaf || leaf === '.' || leaf === '..' || /^_+$/.test(leaf)) {
    throw new Error('files.save requires a valid filename');
  }
  return leaf.slice(0, 180);
}

function clampInt(val, fallback, min, max) {
  if (val === undefined || val === null) return fallback;
  if (typeof val !== 'number' || !Number.isFinite(val)) return fallback;
  const n = Math.floor(val);
  if (n < min) return min;
  if (n > max) return max;
  return n;
}

function clampFloat(val, fallback, min, max) {
  if (val === undefined || val === null) return fallback;
  if (typeof val !== 'number' || !Number.isFinite(val)) return fallback;
  if (val < min) return min;
  if (val > max) return max;
  return val;
}

function readBlobAsText(blob) {
  return new Promise(function (resolve, reject) {
    const reader = new FileReader();
    reader.onload = function () { resolve(String(reader.result || '')); };
    reader.onerror = function () { reject(new Error('failed to read file')); };
    reader.readAsText(blob);
  });
}

function readBlobAsBytes(blob) {
  return new Promise(function (resolve, reject) {
    const reader = new FileReader();
    reader.onload = function () { resolve(new Uint8Array(reader.result)); };
    reader.onerror = function () { reject(new Error('failed to read file')); };
    reader.readAsArrayBuffer(blob);
  });
}

function openFileInput(attrs) {
  return new Promise(function (resolve, reject) {
    if (typeof document === 'undefined' || !document.body) {
      reject(new Error('file picker requires a browser document'));
      return;
    }
    const input = document.createElement('input');
    input.type = 'file';
    input.style.position = 'fixed';
    input.style.left = '-9999px';
    input.style.top = '-9999px';
    input.style.opacity = '0';
    if (attrs.accept) input.accept = String(attrs.accept);
    if (attrs.multiple) input.multiple = true;
    if (attrs.capture) input.setAttribute('capture', String(attrs.capture));

    let settled = false;
    let sawBlur = false;
    let focusTimer = null;

    function finish(files) {
      if (settled) return;
      settled = true;
      if (focusTimer !== null) {
        clearTimeout(focusTimer);
        focusTimer = null;
      }
      window.removeEventListener('blur', onBlur, false);
      window.removeEventListener('focus', onFocus, false);
      if (input.parentNode) input.parentNode.removeChild(input);
      resolve(files);
    }

    function onBlur() {
      sawBlur = true;
    }

    function onFocus() {
      if (!sawBlur || settled) return;
      // Fallback for older browsers without the `cancel` event on <input type="file">.
      if (focusTimer !== null) clearTimeout(focusTimer);
      focusTimer = setTimeout(function () {
        focusTimer = null;
        if (!settled) finish([]);
      }, 400);
    }

    input.addEventListener('change', function () {
      const list = input.files ? Array.prototype.slice.call(input.files) : [];
      finish(list);
    });
    input.addEventListener('cancel', function () {
      finish([]);
    });
    window.addEventListener('blur', onBlur, false);
    window.addEventListener('focus', onFocus, false);
    document.body.appendChild(input);
    input.click();
  });
}

async function handleFiles(method, args) {
  const a = args || {};
  if (method === 'pick') {
    if (activeFileInput) throw new Error('a file or camera picker is already open');
    const readAs = a.readAs === undefined || a.readAs === null ? 'text' : String(a.readAs);
    if (readAs !== 'text' && readAs !== 'base64') {
      throw new Error('files.pick "readAs" must be "text" or "base64"');
    }
    const maxBytes = clampInt(a.maxBytes, DEFAULT_MAX_FILE_BYTES, 1, HARD_MAX_FILE_BYTES);
    activeFileInput = true;
    let picked;
    try {
      picked = await openFileInput({
        accept: typeof a.accept === 'string' ? a.accept : '',
        multiple: Boolean(a.multiple),
      });
    } finally {
      activeFileInput = false;
    }
    if (!picked.length) return { cancelled: true, files: [] };
    const out = [];
    let totalBytes = 0;
    for (let i = 0; i < picked.length; i++) {
      const f = picked[i];
      totalBytes += f.size;
      if (f.size > maxBytes || totalBytes > maxBytes) {
        throw new Error('file exceeds maximum size limit');
      }
      const entry = {
        name: f.name || 'file',
        mime: f.type || 'application/octet-stream',
        size: f.size,
      };
      if (readAs === 'base64') {
        entry.base64 = bytesToBase64(await readBlobAsBytes(f));
      } else {
        entry.text = await readBlobAsText(f);
      }
      out.push(entry);
    }
    return { cancelled: false, files: out };
  }

  if (method === 'save') {
    const name = sanitizeFilename(a.name);
    const hasText = typeof a.text === 'string';
    const hasBase64 = typeof a.base64 === 'string';
    if (!hasText && !hasBase64) {
      throw new Error('files.save requires either "text" or "base64"');
    }
    if (hasText && hasBase64) {
      throw new Error('files.save accepts either "text" or "base64", not both');
    }
    let blob;
    if (hasBase64) {
      const bytes = base64ToBytes(a.base64);
      const mime = typeof a.mime === 'string' && a.mime ? a.mime : 'application/octet-stream';
      blob = new Blob([bytes], { type: mime });
    } else {
      const mime = typeof a.mime === 'string' && a.mime ? a.mime : 'text/plain;charset=utf-8';
      blob = new Blob([a.text], { type: mime });
      if (blob.size > HARD_MAX_FILE_BYTES) {
        throw new Error('file exceeds maximum size limit');
      }
    }
    if (typeof document === 'undefined' || !document.body || typeof URL === 'undefined' || !URL.createObjectURL) {
      throw new Error('file saving requires a browser document');
    }
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = name;
    link.style.display = 'none';
    document.body.appendChild(link);
    link.click();
    document.body.removeChild(link);
    setTimeout(function () { URL.revokeObjectURL(url); }, 1000);
    return { saved: true, cancelled: false, name: name };
  }

  throw new Error('unknown files method: ' + method);
}

function requestWebNotificationPermission() {
  return new Promise(function (resolve, reject) {
    let settled = false;
    function done(status) {
      if (settled) return;
      settled = true;
      resolve(String(status));
    }
    try {
      // Supports both Promise-returning browsers and 2019-era Safari callback-only API.
      const maybe = window.Notification.requestPermission(done);
      if (maybe && typeof maybe.then === 'function') {
        maybe.then(done, function (err) {
          if (!settled) { settled = true; reject(err); }
        });
      }
    } catch (err) {
      if (!settled) { settled = true; reject(err); }
    }
  });
}

async function handleNotifications(method, args) {
  const a = args || {};
  const supported = typeof window !== 'undefined' && typeof window.Notification === 'function';
  if (method === 'requestPermission') {
    if (!supported) return { granted: false, status: 'unsupported' };
    const status = await requestWebNotificationPermission();
    return { granted: status === 'granted', status: status };
  }
  if (method === 'notify') {
    if (!supported) throw new Error('notifications are not supported in this browser');
    if (window.Notification.permission !== 'granted') {
      throw new Error('notification permission not granted');
    }
    const title = typeof a.title === 'string' ? a.title.trim() : '';
    if (!title) throw new Error('notifications.notify requires a non-empty "title"');
    if (title.length > 256) throw new Error('notifications.notify "title" is too long');
    const body = typeof a.body === 'string' ? a.body.slice(0, 2048) : '';
    const id = typeof a.id === 'string' && a.id.trim() ? a.id.trim().slice(0, 128) : ('tj-' + (nextNotifId++));
    new window.Notification(title, { body: body, tag: id });
    return { delivered: true, id: id };
  }
  throw new Error('unknown notifications method: ' + method);
}

function handleLocation(method, args) {
  const a = args || {};
  if (method !== 'currentPosition') {
    throw new Error('unknown location method: ' + method);
  }
  if (typeof navigator === 'undefined' || !navigator.geolocation) {
    throw new Error('geolocation is not supported in this browser');
  }
  const highAccuracy = Boolean(a.highAccuracy);
  const timeoutMs = clampInt(a.timeoutMs, 15000, 1000, 60000);
  return new Promise(function (resolve, reject) {
    navigator.geolocation.getCurrentPosition(
      function (pos) {
        const c = pos.coords || {};
        if (typeof c.latitude !== 'number' || !Number.isFinite(c.latitude) ||
            typeof c.longitude !== 'number' || !Number.isFinite(c.longitude)) {
          reject(new Error('location returned non-finite coordinates'));
          return;
        }
        resolve({
          lat: c.latitude,
          lng: c.longitude,
          accuracy: typeof c.accuracy === 'number' && Number.isFinite(c.accuracy) && c.accuracy >= 0 ? c.accuracy : 0,
          altitude: typeof c.altitude === 'number' && Number.isFinite(c.altitude) ? c.altitude : null,
          timestamp: typeof pos.timestamp === 'number' && Number.isFinite(pos.timestamp) ? pos.timestamp : Date.now(),
        });
      },
      function (err) {
        reject(new Error((err && err.message) || 'location unavailable'));
      },
      { enableHighAccuracy: highAccuracy, timeout: timeoutMs, maximumAge: 0 }
    );
  });
}

function encodeImageBlob(file, maxDimension, quality) {
  return new Promise(function (resolve, reject) {
    if (typeof document === 'undefined' || typeof URL === 'undefined' || !URL.createObjectURL) {
      reject(new Error('camera capture requires a browser document'));
      return;
    }
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = function () {
      URL.revokeObjectURL(url);
      let w = img.naturalWidth || img.width || 0;
      let h = img.naturalHeight || img.height || 0;
      if (w <= 0 || h <= 0) {
        reject(new Error('captured image has invalid dimensions'));
        return;
      }
      if (w > maxDimension || h > maxDimension) {
        const scale = Math.min(maxDimension / w, maxDimension / h);
        w = Math.max(1, Math.round(w * scale));
        h = Math.max(1, Math.round(h * scale));
      }
      const canvas = document.createElement('canvas');
      canvas.width = w;
      canvas.height = h;
      const ctx = canvas.getContext('2d');
      if (!ctx) {
        reject(new Error('2D canvas unavailable'));
        return;
      }
      ctx.drawImage(img, 0, 0, w, h);
      const dataUrl = canvas.toDataURL('image/jpeg', quality);
      const comma = dataUrl.indexOf(',');
      if (comma < 0) {
        reject(new Error('failed to encode captured image'));
        return;
      }
      const b64 = dataUrl.slice(comma + 1);
      if (b64.length > MAX_BASE64_CHARS) {
        reject(new Error('captured image exceeds maximum size limit'));
        return;
      }
      resolve({
        cancelled: false,
        mime: 'image/jpeg',
        width: w,
        height: h,
        base64: b64,
      });
    };
    img.onerror = function () {
      URL.revokeObjectURL(url);
      reject(new Error('failed to decode captured image'));
    };
    img.src = url;
  });
}

async function handleCamera(method, args) {
  const a = args || {};
  if (method !== 'capture') {
    throw new Error('unknown camera method: ' + method);
  }
  if (activeFileInput) throw new Error('a file or camera picker is already open');
  const facing = a.facing === undefined || a.facing === null ? 'back' : String(a.facing);
  if (facing !== 'back' && facing !== 'front') {
    throw new Error('camera.capture "facing" must be "back" or "front"');
  }
  const maxDimension = clampInt(a.maxDimension, 1280, 64, 4096);
  const quality = clampFloat(a.quality, 0.85, 0.1, 1.0);
  activeFileInput = true;
  let picked;
  try {
    picked = await openFileInput({
      accept: 'image/*',
      multiple: false,
      capture: facing === 'front' ? 'user' : 'environment',
    });
  } finally {
    activeFileInput = false;
  }
  if (!picked.length) {
    return { cancelled: true, mime: '', width: 0, height: 0, base64: '' };
  }
  if (picked[0].size > MAX_RAW_CAMERA_BYTES) {
    throw new Error('captured image exceeds maximum size limit');
  }
  return encodeImageBlob(picked[0], maxDimension, quality);
}

const HANDLERS = {
  files: handleFiles,
  notifications: handleNotifications,
  location: handleLocation,
  camera: handleCamera,
};

/**
 * Returns a `{ [pluginName]: handler }` map for `TinyJump.start({ plugins })`,
 * containing ONLY the requested opt-in capabilities.
 */
export function createDevicePlugins(capabilities) {
  const out = {};
  const list = Array.isArray(capabilities) ? capabilities : [];
  for (let i = 0; i < list.length; i++) {
    const cap = list[i] === 'location-background' ? 'location' : list[i];
    if (Object.prototype.hasOwnProperty.call(HANDLERS, cap)) {
      out[cap] = HANDLERS[cap];
    }
  }
  return out;
}

/**
 * Registers the requested opt-in capabilities onto a `registerPlugin(name, fn)` function.
 */
export function registerDevicePlugins(registerFn, capabilities) {
  const map = createDevicePlugins(capabilities);
  const names = Object.keys(map);
  for (let i = 0; i < names.length; i++) {
    registerFn(names[i], map[names[i]]);
  }
}
