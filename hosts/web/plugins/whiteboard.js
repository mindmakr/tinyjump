// Opt-in TinyJump Web collaborative drawing board `<Native name="Whiteboard" />` component (plain ES2017, zero deps).
// Renders synchronized strokes from a @server topic (`BoardStroke.all(room + ":")`) and emits
// normalized `0..1000` coordinate strokes via `onStroke`.

const MAX_STROKES = 500;
const MAX_COORDS_PER_STROKE = 240; // 120 (x, y) pairs
const DEFAULT_COLOR = '#d9480f';
const DEFAULT_BG = '#ffffff';
const HEX_COLOR_RE = /^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/;

function sanitizeColor(c, fallback) {
  if (typeof c === 'string') {
    const s = c.trim();
    if (HEX_COLOR_RE.test(s)) return s;
  }
  return fallback;
}

function clampWidth(w) {
  const n = typeof w === 'number' && isFinite(w) ? Math.round(w) : 3;
  return Math.max(1, Math.min(24, n));
}

function clampCoord(v) {
  const n = typeof v === 'number' && isFinite(v) ? Math.round(v) : 0;
  return Math.max(0, Math.min(1000, n));
}

function sanitizePoints(raw) {
  if (!Array.isArray(raw) || raw.length < 2) return [];
  const even = raw.length - (raw.length % 2);
  const capped = Math.min(even, 400);
  const out = [];
  for (let i = 0; i < capped; i += 2) {
    out.push(clampCoord(raw[i]), clampCoord(raw[i + 1]));
  }
  return out;
}

function downsamplePoints(pts) {
  if (pts.length <= MAX_COORDS_PER_STROKE) return pts;
  const pairs = Math.floor(pts.length / 2);
  const maxPairs = Math.floor(MAX_COORDS_PER_STROKE / 2);
  const out = [pts[0], pts[1]];
  const span = pairs - 1;
  for (let k = 1; k < maxPairs - 1; k++) {
    const idx = Math.round((k * span) / (maxPairs - 1)) * 2;
    out.push(pts[idx], pts[idx + 1]);
  }
  out.push(pts[pts.length - 2], pts[pts.length - 1]);
  return out;
}

function drawStroke(ctx, w, h, color, width, pts) {
  if (!pts || pts.length < 2) return;
  ctx.strokeStyle = color;
  ctx.fillStyle = color;
  ctx.lineWidth = width;
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';
  const x0 = (pts[0] / 1000) * w;
  const y0 = (pts[1] / 1000) * h;
  if (pts.length === 2 || (pts.length === 4 && pts[0] === pts[2] && pts[1] === pts[3])) {
    ctx.beginPath();
    ctx.arc(x0, y0, width * 0.5, 0, Math.PI * 2);
    ctx.fill();
    return;
  }
  ctx.beginPath();
  ctx.moveTo(x0, y0);
  for (let i = 2; i < pts.length - 2; i += 2) {
    const cx = (pts[i] / 1000) * w;
    const cy = (pts[i + 1] / 1000) * h;
    const nx = (pts[i + 2] / 1000) * w;
    const ny = (pts[i + 3] / 1000) * h;
    ctx.quadraticCurveTo(cx, cy, (cx + nx) * 0.5, (cy + ny) * 0.5);
  }
  ctx.lineTo((pts[pts.length - 2] / 1000) * w, (pts[pts.length - 1] / 1000) * h);
  ctx.stroke();
}

/**
 * Creates the `<Native name="Whiteboard" ... />` host component spec for `TinyJump`.
 */
