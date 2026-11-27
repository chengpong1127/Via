# Design notes

Lasting knowledge from the first Via v1 push (epic #21), moved here from the old
`.team/` working folder (written 2026-11-23 to 2026-11-26, before the GitHub board
existed). Grouped by topic. What shipped is in git history and on the board; these
notes keep the reasons and findings that code and tickets do not show.

- [AWS production backend](aws-production.md): why single-node k3s in Sydney, costs, credits, domain, review lessons, what was never verified.
- [iOS release](ios-release.md): App Store audit, decisions, app name research, attribution rules, release checklist.
- [Log and coordinate privacy](log-privacy.md): what the server may log and how the 7-day bound is enforced.
- [Routing research loop](routing-research.md): how the loop runs, TDX rules, experiment findings, open items.

Decisions that are hard to reverse live in [`../adr/`](../adr/).
