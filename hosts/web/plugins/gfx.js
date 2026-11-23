// Opt-in TinyJump Web 3D renderer (`<Scene>`, `<Mesh>`, `<Camera>`, `<Light>` + `@frame(dt)`).
// Plain ES2017, zero external dependencies, loaded ONLY when an app uses `<Scene>` or `@frame`.
// Uses WebGL2 (with WebGL1 / Canvas 2D software projection fallback on headless or legacy browsers).

const VS_SRC = [
  'attribute vec3 aPos;',
  'attribute vec3 aNorm;',
  'uniform mat4 uViewProj;',
  'uniform mat4 uModel;',
  'uniform mat4 uNormMat;',
  'varying vec3 vWorldPos;',
  'varying vec3 vWorldNorm;',
  'void main(){',
  '  vec4 w = uModel * vec4(aPos, 1.0);',
  '  vWorldPos = w.xyz;',
  '  vWorldNorm = normalize((uNormMat * vec4(aNorm, 0.0)).xyz);',
  '  gl_Position = uViewProj * w;',
  '}',
].join('\n');

const FS_SRC = [
  'precision mediump float;',
  'varying vec3 vWorldPos;',
  'varying vec3 vWorldNorm;',
  'uniform vec3 uCamPos;',
  'uniform vec3 uAmbient;',
  'uniform vec3 uBaseColor;',
  'uniform vec2 uMaterial;',
  'uniform int uLightCount;',
  'uniform vec4 uLightPosDir[8];',
  'uniform vec4 uLightColInt[8];',
  'void main(){',
  '  vec3 n = normalize(vWorldNorm);',
  '  vec3 v = normalize(uCamPos - vWorldPos);',
  '  float metallic = clamp(uMaterial.x, 0.0, 1.0);',
  '  float roughness = clamp(uMaterial.y, 0.05, 1.0);',
  '  float shininess = mix(96.0, 4.0, roughness * roughness);',
  '  vec3 specCol = mix(vec3(0.04), uBaseColor, metallic);',
  '  float diffW = 1.0 - metallic * 0.7;',
  '  vec3 col = uBaseColor * uAmbient;',
  '  for (int i = 0; i < 8; i++) {',
  '    if (i >= uLightCount) break;',
  '    vec4 lp = uLightPosDir[i];',
  '    vec4 lc = uLightColInt[i];',
  '    vec3 lDir;',
  '    float atten = 1.0;',
  '    if (lp.w > 0.5) {',
  '      vec3 d = lp.xyz - vWorldPos;',
  '      float dist = max(length(d), 0.001);',
  '      lDir = d / dist;',
  '      atten = 1.0 / (1.0 + 0.09 * dist + 0.032 * dist * dist);',
  '    } else {',
  '      lDir = normalize(-lp.xyz);',
  '    }',
  '    float ndotl = max(dot(n, lDir), 0.0);',
  '    if (ndotl > 0.0) {',
  '      vec3 h = normalize(lDir + v);',
  '      float spec = pow(max(dot(n, h), 0.0), shininess) * (1.0 - roughness * 0.6);',
  '      vec3 rad = lc.rgb * lc.w * atten;',
  '      col += (uBaseColor * diffW * ndotl + specCol * spec) * rad;',
  '    }',
  '  }',
  '  vec3 mapped = col / (col + vec3(1.0));',
  '  gl_FragColor = vec4(pow(mapped, vec3(1.0 / 2.2)), 1.0);',
  '}',
].join('\n');

function parseHex(str, fallback) {
  if (typeof str !== 'string') return fallback;
  const s = str.trim();
  if (s.charAt(0) !== '#') return fallback;
  const h = s.slice(1);
  let r, g, b;
  if (h.length === 3) {
    r = parseInt(h[0] + h[0], 16);
    g = parseInt(h[1] + h[1], 16);
    b = parseInt(h[2] + h[2], 16);
  } else if (h.length === 6 || h.length === 8) {
    r = parseInt(h.slice(0, 2), 16);
    g = parseInt(h.slice(2, 4), 16);
    b = parseInt(h.slice(4, 6), 16);
  } else {
    return fallback;
  }
  if (!isFinite(r) || !isFinite(g) || !isFinite(b)) return fallback;
  const lin = (u) => {
    const c = u / 255;
    return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
  };
  return [lin(r), lin(g), lin(b)];
}

function num(v, d) {
  return typeof v === 'number' && isFinite(v) ? v : d;
}

