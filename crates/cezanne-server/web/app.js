// Cezanne Scan web interface. The server owns the scanner, the prescan and the finished
// files. This page keeps the settings and the frames (saved in the browser), so a reload
// or a second visit carries on where it left off.
'use strict';

const $ = (id) => document.getElementById(id);
const STORE = 'cezanne-web-v1';

// ------------------------------------------------------------------ settings and frames

const S = {
  connKind: 'Simulator', device: '', bridgeHost: '', bridgeToken: '',
  mode: 'Rgb48', dpi: 2000, prescanDpi: 75, film: 'ColorNegative',
  prefix: 'scan_', sixteen: true, autoDownload: true,
  areaUnit: 'TenthMm', resScale: 10,
};
let frames = []; // { name, area: {x_mm, y_mm, width_mm, height_mm}, adj, auto_levels }
let selected = -1;
let meta = null;
let server = null; // last /api/state answer
let logSeq = 0;
let freshBrowser = true; // nothing saved yet, so take the server's connection
let picker = null; // 'hl' | 'sd'
let myScanFrom = null; // file id this browser's scan started after, for automatic downloads
const downloaded = new Set();

function load() {
  try {
    const v = JSON.parse(localStorage.getItem(STORE) || '{}');
    if (v.settings) freshBrowser = false;
    Object.assign(S, v.settings || {});
    frames = Array.isArray(v.frames) ? v.frames : [];
    selected = Number.isInteger(v.selected) && v.selected < frames.length ? v.selected : -1;
    (v.downloaded || []).forEach((k) => downloaded.add(k));
  } catch (e) { /* storage blocked or corrupt, start fresh */ }
}

let saveTimer = 0;
function save() {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    try {
      localStorage.setItem(STORE, JSON.stringify({ settings: S, frames, selected, downloaded: [...downloaded].slice(-500) }));
    } catch (e) { /* private window */ }
  }, 200);
}

const clone = (o) => JSON.parse(JSON.stringify(o));
const film = (id) => meta.films.find((f) => f.id === id) || meta.films[0];
const modeInfo = (id) => meta.modes.find((m) => m.id === id) || meta.modes[0];
const frame = () => frames[selected];

// ------------------------------------------------------------------ server calls

async function api(path, opts = {}) {
  const init = { method: opts.method || 'GET', headers: {} };
  if (opts.body !== undefined) {
    init.method = opts.method || 'POST';
    init.headers['Content-Type'] = 'application/json';
    init.body = JSON.stringify(opts.body);
  }
  const r = await fetch('api/' + path, init);
  if (r.status === 401) {
    showLogin();
    throw new Error('Access token needed');
  }
  if (opts.raw) {
    if (!r.ok) throw new Error((await r.json().catch(() => ({}))).error || r.statusText);
    return r;
  }
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(j.error || r.statusText);
  return j;
}

function status(text, err = false) {
  const s = $('status');
  s.textContent = text;
  s.classList.toggle('err', err);
}

async function act(fn) {
  try {
    await fn();
  } catch (e) {
    status(e.message, true);
  }
}

// ------------------------------------------------------------------ login

function showLogin() {
  $('login').hidden = false;
  $('login-token').focus();
}

$('login-form').addEventListener('submit', async (e) => {
  e.preventDefault();
  try {
    await api('login', { body: { token: $('login-token').value } });
    $('login').hidden = true;
    $('login-error').textContent = '';
    start();
  } catch (err) {
    $('login-error').textContent = err.message;
  }
});

// ------------------------------------------------------------------ left panel

function seg(id, value, onPick) {
  const el = $(id);
  el.querySelectorAll('button').forEach((b) => {
    b.classList.toggle('on', b.dataset.v === String(value));
    b.onclick = () => onPick(b.dataset.v);
  });
}

function renderLeft() {
  seg('conn-kind', S.connKind, (v) => { S.connKind = v; save(); renderLeft(); });
  $('conn-sim').hidden = S.connKind !== 'Simulator';
  $('conn-local').hidden = S.connKind !== 'Local';
  $('conn-bridge').hidden = S.connKind !== 'Bridge';
  $('device').value = S.device;
  $('bridge-host').value = S.bridgeHost;
  $('bridge-token').value = S.bridgeToken;

  const f = film(S.film);
  $('film').value = S.film;
  $('film-note').textContent = `Scanned as ${f.original.toLowerCase()} with the ${f.reflective ? 'reflective' : 'transmission'} lamp${f.negative ? ', inverted on the computer' : ''}`;
  $('mode').value = S.mode;
  $('dpi').value = S.dpi;
  $('prefix').value = S.prefix;
  $('sixteen').checked = S.sixteen;
  $('auto-download').checked = S.autoDownload;
  seg('area-unit', S.areaUnit, (v) => { S.areaUnit = v; save(); renderLeft(); drawBed(); });
  $('res-scale').value = S.resScale;
  $('prescan-dpi').value = S.prescanDpi;
  renderScanSize();
}