export function createWhiteboardComponent() {
  return {
    create: function (initialProps, rawEmit) {
      const root = document.createElement('div');
      root.className = 'tj-whiteboard';
      root.setAttribute('role', 'img');
      root.setAttribute('aria-label', (initialProps && initialProps.label) || 'Collaborative drawing board');
      root.style.position = 'relative';
      root.style.width = '100%';
      root.style.minHeight = '240px';
      root.style.height = '100%';
      root.style.overflow = 'hidden';
      root.style.borderRadius = '10px';
      root.style.border = '1px solid #d4d0c8';
      root.style.background = sanitizeColor(initialProps && initialProps.background, DEFAULT_BG);
      root.style.touchAction = 'none';
      root.style.userSelect = 'none';

      const canvas = document.createElement('canvas');
      canvas.style.display = 'block';
      canvas.style.width = '100%';
      canvas.style.height = '100%';
      canvas.style.touchAction = 'none';
      root.appendChild(canvas);

      const st = {
        props: Object.assign({}, initialProps || {}),
        destroyed: false,
        strokes: [],
        draft: null,
        pointerId: null,
        cssW: 0,
        cssH: 0,
        dpr: 1,
        ro: null,
      };

      function emit(event, value) {
        Promise.resolve().then(function () {
          if (!st.destroyed) rawEmit(event, value);
        });
      }

      function redraw() {
        const w = st.cssW;
        const h = st.cssH;
        if (w <= 0 || h <= 0) return;
        const ctx = canvas.getContext && canvas.getContext('2d');
        if (!ctx) return;
        ctx.setTransform(st.dpr, 0, 0, st.dpr, 0, 0);
        ctx.fillStyle = sanitizeColor(st.props.background, DEFAULT_BG);
        ctx.fillRect(0, 0, w, h);
        for (let i = 0; i < st.strokes.length; i++) {
          const s = st.strokes[i];
          drawStroke(ctx, w, h, s.color, s.width, s.points);
        }
        if (st.draft) {
          drawStroke(ctx, w, h, st.draft.color, st.draft.width, st.draft.points);
        }
      }

      function resizeCanvas() {
        if (st.destroyed) return;
        const rect = root.getBoundingClientRect();
        const w = Math.max(1, Math.round(rect.width || root.clientWidth || 320));
        const h = Math.max(1, Math.round(rect.height || root.clientHeight || 240));
        const dpr = Math.min(3, Math.max(1, (typeof window !== 'undefined' && window.devicePixelRatio) || 1));
        if (w !== st.cssW || h !== st.cssH || dpr !== st.dpr) {
          st.cssW = w;
          st.cssH = h;
          st.dpr = dpr;
          canvas.width = Math.round(w * dpr);
          canvas.height = Math.round(h * dpr);
        }
        redraw();
      }

      function toNormalized(ev) {
        const rect = canvas.getBoundingClientRect();
        const w = rect.width || st.cssW || 1;
        const h = rect.height || st.cssH || 1;
        const nx = clampCoord(((ev.clientX - rect.left) / w) * 1000);
        const ny = clampCoord(((ev.clientY - rect.top) / h) * 1000);
        return [nx, ny];
      }

      function onPointerDown(ev) {
        if (st.destroyed || st.props.readOnly) return;
        if (typeof ev.button === 'number' && ev.button !== 0 && ev.pointerType === 'mouse') return;
        if (st.pointerId !== null) return;
        st.pointerId = ev.pointerId;
        if (typeof canvas.setPointerCapture === 'function') {
          try { canvas.setPointerCapture(ev.pointerId); } catch (e) { /* ignore */ }
        }
        const pt = toNormalized(ev);
        st.draft = {
          color: sanitizeColor(st.props.color, DEFAULT_COLOR),
          width: clampWidth(st.props.width),
          points: [pt[0], pt[1]],
        };
        resizeCanvas();
      }

      function onPointerMove(ev) {
        if (st.destroyed || !st.draft || ev.pointerId !== st.pointerId) return;
        const pt = toNormalized(ev);
        const pts = st.draft.points;
        const dx = pt[0] - pts[pts.length - 2];
        const dy = pt[1] - pts[pts.length - 1];
        if (dx * dx + dy * dy < 16) return;
        pts.push(pt[0], pt[1]);
        redraw();
      }

      function finishPointer(ev, commit) {
        if (!st.draft || ev.pointerId !== st.pointerId) return;
        if (typeof canvas.releasePointerCapture === 'function') {
          try { canvas.releasePointerCapture(ev.pointerId); } catch (e) { /* ignore */ }
        }
        st.pointerId = null;
        const draft = st.draft;
        st.draft = null;
        if (commit && !st.props.readOnly) {
          let pts = downsamplePoints(draft.points);
          if (pts.length === 2) pts = [pts[0], pts[1], pts[0], pts[1]];
          emit('onStroke', {
            color: draft.color,
            width: draft.width,
            points: pts,
          });
        }
        redraw();
      }

      function onPointerUp(ev) { finishPointer(ev, true); }
      function onPointerCancel(ev) { finishPointer(ev, false); }

      canvas.addEventListener('pointerdown', onPointerDown);
      canvas.addEventListener('pointermove', onPointerMove);
      canvas.addEventListener('pointerup', onPointerUp);
      canvas.addEventListener('pointercancel', onPointerCancel);

      if (typeof ResizeObserver === 'function') {
        st.ro = new ResizeObserver(function () { resizeCanvas(); });
        st.ro.observe(root);
      }
      if (typeof window !== 'undefined' && typeof window.addEventListener === 'function') {
        window.addEventListener('resize', resizeCanvas);
      }

      function sync(nextProps) {
        if (st.destroyed) return;
        st.props = Object.assign({}, nextProps || {});
        if (st.props.label) root.setAttribute('aria-label', String(st.props.label));
        root.style.background = sanitizeColor(st.props.background, DEFAULT_BG);
        canvas.style.cursor = st.props.readOnly ? 'default' : 'crosshair';
        const raw = Array.isArray(st.props.strokes) ? st.props.strokes : [];
        const start = Math.max(0, raw.length - MAX_STROKES);
        const cleaned = [];
        for (let i = start; i < raw.length; i++) {
          const item = raw[i];
          if (!item || typeof item !== 'object') continue;
          const pts = sanitizePoints(item.points);
          if (pts.length < 2) continue;
          cleaned.push({
            color: sanitizeColor(item.color, DEFAULT_COLOR),
            width: clampWidth(item.width),
            points: pts,
          });
        }
        st.strokes = cleaned;
        resizeCanvas();
      }

      root.__tjWhiteboard = {
        sync: sync,
        destroy: function () {
          st.destroyed = true;
          canvas.removeEventListener('pointerdown', onPointerDown);
          canvas.removeEventListener('pointermove', onPointerMove);
          canvas.removeEventListener('pointerup', onPointerUp);
          canvas.removeEventListener('pointercancel', onPointerCancel);
          if (st.ro) {
            try { st.ro.disconnect(); } catch (e) { /* ignore */ }
            st.ro = null;
          }
          if (typeof window !== 'undefined' && typeof window.removeEventListener === 'function') {
            window.removeEventListener('resize', resizeCanvas);
          }
        },
      };

      Promise.resolve().then(function () {
        if (!st.destroyed) sync(st.props);
      });

      return root;
    },

    update: function (el, props) {
      if (el && el.__tjWhiteboard) el.__tjWhiteboard.sync(props);
    },

    destroy: function (el) {
      if (el && el.__tjWhiteboard) {
        el.__tjWhiteboard.destroy();
        el.__tjWhiteboard = null;
      }
    },
  };
}

/**
 * Returns `{ Whiteboard: spec }` ready for `TinyJump.start({ components })`.
 */
export function createWhiteboardComponents() {
  return { Whiteboard: createWhiteboardComponent() };
}

/**
 * Registers `<Native name="Whiteboard" ... />` on a `registerComponent(name, spec)` function.
 */
export function registerWhiteboard(registerComponentFn) {
  registerComponentFn('Whiteboard', createWhiteboardComponent());
}