function vec3Prop(props, key, d) {
  const a = props[key];
  if (!Array.isArray(a)) return d.slice();
  return [num(a[0], d[0]), num(a[1], d[1]), num(a[2], d[2])];
}

function scaleProp(props) {
  if (typeof props.scale === 'number' && isFinite(props.scale)) {
    return [props.scale, props.scale, props.scale];
  }
  return vec3Prop(props, 'scale', [1, 1, 1]);
}

function vSub(a, b) { return [a[0] - b[0], a[1] - b[1], a[2] - b[2]]; }
function vAdd(a, b) { return [a[0] + b[0], a[1] + b[1], a[2] + b[2]]; }
function vDot(a, b) { return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]; }
function vCross(a, b) {
  return [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
}
function vLen(a) { return Math.sqrt(vDot(a, a)); }
function vNorm(a) {
  const l = vLen(a);
  return l > 1e-6 ? [a[0] / l, a[1] / l, a[2] / l] : [0, 1, 0];
}

function matIdent() {
  return new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
}

function matMul(a, b) {
  const o = new Float32Array(16);
  for (let c = 0; c < 4; c++) {
    for (let r = 0; r < 4; r++) {
      o[c * 4 + r] = a[r] * b[c * 4] + a[4 + r] * b[c * 4 + 1] + a[8 + r] * b[c * 4 + 2] + a[12 + r] * b[c * 4 + 3];
    }
  }
  return o;
}

function matTrs(p, rot, s) {
  const sx = Math.sin(rot[0]), cx = Math.cos(rot[0]);
  const sy = Math.sin(rot[1]), cy = Math.cos(rot[1]);
  const sz = Math.sin(rot[2]), cz = Math.cos(rot[2]);
  const rx = new Float32Array([1, 0, 0, 0, 0, cx, sx, 0, 0, -sx, cx, 0, 0, 0, 0, 1]);
  const ry = new Float32Array([cy, 0, -sy, 0, 0, 1, 0, 0, sy, 0, cy, 0, 0, 0, 0, 1]);
  const rz = new Float32Array([cz, sz, 0, 0, -sz, cz, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
  const sc = new Float32Array([s[0], 0, 0, 0, 0, s[1], 0, 0, 0, 0, s[2], 0, 0, 0, 0, 1]);
  const tr = new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, p[0], p[1], p[2], 1]);
  return matMul(tr, matMul(matMul(rz, matMul(ry, rx)), sc));
}

function matLookAt(eye, target, up) {
  const fwd = vSub(target, eye);
  const f = vLen(fwd) > 1e-6 ? vNorm(fwd) : [0, 0, -1];
  const u0 = vLen(up) > 1e-6 ? vNorm(up) : [0, 1, 0];
  let s = vCross(f, u0);
  if (vLen(s) <= 1e-6) s = vCross(f, [1, 0, 0]);
  s = vNorm(s);
  const u = vCross(s, f);
  return new Float32Array([
    s[0], u[0], -f[0], 0,
    s[1], u[1], -f[1], 0,
    s[2], u[2], -f[2], 0,
    -vDot(s, eye), -vDot(u, eye), vDot(f, eye), 1,
  ]);
}

function matPerspGL(fovRad, aspect, near, far) {
  const n = Math.max(1e-4, near);
  const fa = Math.max(n + 1e-3, far);
  const a = Math.max(1e-4, aspect);
  const f = 1 / Math.tan(Math.min(Math.PI - 0.05, Math.max(0.05, fovRad)) * 0.5);
  const nf = 1 / (n - fa);
  return new Float32Array([
    f / a, 0, 0, 0,
    0, f, 0, 0,
    0, 0, (fa + n) * nf, -1,
    0, 0, 2 * fa * n * nf, 0,
  ]);
}

function matOrthoGL(hh, aspect, near, far) {
  const h = Math.max(1e-3, hh);
  const w = h * Math.max(1e-4, aspect);
  const fa = Math.max(near + 1e-3, far);
  const nf = 1 / (near - fa);
  return new Float32Array([
    1 / w, 0, 0, 0,
    0, 1 / h, 0, 0,
    0, 0, 2 * nf, 0,
    0, 0, (fa + near) * nf, 1,
  ]);
}

function matNormal(m) {
  const a = m[0], b = m[4], c = m[8];
  const d = m[1], e = m[5], f = m[9];
  const g = m[2], h = m[6], i = m[10];
  const c00 = e * i - f * h, c01 = -(d * i - f * g), c02 = d * h - e * g;
  const c10 = -(b * i - c * h), c11 = a * i - c * g, c12 = -(a * h - b * g);
  const c20 = b * f - c * e, c21 = -(a * f - c * d), c22 = a * e - b * d;
  const det = a * c00 + b * c01 + c * c02;
  if (Math.abs(det) <= 1e-8) return matIdent();
  const inv = 1 / det;
  return new Float32Array([
    c00 * inv, c10 * inv, c20 * inv, 0,
    c01 * inv, c11 * inv, c21 * inv, 0,
    c02 * inv, c12 * inv, c22 * inv, 0,
    0, 0, 0, 1,
  ]);
}

function matInvert(m) {
  const a = [];
  for (let r = 0; r < 4; r++) {
    a[r] = [m[r], m[4 + r], m[8 + r], m[12 + r], r === 0 ? 1 : 0, r === 1 ? 1 : 0, r === 2 ? 1 : 0, r === 3 ? 1 : 0];
  }
  for (let col = 0; col < 4; col++) {
    let piv = col;
    let mx = Math.abs(a[col][col]);
    for (let r = col + 1; r < 4; r++) {
      if (Math.abs(a[r][col]) > mx) { mx = Math.abs(a[r][col]); piv = r; }
    }
    if (mx <= 1e-8) return null;
    if (piv !== col) { const t = a[col]; a[col] = a[piv]; a[piv] = t; }
    const inv = 1 / a[col][col];
    for (let k = 0; k < 8; k++) a[col][k] *= inv;
    for (let r = 0; r < 4; r++) {
      if (r === col) continue;
      const fac = a[r][col];
      for (let k = 0; k < 8; k++) a[r][k] -= fac * a[col][k];
    }
  }
  const o = new Float32Array(16);
  for (let r = 0; r < 4; r++) {
    for (let c = 0; c < 4; c++) o[c * 4 + r] = a[r][c + 4];
  }
  return o;
}

function transformPoint(m, p) {
  const x = m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12];
  const y = m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13];
  const z = m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14];
  const w = m[3] * p[0] + m[7] * p[1] + m[11] * p[2] + m[15];
  const iw = Math.abs(w) > 1e-6 ? 1 / w : 1;
  return [x * iw, y * iw, z * iw];
}