function geometry(area, dpi) {
  const px = (mm) => Math.max(1, Math.floor(mm / 25.4 * dpi + 1e-6));
  return { w: px(area.width_mm), h: px(area.height_mm) };
}

function renderScanSize() {
  const f = frame();
  const el = $('scan-size');
  if (!f || !meta) { el.textContent = ''; return; }
  const g = geometry(f.area, S.dpi);
  const m = modeInfo(S.mode);
  const mb = g.w * g.h * m.channels * m.bytes / 1e6;
  let t = `${g.w} x ${g.h} px, ${(g.w * g.h / 1e6).toFixed(1)} MP, ${mb.toFixed(0)} MB`;
  if (server && g.w > server.max_pixels_per_line) t += '. Too wide for the scanner at this resolution';
  el.textContent = t;
  el.classList.toggle('err', !!(server && g.w > server.max_pixels_per_line));
}

function bindLeft() {
  $('device').oninput = (e) => { S.device = e.target.value; save(); };
  $('bridge-host').oninput = (e) => { S.bridgeHost = e.target.value; save(); };
  $('bridge-token').oninput = (e) => { S.bridgeToken = e.target.value; save(); };
  $('film').onchange = (e) => {
    S.film = e.target.value;
    const f = frame();
    if (f && f.adj.film !== S.film) {
      f.adj = clone(film(S.film).adjustments);
      f.auto_levels = true;
      frameChanged();
    }
    save();
    renderLeft();
  };
  $('mode').onchange = (e) => { S.mode = e.target.value; save(); renderScanSize(); };
  $('dpi').oninput = (e) => { S.dpi = Number(e.target.value) || S.dpi; save(); renderScanSize(); };
  $('prefix').oninput = (e) => { S.prefix = e.target.value; save(); };
  $('sixteen').onchange = (e) => { S.sixteen = e.target.checked; save(); };
  $('auto-download').onchange = (e) => { S.autoDownload = e.target.checked; save(); };
  $('res-scale').oninput = (e) => { S.resScale = Number(e.target.value) || 10; save(); };
  $('prescan-dpi').oninput = (e) => { S.prescanDpi = Number(e.target.value) || 75; save(); };
  const q = $('dpi-quick');
  for (const d of [600, 1200, 2000, 3000, 4000]) {
    const b = document.createElement('button');
    b.textContent = d;
    b.onclick = () => { S.dpi = d; save(); renderLeft(); };
    q.appendChild(b);
  }

  $('connect').onclick = () => act(async () => {
    const body = S.connKind === 'Simulator' ? { kind: 'Simulator' }
      : S.connKind === 'Local' ? { kind: 'Local', device: S.device }
      : { kind: 'Bridge', host: S.bridgeHost, token: S.bridgeToken };
    await api('connect', { body });
    status('Connecting…');
    poll();
  });
  $('disconnect').onclick = () => act(async () => { await api('disconnect', { body: {} }); poll(); });
  $('probe').onclick = () => act(async () => { await api('probe', { body: {} }); $('log').hidden = false; poll(); });
  $('find-devices').onclick = () => act(async () => {
    $('device-note').textContent = 'Searching…';
    const r = await api('devices');
    const dl = $('device-list');
    dl.innerHTML = '';
    for (const d of r.devices) {
      const o = document.createElement('option');
      o.value = d.address;
      o.label = `${d.vendor} ${d.product} ${d.revision}`;
      dl.appendChild(o);
    }
    $('device-note').textContent = r.error ? r.error
      : r.devices.length ? r.devices.map((d) => `${d.product} at ${d.address}`).join(', ')
      : 'No scanner found';
    if (r.devices.length === 1) { S.device = r.devices[0].address; save(); renderLeft(); }
  });

  $('preset').onchange = (e) => {
    const p = presets[Number(e.target.value)];
    e.target.value = '';
    if (!p) return;
    S.mode = p.mode;
    S.dpi = p.dpi;
    S.film = p.adjustments.film;
    S.sixteen = p.output_depth === 'Sixteen';
    const f = frame();
    if (f) {
      f.adj = clone(p.adjustments);
      f.auto_levels = p.auto_levels;
      frameChanged();
    }
    status('Preset: ' + p.name);
    save();
    renderLeft();
  };
  $('save-preset').onclick = () => act(async () => {
    const name = prompt('Preset name');
    if (!name) return;
    const f = frame();
    const adj = f ? clone(f.adj) : clone(film(S.film).adjustments);
    await api('presets', {
      body: {
        name, mode: S.mode, dpi: S.dpi, adjustments: adj,
        light: film(adj.film).reflective ? 'Reflective' : 'Transmissive',
        output_depth: S.sixteen ? 'Sixteen' : 'Eight',
        auto_levels: !f,
      },
    });
    await loadPresets();
    status('Saved preset ' + name);
  });
}

