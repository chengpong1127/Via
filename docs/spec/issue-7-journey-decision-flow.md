# Issue #7 — Journey decision-flow interaction

Decision date: 2026-08-29  
Status: Accepted

Final product-owner confirmation: 2026-08-29. The approved interaction is prototype Variant A in its refined form: compact Journey Options, a tappable route that opens the separate Details sheet, and only a small green Live Status indicator.

## Decision

Via will use the map-plus-bottom-sheet direction explored as prototype Variant A. The map supplies spatial context and highlights the route for the selected Journey Option. The bottom sheet owns the complete decision flow and follows the familiar hierarchy of Apple Maps' Directions sheet without copying its mutually exclusive Travel Mode selector.

## Directions sheet hierarchy

From top to bottom, the sheet contains:

1. the **Directions** title;
2. independent Bus, Metro, and YouBike Travel Mode filters;
3. editable From and To locations;
4. a departure-time control;
5. up to three Journey Options; and
6. the complete route and static Guidance for the selected Journey Option.

Walking remains available automatically as a connector and is not shown as a filter. There is no car/walk/transit-style single-choice mode selector and no Prefer control.

## Journey Option selection

- Each Journey Option summary shows duration, arrival time, Travel Modes, Walking time, Transfer count, and the Live Status observation time.
- Selecting a Journey Option redraws its highlighted route on the map.
- The selected option remains compact. Tapping its route opens a separate scrollable Details sheet that shows every Leg and its static Guidance in sequence.
- Live Status appears with its observation time and does not imply a reservation or guarantee.
- The Directions sheet does not show a Live Status label or status copy. It shows only a small green status indicator beside departure time. It does not show the “Walking connects every option” helper sentence.

## Start control

Start remains visible so the intended hierarchy can be evaluated, but it is intentionally nonfunctional for this scope. Activating it explains that Via provides static Guidance and does not track the rider or advance through Legs.

## Prototype source

The decision was validated in `prototypes/issue-7-journey-decision-flow/index.html?variant=A`. This HTML is a throwaway decision aid, not production implementation.
