# ExamWatch — Design Document (Deliverables 1–4)

Status: **draft for review** · 2026-10-29

## 0. Confirmed constraints

| Item | Value |
|---|---|
| Labs | **5** (§0.1) |
| Student PCs | **114** total: 28 + 27 + 26 + 27 + 6 (design headroom: 200) |
| OS | **Windows 10 and Windows 11, mixed inside the labs** (student and teacher PCs). Supported: Windows 10 22H2 and Windows 11 23H2 / 24H2 / 25H2, x64 |
| Network | Wired; each lab on its own VLAN/subnet; Cisco SF300 switches (100 Mbps to each PC) in four labs, BDCOM in the Game lab |
| Exam length | **2 h** (soak tests run 3 h = 1.5× exam length) |
| Student GPU | **NVIDIA in every lab** (§0.1): NVENC on RTX 3060, RTX 4060 Ti and GTX 1050 Ti; the GT 730 labs have no hardware encoder and use GPU scaling plus a capped software H.264 encoder |
| Recording storage | **Google Drive folder** `<folder-id>` (long-term store), fed by the central server's local buffer (§2.10) |
| Deployment | Active Directory / Group Policy |
| Retention | 48 h after a session ends, then hard delete |

### 0.1 Lab hardware analysis (from the lab sheets, 2026-10-29)

| Lab | Room | PCs | CPU | GPU | Hardware H.264 encoder | Storage | Monitors | Switch |
|---|---|---|---|---|---|---|---|---|
| 3D Animation Lab 1 | 1018 | 28 | Core i7 12th gen (Z690) | RTX 3060 | **NVENC** (Ampere) | M.2 980 Pro 500 GB + 1 TB HDD | LG 21.5" (26), Daffodil 19.5" (5) | Cisco SF300 ×2 |
| Visual Arts & Communication | 1019 | 27 | Core i5 4th gen (Haswell) | GT 730 | **None** | 1 TB HDD only | Dell 21.5" (16 listed) | Cisco SF300 ×2 |
| Game & AR/VR Development | 1015 | 26 | Core i7 13th gen (Z790) | RTX 4060 Ti | **NVENC** (Ada) | 980 Pro 2 TB | LG 22" | BDCOM |
| 3D Animation Lab 2 | 1018 (AB-04) | 27 | Core i5 4th gen (Haswell) | GT 730 | **None** | 1 TB HDD only | Dell 21.5" (16 listed) | Cisco SF300 ×2 |
| Professional Development | 1020 (AB-04) | 6 | Core i5 7th gen (B250) | GTX 1050 Ti | **NVENC** (Pascal) | 870 EVO 500 GB + 1 TB HDD | Daffodil 21.5" (2 listed) | Cisco SF300 ×2 |
| **Total** | | **114** | | | 60 NVENC, 54 none | | | |

The sheets list Windows 10, but the labs actually **mix Windows 10 and Windows 11** (confirmed 2026-10-29). See §0.2.

**Decisions that follow from this:**

1. **Intel Quick Sync is dropped as the main encoder.** Every PC has an NVIDIA card driving the display, so the Intel iGPU is normally disabled or absent (on 12th/13th gen "F" CPUs). Encoding on it would need a cross-adapter copy. Instead the encoder is **NVENC on the same NVIDIA GPU that Desktop Duplication captures from**, giving a zero-copy path for 60 PCs.
2. **The GT 730 labs (Visual Arts, 3D Animation 2; 54 PCs) are the worst case and the design target.** The GT 730 has no NVENC. These are also the oldest CPUs (4th gen i5, 4 cores) on HDD-only storage. Pipeline: the GT 730 scales to 720p NV12 → a ≈1.4 MB readback → Windows' software H.264 encoder on one below-normal thread. Rates are REC 2 fps, THUMB 1 fps, LIVE capped at 720p/5 fps. Estimated cost: ≈1–2% of total CPU on average. **Phase 0 must measure a GT 730 PC first; if it misses the §2.8 budget, those labs drop to REC 1 fps.** Optional later: enable the Haswell iGPU in BIOS and use its Quick Sync.
3. **3D Animation and Game labs run heavy GPU software** (Blender, Maya, Unity, Unreal). NVENC doesn't use the 3D or CUDA cores, and our scaling runs at idle GPU priority. The §2.8 governor also backs off when the GPU is busy. These labs get their own A/B test with a GPU-heavy workload (Blender viewport and Unity play mode).
4. **NVENC session limit on GeForce cards:** the driver caps how many encode sessions can run at once, and other software (OBS, ShadowPlay) may also use them. ExamWatch uses at most 3 and falls back to software per stream.
5. **Network:** the Cisco SF300 gives 100 Mbps to each PC, and often 1 Gbps uplinks. Our peak per PC is ≈2.4 Mbps (LIVE + REC + THUMB) against 100 Mbps, and about 12 Mbps per lab in normal operation, so there's plenty of room. Agents mark traffic with low-priority DSCP, which the SF300 supports.
6. **HDD-only PCs:** the agent is a small binary (a few MB). The local buffer is written only during a disconnect, at very-low I/O priority, in 1 MB batches, and capped at 1 GB on those PCs. Startup is lazy, and nothing runs on the logon critical path.
7. **Monitors:** 19.5" and 21.5" panels (1600×900 or 1920×1080). Both record at 720 px tall (§2.9).
8. **Windows 10 is past its end of support (14 Oct 2025).** It isn't a blocker; every API we use exists on Windows 10 1809+. It is a security risk for the labs, though, so Extended Security Updates or an upgrade is recommended (R25).
9. **Server and teacher PCs** aren't on the sheets. Server minimum: Core i5, 8 GB RAM, **gigabit** network, 256 GB SSD, and a UPS, placed where every lab VLAN can reach it. Teacher PCs need only Edge or Chrome.
10. **Unclear from the sheets:** 3D Animation Lab 1 and Lab 2 are both "room 1018" (one is marked AB-04, so possibly another building). "W-Com 25" isn't explained. The monitor counts are lower than the PC counts. None of these change the design.

### 0.2 Windows 10 + Windows 11 in the same lab