function intersectUnitBox(o, d) {
  for (let i = 0; i < 3; i++) {
    if (!isFinite(o[i]) || !isFinite(d[i])) return null;
  }
  let tmin = 0, tmax = Infinity;
  for (let i = 0; i < 3; i++) {
    if (Math.abs(d[i]) <= 1e-7) {
      if (o[i] < -0.5 || o[i] > 0.5) return null;
    } else {
      let t1 = (-0.5 - o[i]) / d[i];
      let t2 = (0.5 - o[i]) / d[i];
      if (!isFinite(t1) || !isFinite(t2)) return null;
      if (t1 > t2) { const tmp = t1; t1 = t2; t2 = tmp; }
      tmin = Math.max(tmin, t1);
      tmax = Math.min(tmax, t2);
      if (tmin > tmax) return null;
    }
  }
  return tmax >= 0 ? tmin : null;
}

function intersectUnitPlane(o, d) {
  for (let i = 0; i < 3; i++) {
    if (!isFinite(o[i]) || !isFinite(d[i])) return null;
  }
  if (Math.abs(d[1]) <= 1e-7) return null;
  const t = -o[1] / d[1];
  if (!isFinite(t) || t < 0) return null;
  const x = o[0] + d[0] * t;
  const z = o[2] + d[2] * t;
  return x >= -0.5 && x <= 0.5 && z >= -0.5 && z <= 0.5 ? t : null;
}

