# Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io/),
certificate by [SignPath Foundation](https://signpath.org/).
*(This line applies once the SignPath Foundation application is approved.)*

## What is signed

Only release builds produced by this repository's GitHub Actions workflow
(`.github/workflows/ci.yml`), from a tagged commit on the default branch:

* `ew-server.exe`, `ew-agent.exe`, `ew-service.exe`
* `ExamWatch-Teacher-Setup.exe` and `ExamWatch-Student.msi`

Nothing built on a developer's own machine is ever signed.

## Team roles

| Role | Who | Responsibility |
|---|---|---|
| Committers and reviewers | Members of the GitHub organisation/repository with write access | Write and review changes; every change to `main` goes through a pull request |
| Approvers | Repository owner(s) | Approve each signing request in SignPath before release |

All members use multi-factor authentication on GitHub and SignPath.

## Privacy

ExamWatch transfers screen images only between a school's own student PCs and
its own ExamWatch server, and (if the school configures it) to the school's own
cloud storage. It sends nothing to the ExamWatch developers or to any other
third party. See [PRIVACY.md](PRIVACY.md).
