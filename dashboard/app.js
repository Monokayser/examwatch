// ExamWatch teacher dashboard.
// Live tiles decode H.264 with WebCodecs (GPU), one decoder per visible tile.
// DOM nodes for tiles are created once and moved, never rebuilt, so screens
// don't flicker when status changes or labs are switched.
"use strict";

// ---------------------------------------------------------------- icons
// Lucide geometry (ISC licence), drawn with one stroke width via CSS.
const ICONS = {
  shield: '<path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z"/><path d="m9 12 2 2 4-4"/>',
  film: '<rect width="18" height="18" x="3" y="3" rx="2"/><path d="M7 3v18M3 7.5h4M3 12h18M3 16.5h4M17 3v18M17 7.5h4M17 16.5h4"/>',
  logout: '<path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/><path d="m16 17 5-5-5-5"/><path d="M21 12H9"/>',
  wifioff: '<path d="M12 20h.01"/><path d="M8.5 16.43a5 5 0 0 1 7 0"/><path d="M5 12.86a10 10 0 0 1 5.17-2.69"/><path d="M19 12.86a10 10 0 0 0-2-1.52"/><path d="M2 8.82a15 15 0 0 1 4.18-2.64"/><path d="M22 8.82a15 15 0 0 0-11.29-3.76"/><path d="m2 2 20 20"/>',
  search: '<circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>',
  grid: '<rect width="7" height="7" x="3" y="3" rx="1"/><rect width="7" height="7" x="14" y="3" rx="1"/><rect width="7" height="7" x="14" y="14" rx="1"/><rect width="7" height="7" x="3" y="14" rx="1"/>',
  columns: '<rect width="18" height="18" x="3" y="3" rx="2"/><path d="M9 3v18M15 3v18"/>',
  plus: '<path d="M5 12h14M12 5v14"/>',
  stop: '<rect width="14" height="14" x="5" y="5" rx="2"/>',
  back: '<path d="m12 19-7-7 7-7"/><path d="M19 12H5"/>',
  x: '<path d="M18 6 6 18M6 6l12 12"/>',
  reset: '<path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/>',
  download: '<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><path d="m7 10 5 5 5-5"/><path d="M12 15V3"/>',
  upload: '<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><path d="m17 8-5-5-5 5"/><path d="M12 3v12"/>',
  play: '<path d="M6 3l14 9-14 9z"/>',
  pause: '<rect x="14" y="4" width="4" height="16" rx="1"/><rect x="6" y="4" width="4" height="16" rx="1"/>',
  monitor: '<rect width="20" height="14" x="2" y="3" rx="2"/><path d="M8 21h8M12 17v4"/>',
  user: '<path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2"/><circle cx="12" cy="7" r="4"/>',
  alert: '<path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4M12 17h.01"/>',
  info: '<circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/>',
  clock: '<circle cx="12" cy="12" r="10"/><path d="M12 6v6l4 2"/>',
  pointer: '<path d="M4.04 4.66a.5.5 0 0 1 .62-.62l15.5 5.5a.5.5 0 0 1-.06.96l-6.07 1.52a2 2 0 0 0-1.45 1.45l-1.52 6.07a.5.5 0 0 1-.96.06z"/>',
};
const icon = (n) => `<span class="ico"><svg viewBox="0 0 24 24" aria-hidden="true">${ICONS[n] || ""}</svg></span>`;
function paintIcons(root = document) {
  root.querySelectorAll(".ico[data-i]").forEach((el) => {
    el.innerHTML = `<svg viewBox="0 0 24 24" aria-hidden="true">${ICONS[el.dataset.i] || ""}</svg>`;
    el.removeAttribute("data-i");
  });
}