function buildGeometry(kind) {
  const v = [];
  const idx = [];
  const pushQuad = (pts, n) => {
    const base = v.length / 6;
    for (let i = 0; i < 4; i++) v.push(pts[i][0], pts[i][1], pts[i][2], n[0], n[1], n[2]);
    idx.push(base, base + 1, base + 2, base, base + 2, base + 3);
  };
  const h = 0.5;
  if (kind === 'plane') {
    pushQuad([[-h, 0, h], [h, 0, h], [h, 0, -h], [-h, 0, -h]], [0, 1, 0]);
    pushQuad([[-h, 0, -h], [h, 0, -h], [h, 0, h], [-h, 0, h]], [0, -1, 0]);
  } else if (kind === 'sphere') {
    const sec = 20, stk = 14;
    for (let st = 0; st <= stk; st++) {
      const phi = (st / stk) * Math.PI;
      const sp = Math.sin(phi), cp = Math.cos(phi);
      for (let se = 0; se <= sec; se++) {
        const th = (se / sec) * Math.PI * 2;
        const nx = sp * Math.cos(th), ny = cp, nz = sp * Math.sin(th);
        v.push(nx * h, ny * h, nz * h, nx, ny, nz);
      }
    }
    const stride = sec + 1;
    for (let st = 0; st < stk; st++) {
      for (let se = 0; se < sec; se++) {
        const a = st * stride + se, b = a + stride;
        idx.push(a, a + 1, b, a + 1, b + 1, b);
      }
    }
  } else if (kind === 'pyramid') {
    const apex = [0, h, 0];
    const c = [[-h, -h, h], [h, -h, h], [h, -h, -h], [-h, -h, -h]];
    for (let s = 0; s < 4; s++) {
      const p0 = c[s], p1 = c[(s + 1) % 4];
      const n = vNorm(vCross(vSub(p1, p0), vSub(apex, p0)));
      const base = v.length / 6;
      v.push(p0[0], p0[1], p0[2], n[0], n[1], n[2]);
      v.push(p1[0], p1[1], p1[2], n[0], n[1], n[2]);
      v.push(apex[0], apex[1], apex[2], n[0], n[1], n[2]);
      idx.push(base, base + 1, base + 2);
    }
    pushQuad([[-h, -h, -h], [h, -h, -h], [h, -h, h], [-h, -h, h]], [0, -1, 0]);
  } else if (kind === 'cylinder') {
    const seg = 20;
    for (let s = 0; s < seg; s++) {
      const a0 = (s / seg) * Math.PI * 2, a1 = ((s + 1) / seg) * Math.PI * 2;
      const c0 = Math.cos(a0), s0 = Math.sin(a0), c1 = Math.cos(a1), s1 = Math.sin(a1);
      const base = v.length / 6;
      v.push(c0 * h, -h, s0 * h, c0, 0, s0);
      v.push(c1 * h, -h, s1 * h, c1, 0, s1);
      v.push(c1 * h, h, s1 * h, c1, 0, s1);
      v.push(c0 * h, h, s0 * h, c0, 0, s0);
      idx.push(base, base + 2, base + 1, base, base + 3, base + 2);
    }
    const tc = v.length / 6;
    v.push(0, h, 0, 0, 1, 0);
    for (let s = 0; s < seg; s++) {
      const a = (s / seg) * Math.PI * 2;
      v.push(Math.cos(a) * h, h, Math.sin(a) * h, 0, 1, 0);
    }
    for (let s = 0; s < seg; s++) idx.push(tc, tc + 1 + ((s + 1) % seg), tc + 1 + s);
    const bc = v.length / 6;
    v.push(0, -h, 0, 0, -1, 0);
    for (let s = 0; s < seg; s++) {
      const a = (s / seg) * Math.PI * 2;
      v.push(Math.cos(a) * h, -h, Math.sin(a) * h, 0, -1, 0);
    }
    for (let s = 0; s < seg; s++) idx.push(bc, bc + 1 + s, bc + 1 + ((s + 1) % seg));
  } else {
    pushQuad([[-h, -h, h], [h, -h, h], [h, h, h], [-h, h, h]], [0, 0, 1]);
    pushQuad([[h, -h, -h], [-h, -h, -h], [-h, h, -h], [h, h, -h]], [0, 0, -1]);
    pushQuad([[h, -h, h], [h, -h, -h], [h, h, -h], [h, h, h]], [1, 0, 0]);
    pushQuad([[-h, -h, -h], [-h, -h, h], [-h, h, h], [-h, h, -h]], [-1, 0, 0]);
    pushQuad([[-h, h, h], [h, h, h], [h, h, -h], [-h, h, -h]], [0, 1, 0]);
    pushQuad([[-h, -h, -h], [h, -h, -h], [h, -h, h], [-h, -h, h]], [0, -1, 0]);
  }
  return { vertices: new Float32Array(v), indices: new Uint16Array(idx) };
}

