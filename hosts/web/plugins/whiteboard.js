// Opt-in TinyJump Web collaborative drawing board `<Native name="Whiteboard" />` component (plain ES2017, zero deps).
// Includes built-in Pen/Eraser, color swatches, stroke sizes, Undo/Clear controls, and DPR canvas
// rendering for synchronized strokes (`BoardStroke.all(room + ":")`).

const MAX_STROKES = 500;
const MAX_COORDS_PER_STROKE = 240; // 120 (x, y) pairs
const DEFAULT_COLOR = '#111114';
const DEFAULT_WIDTH = 4;
const DEFAULT_BG = '#ffffff';
const DEFAULT_PALETTE = ['#111114', '#9a3412', '#0f766e', '#1d4ed8', '#7c3aed', '#dc2626'];
const SIZE_PRESETS = [
  { label: 'Fine', value: 2 },
  { label: 'Med', value: 4 },
  { label: 'Bold', value: 8 },
];
const HEX_COLOR_RE = /^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/;
const PEN_SVG = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z"/></svg>';
const ERASER_SVG = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m7 21-4.3-4.3c-1-1-1-2.5 0-3.4l9.6-9.6c1-1 2.5-1 3.4 0l5.6 5.6c1 1 1 2.5 0 3.4L13 21"/><path d="M22 21H7"/><path d="m5 11 9 9"/></svg>';

function sanitizeColor(c, fallback) {
  if (typeof c === 'string') {
    const s = c.trim();
    if (HEX_COLOR_RE.test(s)) return s;
  }
  return fallback;
}

function sanitizePalette(raw) {
  if (!Array.isArray(raw)) return DEFAULT_PALETTE;
  const out = [];
  for (let i = 0; i < raw.length && out.length < 8; i++) {
    const c = sanitizeColor(raw[i], '');
    if (c && out.indexOf(c) < 0) out.push(c);
  }
  return out.length > 0 ? out : DEFAULT_PALETTE;
}