// ---------------------------------------------------------------- helpers
const $ = (id) => document.getElementById(id);
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const pref = {
  get(k, d) { try { return localStorage.getItem("ew." + k) ?? d; } catch { return d; } },
  set(k, v) { try { localStorage.setItem("ew." + k, v); } catch { /* private mode */ } },
};
const pad = (n) => String(n).padStart(2, "0");
function clock(us) { const d = new Date(us / 1000); return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`; }
function dur(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  return `${pad(Math.floor(s / 3600))}:${pad(Math.floor((s % 3600) / 60))}:${pad(s % 60)}`;
}
function bytes(n) { return n > 1e9 ? (n / 1e9).toFixed(1) + " GB" : n > 1e6 ? (n / 1e6).toFixed(0) + " MB" : Math.ceil(n / 1e3) + " kB"; }
function dateTime(us) { const d = new Date(us / 1000); return d.toLocaleDateString(undefined, { day: "numeric", month: "short" }) + " " + clock(us).slice(0, 5); }

async function api(path, opts = {}) {
  const r = await fetch(path, {
    method: opts.method || "GET",
    headers: { "X-EW": "1", ...(opts.body ? { "Content-Type": "application/json" } : {}) },
    body: opts.body ? JSON.stringify(opts.body) : undefined,
    credentials: "same-origin",
  });
  if (r.status === 401 && path !== "/api/login") { showLogin(); throw new Error("Please sign in"); }
  if (opts.raw) { if (!r.ok) throw new Error((await r.json().catch(() => ({}))).error || r.statusText); return r; }
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(j.error || r.statusText);
  return j;
}

// Mirrors ew_proto::normalize_student_id / student_id_format_ok.
const normId = (s) => s.replace(/\s+/g, "").toUpperCase();
const idOk = (s) => /^[A-Z0-9_-]{3,32}$/.test(s);
function parseRoster(text) {
  const out = new Set();
  for (const line of text.split(/\r?\n/)) {
    const id = normId((line.split(/[,;\t]/)[0] || "").replace(/"/g, ""));
    if (idOk(id) && !["ID", "STUDENTID", "STUDENT_ID", "STUDENT-ID"].includes(id)) out.add(id);
  }
  return [...out];
}

// ---------------------------------------------------------------- H.264 view
// Wraps one VideoDecoder + canvas. Frames before the first keyframe (or after a
// decode error) are skipped, so a corrupt frame never freezes the tile.
function codecFromSps(data) {
  for (let i = 0; i + 4 < data.length; i++) {
    if (data[i] === 0 && data[i + 1] === 0 && data[i + 2] === 1 && (data[i + 3] & 0x1f) === 7) {
      const h = (b) => b.toString(16).padStart(2, "0");
      return "avc1." + h(data[i + 4]) + h(data[i + 5]) + h(data[i + 6]);
    }
  }
  return null;
}

class H264View {
  constructor(canvas, { onFrame } = {}) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d", { alpha: false, desynchronized: true });
    this.dec = null; this.codec = null; this.needKey = true; this.last = null;
    this.onFrame = onFrame; this.drawFrom = 0;
    this.ro = new ResizeObserver(() => this.fit());
    this.ro.observe(canvas);
  }
  fit() {
    const dpr = window.devicePixelRatio || 1;
    const w = Math.round(this.canvas.clientWidth * dpr), h = Math.round(this.canvas.clientHeight * dpr);
    if (w && h && (this.canvas.width !== w || this.canvas.height !== h)) {
      this.canvas.width = w; this.canvas.height = h;
      if (this.last) this.draw(this.last);
    }
  }
  draw(frame) {
    const cw = this.canvas.width, ch = this.canvas.height;
    if (!cw || !ch) return;
    const fw = frame.displayWidth, fh = frame.displayHeight;
    const s = Math.min(cw / fw, ch / fh), w = fw * s, h = fh * s;
    this.ctx.fillStyle = "#0d0f12";
    this.ctx.fillRect(0, 0, cw, ch);
    this.ctx.imageSmoothingQuality = "high";
    this.ctx.drawImage(frame, (cw - w) / 2, (ch - h) / 2, w, h);
  }
  ensure(codec) {
    if (this.dec && this.dec.state === "configured" && this.codec === codec) return true;
    this.closeDecoder();
    try {
      this.dec = new VideoDecoder({
        output: (f) => {
          if (f.timestamp < this.drawFrom) { f.close(); return; }
          this.draw(f);
          if (this.last) this.last.close();
          this.last = f;
          this.onFrame && this.onFrame(f);
        },
        error: () => { this.needKey = true; this.dec = null; },
      });
      this.dec.configure({ codec, optimizeForLatency: true });
      this.codec = codec;
      return true;
    } catch (e) { console.warn("decoder config failed", codec, e); this.dec = null; return false; }
  }
  feed(isKey, ts, data) {
    if (isKey) {
      const codec = codecFromSps(data) || this.codec || "avc1.42e01f";
      if (!this.ensure(codec)) return;
      this.needKey = false;
    } else if (this.needKey || !this.dec || this.dec.state !== "configured") return;
    // Live view: if the GPU decoder is behind, drop to the next keyframe instead of lagging.
    if (!isKey && this.dec.decodeQueueSize > 6) { this.needKey = true; return; }
    try { this.dec.decode(new EncodedVideoChunk({ type: isKey ? "key" : "delta", timestamp: ts, data })); }
    catch { this.needKey = true; }
  }
  async flush() { if (this.dec && this.dec.state === "configured") { try { await this.dec.flush(); } catch { /* reset mid-flush */ } } }
  closeDecoder() { if (this.dec && this.dec.state !== "closed") { try { this.dec.close(); } catch { /* already closed */ } } this.dec = null; this.needKey = true; }
  clear() { if (this.last) { this.last.close(); this.last = null; } this.ctx.fillStyle = "#0d0f12"; this.ctx.fillRect(0, 0, this.canvas.width, this.canvas.height); }
  destroy() { this.closeDecoder(); this.ro.disconnect(); if (this.last) this.last.close(); this.last = null; }
}

// ---------------------------------------------------------------- state
const S = {
  me: null, labs: [], pcs: new Map(), exams: [],
  lab: pref.get("lab", "*"), layout: pref.get("layout", "stacked"), size: pref.get("size", "m"), q: "",
  ws: null, wsBackoff: 500, tiles: new Map(), zoomId: null, alerts: new Set(),
};
const labName = (id) => (S.labs.find((l) => l.id === id) || {}).name || "Unassigned";
const examFor = (labId) => S.exams.find((e) => e.labs.includes(labId));

// ---------------------------------------------------------------- tiles
class Tile {
  constructor(pc) {
    this.id = pc.machine_id;
    const el = document.createElement("article");
    el.className = "tile";
    el.tabIndex = 0;
    el.innerHTML = `<div class="screen"><canvas></canvas><div class="overlay"></div><div class="badge"></div></div>
      <div class="meta"><span class="dot"></span><div class="who"><span class="sid"></span><span class="pc"></span></div></div>`;
    this.el = el;
    this.overlay = el.querySelector(".overlay");
    this.sid = el.querySelector(".sid");
    this.pcEl = el.querySelector(".pc");
    this.badge = el.querySelector(".badge");
    this.view = new H264View(el.querySelector("canvas"), { onFrame: () => el.classList.add("has-frame") });
    el.addEventListener("click", () => openZoom(this.id));
    el.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); openZoom(this.id); } });
    this.key = "";
    this.update(pc);
  }
  update(pc) {
    this.pc = pc;
    const inExam = !!pc.exam_id;
    const key = [pc.status, pc.student_id, pc.pc_name, inExam, pc.encoder, pc.viewers, pc.throttle].join("|");
    if (key === this.key) return;
    const prevStatus = this.key.split("|")[0];
    this.key = key;
    this.el.className = `tile st-${pc.status}` + (inExam ? " in-exam" : "") + (this.view.last ? " has-frame" : "");
    this.sid.textContent = pc.student_id || { offline: "Offline", idle: "No exam", waiting: "Not started", live: "" }[pc.status];
    this.pcEl.textContent = `${pc.pc_name} · ${labName(pc.lab_id)}`;
    this.el.title = `${pc.student_id || "No student"} — ${pc.pc_name} (${labName(pc.lab_id)})`;
    let ov = "";
    if (pc.status === "offline") ov = `${icon("wifioff")}<span>Offline${inExam ? ` <span data-since="${pc.since_us}">${dur(Date.now() - pc.since_us / 1000)}</span>` : ""}</span>`;
    else if (pc.status === "waiting") ov = `${icon("clock")}<span>Waiting for V1</span>`;
    else if (pc.status === "idle") ov = `${icon("monitor")}<span>Connected · no exam open</span>`;
    else ov = `${icon("monitor")}<span>Starting…</span>`;
    this.overlay.innerHTML = ov;
    const badges = [];
    if (pc.encoder === "software") badges.push("SW");
    if (pc.throttle > 0) badges.push("ECO");
    if (pc.viewers > 0) badges.push("VIEWED");
    this.badge.innerHTML = badges.map((b) => `<span>${b}</span>`).join("");
    if (pc.status !== "live" && pc.status !== "offline") { this.view.clear(); this.el.classList.remove("has-frame"); }
    if (pc.status === "offline" && inExam && prevStatus && prevStatus !== "offline") {
      this.el.classList.remove("flash"); void this.el.offsetWidth; this.el.classList.add("flash");
    }
  }
  destroy() { this.view.destroy(); this.el.remove(); }
}

function visiblePcs() {
  const q = S.q.trim().toUpperCase();
  return [...S.pcs.values()].filter((p) =>
    (S.lab === "*" || p.lab_id === S.lab) &&
    (p.lab_id !== "unassigned" || S.lab === "*" || S.lab === "unassigned") &&
    (!q || (p.student_id || "").toUpperCase().includes(q) || p.pc_name.toUpperCase().includes(q)));
}

function sortPcs(a, b) {
  const rank = (p) => (p.exam_id && p.status === "offline" ? 0 : 1);
  return rank(a) - rank(b) || a.pc_name.localeCompare(b.pc_name, undefined, { numeric: true });
}

function renderGrid() {
  const root = $("grid-root");
  root.className = `grid-root size-${S.size}` + (S.lab === "*" && S.layout === "columns" ? " columns" : "");
  const pcs = visiblePcs();
  const labs = S.lab === "*" ? [...S.labs.map((l) => l.id)] : [S.lab];
  const wanted = new Set(pcs.map((p) => p.machine_id));

  // Destroy tiles that left the view (frees their decoders).
  for (const [id, t] of S.tiles) if (!wanted.has(id)) { t.destroy(); S.tiles.delete(id); }

  const sections = [];
  for (const labId of labs) {
    const list = pcs.filter((p) => p.lab_id === labId).sort(sortPcs);
    if (!list.length && (labId === "unassigned" || S.q)) continue;
    let sec = root.querySelector(`section[data-lab="${CSS.escape(labId)}"]`);
    if (!sec) {
      sec = document.createElement("section");
      sec.className = "lab-section";
      sec.dataset.lab = labId;
      sec.innerHTML = `<div class="lab-head"><h2></h2><span class="muted"></span></div><div class="assign"></div><div class="grid"></div>`;
    }
    sec.querySelector("h2").textContent = labName(labId);
    sec.querySelector(".lab-head").hidden = S.lab !== "*";
    sec.querySelector(".muted").textContent = labSummary(labId);
    renderAssign(sec.querySelector(".assign"), labId === "unassigned" ? list : []);
    const grid = sec.querySelector(".grid");
    list.forEach((p, i) => {
      let t = S.tiles.get(p.machine_id);
      if (!t) { t = new Tile(p); S.tiles.set(p.machine_id, t); } else t.update(p);
      if (grid.children[i] !== t.el) grid.insertBefore(t.el, grid.children[i] || null);
    });
    while (grid.children.length > list.length) grid.lastElementChild.remove();
    if (!list.length) grid.innerHTML = "";
    sections.push(sec);
  }
  sections.forEach((s, i) => { if (root.children[i] !== s) root.insertBefore(s, root.children[i] || null); });
  while (root.children.length > sections.length) root.lastElementChild.remove();

  const empty = $("empty");
  empty.hidden = pcs.length > 0;
  empty.textContent = S.q ? "No screens match your search." : "No student PCs have connected to this lab yet.";
  renderCounts();
}

// "Assign to lab": one row per network among unassigned PCs. Rebuilt only when the
// set of networks changes, so a half-made choice in a <select> is never reset.
function renderAssign(slot, list) {
  const groups = new Map();
  for (const p of list) {
    const k = p.subnet || "";
    if (!groups.has(k)) groups.set(k, []);
    groups.get(k).push(p);
  }
  const key = [...groups].map(([k, v]) => k + ":" + v.length).join("|");
  if (slot.dataset.key === key) return;
  slot.dataset.key = key;
  if (!groups.size) { slot.innerHTML = ""; return; }
  const labs = S.labs.filter((l) => l.id !== "unassigned");
  const opts = `<option value="">Choose a lab…</option>` + labs.map((l) => `<option value="${esc(l.id)}">${esc(l.name)}</option>`).join("");
  slot.innerHTML = `<div class="assign-bar">${icon("info")}<div><strong>These PCs aren't in a lab yet.</strong> ` +
    `Choose the lab for each network once. PCs that join later from the same network go there automatically.</div></div>` +
    [...groups].map(([subnet, pcs]) => {
      const pick = pcs.find((p) => p.status !== "offline") || pcs[0];
      const label = subnet ? `Network ${esc(subnet)}` : "Unknown network";
      return `<div class="assign-row"><span class="assign-net">${label}</span><span class="muted">${pcs.length} PC${pcs.length === 1 ? "" : "s"}: ` +
        `${pcs.slice(0, 4).map((p) => esc(p.pc_name)).join(", ")}${pcs.length > 4 ? "…" : ""}</span>` +
        `<select aria-label="Lab for ${label}" ${subnet ? "" : "disabled"}>${opts}</select>` +
        `<button class="btn btn-primary btn-sm" type="button" data-assign="${esc(pick.machine_id)}" ${subnet ? "" : "disabled"}>Assign</button></div>`;
    }).join("");
}