let presets = [];
async function loadPresets() {
  const r = await api('presets');
  presets = r.presets;
  const sel = $('preset');
  sel.innerHTML = '<option value="">Choose…</option>';
  presets.forEach((p, i) => {
    const o = document.createElement('option');
    o.value = i;
    o.textContent = p.builtin ? p.name : p.name + ' (saved)';
    sel.appendChild(o);
  });
}

// ------------------------------------------------------------------ bed view

const bed = $('bed');
const bedImg = new Image();
let bedImgSeq = -1;
let drag = null; // { from: [mm, mm], to: [mm, mm] }

function bedMm() {
  if (!server) return [342, 531];
  return server.bed_mm[S.areaUnit];
}

function bedRect() {
  const [bw, bh] = bedMm();
  const W = bed.clientWidth, H = bed.clientHeight;
  const pad = 8;
  const scale = Math.min((W - 2 * pad) / bw, (H - 2 * pad) / bh);
  const w = bw * scale, h = bh * scale;
  return { x: (W - w) / 2, y: (H - h) / 2, w, h, scale, bw, bh };
}

function toMm(ev) {
  const b = bed.getBoundingClientRect();
  const r = bedRect();
  return [(ev.clientX - b.left - r.x) / r.scale, (ev.clientY - b.top - r.y) / r.scale];
}

function drawBed() {
  const dpr = window.devicePixelRatio || 1;
  const W = bed.clientWidth, H = bed.clientHeight;
  if (bed.width !== Math.round(W * dpr) || bed.height !== Math.round(H * dpr)) {
    bed.width = Math.round(W * dpr);
    bed.height = Math.round(H * dpr);
  }
  const c = bed.getContext('2d');
  c.setTransform(dpr, 0, 0, dpr, 0, 0);
  c.clearRect(0, 0, W, H);
  const r = bedRect();
  c.fillStyle = '#1c1c1c';
  c.fillRect(r.x, r.y, r.w, r.h);
  const haveImg = bedImgSeq > 0 && bedImg.complete && bedImg.naturalWidth > 0;
  if (haveImg) {
    // the prescan covers the bed it was made with, which can differ when the unit changes
    const pb = server.prescan_bed_mm || [r.bw, r.bh];
    c.drawImage(bedImg, r.x, r.y, pb[0] * r.scale, pb[1] * r.scale);
  }
  $('bed-hint').hidden = haveImg;
  const rect = (a) => [r.x + a.x_mm * r.scale, r.y + a.y_mm * r.scale, a.width_mm * r.scale, a.height_mm * r.scale];
  c.font = '13px sans-serif';
  c.textBaseline = 'top';
  frames.forEach((f, i) => {
    const [x, y, w, h] = rect(f.area);
    const col = i === selected ? '#50c8ff' : '#ffaa3c';
    c.strokeStyle = col;
    c.lineWidth = 2;
    c.strokeRect(x, y, w, h);
    c.fillStyle = col;
    c.fillText(f.name, x + 4, y + 3);
  });
  if (drag) {
    const x0 = Math.min(drag.from[0], drag.to[0]), y0 = Math.min(drag.from[1], drag.to[1]);
    const [x, y, w, h] = rect({ x_mm: x0, y_mm: y0, width_mm: Math.abs(drag.to[0] - drag.from[0]), height_mm: Math.abs(drag.to[1] - drag.from[1]) });
    c.strokeStyle = '#ffff00';
    c.lineWidth = 1.5;
    c.strokeRect(x, y, w, h);
  }
}

function frameAt(mm) {
  for (let i = frames.length - 1; i >= 0; i--) {
    const a = frames[i].area;
    if (mm[0] >= a.x_mm && mm[0] <= a.x_mm + a.width_mm && mm[1] >= a.y_mm && mm[1] <= a.y_mm + a.height_mm) return i;
  }
  return -1;
}

function addFrame(area) {
  const f = frame();
  // new frames on the same roll start from the frame being worked on
  const adj = f ? clone(f.adj) : clone(film(S.film).adjustments);
  frames.push({ name: `Frame ${frames.length + 1}`, area, adj, auto_levels: true });
  select(frames.length - 1);
}