| Area | Windows 10 (22H2) | Windows 11 (23H2+) | What ExamWatch does |
|---|---|---|---|
| **Smart App Control** | Doesn't exist | On by default on clean, unmanaged installs; blocks unsigned programs | **Every binary and both installers are code-signed** with a trusted certificate (R27). Without signing, the agent may silently fail to start on some Windows 11 PCs. |
| **Efficiency mode (EcoQoS)** | Power throttling exists but is weaker; no E-core steering | Full EcoQoS: the agent runs on **E-cores** on the 12th/13th-gen Intel PCs (3D Animation Lab 1, Game lab) | The same API call on both. The governor (§2.8) doesn't rely on EcoQoS for correctness. Task Manager on Windows 11 shows the agent with the green "Efficiency mode" leaf. |
| **Hybrid CPUs (12th/13th gen)** | Windows 10 doesn't understand P/E cores (no Thread Director), so a low-priority thread may land on a P-core | Thread Director places it correctly | BELOW_NORMAL priority + Job Object CPU cap on both, so even on a P-core it never takes time a student's app needs. A/B test on **both** OSes in Lab 1. |
| **Screen capture** | DXGI Desktop Duplication | Same API (no yellow border; that only applies to Windows.Graphics.Capture) | One code path. |
| **GPU scheduling (HAGS)** | Optional | On by default | Idle GPU priority honoured either way; tested with HAGS on and off. |
| **Media Foundation H.264** | Built in (except "N" editions without the Media Feature Pack) | Built in (same N-edition caveat) | Pre-flight detects a missing encoder and the dashboard flags the PC. |
| **Installer / MSI / services** | Same | Same | One MSI for both. |
| **Session 0, UAC, lock screen** | Same behaviour | Same behaviour | Handled as in R10. |

**Test matrix:** every test in §6 runs on Windows 10 22H2 **and** Windows 11 24H2, and the A/B "invisible capture" test runs on both OSes on the oldest (GT 730) and newest (12th/13th gen hybrid) lab hardware.

Non-negotiable priority: **zero impact on students.** Every design choice below is checked against it first.

---

## 1. Veyon analysis: why it lags and how we avoid it

Sources: Veyon's `core/src/VncConnection.cpp` (master branch), the configuration reference at docs.veyon.io, and community forum reports. I did not find GitHub issues that confirm these causes, so the forum material counts as user reports, not confirmed bugs.

### 1.1 How Veyon moves pixels