function labSummary(labId) {
  const list = [...S.pcs.values()].filter((p) => p.lab_id === labId);
  const live = list.filter((p) => p.status === "live").length;
  const off = list.filter((p) => p.status === "offline" && p.exam_id).length;
  const exp = (S.labs.find((l) => l.id === labId) || {}).expected_pcs || list.length;
  const e = examFor(labId);
  return e ? `${live}/${exp} started${off ? ` · ${off} offline` : ""}` : `${list.filter((p) => p.status !== "offline").length}/${exp} online · no exam`;
}

function renderCounts() {
  const list = [...S.pcs.values()].filter((p) => S.lab === "*" || p.lab_id === S.lab);
  const n = (st) => list.filter((p) => p.status === st).length;
  $("counts").innerHTML =
    `<span class="c-live"><b>${n("live")}</b> live</span><span><b>${n("waiting")}</b> waiting</span>` +
    `<span class="c-off"><b>${list.filter((p) => p.status === "offline" && p.exam_id).length}</b> offline</span>`;
  for (const sec of document.querySelectorAll(".lab-section")) sec.querySelector(".muted").textContent = labSummary(sec.dataset.lab);
}

function renderTabs() {
  const tabs = [{ id: "*", name: "All labs" }, ...S.labs.filter((l) => l.id !== "unassigned" || [...S.pcs.values()].some((p) => p.lab_id === "unassigned"))];
  $("lab-tabs").innerHTML = tabs.map((l) => {
    const list = [...S.pcs.values()].filter((p) => l.id === "*" || p.lab_id === l.id);
    const live = list.filter((p) => p.status === "live").length;
    const alert = list.some((p) => p.status === "offline" && p.exam_id);
    return `<button class="tab" role="tab" data-lab="${esc(l.id)}" aria-selected="${S.lab === l.id}">${esc(l.name)}` +
      `<span class="num">${live}/${list.length}</span>${alert ? '<span class="alert-dot" title="Offline PCs"></span>' : ""}</button>`;
  }).join("");
}