bed.addEventListener('pointerdown', (ev) => {
  if (ev.button !== 0) return;
  bed.setPointerCapture(ev.pointerId);
  const mm = toMm(ev);
  drag = { from: mm, to: mm };
});
bed.addEventListener('pointermove', (ev) => {
  const mm = toMm(ev);
  if (drag) {
    drag.to = mm;
    drawBed();
  } else {
    densitometer(mm);
  }
});
bed.addEventListener('pointerup', (ev) => {
  if (!drag) return;
  const [bw, bh] = bedMm();
  const clampX = (v) => Math.min(Math.max(v, 0), bw), clampY = (v) => Math.min(Math.max(v, 0), bh);
  const x0 = clampX(Math.min(drag.from[0], drag.to[0])), x1 = clampX(Math.max(drag.from[0], drag.to[0]));
  const y0 = clampY(Math.min(drag.from[1], drag.to[1])), y1 = clampY(Math.max(drag.from[1], drag.to[1]));
  const moved = Math.abs(drag.to[0] - drag.from[0]) + Math.abs(drag.to[1] - drag.from[1]);
  const at = drag.from;
  drag = null;
  if (x1 - x0 > 3 && y1 - y0 > 3) {
    addFrame({ x_mm: round1(x0), y_mm: round1(y0), width_mm: round1(x1 - x0), height_mm: round1(y1 - y0) });
  } else if (moved < 2) {
    select(frameAt(at));
  }
  drawBed();
});
bed.addEventListener('pointerleave', () => { $('densito').textContent = ''; });
new ResizeObserver(drawBed).observe(bed);

const round1 = (v) => Math.round(v * 10) / 10;

let densBusy = false, densNext = null;
async function densitometer(mm) {
  if (!server || !server.prescan_seq) return;
  if (densBusy) { densNext = mm; return; }
  densBusy = true;
  try {
    const r = await api(`density?x=${mm[0].toFixed(2)}&y=${mm[1].toFixed(2)}`);
    $('densito').textContent = r.density
      ? `x ${mm[0].toFixed(1).padStart(5)} y ${mm[1].toFixed(1).padStart(5)} mm  D ${r.density.map((d) => d.toFixed(2)).join(' ')}`
      : '';
  } catch (e) { /* hover only */ }
  densBusy = false;
  if (densNext) { const n = densNext; densNext = null; densitometer(n); }
}

// ------------------------------------------------------------------ frame editor

function select(i) {
  selected = i;
  picker = null;
  const f = frame();
  if (f && f.adj.film !== S.film) S.film = f.adj.film;
  save();
  renderLeft();
  renderFrame();
  drawBed();
  updateButtons();
  if (f) refreshPreview();
}

function frameChanged() {
  save();
  renderFrame();
  refreshPreview();
  drawBed();
  renderScanSize();
}

function setSlider(id, v, digits) {
  $(id).value = v;
  $(id + '-v').textContent = Number(v).toFixed(digits);
}

function renderFrame() {
  const f = frame();
  $('no-frame').hidden = !!f;
  $('frame-editor').hidden = !f;
  if (!f) return;
  const a = f.adj;
  if (document.activeElement !== $('frame-name')) $('frame-name').value = f.name;
  document.querySelectorAll('table.dens input').forEach((inp) => {
    if (document.activeElement !== inp) inp.value = a[inp.dataset.k][inp.dataset.c].toFixed(2);
  });
  setSlider('gamma', a.gamma, 2);
  setSlider('out-min', a.output_min, 0);
  setSlider('out-max', a.output_max, 0);
  $('sharpen').checked = a.sharpen.enabled;
  $('sharpen-opts').hidden = !a.sharpen.enabled;
  setSlider('sh-amount', a.sharpen.amount, 2);
  setSlider('sh-radius', a.sharpen.radius, 1);
  const g = typeof a.gray === 'string' ? a.gray : String(a.gray.Channel);
  seg('gray', g, (v) => {
    a.gray = v === 'Keep' || v === 'Luminance' ? v : { Channel: Number(v) };
    frameChanged();
  });
  $('mirror').checked = a.mirror;
  document.querySelectorAll('.area input').forEach((inp) => {
    if (document.activeElement !== inp) inp.value = f.area[inp.dataset.a].toFixed(1);
  });
  $('pick-hl').classList.toggle('on', picker === 'hl');
  $('pick-sd').classList.toggle('on', picker === 'sd');
  $('preview').classList.toggle('picking', !!picker);
  drawCurve();
}