function initGL(canvas, antialias) {
  const opts = { antialias: antialias !== false, preserveDrawingBuffer: true };
  const gl = canvas.getContext('webgl2', opts) || canvas.getContext('webgl', opts);
  if (!gl) return null;
  const compile = (type, src) => {
    const sh = gl.createShader(type);
    gl.shaderSource(sh, src);
    gl.compileShader(sh);
    if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS)) return null;
    return sh;
  };
  const vs = compile(gl.VERTEX_SHADER, VS_SRC);
  const fs = compile(gl.FRAGMENT_SHADER, FS_SRC);
  if (!vs || !fs) return null;
  const prog = gl.createProgram();
  gl.attachShader(prog, vs);
  gl.attachShader(prog, fs);
  gl.linkProgram(prog);
  if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) return null;

  const geos = {};
  ['box', 'sphere', 'plane', 'pyramid', 'cylinder'].forEach((name) => {
    const g = buildGeometry(name);
    const vb = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, vb);
    gl.bufferData(gl.ARRAY_BUFFER, g.vertices, gl.STATIC_DRAW);
    const ib = gl.createBuffer();
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, ib);
    gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, g.indices, gl.STATIC_DRAW);
    // Edge index buffer (i0,i1, i1,i2, i2,i0) so wireframe mode draws complete triangle edges.
    const wire = new Uint16Array(g.indices.length * 2);
    for (let i = 0, w = 0; i + 2 < g.indices.length; i += 3) {
      const a = g.indices[i], b = g.indices[i + 1], c = g.indices[i + 2];
      wire[w++] = a; wire[w++] = b;
      wire[w++] = b; wire[w++] = c;
      wire[w++] = c; wire[w++] = a;
    }
    const wireIb = gl.createBuffer();
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, wireIb);
    gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, wire, gl.STATIC_DRAW);
    geos[name] = { vb, ib, count: g.indices.length, wireIb, wireCount: wire.length };
  });

  return {
    gl,
    vs,
    fs,
    prog,
    geos,
    aPos: gl.getAttribLocation(prog, 'aPos'),
    aNorm: gl.getAttribLocation(prog, 'aNorm'),
    uViewProj: gl.getUniformLocation(prog, 'uViewProj'),
    uModel: gl.getUniformLocation(prog, 'uModel'),
    uNormMat: gl.getUniformLocation(prog, 'uNormMat'),
    uCamPos: gl.getUniformLocation(prog, 'uCamPos'),
    uAmbient: gl.getUniformLocation(prog, 'uAmbient'),
    uBaseColor: gl.getUniformLocation(prog, 'uBaseColor'),
    uMaterial: gl.getUniformLocation(prog, 'uMaterial'),
    uLightCount: gl.getUniformLocation(prog, 'uLightCount'),
    uLightPosDir: gl.getUniformLocation(prog, 'uLightPosDir'),
    uLightColInt: gl.getUniformLocation(prog, 'uLightColInt'),
  };
}

/**
 * Creates the opt-in TinyJump Web 3D renderer plugin (`start({ ..., gfx: createGfxPlugin({ animate }) })`).
 * @param {{ animate?: boolean }} [opts]
 */