function renderExams() {
  const relevant = S.exams.filter((e) => S.lab === "*" || e.labs.includes(S.lab));
  $("exam-strip").innerHTML = relevant.length
    ? relevant.map((e) => `<span class="exam-pill"><span class="rec" title="Recording"></span>${esc(e.name)}` +
        `<span class="elapsed" data-opened="${e.opened_us}">${dur(Date.now() - e.opened_us / 1000)}</span>` +
        `<span class="muted">· ${e.labs.map(labName).map(esc).join(", ")}</span>` +
        `<button class="btn btn-danger btn-sm" data-close-exam="${e.id}">${icon("stop")}End exam</button></span>`).join("")
    : `<span class="no-exam">No exam open${S.lab === "*" ? "" : " in this lab"}. Students can't start until you open one.</span>`;
  const free = S.labs.filter((l) => l.id !== "unassigned" && !examFor(l.id));
  $("open-exam-btn").disabled = free.length === 0;
}

function renderAll() { renderTabs(); renderExams(); renderGrid(); }

function selectLab(id) {
  S.lab = id; pref.set("lab", id);
  wsSend({ op: "thumbs", lab: id });
  renderAll();
}

// ---------------------------------------------------------------- websocket
function wsSend(o) { if (S.ws && S.ws.readyState === 1) S.ws.send(JSON.stringify(o)); }

function connectWs() {
  const ws = new WebSocket((location.protocol === "https:" ? "wss://" : "ws://") + location.host + "/ws");
  ws.binaryType = "arraybuffer";
  S.ws = ws;
  ws.onopen = () => {
    S.wsBackoff = 500;
    $("conn-banner").hidden = true;
    wsSend({ op: "thumbs", lab: S.lab });
    if (S.zoomId) wsSend({ op: "live", pc: S.zoomId, on: true });
  };
  ws.onmessage = (ev) => (typeof ev.data === "string" ? onEvent(JSON.parse(ev.data)) : onMedia(ev.data));
  ws.onclose = () => {
    if (S.ws !== ws) return;
    $("conn-banner").hidden = !S.me;
    if (!S.me) return;
    for (const t of S.tiles.values()) t.view.needKey = true;
    setTimeout(async () => {
      try { await api("/api/me"); connectWs(); } catch { /* login screen shown */ }
    }, S.wsBackoff);
    S.wsBackoff = Math.min(S.wsBackoff * 2, 5000);
  };
}

const td = new TextDecoder();
function onMedia(buf) {
  const u8 = new Uint8Array(buf), dv = new DataView(buf);
  const kind = u8[0], idLen = u8[1];
  const id = td.decode(u8.subarray(2, 2 + idLen));
  const h = 2 + idLen;
  const isKey = (u8[h] & 1) === 1;
  const ts = Number(dv.getBigUint64(h + 5, true));
  const len = dv.getUint32(h + 13, true);
  const data = u8.subarray(h + 17, h + 17 + len);
  if (kind === 1) {
    const t = S.tiles.get(id);
    if (t) t.view.feed(isKey, ts, data);
    if (S.zoomId === id && !zoom.live) zoom.view.feed(isKey, ts, data);
  } else if (kind === 3 && S.zoomId === id) {
    if (isKey && !zoom.live) { zoom.live = true; zoom.view.closeDecoder(); setZoomQuality(); }
    if (zoom.live) zoom.view.feed(isKey, ts, data);
  }
}

function onEvent(ev) {
  switch (ev.ev) {
    case "snapshot":
      S.me = ev.me; S.labs = ev.labs; S.exams = ev.exams;
      S.pcs = new Map(ev.pcs.map((p) => [p.machine_id, p]));
      if (S.lab !== "*" && !S.labs.some((l) => l.id === S.lab)) S.lab = "*";
      renderAll();
      break;
    case "pc": {
      const old = S.pcs.get(ev.pc.machine_id);
      S.pcs.set(ev.pc.machine_id, ev.pc);
      const t = S.tiles.get(ev.pc.machine_id);
      if (t && old && old.lab_id === ev.pc.lab_id && old.status === ev.pc.status) { t.update(ev.pc); renderCounts(); }
      else renderGrid();
      renderTabs(); // live counts and offline alert dots
      if (S.zoomId === ev.pc.machine_id) renderZoomHead();
      break;
    }
    case "exams": S.exams = ev.exams; renderAll(); break;
    case "toast": toast(ev.level, ev.text, ev.lab_id); break;
  }
}

