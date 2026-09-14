## Agent skills

### Current architecture

The server's canonical planning flow is:

```text
discover → enrich → bike → search → reconstruct → rank
```

`RouteGraph` is the only public routing graph representation. Keep the dependency
direction `domain ← routing ← app/api`, with providers implementing domain-facing
interfaces. Do not reintroduce the removed `planner`, `normalization`, or
`routing/engine` modules, or a `JourneyPlanningService` wrapper.

iOS remains feature-oriented (`App`, `Components`, `Domain`, `Features`,
`Resources`, `Services`). Production planning flows through
`JourneyPlanner → HTTPJourneyPlanner → JourneyAPI → APIClient`; keep
`DemoJourneyPlanner` for previews and offline tests. Navigation is future work
under `Features/Navigation` and is not part of the current implementation.

Architecture decisions are authoritative in `docs/adr/`. Specs describe current
contracts; research documents are historical unless their status says otherwise.

### Issue tracker

Issues and specs are tracked in this repository's GitHub Issues. See `docs/agents/issue-tracker.md`.

### Triage labels

Triage uses the five default canonical labels. See `docs/agents/triage-labels.md`.

### Domain docs

Domain documentation uses the single-context layout. See `docs/agents/domain.md`.