function clampWidth(w) {
  const n = typeof w === 'number' && isFinite(w) ? Math.round(w) : DEFAULT_WIDTH;
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

function styleToolButton(btn) {
  btn.type = 'button';
  btn.style.display = 'inline-flex';
  btn.style.alignItems = 'center';
  btn.style.gap = '4px';
  btn.style.minHeight = '30px';
  btn.style.padding = '3px 9px';
  btn.style.borderRadius = '7px';
  btn.style.border = '1px solid #d4d0c8';
  btn.style.background = '#ffffff';
  btn.style.color = '#18181b';
  btn.style.fontSize = '12px';
  btn.style.fontWeight = '600';
  btn.style.cursor = 'pointer';
}

/**
 * Creates the `<Native name="Whiteboard" ... />` host component spec for `TinyJump`.
 */
export function createWhiteboardComponent() {
  return {
    create: function (initialProps, rawEmit) {
      const root = document.createElement('div');
      root.className = 'tj-whiteboard';
      root.setAttribute('role', 'region');
      root.setAttribute('aria-label', (initialProps && initialProps.label) || 'Collaborative drawing board');
      root.style.position = 'relative';
      root.style.display = 'flex';
      root.style.flexDirection = 'column';
      root.style.boxSizing = 'border-box';
      root.style.alignSelf = 'stretch';
      root.style.width = '100%';
      root.style.minHeight = '260px';
      root.style.height = '100%';
      root.style.overflow = 'hidden';
      root.style.borderRadius = '10px';
      root.style.border = '1px solid #d4d0c8';
      root.style.background = sanitizeColor(initialProps && initialProps.background, DEFAULT_BG);
      root.style.userSelect = 'none';
      root.style.fontFamily = 'system-ui, -apple-system, sans-serif';

      const toolbar = document.createElement('div');
      toolbar.className = 'tj-whiteboard-toolbar';
      toolbar.style.display = 'flex';
      toolbar.style.flexWrap = 'wrap';
      toolbar.style.alignItems = 'center';
      toolbar.style.gap = '6px';
      toolbar.style.padding = '7px 10px';
      toolbar.style.background = '#f4f2ec';
      toolbar.style.borderBottom = '1px solid #e4e2da';
      toolbar.style.flexShrink = '0';

      const penBtn = document.createElement('button');
      styleToolButton(penBtn);
      penBtn.innerHTML = PEN_SVG + '<span>Pen</span>';
      penBtn.setAttribute('aria-label', 'Pen tool');

      const eraserBtn = document.createElement('button');
      styleToolButton(eraserBtn);
      eraserBtn.innerHTML = ERASER_SVG + '<span>Eraser</span>';
      eraserBtn.setAttribute('aria-label', 'Eraser tool');

      const swatchRow = document.createElement('div');
      swatchRow.style.display = 'inline-flex';
      swatchRow.style.alignItems = 'center';
      swatchRow.style.gap = '5px';
      swatchRow.style.padding = '0 4px';

      const sizeRow = document.createElement('div');
      sizeRow.style.display = 'inline-flex';
      sizeRow.style.alignItems = 'center';
      sizeRow.style.gap = '4px';

      const actionsRow = document.createElement('div');
      actionsRow.style.display = 'inline-flex';
      actionsRow.style.alignItems = 'center';
      actionsRow.style.gap = '4px';
      actionsRow.style.marginLeft = 'auto';

      const undoBtn = document.createElement('button');
      styleToolButton(undoBtn);
      undoBtn.textContent = 'Undo';
      undoBtn.setAttribute('aria-label', 'Undo last stroke');

      const clearBtn = document.createElement('button');
      styleToolButton(clearBtn);
      clearBtn.textContent = 'Clear';
      clearBtn.setAttribute('aria-label', 'Clear drawing board');

      actionsRow.appendChild(undoBtn);
      actionsRow.appendChild(clearBtn);
      toolbar.appendChild(penBtn);
      toolbar.appendChild(eraserBtn);
      toolbar.appendChild(swatchRow);
      toolbar.appendChild(sizeRow);
      toolbar.appendChild(actionsRow);

      const stage = document.createElement('div');
      stage.className = 'tj-whiteboard-stage';
      stage.style.position = 'relative';
      stage.style.flex = '1';
      stage.style.minHeight = '200px';
      stage.style.width = '100%';
      stage.style.touchAction = 'none';

      const canvas = document.createElement('canvas');
      canvas.style.display = 'block';
      canvas.style.width = '100%';
      canvas.style.height = '100%';
      canvas.style.touchAction = 'none';

      const hintPill = document.createElement('div');
      hintPill.className = 'tj-whiteboard-hint';
      hintPill.style.position = 'absolute';
      hintPill.style.left = '50%';
      hintPill.style.top = '50%';
      hintPill.style.transform = 'translate(-50%, -50%)';
      hintPill.style.padding = '6px 12px';
      hintPill.style.borderRadius = '999px';
      hintPill.style.fontSize = '12px';
      hintPill.style.fontWeight = '500';
      hintPill.style.background = 'rgba(24,24,27,0.06)';
      hintPill.style.color = '#52525b';
      hintPill.style.pointerEvents = 'none';
      hintPill.style.whiteSpace = 'nowrap';

      stage.appendChild(canvas);
      stage.appendChild(hintPill);
      root.appendChild(toolbar);
      root.appendChild(stage);

      const initColor = sanitizeColor(initialProps && initialProps.strokeColor, DEFAULT_COLOR);
      const initWidth = clampWidth(initialProps && initialProps.strokeWidth);

      const st = {
        props: Object.assign({}, initialProps || {}),
        destroyed: false,
        tool: 'pen',
        activeColor: initColor,
        activeWidth: initWidth,
        lastPropColor: initialProps ? initialProps.strokeColor : undefined,
        lastPropWidth: initialProps ? initialProps.strokeWidth : undefined,
        paletteKey: '',
        strokes: [],
        draft: null,
        pointerId: null,
        cssW: 0,
        cssH: 0,
        dpr: 1,
        ro: null,
      };

      const swatchBtns = [];
      const sizeBtns = [];

      function emit(event, value) {
        Promise.resolve().then(function () {
          if (!st.destroyed) rawEmit(event, value);
        });
      }

      function hasEvent(name) {
        return Boolean(root.__tj && Array.isArray(root.__tj.events) && root.__tj.events.indexOf(name) >= 0);
      }

      function applyButtonState() {
        const ro = Boolean(st.props.readOnly);
        penBtn.disabled = ro;
        eraserBtn.disabled = ro;
        undoBtn.disabled = ro;
        clearBtn.disabled = ro;

        penBtn.style.borderColor = st.tool === 'pen' ? '#2563eb' : '#d4d0c8';
        penBtn.style.background = st.tool === 'pen' ? '#eff6ff' : '#ffffff';
        penBtn.style.color = st.tool === 'pen' ? '#1d4ed8' : '#18181b';
        penBtn.setAttribute('aria-pressed', st.tool === 'pen' ? 'true' : 'false');

        eraserBtn.style.borderColor = st.tool === 'eraser' ? '#2563eb' : '#d4d0c8';
        eraserBtn.style.background = st.tool === 'eraser' ? '#eff6ff' : '#ffffff';
        eraserBtn.style.color = st.tool === 'eraser' ? '#1d4ed8' : '#18181b';
        eraserBtn.setAttribute('aria-pressed', st.tool === 'eraser' ? 'true' : 'false');

        for (let i = 0; i < swatchBtns.length; i++) {
          const sb = swatchBtns[i];
          sb.disabled = ro;
          const active = st.tool === 'pen' && sb.__tjColor.toLowerCase() === st.activeColor.toLowerCase();
          sb.style.outline = active ? '2px solid #2563eb' : 'none';
          sb.style.outlineOffset = '2px';
          sb.setAttribute('aria-pressed', active ? 'true' : 'false');
        }

        for (let i = 0; i < sizeBtns.length; i++) {
          const zb = sizeBtns[i];
          zb.disabled = ro;
          const active = zb.__tjSize === st.activeWidth;
          zb.style.borderColor = active ? '#2563eb' : '#d4d0c8';
          zb.style.background = active ? '#eff6ff' : '#ffffff';
          zb.style.color = active ? '#1d4ed8' : '#18181b';
          zb.setAttribute('aria-pressed', active ? 'true' : 'false');
        }

        const showUndo = Boolean(st.props.showUndo || hasEvent('onUndo'));
        const showClear = Boolean(st.props.showClear || hasEvent('onClear'));
        undoBtn.style.display = showUndo ? 'inline-flex' : 'none';
        clearBtn.style.display = showClear ? 'inline-flex' : 'none';
        actionsRow.style.display = showUndo || showClear ? 'inline-flex' : 'none';
        canvas.style.cursor = ro ? 'not-allowed' : (st.tool === 'eraser' ? 'cell' : 'crosshair');
      }

      function rebuildPalette(palette) {
        const key = palette.join(',');
        if (key === st.paletteKey) return;
        st.paletteKey = key;
        while (swatchRow.firstChild) swatchRow.removeChild(swatchRow.firstChild);
        swatchBtns.length = 0;
        for (let i = 0; i < palette.length; i++) {
          const hex = palette[i];
          const b = document.createElement('button');
          b.type = 'button';
          b.__tjColor = hex;
          b.setAttribute('aria-label', 'Color ' + hex);
          b.title = hex;
          b.style.width = '22px';
          b.style.height = '22px';
          b.style.padding = '0';
          b.style.borderRadius = '999px';
          b.style.border = '1px solid rgba(0,0,0,0.22)';
          b.style.background = hex;
          b.style.cursor = 'pointer';
          b.addEventListener('click', function () {
            if (st.destroyed || st.props.readOnly) return;
            st.tool = 'pen';
            st.activeColor = hex;
            applyButtonState();
            updateHint();
            emit('onColorChange', hex);
          });
          swatchRow.appendChild(b);
          swatchBtns.push(b);
        }
      }

      for (let i = 0; i < SIZE_PRESETS.length; i++) {
        (function (preset) {
          const b = document.createElement('button');
          styleToolButton(b);
          b.__tjSize = preset.value;
          b.textContent = preset.label;
          b.setAttribute('aria-label', preset.label + ' stroke (' + preset.value + 'px)');
          b.addEventListener('click', function () {
            if (st.destroyed || st.props.readOnly) return;
            st.activeWidth = preset.value;
            applyButtonState();
            emit('onWidthChange', preset.value);
          });
          sizeRow.appendChild(b);
          sizeBtns.push(b);
        })(SIZE_PRESETS[i]);
      }

      penBtn.addEventListener('click', function () {
        if (st.destroyed || st.props.readOnly) return;
        st.tool = 'pen';
        applyButtonState();
      });

      eraserBtn.addEventListener('click', function () {
        if (st.destroyed || st.props.readOnly) return;
        st.tool = 'eraser';
        applyButtonState();
      });

      undoBtn.addEventListener('click', function () {
        if (st.destroyed || st.props.readOnly) return;
        emit('onUndo', null);
      });

      clearBtn.addEventListener('click', function () {
        if (st.destroyed || st.props.readOnly) return;
        emit('onClear', null);
      });

      function updateHint() {
        if (st.props.readOnly) {
          hintPill.textContent = (st.props.readOnlyHint && String(st.props.readOnlyHint)) || 'Read-only board';
          hintPill.style.display = 'block';
          return;
        }
        if (st.strokes.length === 0 && !st.draft) {
          hintPill.textContent = 'Click and drag to draw';
          hintPill.style.display = 'block';
        } else {
          hintPill.style.display = 'none';
        }
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
        const rect = stage.getBoundingClientRect();
        const w = Math.max(1, Math.round(rect.width || stage.clientWidth || root.clientWidth || 320));
        const h = Math.max(1, Math.round(rect.height || stage.clientHeight || 200));
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
        const bg = sanitizeColor(st.props.background, DEFAULT_BG);
        const erasing = st.tool === 'eraser';
        st.draft = {
          color: erasing ? bg : sanitizeColor(st.activeColor, DEFAULT_COLOR),
          width: erasing ? Math.max(12, Math.min(24, st.activeWidth * 3)) : clampWidth(st.activeWidth),
          points: [pt[0], pt[1]],
        };
        updateHint();
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
        updateHint();
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
        st.ro.observe(stage);
      }
      if (typeof window !== 'undefined' && typeof window.addEventListener === 'function') {
        window.addEventListener('resize', resizeCanvas);
      }

      function sync(nextProps) {
        if (st.destroyed) return;
        st.props = Object.assign({}, nextProps || {});
        if (st.props.label) root.setAttribute('aria-label', String(st.props.label));
        const bg = sanitizeColor(st.props.background, DEFAULT_BG);
        stage.style.background = bg;
        root.style.background = bg;
        if (!root.style.width) root.style.width = '100%';
        toolbar.style.display = st.props.controls === false ? 'none' : 'flex';

        if (st.props.strokeColor !== undefined && st.props.strokeColor !== st.lastPropColor) {
          st.lastPropColor = st.props.strokeColor;
          st.activeColor = sanitizeColor(st.props.strokeColor, st.activeColor);
        }
        if (st.props.strokeWidth !== undefined && st.props.strokeWidth !== st.lastPropWidth) {
          st.lastPropWidth = st.props.strokeWidth;
          st.activeWidth = clampWidth(st.props.strokeWidth);
        }

        rebuildPalette(sanitizePalette(st.props.palette));
        applyButtonState();

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
        updateHint();
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