export function createGfxPlugin(opts) {
  const animate = !!(opts && opts.animate);
  const nodes = new Map();
  const scenes = new Map(); // sceneId -> { canvas, glState, ctx2d, cleanupResize }
  let sendFn = null;
  let rafId = null;
  let lastTs = null;
  let dirty = false;

  function resolveScene(sceneId, w, h) {
    const s = nodes.get(sceneId);
    if (!s || s.kind !== 'Scene') return null;
    const p = s.props;
    const clear = parseHex(p.clear || p.background, [0.009, 0.015, 0.032]);
    const amb = Math.max(0, Math.min(2, num(p.ambient, 0.25)));
    const ambient = [amb, amb, amb];
    let camPos = [0, 2, 5], camTarget = [0, 0, 0], camUp = [0, 1, 0], fov = 45, near = 0.1, far = 100, ortho = false, sawCam = false;
    const lights = [];
    let sawLight = false;
    const meshes = [];

    const walkMesh = (id, parentMat, depth) => {
      if (depth > 64) return;
      const n = nodes.get(id);
      if (!n || n.kind !== 'Mesh' || n.props.visible === false) return;
      const world = matMul(parentMat, matTrs(vec3Prop(n.props, 'position', [0, 0, 0]), vec3Prop(n.props, 'rotation', [0, 0, 0]), scaleProp(n.props)));
      const geoRaw = typeof n.props.geometry === 'string' ? n.props.geometry.toLowerCase() : 'box';
      const geo = ['box', 'sphere', 'plane', 'pyramid', 'cylinder'].indexOf(geoRaw) >= 0 ? geoRaw : 'box';
      meshes.push({
        id,
        geo,
        world,
        color: parseHex(n.props.color, [0.6, 0.7, 0.85]),
        rawColor: typeof n.props.color === 'string' ? n.props.color : '#94a3b8',
        metallic: num(n.props.metallic, 0.1),
        roughness: num(n.props.roughness, 0.55),
        wireframe: !!n.props.wireframe,
        clickable: n.events.indexOf('onPress') >= 0,
      });
      for (let i = 0; i < n.children.length; i++) walkMesh(n.children[i], world, depth + 1);
    };

    for (let i = 0; i < s.children.length; i++) {
      const c = nodes.get(s.children[i]);
      if (!c) continue;
      if (c.kind === 'Camera' && !sawCam) {
        sawCam = true;
        camPos = vec3Prop(c.props, 'position', camPos);
        camTarget = vec3Prop(c.props, 'target', camTarget);
        camUp = vec3Prop(c.props, 'up', camUp);
        fov = num(c.props.fov, fov);
        near = num(c.props.near, near);
        far = num(c.props.far, far);
        ortho = c.props.projection === 'orthographic';
      } else if (c.kind === 'Light') {
        sawLight = true;
        const lkind = c.props.kind || 'directional';
        const col = parseHex(c.props.color, [1, 1, 1]);
        const intensity = Math.max(0, Math.min(20, num(c.props.intensity, 1.0)));
        if (lkind === 'ambient') {
          ambient[0] += col[0] * intensity;
          ambient[1] += col[1] * intensity;
          ambient[2] += col[2] * intensity;
        } else if (lights.length < 8) {
          const isPoint = lkind === 'point';
          const vec = isPoint ? vec3Prop(c.props, 'position', [2, 4, 3]) : vNorm(vec3Prop(c.props, 'direction', [-0.5, -1, -0.6]));
          lights.push({ posDir: [vec[0], vec[1], vec[2], isPoint ? 1 : 0], colInt: [col[0], col[1], col[2], intensity] });
        }
      } else if (c.kind === 'Mesh') {
        walkMesh(c.id, matIdent(), 0);
      }
    }

    if (!sawLight) {
      const d = vNorm([-0.5, -1, -0.6]);
      lights.push({ posDir: [d[0], d[1], d[2], 0], colInt: [1, 1, 1, 1.1] });
    }
    const aspect = Math.max(1, w) / Math.max(1, h);
    const view = matLookAt(camPos, camTarget, camUp);
    const proj = ortho ? matOrthoGL(Math.max(1, vLen(vSub(camTarget, camPos))) * 0.5, aspect, near, far) : matPerspGL((fov * Math.PI) / 180, aspect, near, far);
    return { clear, rawClear: s.props.clear || s.props.background || '#0f172a', ambient, camPos, viewProj: matMul(proj, view), lights, meshes };
  }

  function renderOne(sceneId, entry) {
    const canvas = entry.canvas;
    const rect = canvas.getBoundingClientRect();
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    const cssW = Math.max(1, Math.round(rect.width || canvas.clientWidth || 300));
    const cssH = Math.max(1, Math.round(rect.height || canvas.clientHeight || 200));
    const pw = Math.round(cssW * dpr);
    const ph = Math.round(cssH * dpr);
    if (canvas.width !== pw || canvas.height !== ph) {
      canvas.width = pw;
      canvas.height = ph;
    }
    const scene = resolveScene(sceneId, cssW, cssH);
    if (!scene) return;

    if (entry.glState) {
      const g = entry.glState;
      const gl = g.gl;
      gl.viewport(0, 0, pw, ph);
      gl.enable(gl.DEPTH_TEST);
      gl.depthFunc(gl.LESS);
      const toSrgb = (c) => Math.pow(Math.max(0, Math.min(1, c)), 1 / 2.2);
      gl.clearColor(toSrgb(scene.clear[0]), toSrgb(scene.clear[1]), toSrgb(scene.clear[2]), 1);
      gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
      gl.useProgram(g.prog);
      gl.uniformMatrix4fv(g.uViewProj, false, scene.viewProj);
      gl.uniform3fv(g.uCamPos, scene.camPos);
      gl.uniform3fv(g.uAmbient, scene.ambient);
      gl.uniform1i(g.uLightCount, scene.lights.length);
      const lp = new Float32Array(32);
      const lc = new Float32Array(32);
      for (let i = 0; i < scene.lights.length; i++) {
        lp.set(scene.lights[i].posDir, i * 4);
        lc.set(scene.lights[i].colInt, i * 4);
      }
      gl.uniform4fv(g.uLightPosDir, lp);
      gl.uniform4fv(g.uLightColInt, lc);

      for (let i = 0; i < scene.meshes.length; i++) {
        const m = scene.meshes[i];
        const geo = g.geos[m.geo] || g.geos.box;
        gl.uniformMatrix4fv(g.uModel, false, m.world);
        gl.uniformMatrix4fv(g.uNormMat, false, matNormal(m.world));
        gl.uniform3fv(g.uBaseColor, m.color);
        gl.uniform2f(g.uMaterial, m.metallic, m.roughness);
        gl.bindBuffer(gl.ARRAY_BUFFER, geo.vb);
        gl.enableVertexAttribArray(g.aPos);
        gl.vertexAttribPointer(g.aPos, 3, gl.FLOAT, false, 24, 0);
        gl.enableVertexAttribArray(g.aNorm);
        gl.vertexAttribPointer(g.aNorm, 3, gl.FLOAT, false, 24, 12);
        if (m.wireframe) {
          gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, geo.wireIb);
          gl.drawElements(gl.LINES, geo.wireCount, gl.UNSIGNED_SHORT, 0);
        } else {
          gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, geo.ib);
          gl.drawElements(gl.TRIANGLES, geo.count, gl.UNSIGNED_SHORT, 0);
        }
      }
      return;
    }

    // Fallback Canvas 2D software projection when WebGL is unavailable (e.g. headless browser).
    if (entry.ctx2d) {
      const ctx = entry.ctx2d;
      ctx.fillStyle = scene.rawClear;
      ctx.fillRect(0, 0, pw, ph);
      for (let i = 0; i < scene.meshes.length; i++) {
        const m = scene.meshes[i];
        const wp = transformPoint(m.world, [0, 0, 0]);
        const vp = scene.viewProj;
        const cw = vp[3] * wp[0] + vp[7] * wp[1] + vp[11] * wp[2] + vp[15];
        if (cw <= 1e-4) continue;
        const cx = (vp[0] * wp[0] + vp[4] * wp[1] + vp[8] * wp[2] + vp[12]) / cw;
        const cy = (vp[1] * wp[0] + vp[5] * wp[1] + vp[9] * wp[2] + vp[13]) / cw;
        const cz = (vp[2] * wp[0] + vp[6] * wp[1] + vp[10] * wp[2] + vp[14]) / cw;
        if (cz < -1 || cz > 1) continue;
        const sx = (cx * 0.5 + 0.5) * pw;
        const sy = (0.5 - cy * 0.5) * ph;
        ctx.fillStyle = m.rawColor;
        ctx.beginPath();
        ctx.arc(sx, sy, Math.max(8, Math.min(pw, ph) * 0.08), 0, Math.PI * 2);
        ctx.fill();
      }
    }
  }

  function renderAll() {
    dirty = false;
    scenes.forEach((entry, id) => renderOne(id, entry));
  }

  function pickMesh(sceneId, clientX, clientY, canvas) {
    const rect = canvas.getBoundingClientRect();
    const w = Math.max(1, rect.width);
    const h = Math.max(1, rect.height);
    const x = clientX - rect.left;
    const y = clientY - rect.top;
    if (!isFinite(x) || !isFinite(y)) return null;
    const scene = resolveScene(sceneId, w, h);
    if (!scene) return null;
    const invVp = matInvert(scene.viewProj);
    if (!invVp) return null;
    const ndcX = (2 * x) / w - 1;
    const ndcY = 1 - (2 * y) / h;
    const nearW = transformPoint(invVp, [ndcX, ndcY, -1]);
    const farW = transformPoint(invVp, [ndcX, ndcY, 1]);
    const dirW = vNorm(vSub(farW, nearW));
    let bestT = Infinity, bestId = null, bestClickable = false;
    for (let i = 0; i < scene.meshes.length; i++) {
      const m = scene.meshes[i];
      const invM = matInvert(m.world);
      if (!invM) continue;
      const lo = transformPoint(invM, nearW);
      const lf = transformPoint(invM, vAdd(nearW, dirW));
      const ld = vSub(lf, lo);
      const t = m.geo === 'plane' ? intersectUnitPlane(lo, ld) : intersectUnitBox(lo, ld);
      if (t !== null && t < bestT) {
        bestT = t;
        bestId = m.id;
        bestClickable = m.clickable;
      }
    }
    return bestClickable ? bestId : null;
  }

  function cleanupScene(id) {
    const entry = scenes.get(id);
    if (!entry) return;
    scenes.delete(id);
    if (entry.cleanupResize) entry.cleanupResize();
    if (entry.glState) {
      const g = entry.glState;
      const gl = g.gl;
      Object.keys(g.geos).forEach((k) => {
        gl.deleteBuffer(g.geos[k].vb);
        gl.deleteBuffer(g.geos[k].ib);
        gl.deleteBuffer(g.geos[k].wireIb);
      });
      gl.deleteProgram(g.prog);
      gl.deleteShader(g.vs);
      gl.deleteShader(g.fs);
      const ext = gl.getExtension('WEBGL_lose_context');
      if (ext) ext.loseContext();
    }
  }

  function tickLoop(ts) {
    rafId = null;
    if (!sendFn || !animate) return;
    if (!document.hidden) {
      const dt = lastTs === null ? 0.016 : Math.min(0.1, Math.max(0, (ts - lastTs) / 1000));
      lastTs = ts;
      sendFn({ type: 'frame', dt });
      if (dirty) renderAll();
    } else {
      lastTs = null;
    }
    if (animate && typeof requestAnimationFrame === 'function') {
      rafId = requestAnimationFrame(tickLoop);
    }
  }

  function ensureLoop() {
    if (animate && sendFn && rafId === null && typeof requestAnimationFrame === 'function') {
      lastTs = null;
      rafId = requestAnimationFrame(tickLoop);
    }
  }

  return {
    attach(send) {
      sendFn = send;
      ensureLoop();
    },
    createScene(id, props, emit) {
      const canvas = document.createElement('canvas');
      canvas.className = 'tj-scene';
      canvas.style.display = 'block';
      if (props.height === undefined || props.height === null) canvas.style.minHeight = '160px';
      canvas.style.width = '100%';
      canvas.style.touchAction = 'none';
      const glState = initGL(canvas, props.antialias);
      const ctx2d = glState ? null : canvas.getContext('2d');
      let cleanupResize = null;
      const onResize = () => {
        const cur = scenes.get(id);
        if (cur) renderOne(id, cur);
      };
      if (typeof ResizeObserver === 'function') {
        const ro = new ResizeObserver(onResize);
        ro.observe(canvas);
        cleanupResize = () => ro.disconnect();
      } else {
        window.addEventListener('resize', onResize);
        cleanupResize = () => window.removeEventListener('resize', onResize);
      }
      const entry = { canvas, glState, ctx2d, cleanupResize };
      scenes.set(id, entry);

      const pointerPayload = (e) => {
        const r = canvas.getBoundingClientRect();
        return { x: Math.round(e.clientX - r.left), y: Math.round(e.clientY - r.top), w: Math.round(r.width), h: Math.round(r.height) };
      };
      canvas.addEventListener('pointerdown', (e) => {
        emit('onPointerDown', pointerPayload(e));
        const hit = pickMesh(id, e.clientX, e.clientY, canvas);
        if (hit !== null && sendFn) {
          sendFn({ type: 'ui', target: { node: hit }, event: 'onPress', value: null });
        }
      });
      canvas.addEventListener('pointermove', (e) => emit('onPointerMove', pointerPayload(e)));
      canvas.addEventListener('pointerup', (e) => emit('onPointerUp', pointerPayload(e)));

      ensureLoop();
      return canvas;
    },
    destroyScene(id) {
      cleanupScene(id);
    },
    onPatch(op) {
      if (op.op === 'create') {
        nodes.set(op.id, { id: op.id, kind: op.kind, props: Object.assign({}, op.props), events: (op.events || []).slice(), children: [] });
        dirty = true;
      } else if (op.op === 'update') {
        const n = nodes.get(op.id);
        if (n) {
          Object.keys(op.props || {}).forEach((k) => {
            if (op.props[k] === null) delete n.props[k];
            else n.props[k] = op.props[k];
          });
          if (op.events) n.events = op.events.slice();
          dirty = true;
        }
      } else if (op.op === 'children') {
        const n = nodes.get(op.id);
        if (n) {
          n.children = (op.ids || []).slice();
          dirty = true;
        }
      } else if (op.op === 'remove') {
        nodes.delete(op.id);
        cleanupScene(op.id);
        dirty = true;
      }
    },
    flush() {
      if (dirty) renderAll();
    },
  };
}