function bindFrame() {
  $('frame-name').oninput = (e) => { frame().name = e.target.value; save(); drawBed(); };
  $('frame-delete').onclick = deleteSelected;
  document.querySelectorAll('table.dens input').forEach((inp) => {
    inp.oninput = () => {
      const v = Number(inp.value);
      if (Number.isFinite(v)) { frame().adj[inp.dataset.k][inp.dataset.c] = Math.min(Math.max(v, 0), 4.5); frameChanged(); }
    };
  });
  $('link-g').onclick = () => {
    const a = frame().adj;
    a.hd = [a.hd[1], a.hd[1], a.hd[1]];
    a.sd = [a.sd[1], a.sd[1], a.sd[1]];
    frameChanged();
  };
  const slider = (id, set) => { $(id).oninput = (e) => { set(frame().adj, Number(e.target.value)); frameChanged(); }; };
  slider('gamma', (a, v) => { a.gamma = v; });
  slider('out-min', (a, v) => { a.output_min = v; });
  slider('out-max', (a, v) => { a.output_max = v; });
  slider('sh-amount', (a, v) => { a.sharpen.amount = v; });
  slider('sh-radius', (a, v) => { a.sharpen.radius = v; });
  $('sharpen').onchange = (e) => { frame().adj.sharpen.enabled = e.target.checked; frameChanged(); };
  $('mirror').onchange = (e) => { frame().adj.mirror = e.target.checked; frameChanged(); };
  $('rotate').onclick = () => { const a = frame().adj; a.rotate = (a.rotate + 1) % 4; frameChanged(); };
  $('auto').onclick = () => { frame().auto_levels = true; frameChanged(); };
  $('reset').onclick = () => {
    const f = frame();
    f.adj = clone(film(f.adj.film).adjustments);
    frameChanged();
  };
  $('curve-linear').onclick = () => { frame().adj.curve = { points: [[0, 0], [1, 1]] }; frameChanged(); };
  $('curve-s').onclick = () => {
    const k = 0.06;
    frame().adj.curve = { points: [[0, 0], [0.25, 0.25 - k], [0.75, 0.75 + k], [1, 1]] };
    frameChanged();
  };
  $('pick-hl').onclick = () => { picker = picker === 'hl' ? null : 'hl'; renderFrame(); };
  $('pick-sd').onclick = () => { picker = picker === 'sd' ? null : 'sd'; renderFrame(); };
  document.querySelectorAll('.area input').forEach((inp) => {
    inp.onchange = () => {
      const v = Number(inp.value);
      if (!Number.isFinite(v)) return;
      const a = frame().area;
      a[inp.dataset.a] = Math.max(inp.dataset.a.startsWith('w') || inp.dataset.a.startsWith('h') ? 1 : 0, v);
      frameChanged();
    };
  });
  $('preview').addEventListener('click', (ev) => act(async () => {
    if (!picker) return;
    const f = frame();
    const img = $('preview');
    const b = img.getBoundingClientRect();
    let u = (ev.clientX - b.left) / b.width, v = (ev.clientY - b.top) / b.height;
    // undo the rotation and mirror so the dropper reads the right spot on the film
    for (let k = 0; k < f.adj.rotate % 4; k++) [u, v] = [v, 1 - u];
    if (f.adj.mirror) u = 1 - u;
    const x = f.area.x_mm + u * f.area.width_mm, y = f.area.y_mm + v * f.area.height_mm;
    const r = await api(`density?x=${x.toFixed(2)}&y=${y.toFixed(2)}`);
    if (!r.density) return;
    const key = picker === 'hl' ? 'hd' : 'sd';
    for (let c = 0; c < 3; c++) f.adj[key][c] = r.density[Math.min(c, r.density.length - 1)];
    picker = null;
    frameChanged();
  }));
}

function deleteSelected() {
  if (selected < 0) return;
  frames.splice(selected, 1);
  selected = -1;
  save();
  renderFrame();
  drawBed();
  renderScanSize();
  updateButtons();
}

document.addEventListener('keydown', (ev) => {
  const t = ev.target.tagName;
  if ((ev.key === 'Delete' || ev.key === 'Backspace') && t !== 'INPUT' && t !== 'SELECT' && t !== 'TEXTAREA') deleteSelected();
});