// ---------------------------------------------------------------- toasts
let audioCtx = null;
function beep() {
  try {
    audioCtx = audioCtx || new AudioContext();
    const o = audioCtx.createOscillator(), g = audioCtx.createGain();
    o.frequency.value = 880; o.type = "sine";
    g.gain.setValueAtTime(0.0001, audioCtx.currentTime);
    g.gain.exponentialRampToValueAtTime(0.25, audioCtx.currentTime + 0.02);
    g.gain.exponentialRampToValueAtTime(0.0001, audioCtx.currentTime + 0.35);
    o.connect(g).connect(audioCtx.destination); o.start(); o.stop(audioCtx.currentTime + 0.4);
  } catch { /* audio blocked */ }
}

function toast(level, text, labId) {
  if (labId && S.lab !== "*" && labId !== S.lab && level !== "alert") return; // keep other labs' joins quiet
  const box = $("toasts");
  const el = document.createElement("div");
  el.className = `toast ${level}`;
  el.setAttribute("role", level === "alert" ? "alert" : "status");
  el.innerHTML = `${icon(level === "alert" ? "alert" : level === "join" ? "user" : "info")}<p>${esc(text)}</p><button aria-label="Dismiss">${icon("x")}</button>`;
  const close = () => { el.classList.add("out"); setTimeout(() => el.remove(), 200); };
  el.querySelector("button").onclick = close;
  box.prepend(el);
  while (box.children.length > 5) box.lastElementChild.remove();
  if (level === "alert") beep(); else setTimeout(close, 5000);
}

// ---------------------------------------------------------------- zoom
const zoom = { view: null, live: false };
function setZoomQuality() {
  const q = $("zoom-quality");
  q.textContent = zoom.live ? "Full quality" : "Thumbnail";
  q.classList.toggle("hq", zoom.live);
  $("zoom-state").textContent = zoom.live ? "" : "Switching to full quality…";
}
function renderZoomHead() {
  const pc = S.pcs.get(S.zoomId);
  if (!pc) return;
  $("zoom-dot").className = `dot st-${pc.status}`;
  $("zoom-dot").parentElement.className = `zoom-title st-${pc.status}`;
  $("zoom-student").textContent = pc.student_id || (pc.status === "offline" ? "Offline" : "Not started");
  const enc = pc.encoder === "hardware" ? "GPU encoder" : pc.encoder === "software" ? "Software encoder" : pc.encoder || "";
  $("zoom-meta").textContent = `${pc.pc_name} · ${labName(pc.lab_id)} · ${pc.ip || "—"} · ${enc}` + (pc.status !== "offline" ? ` · agent CPU ${pc.cpu_pct.toFixed(1)}% · ${pc.rss_mb.toFixed(0)} MB` : "");
  $("zoom-reset").disabled = !pc.student_id;
  if (pc.status === "offline") $("zoom-state").textContent = "PC is offline";
}
function openZoom(id) {
  const t = S.tiles.get(id);
  S.zoomId = id; zoom.live = false;
  const dlg = $("zoom");
  if (!dlg.open) dlg.showModal();
  if (!zoom.view) zoom.view = new H264View($("zoom-canvas"));
  zoom.view.closeDecoder(); zoom.view.clear();
  if (t && t.view.last) { zoom.view.fit(); zoom.view.draw(t.view.last); }
  setZoomQuality(); renderZoomHead();
  wsSend({ op: "live", pc: id, on: true });
}
function closeZoom() {
  if (S.zoomId) wsSend({ op: "live", pc: S.zoomId, on: false });
  S.zoomId = null;
  if (zoom.view) { zoom.view.closeDecoder(); zoom.view.clear(); }
  if ($("zoom").open) $("zoom").close();
}
function stepZoom(dir) {
  const ids = [...document.querySelectorAll("#grid-root .tile")].map((el) => [...S.tiles.entries()].find(([, t]) => t.el === el)?.[0]).filter(Boolean);
  const i = ids.indexOf(S.zoomId);
  if (i < 0 || !ids.length) return;
  const next = ids[(i + dir + ids.length) % ids.length];
  wsSend({ op: "live", pc: S.zoomId, on: false });
  openZoom(next);
}

// ---------------------------------------------------------------- dialogs
function confirmDlg(title, body, yes) {
  return new Promise((resolve) => {
    const d = $("confirm-dlg");
    $("confirm-title").textContent = title; $("confirm-body").textContent = body; $("confirm-yes").textContent = yes;
    d.returnValue = "no";
    d.onclose = () => resolve(d.returnValue === "yes");
    d.showModal();
  });
}

function openExamDialog() {
  const d = $("exam-dlg");
  $("exam-error").textContent = "";
  $("exam-labs").innerHTML = S.labs.filter((l) => l.id !== "unassigned").map((l) => {
    const busy = examFor(l.id);
    const checked = !busy && (S.lab === "*" || S.lab === l.id);
    return `<label class="check${busy ? " disabled" : ""}"><input type="checkbox" value="${esc(l.id)}" ${checked ? "checked" : ""} ${busy ? "disabled" : ""}>` +
      `<span>${esc(l.name)}</span><small>${busy ? "exam open" : (l.room ? "Room " + esc(l.room) : "")}</small></label>`;
  }).join("");
  updateRosterCount();
  d.showModal();
  $("exam-name").focus();
}
function updateRosterCount() {
  const txt = $("exam-roster").value;
  const n = parseRoster(txt).length;
  $("roster-count").textContent = !txt.trim()
    ? "No roster: any well-formed ID is accepted, duplicates are still blocked."
    : n ? `${n} Student ID${n === 1 ? "" : "s"} found. Only these can start.` : "No valid Student IDs found.";
}

