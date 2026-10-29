# ExamWatch: first lab test (Phase 0)

**Goal:** prove on real lab hardware that ExamWatch works end to end, and that students **cannot feel it** (design §2.8), before it is used in any exam.
**Build under test:** `v0.1.0-alpha.1` from the [Releases page](https://github.com/Monokayser/examwatch/releases).
**Time needed:** about 2½ hours for parts A–G, plus a 3-hour soak (part H) that can run unattended.
**People:** one person to run the test (IT or a teacher), and, for part F, two or three student volunteers.

> Every step has a **pass condition**. Tick it only if it is fully met. If a step fails, keep going where you can, and send the logs listed in part J.

---

## What you need

| Item | Which one | Why |
|---|---|---|
| **Server PC** | Any PC with Windows 10/11 64-bit, wired, reachable from the lab VLANs. A teacher PC is fine for this test. | Runs ExamWatch Teacher (server + dashboard) |
| **Student PC 1: worst case** | One PC from **Visual Arts (1019)** or **3D Animation Lab 2**: Core i5 4th gen, **GT 730**, hard disk only | No hardware encoder, the oldest CPU and disk. If this PC can't feel ExamWatch, none can. |
| **Student PC 2: GPU-heavy** | One PC from **3D Animation Lab 1 (RTX 3060)** or **Game & AR/VR (RTX 4060 Ti)** | Hardware encoder (NVENC), Blender/Unity workloads, 12th/13th-gen hybrid CPU |
| **Windows versions** | If possible, one of the two student PCs on **Windows 10** and the other on **Windows 11** | The labs run both |
| USB stick | Any | To carry the student installer |
| A second PC for load (optional) | Any spare PC | To simulate the rest of the lab (part G) |
| Tools on Student PC 1 and 2 | [PresentMon](https://github.com/GameTechDev/PresentMon/releases) (free, from Intel), Task Manager | Frame-time measurement (part F) |

Fill in before you start:

| | Value |
|---|---|
| Server PC name / IP address | |
| Student PC 1 name / lab / Windows version | |
| Student PC 2 name / lab / Windows version | |
| Lab VLAN subnet(s) of the student PCs (e.g. 10.10.19.0/24) | |
| Tester / date | |

---

## A. Pre-flight (10 min)

| # | Step | Pass when | ✔ |
|---|---|---|---|
| A1 | On every **Windows 11** PC in the test, run `powershell -ExecutionPolicy Bypass -File tools\check-smart-app-control.ps1` (from the source ZIP on GitHub) | `SmartAppControl` is **Off** or **Not present**. If **On** or **Evaluation**, switch it off in Windows Security first (design R27) | ☐ |
| A2 | Ask IT: is **UDP 4433** allowed from the student VLAN(s) to the server PC, and **TCP 8443** from teacher PCs? | Both allowed | ☐ |
| A3 | Check that both student PCs show the correct time (domain time sync) | Within 1 minute of the server | ☐ |

## B. Install the teacher side (10 min)

| # | Step | Pass when | ✔ |
|---|---|---|---|
| B1 | On the server PC, download and run **ExamWatch-Teacher-Setup.exe**. If Windows shows "Windows protected your PC", click **More info → Run anyway** (the build isn't signed yet). Accept the licence → **Install** → **Launch** | The installer finishes without errors; an **ExamWatch** window opens | ☐ |
| B2 | In the window: create the admin account (username + a password of at least 10 characters, typed twice) | You land on the dashboard. **No certificate warning** appears | ☐ |
| B3 | Close the window and reopen it from the **ExamWatch** desktop shortcut | It opens straight to the dashboard (or the sign-in page) | ☐ |
| B4 | From **another** PC on the network, open `https://<server name>:8443` in Edge | The sign-in page appears. A certificate warning is expected on other PCs for now (see J) | ☐ |

## C. Install the student side (10 min)

| # | Step | Pass when | ✔ |
|---|---|---|---|
| C1 | In the dashboard click **Add student PCs** and save `ExamWatch-Student.zip` | The ZIP downloads | ☐ |
| C2 | Extract **all** files to the USB stick (same folder) | The folder has `ExamWatch-Student-Setup.exe`, `ExamWatch-Student.msi`, `examwatch-student.cfg` | ☐ |
| C3 | On Student PC 1 and 2: run `ExamWatch-Student-Setup.exe` from the stick → **Install** | Finishes without errors; a **V1** icon appears on the desktop | ☐ |
| C4 | Copy only `ExamWatch-Student-Setup.exe` to the desktop of one PC and run it from there | It **refuses** and explains that `examwatch-student.cfg` must be next to it | ☐ |
| C5 | Look at the dashboard | Both PCs appear within ~10 s, at first under **Unassigned** (the server doesn't know your lab networks yet) | ☐ |
| C6 | In the **Unassigned** section, each network is listed (e.g. *Network 10.10.19.0/24 · 1 PC*). Choose the right lab in the drop-down → **Assign** | Both PCs move to their lab at once. No file is edited | ☐ |
| C7 | Restart one student PC | It comes back **directly in its lab**, not under Unassigned (the network is remembered) | ☐ |

## D. Exam flow (25 min)

| # | Step | Pass when | ✔ |
|---|---|---|---|
| D1 | On Student PC 1 double-click **V1** before any exam is open | The window says **"No exam is open"**. Nothing is recorded (dashboard tile: *No exam*) | ☐ |
| D2 | Dashboard → **Open exam** → name "Lab test", tick the lab, paste a roster with 3 IDs (e.g. `TEST-001`, `TEST-002`, `TEST-003`) → **Open exam** | The exam pill appears with a running timer; both tiles show **Waiting for V1** | ☐ |
| D3 | PC 1: **V1** → type `TEST-001` and `TEST-010` (different) → Start exam | "The two IDs don't match" appears; nothing starts | ☐ |
| D4 | PC 1: type `TEST-999` twice → Start | "This ID is not on the list for this exam" | ☐ |
| D5 | PC 1: type `TEST-001` twice → Start | The window closes; the **"Screen monitored · TEST-001"** pill appears bottom-right; the teacher gets a **"TEST-001 … started"** pop-up; the tile turns live within 2 s | ☐ |
| D6 | PC 2: **V1** → `TEST-001` twice | Refused: **"already in use on <PC 1>"**; the teacher sees a red duplicate alert | ☐ |
| D7 | PC 2: **V1** → `TEST-002` twice | PC 2 goes live | ☐ |
| D8 | Click PC 1's tile | The enlarged view opens and switches to **Full quality** within 2 s; moving a window on PC 1 shows on the teacher screen within ~0.3 s | ☐ |
| D9 | In the enlarged view press → / ← | It moves to the next/previous PC | ☐ |
| D10 | **All labs** tab, then the layout buttons (stacked / side by side) and tile sizes S/M/L | Every view works, with no flicker or jumping tiles | ☐ |
| D11 | Type `TEST-0` in the search box | Only matching tiles are shown | ☐ |

## E. Failure handling (25 min)

| # | Step | Pass when | ✔ |
|---|---|---|---|
| E1 | **Pull PC 1's network cable** and start a stopwatch | Within **3 s** the tile turns red with an offline timer, a red alert pops up and a beep sounds | ☐ |
| E2 | Keep working on PC 1 for **2 minutes**, then plug the cable back in | The tile is live again within ~10 s and a "back online" message appears | ☐ |
| E3 | **Recordings** → Lab test → TEST-001 → **Play**, then jump to the time of the cable pull | The 2 minutes offline **are in the recording** (uploaded after reconnect); the clock on screen matches | ☐ |
| E4 | On PC 1, open Task Manager → Details → end **ew-agent.exe** | It's back within ~5 s, still registered as TEST-001, without retyping. The pill reappears | ☐ |
| E5 | On PC 1, **restart Windows** and log in again | After login the PC rejoins as TEST-001 by itself | ☐ |
| E6 | On PC 1, press Win+L (lock), wait 10 s, unlock. Then open something that asks for admin (UAC) | While locked or in the UAC prompt the tile keeps its last picture (Windows hides the secure screen); within a few seconds of unlocking it is live again. PC 1 never freezes | ☐ |
| E7 | (If available) plug a second monitor into PC 2 | The teacher gets a "monitor count changed" alert | ☐ |
| E8 | In PC 2's enlarged view → **Reset PC** → confirm | PC 2's pill disappears; when the student presses V1 the window explains the PC was reset; TEST-002 can start again (on PC 2 or PC 1) | ☐ |
| E9 | **Restart the server PC** in the middle of the exam (or stop and start the *ExamWatch Server* service) | Students notice nothing; after the server is back, both PCs reconnect and the recording continues | ☐ |
| E10 | Close the dashboard window and reopen it | The exam is still running; nothing was affected | ☐ |

## F. "Students can't feel it" performance test (40 min): the most important part

Do this on **both** student PCs, with and without ExamWatch capturing. *Without* = no exam open, or ExamWatch uninstalled. *With* = registered with V1 and the teacher has the screen **enlarged** (the heaviest case).

**Workload (same each time, 5 minutes):** scroll a long web page in Edge for 1 min → play a 1080p YouTube video for 2 min → type in Word for 1 min → on PC 2 also orbit a scene in Blender (or Unity Play mode) for 1 min.

| # | Measure | How | Pass when | PC 1 | PC 2 |
|---|---|---|---|---|---|
| F1 | ExamWatch CPU | Task Manager → Details → `ew-agent.exe` CPU column, average over the workload. Also shown in the dashboard's enlarged view | **≤ 1.5 %** average; never above 5 % | | |
| F2 | ExamWatch memory | Task Manager → Details → `ew-agent.exe` Memory | **≤ 150 MB** | | |
| F3 | Frame pacing | Run PresentMon during the workload *without*, then *with*. Compare using `tools\frametime-compare.ps1 off.csv on.csv` | p99 frame time increase **≤ 0.5 ms**; no extra dropped frames | | |
| F4 | Typing / mouse | Type quickly and move windows around while capturing | No delay, no cursor lag that you can feel | | |
| F5 | Blind test | 2–3 students use each PC for 5 minutes each, once with and once without capture, **not told which**. Ask "did either feel slower?" | Nobody can tell which was which better than chance | | |
| F6 | Recording readability | Open a document with 9, 10, 11 and 12 pt text; after the exam play the recording | **10 pt and larger is clearly readable** | | |
| F7 | Encoder used | Dashboard → enlarge → title line | PC 1: *Software encoder*; PC 2: *GPU encoder* | | |

**PresentMon commands** (run in an administrator terminal, while the workload runs in Edge):

```bash
PresentMon.exe --process_name msedge.exe --timed 300 --terminate_after_timed --output_file off.csv
```
```bash
PresentMon.exe --process_name msedge.exe --timed 300 --terminate_after_timed --output_file on.csv
```
```bash
powershell -ExecutionPolicy Bypass -File tools\frametime-compare.ps1 off.csv on.csv
```

## G. Load: a full lab (20 min, optional but recommended)

| # | Step | Pass when | ✔ |
|---|---|---|---|
| G1 | On a spare PC, run the simulator for the rest of a lab: `ew-sim.exe --server <server>:4433 --pin <contents of C:\ProgramData\ExamWatch\data\server.fingerprint.txt> --token <enroll_token from C:\ProgramData\ExamWatch\ew-server.toml> --labs VA-:55 --rec 1280x720`. The simulated PCs appear under **Unassigned** (they come from the spare PC's network); assign that network to a lab with **Assign** | 55 simulated PCs appear and start within ~20 s of opening an exam | ☐ |
| G2 | With 55 simulated + 2 real PCs live, use the **All labs** view and enlarge a few screens | The dashboard stays smooth; the real PCs still meet F1–F4 | ☐ |
| G3 | Server PC Task Manager | Server CPU below 30 %, network well below its link speed | ☐ |

## H. Exam-length soak (3 h, unattended)

| # | Step | Pass when | ✔ |
|---|---|---|---|
| H1 | Leave the exam open with both PCs in use (or a looping video) for **3 hours** | Nothing disconnects; no alerts except those you caused | ☐ |
| H2 | Compare `ew-agent.exe` memory at 10 min and at 3 h | Grows by **less than 5 %** | ☐ |
| H3 | Play back the last 10 minutes of each recording | Plays with correct timestamps | ☐ |

## I. End of exam and clean-up (5 min)

| # | Step | Pass when | ✔ |
|---|---|---|---|
| I1 | Dashboard → **End exam** → confirm | Both pills disappear immediately; tiles show *No exam*; `ew-agent.exe` CPU drops to ~0 % | ☐ |
| I2 | **Recordings** list | The exam shows "kept until <date + 48 h>" | ☐ |
| I3 | Uninstall from *Settings → Apps* on one student PC and on the server | Uninstalls cleanly; V1 icon and services removed | ☐ |

---

## J. If something fails: what to send

| Where | File |
|---|---|
| Student PC | `%LOCALAPPDATA%\ExamWatch\agent.log` (of the logged-in student) and `C:\ProgramData\ExamWatch\logs\service.log` |
| Server PC | `C:\ProgramData\ExamWatch\data\logs\server.log` |
| Installer problems | Run the installer again with `/log install.log` and send that file |

Also note the step number, what you expected, and what happened (a phone photo of the screen helps).

**Expected in this build (not failures):**
* certificate warning when opening the dashboard from a PC other than the server (fix: import `C:\ProgramData\ExamWatch\data\server.crt.pem` into "Trusted Root" on teacher PCs, or push it by GPO);
* "Windows protected your PC" on the installers (not signed yet);
* the mouse pointer is not drawn in recordings;
* the Google Drive upload of recordings is not built yet (recordings stay on the server PC).

---

## Sign-off

| Result | |
|---|---|
| All of A–E passed | ☐ yes ☐ no |
| F passed on the **GT 730** PC | ☐ yes ☐ no |
| F passed on the **RTX** PC | ☐ yes ☐ no |
| G and H passed | ☐ yes ☐ no ☐ not run |
| **Ready for a mock exam with a full lab** | ☐ yes ☐ no |
| Tester, date | |