// processed preview, newest request wins
let previewTimer = 0, previewSeq = 0, previewUrl = null;
function refreshPreview() {
  clearTimeout(previewTimer);
  previewTimer = setTimeout(async () => {
    const f = frame();
    if (!f) return;
    if (!server || !server.prescan_seq) {
      $('preview').hidden = true;
      $('preview-msg').textContent = 'Prescan to see this frame';
      return;
    }
    const seq = ++previewSeq;
    const wasAuto = f.auto_levels;
    try {
      const r = await api('preview', { body: { area: f.area, adjustments: f.adj, auto_levels: wasAuto }, raw: true });
      const blob = await r.blob();
      if (seq !== previewSeq) return;
      if (wasAuto) {
        const adj = r.headers.get('x-adjustments');
        if (adj) { f.adj = JSON.parse(adj); f.auto_levels = false; save(); renderFrame(); }
      }
      if (previewUrl) URL.revokeObjectURL(previewUrl);
      previewUrl = URL.createObjectURL(blob);
      $('preview').src = previewUrl;
      $('preview').hidden = false;
      $('preview-msg').textContent = '';
    } catch (e) {
      if (seq === previewSeq) { $('preview').hidden = true; $('preview-msg').textContent = e.message; }
    }
  }, 90);
}

// ------------------------------------------------------------------ curve editor

const curveEl = $('curve');
let curveDrag = -1;

// Fritsch and Carlson monotone cubic, the same as the core library
function evalCurve(points, xs) {
  const p = points.map((q) => [Math.min(Math.max(q[0], 0), 1), Math.min(Math.max(q[1], 0), 1)]).sort((a, b) => a[0] - b[0]);
  const n = p.length;
  if (n < 2) return xs.slice();
  const d = [];
  for (let i = 0; i < n - 1; i++) d.push((p[i + 1][1] - p[i][1]) / (p[i + 1][0] - p[i][0] || 1e-9));
  const m = new Array(n).fill(0);
  m[0] = d[0];
  m[n - 1] = d[n - 2];
  for (let i = 1; i < n - 1; i++) m[i] = d[i - 1] * d[i] <= 0 ? 0 : (d[i - 1] + d[i]) / 2;
  for (let i = 0; i < n - 1; i++) {
    if (d[i] === 0) { m[i] = 0; m[i + 1] = 0; continue; }
    const a = m[i] / d[i], b = m[i + 1] / d[i], s = a * a + b * b;
    if (s > 9) { const t = 3 / Math.sqrt(s); m[i] = t * a * d[i]; m[i + 1] = t * b * d[i]; }
  }
  return xs.map((x) => {
    if (x <= p[0][0]) return p[0][1];
    if (x >= p[n - 1][0]) return p[n - 1][1];
    let i = 0;
    while (i < n - 2 && x > p[i + 1][0]) i++;
    const h = p[i + 1][0] - p[i][0], t = (x - p[i][0]) / h, t2 = t * t, t3 = t2 * t;
    const y = (2 * t3 - 3 * t2 + 1) * p[i][1] + (t3 - 2 * t2 + t) * h * m[i] + (-2 * t3 + 3 * t2) * p[i + 1][1] + (t3 - t2) * h * m[i + 1];
    return Math.min(Math.max(y, 0), 1);
  });
}

function curveXY(ev) {
  const b = curveEl.getBoundingClientRect();
  return [(ev.clientX - b.left) / b.width, 1 - (ev.clientY - b.top) / b.height].map((v) => Math.min(Math.max(v, 0), 1));
}

function nearestPoint(pts, q) {
  const S2 = curveEl.getBoundingClientRect().width;
  let best = -1, bd = 10;
  pts.forEach((p, i) => {
    const dd = Math.hypot((p[0] - q[0]) * S2, (p[1] - q[1]) * S2);
    if (dd < bd) { bd = dd; best = i; }
  });
  return best;
}

function drawCurve() {
  const f = frame();
  if (!f) return;
  const c = curveEl.getContext('2d');
  const W = curveEl.width, H = curveEl.height;
  c.clearRect(0, 0, W, H);
  c.strokeStyle = '#323232';
  c.lineWidth = 1;
  for (let k = 1; k < 4; k++) {
    c.beginPath(); c.moveTo(k * W / 4, 0); c.lineTo(k * W / 4, H); c.stroke();
    c.beginPath(); c.moveTo(0, k * H / 4); c.lineTo(W, k * H / 4); c.stroke();
  }
  c.strokeStyle = '#464646';
  c.beginPath(); c.moveTo(0, H); c.lineTo(W, 0); c.stroke();
  const xs = Array.from({ length: 97 }, (_, i) => i / 96);
  const ys = evalCurve(f.adj.curve.points, xs);
  c.strokeStyle = '#e6e6e6';
  c.lineWidth = 2;
  c.beginPath();
  xs.forEach((x, i) => { const px = x * W, py = H - ys[i] * H; if (i) c.lineTo(px, py); else c.moveTo(px, py); });
  c.stroke();
  c.fillStyle = '#50c8ff';
  for (const p of f.adj.curve.points) { c.beginPath(); c.arc(p[0] * W, H - p[1] * H, 4.5, 0, 7); c.fill(); }
}