// ---------------------------------------------------------------- recordings
const rec = { exams: [], examId: null, player: null };
async function showRecordings() {
  $("live-view").hidden = true; $("rec-view").hidden = false;
  wsSend({ op: "thumbs", lab: null });
  const j = await api("/api/exams/history");
  rec.exams = j.exams;
  $("rec-retention").textContent = `Recordings are deleted ${j.retention_hours} hours after an exam ends.`;
  $("rec-exams").innerHTML = j.exams.length ? j.exams.map((e) => {
    const tag = !e.closed_us ? '<span class="tag open">recording</span>' : e.purged ? '<span class="tag purged">deleted</span>' : "";
    const keep = e.closed_us && !e.purged ? `kept until ${dateTime(e.closed_us + j.retention_hours * 3600e6)}` : "";
    return `<li><button data-exam="${e.id}" aria-current="${rec.examId === e.id}"><span><strong>${esc(e.name)}</strong>${tag}</span>` +
      `<small>${dateTime(e.opened_us)} · ${e.labs.map(labName).map(esc).join(", ")}</small>${keep ? `<small>${keep}</small>` : ""}</button></li>`;
  }).join("") : '<li class="muted">No exams yet.</li>';
  if (rec.examId) loadExamRecordings(rec.examId);
}
function showLive() {
  $("rec-view").hidden = true; $("live-view").hidden = false;
  wsSend({ op: "thumbs", lab: S.lab });
  for (const t of S.tiles.values()) t.view.needKey = true;
}
async function loadExamRecordings(id) {
  rec.examId = id;
  document.querySelectorAll("#rec-exams button").forEach((b) => b.setAttribute("aria-current", String(Number(b.dataset.exam) === id)));
  const j = await api(`/api/exams/${id}/recordings`);
  $("rec-title").textContent = j.exam.name;
  const pcOf = new Map(j.claims.map((c) => [c.student_id, c.pc_name]));
  $("rec-students").innerHTML = j.exam.purged
    ? '<p class="muted">These recordings were deleted under the retention policy.</p>'
    : j.students.length
      ? `<table><thead><tr><th>Student ID</th><th>Lab</th><th>PC</th><th>From</th><th>To</th><th>Size</th><th></th></tr></thead><tbody>` +
        j.students.map((s) => `<tr><td><strong>${esc(s.student_id)}</strong></td><td>${esc(labName(s.lab_id))}</td><td>${esc(pcOf.get(s.student_id) || "")}</td>` +
          `<td>${clock(s.start_us)}</td><td>${clock(s.end_us)}</td><td>${bytes(s.bytes)}</td>` +
          `<td><button class="btn btn-ghost btn-sm" data-play="${esc(s.student_id)}">${icon("play")}Play</button></td></tr>`).join("") + "</tbody></table>"
      : '<p class="muted">No recordings for this exam yet.</p>';
}

// Playback: segments are independent (each starts with a keyframe), fetched on demand.
class Player {
  constructor(examId, sid, segs) {
    this.examId = examId; this.sid = sid; this.segs = segs;
    this.t0 = segs[0].start_us; this.t1 = Math.max(...segs.map((s) => s.end_us));
    this.pos = this.t0; this.playing = false; this.speed = 1; this.cache = new Map(); this.gen = 0;
    this.view = new H264View($("pl-canvas"));
  }
  span() { return Math.max(1, this.t1 - this.t0); }
  async frames(seg) {
    if (this.cache.has(seg.id)) return this.cache.get(seg.id);
    const buf = await (await api(`/api/segments/${seg.id}`, { raw: true })).arrayBuffer();
    const u8 = new Uint8Array(buf), dv = new DataView(buf), out = [];
    for (let p = 0; p + 17 <= u8.length;) {
      const len = dv.getUint32(p + 13, true);
      out.push({ key: (u8[p] & 1) === 1, ts: Number(dv.getBigUint64(p + 5, true)), data: u8.slice(p + 17, p + 17 + len) });
      p += 17 + len;
    }
    if (this.cache.size > 6) this.cache.delete(this.cache.keys().next().value);
    this.cache.set(seg.id, out);
    return out;
  }
  segAt(t) { return this.segs.find((s) => t >= s.start_us && t <= s.end_us + 1e6) || this.segs.find((s) => s.start_us >= t); }
  async seek(t, autoplay = this.playing) {
    const gen = ++this.gen;
    this.pos = Math.min(Math.max(t, this.t0), this.t1);
    this.ui();
    const seg = this.segAt(this.pos);
    if (!seg) { this.stop(); return; }
    $("pl-state").textContent = "Loading…";
    let fr;
    try { fr = await this.frames(seg); } catch (e) { $("pl-state").textContent = e.message; return; }
    if (gen !== this.gen) return;
    $("pl-state").textContent = "";
    this.view.closeDecoder();
    // Decode from the segment's keyframe up to the target, drawing only the last one.
    const target = Math.max(this.pos, seg.start_us);
    this.view.drawFrom = fr.filter((f) => f.ts <= target).pop()?.ts ?? target;
    let i = 0;
    for (; i < fr.length && fr[i].ts <= target; i++) this.view.feed(fr[i].key, fr[i].ts, fr[i].data);
    await this.view.flush();
    if (gen !== this.gen) return;
    if (autoplay) this.run(seg, fr, i, gen);
  }
  async run(seg, fr, i, gen) {
    this.playing = true; this.ui();
    let wallStart = performance.now(), mediaStart = this.pos;
    while (gen === this.gen && this.playing) {
      if (i >= fr.length) {
        const next = this.segs.find((s) => s.start_us > seg.start_us);
        if (!next) { this.playing = false; this.pos = this.t1; break; }
        seg = next; fr = await this.frames(seg); i = 0;
        if (gen !== this.gen) return;
        if (fr.length && fr[0].ts > this.pos) { // gap = PC was offline; jump it
          mediaStart = fr[0].ts; wallStart = performance.now();
        }
        this.view.closeDecoder();
        continue;
      }
      const f = fr[i];
      const due = wallStart + (f.ts - mediaStart) / 1000 / this.speed;
      const wait = due - performance.now();
      if (wait > 0) await new Promise((r) => setTimeout(r, Math.min(wait, 250)));
      if (gen !== this.gen || !this.playing) return;
      if (performance.now() + 1 < due) continue;
      this.view.drawFrom = 0;
      this.view.feed(f.key, f.ts, f.data);
      this.pos = f.ts; i++;
      this.ui();
    }
    this.ui();
  }
  toggle() { if (this.playing) this.stop(); else this.seek(this.pos >= this.t1 ? this.t0 : this.pos, true); }
  stop() { this.playing = false; this.gen++; this.ui(); }
  ui() {
    $("pl-time").textContent = clock(this.pos);
    $("pl-end").textContent = clock(this.t1);
    $("pl-head").style.left = ((this.pos - this.t0) / this.span()) * 100 + "%";
    const b = $("pl-play");
    b.innerHTML = icon(this.playing ? "pause" : "play");
    b.setAttribute("aria-label", this.playing ? "Pause" : "Play");
    document.querySelectorAll("#pl-speed button").forEach((x) => x.setAttribute("aria-pressed", String(Number(x.dataset.speed) === this.speed)));
  }
  destroy() { this.stop(); this.view.destroy(); }
}

