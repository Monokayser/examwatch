# ExamWatch

Low-latency, low-impact screen monitoring and recording for **computer-lab exams**.
One teacher watches every student screen across several labs, live, and every
student's screen is recorded with timestamps for later review.

> **Status: pre-alpha, under active development.** The design is complete
> ([docs/DESIGN.md](docs/DESIGN.md)). The server, dashboard, network client, video
> pipeline and load simulator are written. The student agent UI, the watchdog
> service and the installers are in progress. **Not yet ready for real exams.**

## Why another tool?

Existing classroom tools pull full-resolution frames from every PC and scale
them on the teacher's CPU. That lags with more than a few dozen screens and
slows down student PCs. ExamWatch:

* captures with **DXGI Desktop Duplication** and encodes **H.264 on the GPU**
  (NVENC), with a single-thread software fallback for PCs without a hardware encoder;
* sends a tiny **thumbnail stream** to the grid and **full quality only for the
  enlarged screen**;
* **records** a 720p stream per student, encrypted at rest, deleted automatically
  after the retention period;
* is built so the student never notices it in performance (design §2.8). It
  is **always visible**: students see a notice that monitoring is active.

## Components

| Path | What |
|---|---|
| `crates/ew-server` | Central server: student connections (QUIC/TLS 1.3), recording, retention, teacher API |
| `dashboard/` | Teacher web dashboard (served by ew-server; WebCodecs decoding) |
| `crates/ew-agent` | Student PC agent: V1 button, Student ID entry, monitoring notice, capture |
| `crates/ew-service` | Student PC watchdog (Windows service) |
| `crates/ew-media` | Screen capture, GPU scaling, H.264 encoding (Media Foundation) |
| `crates/ew-proto` | Wire protocol and the agent's connection manager |
| `crates/ew-sim` | Load/failure simulator: many fake student PCs against a real server |
| `tools/` | Admin scripts (e.g. Smart App Control check) |

## Requirements

* Student and teacher PCs: Windows 10 22H2 or Windows 11 (x64).
* Server: any Windows PC with a gigabit connection reachable from all lab VLANs.
* Teacher dashboard: current Microsoft Edge or Google Chrome.

## Building from source

```powershell
rustup default stable-x86_64-pc-windows-msvc   # plus Visual Studio Build Tools (C++)
cargo test --workspace
cargo build --release
```

If the source folder is inside a virtualised/app-container path, set
`CARGO_TARGET_DIR` to a normal folder (e.g. `C:\build\examwatch`).

On Windows 11 with Smart App Control on, locally built (unsigned) binaries
are blocked; build in CI or on a development machine without it.

## Running a local test

```powershell
ew-server init                       # writes ew-server.toml with a random enrolment token
ew-server add-teacher admin --admin  # asks for a password
ew-server                            # dashboard: https://localhost:8443
ew-sim --pin (Get-Content data\server.fingerprint.txt) --token <enroll_token from ew-server.toml>
```

## Privacy

ExamWatch records student screens **only while an exam is open** and only
after the student presses **V1** and enters their ID. A notice stays on screen
while monitoring is active. See [PRIVACY.md](PRIVACY.md).

## Code signing

Release binaries are signed. See [CODE_SIGNING_POLICY.md](CODE_SIGNING_POLICY.md).

## Licence

Apache License 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
ExamWatch is an original implementation and contains no code from Veyon.