curveEl.addEventListener('pointerdown', (ev) => {
  const f = frame();
  if (!f || ev.button !== 0) return;
  curveDrag = nearestPoint(f.adj.curve.points, curveXY(ev));
  if (curveDrag >= 0) curveEl.setPointerCapture(ev.pointerId);
});
curveEl.addEventListener('pointermove', (ev) => {
  const f = frame();
  if (!f || curveDrag < 0) return;
  const pts = f.adj.curve.points, last = pts.length - 1;
  const q = curveXY(ev);
  // keep the ends on the left and right edges and the points in order
  if (curveDrag === 0) q[0] = 0;
  else if (curveDrag === last) q[0] = 1;
  else {
    const lo = pts[curveDrag - 1][0] + 0.01, hi = pts[curveDrag + 1][0] - 0.01;
    q[0] = Math.min(Math.max(q[0], lo), Math.max(hi, lo));
  }
  pts[curveDrag] = q;
  drawCurve();
  refreshPreview();
});
curveEl.addEventListener('pointerup', () => { if (curveDrag >= 0) { curveDrag = -1; save(); } });
curveEl.addEventListener('dblclick', (ev) => {
  const f = frame();
  if (!f) return;
  const q = curveXY(ev);
  if (q[0] <= 0.01 || q[0] >= 0.99) return;
  const pts = f.adj.curve.points;
  let at = pts.findIndex((p) => p[0] > q[0]);
  if (at < 0) at = pts.length;
  pts.splice(at, 0, [q[0], evalCurve(pts, [q[0]])[0]]);
  frameChanged();
});
curveEl.addEventListener('contextmenu', (ev) => {
  ev.preventDefault();
  const f = frame();
  if (!f) return;
  const pts = f.adj.curve.points;
  const i = nearestPoint(pts, curveXY(ev));
  if (i > 0 && i < pts.length - 1) { pts.splice(i, 1); frameChanged(); }
});

// ------------------------------------------------------------------ scanning

function advanced() {
  return { area_unit: S.areaUnit, res_scale: S.resScale };
}

$('prescan').onclick = () => act(async () => {
  await api('prescan', { body: { dpi: S.prescanDpi, film: S.film, advanced: advanced() } });
  poll();
});

async function startScan(only) {
  const list = only ? (frame() ? [frame()] : []) : frames;
  if (!list.length) throw new Error('Draw a frame on the prescan first');
  myScanFrom = server && server.files.length ? Math.max(...server.files.map((f) => f.id)) : 0;
  await api('scan', {
    body: {
      frames: list.map((f) => ({ name: f.name, area: f.area, adjustments: f.adj, auto_levels: f.auto_levels })),
      mode: S.mode, dpi: S.dpi, sixteen_bit: S.sixteen, prefix: S.prefix, advanced: advanced(),
    },
  });
  poll();
}
$('scan-one').onclick = () => act(() => startScan(true));
$('scan-all').onclick = () => act(() => startScan(false));
$('cancel').onclick = () => act(() => api('cancel', { body: {} }));

// ------------------------------------------------------------------ files

const fileKey = (f) => `${server.host}|${f.id}|${f.created}`;

function download(f) {
  const a = document.createElement('a');
  a.href = `api/files/${f.id}`;
  a.download = f.file_name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  downloaded.add(fileKey(f));
  save();
  renderFiles();
}

function renderFiles() {
  const files = server ? server.files : [];
  $('files-count').textContent = files.length;
  $('files-empty').hidden = files.length > 0;
  const tb = $('files-table').querySelector('tbody');
  tb.innerHTML = '';
  for (const f of [...files].reverse()) {
    const tr = document.createElement('tr');
    const cell = (t) => { const td = document.createElement('td'); td.textContent = t; tr.appendChild(td); return td; };
    cell(f.file_name);
    cell(f.frame);
    cell(`${f.width} x ${f.height}, ${f.bits} bit ${f.channels === 3 ? 'RGB' : 'grey'}`);
    cell(`${(f.bytes / 1e6).toFixed(1)} MB`);
    const td = cell('');
    const dl = document.createElement('button');
    dl.className = 'small' + (downloaded.has(fileKey(f)) ? '' : ' primary');
    dl.textContent = downloaded.has(fileKey(f)) ? 'Download again' : 'Download';
    dl.onclick = () => download(f);
    const rm = document.createElement('button');
    rm.className = 'small danger';
    rm.textContent = 'Remove';
    rm.title = 'Delete the copy on the server';
    rm.onclick = () => act(async () => {
      if (!downloaded.has(fileKey(f)) && !confirm(`${f.file_name} has not been downloaded to this computer. Remove it from the server anyway?`)) return;
      await api(`files/${f.id}`, { method: 'DELETE' });
      poll();
    });
    td.append(dl, ' ', rm);
    tb.appendChild(tr);
  }
}