1. Each student runs **Veyon Service**. It starts a **built-in VNC server**, which listens only on localhost:11200, and a **Veyon Server** that proxies RFB (VNC) on port 11100.
2. **Veyon Master** (the teacher's app) opens **one libvncclient connection per student, each on its own `QThread`**.
3. The master **requests** framebuffer updates on a timer (`requestFrameufferUpdate(Incremental/Full)`, driven by `incrementalFramebufferUpdateTimeout()` and `fullFramebufferUpdateTimeout()`). The default thumbnail update interval is 1000 ms.
4. The encodings are classic RFB rectangle codecs: Tight, ZYWRLE, ZRLE and Ultra, with JPEG quality 5 by default. At "Highest" quality they switch to ZRLE, Hextile, Zlib and Raw.
5. The master keeps a **full-resolution RGB32 framebuffer for every student** (`initFrameBuffer()`). For thumbnails it **downscales on the CPU** with `QImage::scaled(..., Qt::SmoothTransformation)` (`rescaleFramebuffer()`).

### 1.2 Bottlenecks

| # | Bottleneck | Effect in an exam lab |
|---|---|---|
| B1 | **Full-resolution frames are pulled from every student, even for a 200 px thumbnail.** | Bandwidth, master RAM (~8 MB per 1080p screen × N) and decode cost all grow with full resolution × N students. Forum reports describe the master freezing at large scale (about 800 PCs). |
| B2 | **The master downscales thumbnails on the CPU** with smooth (bilinear or better) filtering. | High CPU load on the teacher PC. The forum's standard advice is to "raise the thumbnail interval", which only trades freshness for CPU. |
| B3 | **Updates are pulled with a request/response cycle.** | Each update costs at least one round trip plus the timer interval. Latency is jittery and the link sits idle between requests. |
| B4 | **Intra-only rectangle codecs** (JPEG/zlib tiles) with no inter-frame prediction. | Scrolling, video or animated content re-sends whole regions. Student CPU spikes because compression runs on the CPU in the VNC server. |
| B5 | **GDI-style capture in the built-in VNC server.** Forum advice is to disable "capturing of layered windows" when freezes happen. | Capture runs on the CPU and can interfere with the student's foreground app, which is exactly the "zero impact" violation we must avoid. |
| B6 | **One thread per connection plus UI-thread image work.** | Many threads contend for locks. Image conversion and painting on the UI thread freezes the master during actions. |
| B7 | **Peer-to-peer topology.** The master connects to every student directly. | The teacher PC is a single point of failure and load. Across VLANs, every student port must be reachable from every teacher PC. There is no recording pipeline (Veyon isn't built to record). |
| B8 | **Encryption is not designed around the stream.** Auth is a key-file challenge; the docs list no TLS option for the VNC stream itself. | It isn't suitable for exam evidence as-is. |

### 1.3 How ExamWatch avoids each one

| Veyon bottleneck | ExamWatch approach |
|---|---|
| B1 full-res pull | **The student encodes several sizes on the GPU.** A tiny thumbnail stream (≤ 480×270, 1 fps) always runs. The full-quality live stream runs **only while a teacher has that screen enlarged**. |
| B2 CPU downscale on master | Scaling happens on the **student's GPU** (D3D11 Video Processor), and decoding happens in the **teacher's browser on the GPU** (WebCodecs). The teacher's CPU barely touches pixels. |
| B3 request/response | The student **pushes** frames as soon as Desktop Duplication reports a change. There is no polling round trip. |
| B4 intra-only codecs | **H.264 with inter-frame prediction**, encoded by NVENC (or by a capped software encoder on GT 730 PCs). Static screens cost almost nothing, and scrolling is cheap. |
| B5 GDI capture | **DXGI Desktop Duplication.** The compositor hands over the already-composed frame on the GPU, with dirty rectangles. There is no CPU copy and no window hooks. |
| B6 thread per connection | **Async I/O (Tokio)** on the server and a fixed worker pool. The dashboard renders each tile in its own `<canvas>`, off the main layout path. |
| B7 peer-to-peer | A **central server**. Students make outbound connections only. Teachers talk only to the server, so a teacher PC crash affects no student. |
| B8 weak transport security | **TLS 1.3 (QUIC)** with server certificate pinning and per-machine client identity. Recordings are encrypted at rest. |

---

## 2. Architecture and tech stack

### 2.1 Topology

```
 Lab VLAN A (30–50 PCs)      Lab VLAN B ...      Lab VLAN C ...
 ┌───────────────────┐
 │ Student PC        │
 │  ew-service (SYS) │──┐  QUIC/TLS 1.3, UDP 4433 (TCP 443 fallback)
 │  ew-agent  (user) │  │  outbound only
 └───────────────────┘  │
                        ▼
              ┌─────────────────────────┐
              │  ew-server (central)    │  ingest · fan-out · record · auth
              │  Rust/Tokio, SQLite     │  fMP4 segments, AES-GCM at rest
              └─────────────────────────┘
                        ▲ HTTPS + WebSocket (TLS 1.3)
                        │
              Teacher PC(s): Edge/Chrome → dashboard (served by ew-server)
```

### 2.2 Components

**Student PC: two processes.** A Windows service can't capture the interactive desktop (Session 0 isolation), so the work is split in two.

| Process | Runs as | Job |
|---|---|---|
| `ew-service` | Windows service, LocalSystem, auto-start, SCM recovery = restart | Watchdog. It launches `ew-agent` into the active user session (`WTSQueryUserToken` + `CreateProcessAsUser`), checks a heartbeat over a named pipe every 1 s, and kills and relaunches the agent after 5 s without a heartbeat. It enforces the resource caps through a **Job Object** and owns the encrypted local buffer directory. |
| `ew-agent` | Logged-in student user | Shows the ID-entry window and the "screen is monitored" notice. Runs capture, scaling, encoding and network upload. |

**Capture pipeline on the student PC (all on the GPU):**

```
DXGI Desktop Duplication (AcquireNextFrame, dirty rects)
   └─ ID3D11Texture2D (BGRA, stays on GPU)
        ├─ D3D11 VideoProcessor → 480×270 NV12 → H.264 enc* → THUMB stream  (1 fps, ~30–60 kbps)
        ├─ D3D11 VideoProcessor → ≤720p NV12 → H.264 enc* → REC stream      (2 fps, ~150–300 kbps avg, see §2.9)
        └─ (only when enlarged) native NV12  → H.264 enc* → LIVE stream     (up to 15 fps, ≤ 2 Mbps)
```

- **Delta encoding:** if Desktop Duplication reports no change, we encode nothing. H.264 P-frames carry only what changed, with a keyframe every 10 s on REC and THUMB, and on demand for LIVE.
- **Encoder (\* above):** a Media Foundation **hardware MFT on the same adapter that drives the display** (NVIDIA NVENC), so there is zero copy. On **GT 730 PCs** (no NVENC) the GT 730 still scales the frame. Only the small, already-scaled NV12 frame (≈1.4 MB at 720p) is read back, and it is encoded by **Windows' built-in Microsoft H.264 software MFT** limited to 1 thread, with LIVE capped at 720p and 5 fps. Whichever path a PC uses, it reports it at connect, and the dashboard shows it. See §0.1.
- **No impact:** the agent runs at `BELOW_NORMAL` priority with **EcoQoS** (`ProcessPowerThrottling`), low GPU scheduling priority, and a Job Object hard CPU cap (default 5% of total) and memory cap (default 200 MB). If a frame isn't ready, we **drop it rather than queue it**. Capture never blocks the desktop.

**Server: `ew-server`** (one binary, Rust + Tokio)
- **Ingest:** a QUIC endpoint (`quinn`). Each student connection carries separate streams (control, THUMB, REC, LIVE, BUFFER-UPLOAD), so a burst in one stream can't delay another.
- **Fan-out:** THUMB and LIVE packets are forwarded **without transcoding** to dashboards that subscribe through WebSocket.
- **Recorder:** REC is remuxed (not re-encoded) into fragmented-MP4 segments per student, 1 minute each, encrypted with AES-256-GCM and indexed in SQLite with wall-clock timestamps.
- **Sessions:** start and end, the roster, Student ID registry, lab membership and audit log.
- **Retention job:** deletes a session's segments and keys 48 h after the session ends. Deleting the per-session key makes any leftover blocks unreadable.

**Teacher dashboard:** a web app served by `ew-server`, opened in Edge or Chrome.
- WebCodecs `VideoDecoder` decodes H.264 on the GPU, and each tile draws to its own canvas. Tiles have a fixed size, so nothing reflows when a frame arrives.
- Inter font, Lucide icons (1.5 px stroke, 16/20 px), CSS variables, and `devicePixelRatio`-aware canvases for crisp high-DPI rendering.
- **Why a web app instead of a native app:** any teacher PC can use it with no install, several teachers or labs can be open in tabs or side by side, and it can be tested with automated browser tools. ⚠️ **This is a choice you should approve.** The alternative is a native Windows app (WinUI), which is more work and has no clear latency benefit.

### 2.3 Why this stack is low-latency

| Choice | Reason |
|---|---|
| **Rust** for the agent, service and server | No GC pauses (so no surprise stalls), a small memory footprint (agent ~20–40 MB), memory safety (no crash from a buffer bug during an exam), and direct Win32/D3D11/MF access through the `windows` crate. |
| **DXGI Desktop Duplication** | The only Windows capture API that hands over GPU-resident composed frames plus dirty rects at near-zero CPU cost. |
| **NVENC H.264 (software MFT fallback)** | NVENC is a fixed-function block, separate from the CUDA and 3D cores the student's Blender, Unity or Unreal work uses. A 720p encode costs well under 1% CPU. Every browser can decode H.264 in hardware. Using Media Foundation means one code path for hardware and software encoding, and the Windows software encoder is already licensed and installed. |
| **QUIC (TLS 1.3 built in)** | Independent streams with no head-of-line blocking between thumbnail, recording and upload. Fast reconnect (0-RTT/1-RTT), and connection migration survives DHCP changes. |
| **Push model** | Latency = capture + encode (~10 ms) + network. Polling adds the interval on top. |
| **Server fan-out without transcoding** | Server CPU is O(bytes), not O(pixels). 100 students fit easily on a mid-range PC. |
| **WebCodecs** | GPU decode in the browser keeps the teacher PC smooth with 50 tiles. |
| **SQLite (WAL mode)** | Zero-admin and transactional, more than enough for metadata on 100 PCs. |

### 2.4 Bandwidth and storage budget (100 PCs)

| Stream | Per PC | ×100 |
|---|---|---|
| THUMB | ≤ 60 kbps | 6 Mbps |
| REC | ≤ 300 kbps | 30 Mbps |
| LIVE (only enlarged screens, typically 1–4) | ≤ 2 Mbps | ≤ 8 Mbps |
| **Total into the server** | | **≈ 44 Mbps peak**, well under 1 Gbps |

- **Storage:** 300 kbps × 2 h ≈ 270 MB per PC per exam, so about 27 GB per 100-PC exam. With 48 h retention and up to 4 exams a day, peak storage is ~220 GB. That lives on **Google Drive** (§2.10); the server's local SSD is only a hot buffer, and **256 GB is enough**.
- **Adaptive bitrate:** the server sends a congestion signal from QUIC RTT and loss (BBR stats) back to each agent. The agent first lowers LIVE fps, then THUMB fps (down to 0.2), and lowers REC bitrate last. REC frame rate never goes below 1 fps.
- **No flooding at session start:** agents join with **random jitter (0–3 s)**, the first keyframes are staggered, and buffer uploads are throttled server-side (token bucket, ≤ 20 Mbps total).

### 2.5 Key flows

**Exam start: the student starts with the V1 button, and the teacher is notified automatically**

1. *(Teacher, beforehand, 1–2 clicks)* **Open exam**, optionally with a roster CSV. This applies to all labs or the ones selected. It enables the V1 button on every student PC in those labs and starts the 48 h retention bookkeeping. If no exam is open, V1 shows "No exam is open — please ask your invigilator", and nothing is captured. This keeps the rule that recording happens only during exam sessions.
2. *(Student)* Clicks the **V1** button, a small always-visible launcher on the desktop or taskbar → enters their Student ID → confirms it → the agent starts capturing immediately (REC + THUMB) and shows the "Your screen is monitored" notice.
3. *(Automatic)* The server pushes a **"Student joined"** event to every dashboard of a teacher with access to that lab:
   - A toast appears: "S-1042 · Lab B · PC-B17 joined".
   - The tile goes live in that lab's grid and in the **All labs** overview.
   - The lab's counter updates (for example "B: 31/40 started").
4. The teacher watches every lab from one dashboard: the **All labs** overview (every lab side by side, scaled tiles), a single lab, or any enlarged student. PCs that are online but have **not pressed V1 yet** appear as grey "Waiting" tiles, so missing students stand out.
5. *(Teacher, 1 click)* **End exam** ends capture on every PC in those labs. A student can't end their own capture.

**Student ID validation**
- The ID must match the roster for that lab (catches typos), must not already be claimed in the session (blocks duplicates), and is confirmed by re-entry. Optionally, the ID format has a checksum.
- If an ID is already taken, the student sees "Ask your invigilator" and the teacher can reassign the ID with one click (for example after a PC swap).

**Disconnect**
- The agent keeps encoding REC into an **encrypted local ring buffer** (`%ProgramData%\ExamWatch\buffer`, ACL = SYSTEM only, capped at 2 GB, about 12 h of REC).
- The dashboard turns that tile red within **≤ 3 s** (QUIC idle timeout 3 s plus a 1 s heartbeat), with a sound and a banner.
- On reconnect, the buffered segments upload in the background at low priority. The server merges them into the timeline by wall-clock timestamp.

**Session end:** the agent stops capturing, flushes and uploads its buffer, then sits idle with ~0% CPU. The retention clock starts.

### 2.6 Security

- **Transport:** TLS 1.3 everywhere. Agents pin the server's certificate or the internal CA, distributed by GPO.
- **Student machine identity:** each `ew-service` generates a key pair at install, stored with DPAPI as LocalMachine. An admin approves it once in the dashboard (or AD CS auto-enrollment does it). This stops fake clients from injecting screens.
- **Teacher authentication:** dashboard accounts with Argon2id password hashing plus optional TOTP, or LDAP/AD group membership (`ExamWatch-Teachers`). Sessions use HttpOnly SameSite cookies. Every login and view of a recording goes in the audit log.
- **Students can't view anything,** and the agent opens no listening ports.
- **At rest:** each session gets an AES-256-GCM key, wrapped by a server master key protected with DPAPI. The 48 h purge deletes the segments and the session key.
- **Tamper resistance:** the service is protected from stopping by standard users. If the agent is killed, the service relaunches it and the dashboard logs a "process restarted" event. Changes to the student's display (extra monitor or a resolution change) raise an alert.

### 2.7 Optional: unauthorized app and website alerts (Phase 5)

- The agent samples the **foreground process name and window title** once per second. That costs almost nothing: one call to `GetForegroundWindow` and one to `QueryFullProcessImageName`. The server checks these against an allow-list or block-list per session.
- **Browser URL:** read the address bar of Edge or Chrome through UI Automation, only when the foreground window is a browser, at most once every 2 s.

### 2.8 Invisible capture: zero noticeable performance impact

**Requirement:** while capture runs, the student PC must feel exactly like normal browsing or regular software use. That means no lag, stutter, dropped frames, cursor delay or slowdown. The monitoring notice stays visible; "invisible" refers only to performance.

**Budget.** Each line is a measured acceptance criterion, not a goal.

| Metric | Limit (agent running vs. agent off) | How it's measured |
|---|---|---|
| Foreground frame time (browser scroll, video, Office) | p99 increase ≤ 0.5 ms; **zero added dropped or late frames** | PresentMon, 10 min A/B runs |
| Input-to-photon latency (typing, mouse) | No measurable increase (≤ 1 ms, which is within noise) | Windows Performance Recorder input events + PresentMon latency |
| Cursor | Never touched by us. No hooks, and no cursor drawn into the capture path | Code review + WPR trace |
| Agent CPU | ≤ 1.5% average, ≤ 5% hard cap (Job Object) | PDH counters over a 3 h soak |
| GPU 3D engine | ≤ 1% average | Task Manager / PDH "GPU Engine" counters |
| Agent RAM | ≤ 150 MB working set, flat over 3 h | Soak test |
| Disk I/O | ≈ 0 while online; very-low I/O priority while buffering | ETW disk trace |
| Network | ≤ 400 kbps upload, low QoS priority, paced | Server stats + `netsh` QoS check |
| Login and startup | Agent startup spends ≤ 300 ms of CPU, and none on the logon critical path | WPR boot trace |

**How each limit is met:**

1. **No CPU pixel work, ever.** Desktop Duplication already holds the composed frame on the GPU. We copy it to our own texture with `CopyResource` (a GPU-to-GPU copy, taking microseconds) and call `ReleaseFrame` immediately. Duplication is therefore never held, and DWM is never stalled. Scaling and colour conversion run in the GPU's video processor (D3D11 VideoProcessor) at idle GPU priority, at 2–3 tiny jobs per second. Encoding runs on **NVENC**, separate silicon from the 3D and CUDA cores. On GT 730 PCs only the downscaled frame touches the CPU (one ≈1.4 MB copy, 2×/s), encoded by a single low-priority thread.
2. **No GDI, no hooks, no injection.** No `BitBlt`, which can force DWM synchronisation and cause stutter. No `SetWindowsHookEx`, no DLL injection, and no driver mirroring. The cursor is never composited by us; its position and shape come from Desktop Duplication metadata and are drawn **on the teacher's side**, so the student's cursor path is untouched.
3. **Capture only what's needed, when it's needed.** If Desktop Duplication reports no change, the agent does nothing: 0% CPU and 0% GPU. When the screen changes, REC samples at only 2 fps and THUMB at 1 fps; work never runs at the display's refresh rate. LIVE (up to 15 fps) runs only while a teacher has that screen enlarged, and is still capped.
4. **Always yield to the student.**
   - Process priority `BELOW_NORMAL`, plus EcoQoS (efficiency cores on hybrid CPUs, low clocks).
   - GPU scheduling priority `IDLE` on the agent's D3D11 device.
   - Background I/O priority (very low) and a low-priority network QoS policy.
   - Thread affinity: no pinning, so the OS scheduler is free to put the agent wherever the student's apps aren't.
5. **Self-throttling governor.** Once per second the agent reads system CPU load, GPU engine usage and foreground-app frame pacing (via DWM timing stats). If the system is busy — CPU over 70%, GPU 3D over 80%, or the foreground app missing frames — it steps down: LIVE drops to 5 fps, then THUMB pauses, then REC drops to 1 fps. It recovers slowly once the system has been calm for 10 s. **The student's work always wins over recording smoothness.**
6. **Drop frames, never queue them.** Each stage holds a single slot. If the encoder is busy, the new frame replaces the waiting one, so memory never grows and nothing ever spikes to catch up.
7. **No allocation in the steady state.** Textures, encoder buffers and network buffers are pre-allocated pools created once at start. This avoids allocator churn, page faults and slowdown over long runs.
8. **Smooth start.** Initialisation is lazy and spread over about 2 s after V1 is pressed. The encoder is warmed up before the first frame, so there's no visible hitch when the student confirms their ID.

**Verification** (added to the test plan): an A/B test on a real lab PC with a fixed workload script — Edge scrolling a long page, a 1080p YouTube video, typing in Word, and a web-based exam form — run 10 times with the agent off and 10 times with it on. PresentMon and WPR traces are compared automatically, and **any metric outside its budget fails the build.** We also run a blind check during the mock exam: students use PCs with the agent on and off, and we record whether they notice a difference.

### 2.9 Recording resolution: small files, still readable

**Rule:** record at **720 px tall, keeping the screen's aspect ratio, and never upscale.**

| Student screen | Recorded as |
|---|---|
| 1920×1080 | 1280×720 |
| 1366×768 | 1280×720 |
| 1920×1200 / 1680×1050 | 1152×720 |
| 1280×720 or smaller | Native (no scaling) |
| 2560×1440 / 4K | 1280×720, with sharper scaling (below) |

**Why 720p:** it's the lowest resolution at which normal UI text (about 9–11 pt at 100–125% Windows scaling) is still clearly readable after a 1.5× downscale from 1080p. At 540p, small text and code turn to mush. Going above 720p roughly doubles file size and encoder load for little gain in readability.

**Keeping text crisp at a low bitrate:**
- **Downscale filter:** the VideoProcessor's high-quality scaling mode (the driver's high-quality scaling path) instead of bilinear. This keeps glyph edges sharp.
- **Rate control:** VBR with a **quality floor** (a QP cap of about 28) instead of strict CBR. When text changes, that frame gets the bits it needs; static screens cost nearly nothing. Peaks reach about 600 kbps, and the average stays around 150–300 kbps.
- **Low frame rate, high quality per frame:** 2 fps. For review, each frame being readable matters more than smooth motion.
- **Encoder tuning:** the content type is set to "screen/text", with adaptive quantisation on, so flat areas get fewer bits and text edges get more.
- **Size:** about 270 MB per student for a 2 h exam, as in §2.4.

**Readability is tested, not assumed.** An automated test renders a page of text at 9, 10, 11 and 12 pt in Segoe UI, Calibri and Consolas. It records the page through the real pipeline and runs OCR (Tesseract) on the decoded frames. **Pass: ≥ 98% character accuracy at 10 pt and above at 1080p → 720p.** The same test runs against your actual exam software during the mock exam.

Live viewing quality (thumbnails versus enlarged) is set separately by adaptive streaming (§2.4) and doesn't affect recording quality.

### 2.10 Recording storage on Google Drive

**Target folder:** `the school's Drive folder (URL kept in the server config, not in this repository)`. The folder ID is set in the server config as `storage.gdrive.folder_id`.

**Why Drive is not written to live during the exam:** a write to Drive goes over the school's internet link to Google. If that link slows down or drops mid-exam, recordings must not be lost and students must not notice. So storage has two tiers:

```
Student agent ──LAN/QUIC──▶ ew-server local SSD (hot buffer, encrypted fMP4 segments)
                                   │  background uploader, rate-limited, resumable
                                   ▼
                        Google Drive folder (system of record)
                        /ExamWatch/<exam-date>_<exam-name>/<lab>/<student-id>/<segment>.ewseg
```

| Concern | Design |
|---|---|
| **Internet outage during an exam** | Has no effect on the exam. Segments keep landing on the server's SSD, and the uploader resumes when the internet returns. The dashboard plays from the local copy until the Drive copy is confirmed. |
| **Upload speed** | Upload starts as segments are finalised (every 1 min), capped by default at **20 Mbps** so it never saturates the school's uplink. Configurable. A 27 GB exam takes about 3 h to upload at 20 Mbps, and about 1.2 h at 50 Mbps. |
| **Upload method** | Drive API v3 **resumable uploads** (`uploadType=resumable`). An interrupted upload continues from the last confirmed byte. Each segment's SHA-256 is stored in `appProperties` and verified before the local copy is released. |
| **Privacy** | Every segment is **encrypted on the server with AES-256-GCM before it is uploaded.** Google, and anyone who gets the folder link, sees only ciphertext. Decryption keys never leave the ExamWatch server. Recordings are viewed only through the teacher dashboard, which streams from Drive and decrypts on the server. |
| **Folder sharing** | The link you shared ends in `usp=sharing`. If the folder is set to "Anyone with the link", **change it to Restricted.** Encryption keeps the recordings unreadable either way, but file names (student IDs and lab names) would still be visible, so names are also replaced with opaque IDs; the mapping lives only in the server database. |
| **Authentication** | OAuth 2.0 with a Google account that owns or can edit the folder, scope `drive.file` (the app can only touch files it created). The refresh token is stored on the server encrypted with DPAPI. A service account can't be used here: service accounts have no storage quota in a personal ("My Drive") account. |
| **48 h retention** | The purge job deletes each session's files with `files.delete`, which skips the Drive trash and deletes permanently, and then destroys the session key. Every purge is logged. A daily check lists the folder and deletes anything past its retention time that was missed (for example during an outage). |
| **Local SSD** | Holds the current exam plus anything not yet uploaded. **256 GB is enough** (≥ 5 exams of headroom). The local copy is deleted once Drive confirms the checksum, or after 48 h, whichever comes first. |
| **Quota** | See the warning below. |

> ⚠️ **Drive capacity is a blocker on a free account.** A free Google account has **15 GB** in total, which is shared with Gmail and Photos. One 100-PC, 2 h exam is about **27 GB**, and 48 h retention at up to 4 exams a day peaks around **220 GB**. You'll need **Google One 2 TB** (or a Google Workspace shared drive). With only 100 GB, ExamWatch would hold about 3 exams. The dashboard shows Drive's free space, and the **Open exam** button warns if the estimated size won't fit.

> ⚠️ **Daily upload cap:** Google limits each account to 750 GB of uploads per day. Our peak is ~110 GB/day, which is well inside that limit.

### 2.11 Installers: two double-click setups, no configuration afterwards

| | **ExamWatch Teacher Setup** (`ExamWatch-Teacher-Setup.exe`) | **ExamWatch Student Setup** (`ExamWatch-Student.msi`) |
|---|---|---|
| Where | The PC that will be the ExamWatch server (a teacher PC or a dedicated box) | Every student PC (114) |
| Steps | Welcome → Licence → Install → Finish ("Open ExamWatch" ticked) | Welcome → Install → Finish |
| What it does | Installs `ew-server` as an auto-start Windows service with restart on failure. Generates the config, a random enrolment token, the TLS certificate and the master key. Pre-loads the 5 labs from the lab sheets. Adds Windows Firewall rules (UDP 4433, TCP 8443). Trusts its own certificate on this PC, so the browser shows no warning. Adds a desktop and Start-menu shortcut **ExamWatch** that opens the dashboard in its own app window (Edge `--app`). | Installs `ew-service` (watchdog, auto-start, restart on failure) and `ew-agent`. Adds the **V1** shortcut on the Public Desktop and in Start. Creates the `%ProgramData%\ExamWatch` buffer folder (SYSTEM-only ACL). Adds a firewall rule (outbound only; no inbound ports). |
| First run | The dashboard opens a short **first-run wizard** in the browser: create the admin password, confirm the lab names. No files to edit. | Nothing. The agent connects by itself and the PC appears in the teacher's grid. |
| Uninstall | Apps & features. Asks whether to keep recordings until retention expires. | Apps & features, or GPO removal. Only an admin can uninstall. |
| Silent / GPO | `/VERYSILENT` for scripted installs | Assign the same MSI by GPO (Computer Configuration → Software Installation); no transform needed |

**How the student installer needs no configuration (works with Windows 11 Smart App Control).** The student MSI is **never modified after signing**, because a modified file would be blocked on Windows 11 PCs with Smart App Control. Instead the teacher dashboard's **Add student PCs → Download student installer** gives a ZIP containing:

* `ExamWatch-Student.msi`: the signed, generic installer;
* `examwatch-student.cfg`: this server's address(es), certificate pin and enrolment token, signed by the server's key so it can't be tampered with.

Extract both into one folder (USB stick or network share) and double-click the MSI: its setup step reads the `.cfg` next to it and writes the settings to `HKLM\SOFTWARE\ExamWatch`. For Group Policy, put both files on the share and assign the MSI; it finds the `.cfg` the same way. If the `.cfg` is missing, the installer stops with a plain-language message ("Copy examwatch-student.cfg next to this installer") rather than installing an agent that can't connect.

**Labs without editing subnets.** The first time PCs from a new subnet connect, they appear under **Unassigned** with a one-click "Assign to lab…" action. The server remembers that subnet → lab mapping, so every other PC on that VLAN lands in the right lab automatically.

**Tooling:** WiX Toolset (MSI) for both packages, with a WiX "bundle" wrapping the teacher MSI as a friendly `.exe`. This needs the .NET SDK and WiX installed on the build PC.

**Signing:** both installers, every `.exe`/`.dll` inside them, and the MSI's custom-action DLL are signed (R27). Nothing is modified after signing.

**Acceptance test:** on a clean Windows 10 VM **and** a clean Windows 11 VM with Smart App Control **on**, a person who has never seen ExamWatch installs Teacher Setup, then installs the downloaded Student MSI on 2 PCs. Within 5 minutes and without opening any file, both PCs appear in the dashboard and an exam can be run end to end.

---

## 3. Phased plan

Each phase ends with its own test gate. Nothing moves forward with failing tests.

| Phase | Scope | Exit criteria |
|---|---|---|
| **0. Spikes (2–3 days)** | (a) Desktop Duplication + encode on **one PC from each hardware class**: RTX 3060 (Lab 1), RTX 4060 Ti (Game lab), GTX 1050 Ti (Prof. Dev), and **GT 730 + i5-4th gen + HDD (Visual Arts or 3D Lab 2), the worst case**: measure CPU, GPU, RAM and latency. (b) QUIC from a student VLAN to the server through your firewall. (c) Decode 50 tiles with WebCodecs in Edge on a teacher PC. | Every metric in the **§2.8 invisibility budget** passes the PresentMon/WPR A/B test on a real lab PC. **§2.9 OCR readability test passes** (≥ 98% at 10 pt+). Screen-to-glass latency < 300 ms. UDP 4433 open, or the TCP fallback is confirmed. |
| **1. MVP: one lab** | `ew-server` (ingest, fan-out, record, sessions, roster, auth), `ew-agent` (**V1 button** → ID entry, notice, capture → THUMB/REC/LIVE), `ew-service` (watchdog), and the dashboard (login, lab grid with "Waiting"/"Live"/"Offline" tiles, **student-joined notifications**, enlarge, open/end exam, recordings list and playback). | 50 simulated agents plus 3 real PCs run a full 2 h exam plus 1 h overrun (3 h) with zero errors. Offline alert ≤ 3 s. Recordings play back with correct timestamps. |
| **2. Resilience and caps** | Local buffer and upload, adaptive bitrate, Job Object caps, join jitter, crash/kill/reboot recovery, memory-leak soak tests. | Failure test suite passes (§6). A 3 h soak (1.5× exam length) shows RSS growth < 5% after warm-up. |
| **3. Multi-lab** | **All labs** overview (every lab on one screen), lab switching, side-by-side view, multiple teachers, per-lab permissions, cross-lab join notifications. | 200 simulated agents across 5 labs. Dashboard stays above 55 fps. |
| **4. Security, retention and Drive** | Machine enrollment, encryption at rest, **Google Drive uploader** (OAuth, resumable, checksummed, rate-limited), 48 h purge from both the local SSD and Drive, audit log, optional TOTP or LDAP. | Security checklist passes. Purge is verified to leave no recoverable data. |
| **5. Extras and rollout** | App and website alerts, MSI packages, GPO templates (ADMX), deployment guide, pilot mock exam. | Mock exam in a real lab passes the verification checklist. |
| **6. Installers** | Teacher Setup (.exe wrapping an MSI) and Student Setup (.msi, per-site stamped), first-run wizard, subnet auto-learning, uninstall paths, signing. | Clean-VM acceptance test (§2.11) passes, plus install/upgrade/uninstall/reinstall on Windows 10 and 11. |

**Benchmark against Veyon:** in Phase 0 and again in Phase 3, run Veyon and ExamWatch on the same lab PCs. For each, measure student CPU, RAM, GPU and network, teacher CPU, and latency (a timestamp overlay photographed against the dashboard).

---

## 4. Risk analysis: anything that could disrupt an exam

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| R1 | Agent causes any noticeable lag, stutter or cursor delay | Med | **Critical** | §2.8: zero CPU pixel work, no GDI or hooks, the cursor is never touched, GPU-side scaling and NVENC (single-thread capped software on GT 730), idle GPU priority, a self-throttling governor that yields to the foreground app, drop-don't-queue, no steady-state allocation, Job Object hard caps. **Enforced by the automated PresentMon/WPR A/B test in CI and on a real lab PC before every release.** |
| R1b | Recordings too blurry to read text | Low | High | §2.9: 720p, high-quality downscale, VBR with a quality floor, screen-content encoder tuning. Automated OCR readability test gates every release. |
| R2 | Agent crash shows an error dialog to the student | Low | High | `SetErrorMode` / WER suppression, so no dialog appears. The watchdog restarts the agent silently, and the crash is logged to the server. |
| R3 | Agent memory leak over a 2 h exam | Low | High | Rust, plus fixed-size buffer pools. Leak tests over 3 h soaks. The watchdog recycles the agent if RSS exceeds its cap; recording loses < 2 s. |
| R4 | Network drop or switch reboot | Med | Med | Local encrypted buffer (about 12 h of capacity), automatic reconnect with backoff (0.5 s → 5 s max), upload after reconnect. The teacher is alerted within 3 s. |
| R5 | Server crash or power loss | Low | **Critical** | Agents keep recording into their local buffer and upload when the server returns. The server runs as a Windows service with automatic restart. SQLite WAL survives a crash. UPS recommended. A warm standby server is optional in Phase 4. |
| R6 | Teacher PC crash or browser closed | Med | Low | No effect on students or recordings. Re-open the dashboard and continue. |
| R7 | Many PCs connect at once (flood) | Med | Med | Join jitter, staggered keyframes, server token bucket, and THUMB at 1 fps. The budget is ~44 Mbps against a 1 Gbps link. |
| R8 | Firewall or VLAN blocks UDP | Med | High | Phase 0 test. Automatic fallback to TLS over TCP 443. The GPO deploys the firewall rule. |
| R9 | NVENC unavailable (GT 730 labs, an old driver, or the GeForce concurrent-session limit) | **Certain for 54 PCs** | Med | Media Foundation software fallback, built in from day one and tested as a first-class path. ExamWatch uses at most 3 NVENC sessions; if opening one fails, that stream falls back to software. The dashboard shows each PC's encoder path in pre-flight. |
| R10 | Capture lost on UAC prompt, lock screen, display mode change or RDP | Med | Low | Handle `DXGI_ERROR_ACCESS_LOST`: re-create the duplication. The tile shows "secure desktop" instead of freezing, and the event is logged. |
| R11 | Wrong or duplicate Student ID | Med | High | Roster match, duplicate block, confirm by re-entry, and teacher reassignment. Every ID change goes in the audit log. |
| R12 | Student kills the agent or unplugs the network to cheat | Med | High | Standard users can't stop the service. Killing the agent triggers an instant restart and an alert. A disconnect is visible within 3 s, and the local buffer still records the screen during the outage. |
| R13 | Second monitor or projector hides activity | Low | Med | Capture all outputs and alert when the monitor count changes. |
| R14 | Windows Update reboot during an exam | Med | High | Deployment guide: GPO "active hours" or maintenance windows for exam days. The service starts at boot and the agent at logon, and it resumes the active session automatically. |
| R15 | Recordings leaked or kept too long | Low | High | Encryption at rest, teacher-only access, audit log, enforced 48 h purge with key destruction. |
| R16 | Antivirus flags or blocks the agent | Med | Med | Signed binaries. AV exclusions documented in the deployment guide. Phase 0 test with your AV. |
| R17 | Clock drift across PCs breaks timestamps | Low | Med | Segment times use server-side receive time plus agent monotonic offsets (synced at connect). Recommend domain NTP. |
| R19 | Student doesn't press V1, or presses it late | Med | High | The PC shows as a grey "Waiting" tile in the teacher's grid, with a per-lab "started/total" counter. Optionally, the teacher gets an alert N minutes after the exam opens. The agent stays connected to the server before V1 (heartbeat only, no capture), so the teacher can still tell an offline PC from a student who hasn't started. |
| R20 | V1 pressed when no exam is open | Med | Low | V1 shows "No exam is open" and captures nothing. The attempt is logged. |
| R21 | Internet or Google outage during an exam | Med | Low | Recordings land on the server's SSD first. Upload resumes later. The exam is unaffected (§2.10). |
| R22 | Drive quota full | **High on a free account** | High | Google One 2 TB required. The **Open exam** button checks free space against the estimated size. The uploader pauses (it never deletes unconfirmed data), and the dashboard shows a warning. |
| R23 | Drive folder link exposed | Med | Med | Folder set to Restricted, segments encrypted before upload, opaque file names, `drive.file` scope. |
| R24 | OAuth token revoked or expired | Low | Med | Dashboard banner plus an email to the admin. Segments queue on the local SSD until the account is re-authorised. |
| R25 | Windows 10 is out of support, with no security patches | High | Med | Not caused by ExamWatch, but recommend ESU or Windows 11 where the hardware allows. ExamWatch only targets Windows 10 1809+ APIs. |
| R26 | Heavy GPU work (Blender, Unity) in the 3D and Game labs | Med | High | NVENC is separate silicon, scaling runs at idle GPU priority, and the governor backs off when the GPU is busy. GPU-heavy A/B test in Phase 0. |
| R27 | Windows blocks unsigned ExamWatch programs (Smart App Control on Windows 11, or a WDAC/AppLocker policy) | **High**: some lab PCs run Windows 11 | **Critical** (the agent doesn't start, so the student isn't monitored) Sign releases if possible (see open decision 6 for no-cost routes). Without signing: check every Windows 11 lab PC with `tools/check-smart-app-control.ps1` before rollout, and switch Smart App Control off where it's On or Evaluation; deploy by GPO, share or USB so no SmartScreen prompt appears. Pre-flight: the dashboard shows any PC whose agent never connected after install. On domain PCs, an AppLocker/WDAC publisher rule can be set by GPO. Found on the development PC (Windows 11) on 2026-10-29: Smart App Control blocked every unsigned build. |
| R18 | Recording disk fills up | Low | High | Pre-flight disk check before Start, a dashboard warning at 80% full, and the per-exam estimate shown in the session dialog. |

---

## 5. Open decisions for you

1. **Web dashboard opened in Edge/Chrome, or a native Windows app?** I recommend the web dashboard (§2.2).
2. **Rust** for the student agent and server. I recommend it (§2.3); the alternative is C++.
3. **Student roster:** will you have a CSV of Student IDs per exam? Without one I can still block duplicates, but I can only catch typos with a format check or checksum.
4. **Teacher login:** dashboard-only accounts, or AD/LDAP group membership?
5. **V1 flow:** I've assumed the teacher first *opens* the exam with one click, and that this enables V1, so nothing is ever recorded outside an exam. Should V1 instead work at any time, with no teacher action first? Also, is "V1" the exact button label you want, a desktop icon, or something else?
6. **Code signing without buying a certificate** (decided 2026-10-30: no paid certificate). In order of preference:
   a. **Check first.** Run `tools/check-smart-app-control.ps1` against the lab PCs. Smart App Control is only switched on for clean, unmanaged Windows 11 installs, so domain-joined lab PCs are usually already **Off** and run unsigned ExamWatch fine.
   b. **Deploy the student MSI by Group Policy** (or from a network share or USB). SmartScreen's "Unknown publisher / Run anyway" warning only appears for files downloaded from the internet, so these routes show no warning.
   c. **Any lab PC that reports On or Evaluation:** IT switches Smart App Control off on it (Windows Security → App & browser control). Windows offers no Group Policy for this. On many Windows 11 builds it can't be switched back on without reinstalling Windows, and the institution's IT controls (Defender, AppLocker) take over.
   d. **Free or near-free signing:** if ExamWatch is published as open source, SignPath Foundation signs open-source releases free of charge, and Certum sells an open-source developer certificate for roughly €30–70 a year. Either removes the issue for good.
   e. **Not an option:** a self-signed or internal-CA certificate. Smart App Control only trusts Microsoft-recognised certificates, so this doesn't help on PCs where it's On (it does help with AppLocker/WDAC).
7. **Google Drive capacity:** which plan is on the account that owns the folder (free 15 GB, 100 GB, or 2 TB)? And what is the school's internet upload speed?
8. **Firewall:** can UDP 4433 be opened from the lab VLANs to the server? If not, it falls back to TCP 443.

Deliverables 5–7 (code, test scripts and verification checklist, deployment guide) start after you approve this document, beginning with Phase 0 and the Phase 1 MVP.
