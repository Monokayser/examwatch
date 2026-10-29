# Privacy

ExamWatch is software that a school runs on its own computers. The ExamWatch
project does not operate any service and receives no data.

## What is collected, and when

| Data | When | Where it goes |
|---|---|---|
| Student screen images (live and recorded) | **Only while an exam is open for that lab and after the student pressed V1** and confirmed their Student ID | The school's own ExamWatch server; optionally the school's own cloud folder |
| Student ID, PC name, IP address, connect/disconnect times | During exams | The school's server (database and audit log) |
| Agent health (CPU %, memory, encoder type) | While the PC is connected | The school's server, shown to teachers |
| Foreground app name / window title (optional feature, off by default) | During exams, if the school enables it | The school's server |

Nothing is captured before V1 is pressed or after the exam ends.

## Transparency to students

While monitoring is active, the student PC shows a visible
"Your screen is being monitored" notice. ExamWatch is not designed to be hidden
and must not be modified to hide it.

## Security and retention

* All traffic is encrypted (TLS 1.3). Student PCs only connect to the server whose certificate they were configured with.
* Recordings are encrypted at rest (AES-256-GCM, one key per exam).
* Recordings are **automatically and permanently deleted** after the school's retention period (default 48 hours after the exam ends), including their encryption keys.
* Only signed-in teachers can view screens or recordings, and every view is logged.

## The school's responsibility

The school decides who is monitored and for how long, and must inform students
in line with its local law (e.g. GDPR or national data-protection law) before
using ExamWatch.