$('files-toggle').onclick = () => { $('files').hidden = !$('files').hidden; };
$('files-close').onclick = () => { $('files').hidden = true; };
$('log-toggle').onclick = () => { $('log').hidden = !$('log').hidden; };

// ------------------------------------------------------------------ polling the server

let pollTimer = 0;
async function poll() {
  clearTimeout(pollTimer);
  try {
    const st = await api(`state?log_after=${logSeq}`);
    const first = !server;
    const prev = server;
    server = st;
    applyState(st, prev, first);
  } catch (e) {
    status('Lost contact with the server: ' + e.message, true);
  }
  pollTimer = setTimeout(poll, server && server.busy ? 400 : 1500);
}

function updateButtons() {
  const can = !!(server && server.connected && !server.busy);
  $('prescan').disabled = !can;
  $('scan-one').disabled = !can || !frame();
  $('scan-all').disabled = !can || !frames.length;
  $('scan-all').textContent = `Scan all (${frames.length})`;
}

function applyState(st, prev, first) {
  $('server-name').textContent = st.host ? `on ${st.host}` : '';
  $('server-dir').textContent = st.output_dir;
  const pill = $('conn-pill');
  pill.textContent = st.connected ? 'Connected' : 'Not connected';
  pill.className = 'pill ' + (st.connected ? 'on' : 'off');
  $('connected-as').textContent = st.connected || '';
  $('connect').hidden = !!st.connected;
  $('disconnect').hidden = !st.connected;
  $('probe').hidden = !st.connected;
  updateButtons();
  $('cancel').hidden = !st.busy;
  $('progress').hidden = !st.busy;
  $('progress-bar').style.width = `${(st.progress * 100).toFixed(1)}%`;
  $('progress-text').textContent = `${st.task} ${(st.progress * 100).toFixed(0)}%`;

  const logEl = $('log');
  for (const l of st.log) {
    const span = document.createElement('span');
    if (l.error) span.className = 'e';
    span.textContent = l.text + '\n';
    logEl.appendChild(span);
    if (!first) status(l.text.split('\n')[0], l.error);
  }
  if (st.log.length) logEl.scrollTop = logEl.scrollHeight;
  logSeq = st.log_seq;
  if (first && freshBrowser && st.start_kind) {
    S.connKind = st.start_kind;
    if (st.start_kind === 'Bridge') S.bridgeHost = '';
    renderLeft();
  }
  if (first) status(st.connected ? 'Connected: ' + st.connected : 'Not connected. Choose a connection and press Connect.');

  if (st.prescan_seq !== bedImgSeq) {
    bedImgSeq = st.prescan_seq;
    if (st.prescan_seq > 0) {
      bedImg.onload = () => { drawBed(); refreshPreview(); };
      bedImg.src = `api/prescan.png?seq=${st.prescan_seq}`;
    } else {
      drawBed();
      refreshPreview();
    }
  }

  // download the files this browser asked for as they finish
  if (myScanFrom !== null && S.autoDownload) {
    for (const f of st.files) {
      if (f.id > myScanFrom && !downloaded.has(fileKey(f))) download(f);
    }
  }
  if (myScanFrom !== null && !st.busy) myScanFrom = null;
  if (!prev || JSON.stringify(prev.files) !== JSON.stringify(st.files)) renderFiles();
  renderScanSize();
}

// ------------------------------------------------------------------ start

let started = false;
async function start() {
  if (started) return;
  meta = await api('meta');
  const fs = $('film');
  fs.innerHTML = '';
  for (const f of meta.films) {
    const o = document.createElement('option');
    o.value = f.id;
    o.textContent = f.label;
    fs.appendChild(o);
  }
  const ms = $('mode');
  ms.innerHTML = '';
  for (const m of meta.modes) {
    const o = document.createElement('option');
    o.value = m.id;
    o.textContent = m.label;
    ms.appendChild(o);
  }
  try {
    await loadPresets();
  } catch (e) {
    if (e.message === 'Access token needed') return;
    throw e;
  }
  started = true;
  bindLeft();
  bindFrame();
  renderLeft();
  renderFrame();
  drawBed();
  poll();
}

load();
(async () => {
  try {
    const info = await api('info');
    if (info.needs_token && !info.authorised) { showLogin(); return; }
    await start();
  } catch (e) {
    status(e.message, true);
  }
})();