async function openPlayer(sid) {
  const j = await api(`/api/exams/${rec.examId}/students/${encodeURIComponent(sid)}/segments`);
  if (!j.segments.length) { toast("info", "No recording for this student."); return; }
  if (rec.player) rec.player.destroy();
  const p = (rec.player = new Player(rec.examId, sid, j.segments));
  $("pl-title").textContent = sid;
  const exam = rec.exams.find((e) => e.id === rec.examId);
  $("pl-meta").textContent = `${exam ? exam.name : ""} · ${labName(j.segments[0].lab_id)}`;
  // Style properties via CSSOM: the page's CSP forbids inline style attributes.
  $("pl-segs").replaceChildren(...j.segments.map((s) => {
    const i = document.createElement("i");
    i.style.left = ((s.start_us - p.t0) / p.span()) * 100 + "%";
    i.style.width = Math.max(0.2, ((s.end_us - s.start_us) / p.span()) * 100) + "%";
    return i;
  }));
  $("player").showModal();
  p.view.fit();
  p.seek(p.t0, false);
}

// ---------------------------------------------------------------- auth/boot
let setupMode = false;
function showSetup() {
  setupMode = true;
  showLogin();
  $("login-sub").textContent = "First start: create the administrator account. You'll use it to sign in from any teacher PC.";
  $("login-confirm-row").hidden = false;
  $("login-confirm").required = true;
  $("login-pass").autocomplete = "new-password";
  $("login-btn").textContent = "Create account";
}
function showLogin() {
  S.me = null;
  if (S.ws) { const w = S.ws; S.ws = null; w.close(); }
  for (const t of S.tiles.values()) t.destroy();
  S.tiles.clear();
  $("app").hidden = true; $("login").hidden = false; $("conn-banner").hidden = true;
  $("login-user").focus();
}
function showApp() {
  $("login").hidden = true; $("app").hidden = false;
  document.querySelectorAll("#layout-seg button").forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.layout === S.layout)));
  document.querySelectorAll("#size-seg button").forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.size === S.size)));
  showLive();
  connectWs();
}

