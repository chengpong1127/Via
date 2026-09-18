# Domain Docs

Status: Current

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Before exploring, read these

- **`CONTEXT.md`** at the repo root, or
- **`CONTEXT-MAP.md`** at the repo root if it exists — it points at one `CONTEXT.md` per context. Read each one relevant to the topic.
- **`docs/adr/`** — read ADRs that touch the area you're about to work in. In multi-context repos, also check `src/<context>/docs/adr/` for context-scoped decisions.

If any of these files don't exist, **proceed silently**. Don't flag their absence; don't suggest creating them upfront. The `/domain-modeling` skill (reached via `/grill-with-docs` and `/improve-codebase-architecture`) creates them lazily when terms or decisions actually get resolved.

## Current Via routing vocabulary

Read the current server flow as `street access/egress → RoutingAlgorithm →
TripBasedRouter → reconstruct → rank`. `RoutingAlgorithm` is the public
algorithm-neutral boundary; Trip-Based owns its private index. `DepartureResolver`
resolves runtime `EffectiveTrip` values and `TransferShortcutIndex` represents
static public-transit interchange edges only. `RouteGraph` is removed from this
 flow. Direct walking and direct BikeShare are independent candidates. Static
 transit↔BikeShare walking connectors live in `TransitNetwork`, while the
 query-time `BikeShareOperator` batches dynamic station availability and bicycle
 movement. TripBased sees the resulting movement as explicit
 `JourneySegment::BikeShareTransfer` values rather than as walking transfer
 shortcuts.
 Realtime is demand-driven and request-local; there is no global realtime refresh
 planner. iOS networking is
`JourneyPlanner → HTTPJourneyPlanner → JourneyAPI → APIClient`, while
`DemoJourneyPlanner` is the offline implementation. TDX is currently
authoritative for Taiwan transit static and realtime data, so matching records
use the same typed IDs derived directly from TDX identifiers.

## File structure

Single-context repo (most repos):

```
/
├── CONTEXT.md
├── docs/adr/
│   ├── 0001-event-sourced-orders.md
│   └── 0002-routing-decision.md
└── src/
```

Multi-context repo (presence of `CONTEXT-MAP.md` at the root):

```
/
├── CONTEXT-MAP.md
├── docs/adr/                          ← system-wide decisions
└── src/
    ├── ordering/
    │   ├── CONTEXT.md
    │   └── docs/adr/                  ← context-specific decisions
    └── billing/
        ├── CONTEXT.md
        └── docs/adr/
```

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a hypothesis, a test name), use the term as defined in `CONTEXT.md`. Don't drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal — either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `/domain-modeling`).

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently overriding:

> _Contradicts ADR-0007 (event-sourced orders) — but worth reopening because…_