function wire() {
  paintIcons();
  $("login-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const btn = $("login-btn");
    btn.disabled = true; $("login-error").textContent = "";
    try {
      if (setupMode) {
        if ($("login-pass").value !== $("login-confirm").value) throw new Error("The two passwords don't match.");
        if ($("login-pass").value.length < 10) throw new Error("Use at least 10 characters for the password.");
      }
      const j = await api(setupMode ? "/api/setup" : "/api/login", { method: "POST", body: { username: $("login-user").value, password: $("login-pass").value } });
      if (setupMode) {
        setupMode = false;
        $("login-confirm-row").hidden = true; $("login-confirm").required = false; $("login-confirm").value = "";
        $("login-sub").textContent = "Teacher sign-in"; $("login-btn").textContent = "Sign in";
      }
      S.me = j.user; $("login-pass").value = "";
      showApp();
    } catch (err) { $("login-error").textContent = err.message; }
    finally { btn.disabled = false; }
  });
  $("add-pcs").onclick = async () => {
    const b = $("add-pcs");
    b.disabled = true;
    try {
      const r = await api("/api/student-installer", { raw: true });
      const url = URL.createObjectURL(await r.blob());
      const a = document.createElement("a");
      a.href = url; a.download = "ExamWatch-Student.zip";
      document.body.append(a); a.click(); a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 10000);
      toast("info", "Downloaded ExamWatch-Student.zip. Extract both files to one folder, then run the MSI on each student PC (or assign it by Group Policy).");
    } catch (err) { toast("alert", err.message); }
    finally { b.disabled = false; }
  };
  $("logout").onclick = async () => { try { await api("/api/logout", { method: "POST" }); } catch { /* already out */ } showLogin(); };
  $("lab-tabs").addEventListener("click", (e) => { const b = e.target.closest(".tab"); if (b) selectLab(b.dataset.lab); });
  $("layout-seg").addEventListener("click", (e) => {
    const b = e.target.closest("button"); if (!b) return;
    S.layout = b.dataset.layout; pref.set("layout", S.layout);
    document.querySelectorAll("#layout-seg button").forEach((x) => x.setAttribute("aria-pressed", String(x === b)));
    renderGrid();
  });
  $("size-seg").addEventListener("click", (e) => {
    const b = e.target.closest("button"); if (!b) return;
    S.size = b.dataset.size; pref.set("size", S.size);
    document.querySelectorAll("#size-seg button").forEach((x) => x.setAttribute("aria-pressed", String(x === b)));
    renderGrid();
  });
  $("search").addEventListener("input", (e) => { S.q = e.target.value; renderGrid(); });
  $("grid-root").addEventListener("click", async (e) => {
    const b = e.target.closest("[data-assign]"); if (!b) return;
    const sel = b.parentElement.querySelector("select");
    if (!sel.value) { sel.focus(); toast("info", "Choose a lab first."); return; }
    b.disabled = true;
    try {
      const j = await api(`/api/pcs/${encodeURIComponent(b.dataset.assign)}/assign`, { method: "POST", body: { lab_id: sel.value } });
      if (!j.moved) toast("info", "No PCs were moved (PCs in a running exam stay where they are).");
    } catch (err) { toast("alert", err.message); b.disabled = false; }
  });

  $("open-exam-btn").onclick = openExamDialog;
  $("exam-roster").addEventListener("input", updateRosterCount);
  $("exam-file").addEventListener("change", async (e) => {
    const f = e.target.files[0]; if (!f) return;
    if (f.size > 2e6) { $("exam-error").textContent = "File is too large (max 2 MB)."; return; }
    $("exam-roster").value = await f.text(); updateRosterCount(); e.target.value = "";
  });
  $("exam-dlg").querySelector("[data-close]").onclick = () => $("exam-dlg").close();
  $("exam-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const labs = [...$("exam-labs").querySelectorAll("input:checked")].map((i) => i.value);
    const btn = $("exam-submit");
    $("exam-error").textContent = "";
    if (!labs.length) { $("exam-error").textContent = "Choose at least one lab."; return; }
    if ($("exam-roster").value.trim() && !parseRoster($("exam-roster").value).length) { $("exam-error").textContent = "No valid Student IDs in the roster."; return; }
    btn.disabled = true;
    try {
      await api("/api/exams", { method: "POST", body: { name: $("exam-name").value, labs, roster: $("exam-roster").value } });
      $("exam-dlg").close(); $("exam-name").value = ""; $("exam-roster").value = "";
      toast("info", `Exam opened in ${labs.map(labName).join(", ")}. Students can now press V1.`);
    } catch (err) { $("exam-error").textContent = err.message; }
    finally { btn.disabled = false; }
  });
  $("exam-strip").addEventListener("click", async (e) => {
    const b = e.target.closest("[data-close-exam]"); if (!b) return;
    const ex = S.exams.find((x) => x.id === Number(b.dataset.closeExam));
    if (!ex) return;
    const live = [...S.pcs.values()].filter((p) => p.exam_id === ex.id && p.status === "live").length;
    if (!(await confirmDlg(`End "${ex.name}"?`, `Recording stops on all PCs in ${ex.labs.map(labName).join(", ")} (${live} students live). This can't be undone.`, "End exam"))) return;
    try { await api(`/api/exams/${ex.id}/close`, { method: "POST" }); toast("info", `"${ex.name}" ended. Recordings are kept for review.`); }
    catch (err) { toast("alert", err.message); }
  });

  $("zoom-close").onclick = closeZoom;
  $("zoom").addEventListener("close", () => { if (S.zoomId) closeZoom(); });
  $("zoom").addEventListener("click", (e) => { if (e.target === $("zoom")) closeZoom(); });
  $("zoom").addEventListener("keydown", (e) => { if (e.key === "ArrowRight") stepZoom(1); if (e.key === "ArrowLeft") stepZoom(-1); });
  $("zoom-reset").onclick = async () => {
    const pc = S.pcs.get(S.zoomId); if (!pc || !pc.student_id) return;
    if (!(await confirmDlg(`Reset ${pc.pc_name}?`, `${pc.student_id} will be released from this PC and must press V1 again (on this or another PC). Their recording so far is kept.`, "Reset PC"))) return;
    try { await api(`/api/pcs/${encodeURIComponent(pc.machine_id)}/release`, { method: "POST" }); } catch (err) { toast("alert", err.message); }
  };

  $("nav-recordings").onclick = () => showRecordings().catch((e) => toast("alert", e.message));
  $("rec-back").onclick = showLive;
  $("rec-exams").addEventListener("click", (e) => { const b = e.target.closest("[data-exam]"); if (b) loadExamRecordings(Number(b.dataset.exam)).catch((er) => toast("alert", er.message)); });
  $("rec-students").addEventListener("click", (e) => { const b = e.target.closest("[data-play]"); if (b) openPlayer(b.dataset.play).catch((er) => toast("alert", er.message)); });
  $("pl-play").onclick = () => rec.player && rec.player.toggle();
  $("pl-close").onclick = () => $("player").close();
  $("player").addEventListener("close", () => { if (rec.player) { rec.player.destroy(); rec.player = null; } });
  $("pl-speed").addEventListener("click", (e) => {
    const b = e.target.closest("button"); if (!b || !rec.player) return;
    rec.player.speed = Number(b.dataset.speed);
    if (rec.player.playing) rec.player.seek(rec.player.pos, true); else rec.player.ui();
  });
  const seekFromEvent = (e) => {
    const p = rec.player; if (!p) return;
    const r = $("pl-track").getBoundingClientRect();
    p.seek(p.t0 + ((e.clientX - r.left) / r.width) * p.span());
  };
  $("pl-track").addEventListener("click", seekFromEvent);
  $("pl-track").addEventListener("keydown", (e) => {
    const p = rec.player; if (!p) return;
    if (e.key === "ArrowRight") p.seek(p.pos + 10e6);
    if (e.key === "ArrowLeft") p.seek(p.pos - 10e6);
    if (e.key === " ") { e.preventDefault(); p.toggle(); }
  });

  // One shared 1 s ticker for elapsed/offline timers (text only, no layout change).
  setInterval(() => {
    document.querySelectorAll("[data-opened]").forEach((el) => (el.textContent = dur(Date.now() - Number(el.dataset.opened) / 1000)));
    document.querySelectorAll("[data-since]").forEach((el) => (el.textContent = dur(Date.now() - Number(el.dataset.since) / 1000)));
  }, 1000);
}

async function boot() {
  wire();
  if (!("VideoDecoder" in window)) {
    document.body.insertAdjacentHTML("afterbegin", '<div class="banner">This browser cannot decode video. Use a current version of Edge or Chrome.</div>');
  }
  try { const j = await api("/api/me"); S.me = j.user; showApp(); }
  catch {
    let setup = false;
    try { setup = (await api("/api/setup")).needs_setup; } catch { /* server unreachable: plain login */ }
    if (setup) showSetup(); else showLogin();
  }
}
document.addEventListener("DOMContentLoaded", boot);
