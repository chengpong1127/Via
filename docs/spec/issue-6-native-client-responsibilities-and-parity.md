# Issue #6 — Native-client responsibilities and parity

Decision date: 2026-08-29  
Status: Current

## Decision

Via uses a server-authoritative Journey Planning model with native presentation. The shared server owns the meaning of a Journey and its Journey Options. Each native client owns platform integration, local presentation, and rider-controlled device state.

iOS leads product discovery and delivery. Every essential behavior accepted on iOS becomes a platform-neutral behavior contract for the later Android client. Parity means equivalent capabilities, information, decisions, states, and rider actions. It does not mean pixel-identical screens or identical gestures.

## Responsibility boundary

| Shared server | Native client |
| --- | --- |
| Validate a Planning Request. | Use the platform map SDK for map display and place search. |
| Use one immutable static revision and request-local evidence context for the complete calculation. | Keep raw queries, browsing, unused candidates, and provider place identifiers on-device. |
| Construct, validate, rank, and return Journey Options. | Send only a selected origin and destination, normalized display text, Travel Mode filters, locale, and departure setting. |
| Define Leg and Transfer structure and the semantic content of Guidance. | Render the map, Journey Options, Details, and Guidance with native components. |
| Interpret Live Status, Freshness, evidence strength, warnings, and failure reasons. | Format values, translate text, provide accessibility labels, and expose platform-native interactions. |
| Return stable machine-readable result, warning, Guidance, and error codes with structured parameters. | Manage permissions, request lifecycle, selection, sheets, loading state, and refresh presentation. |
| Keep provider adapters, provider identifiers, and raw provider semantics behind the server boundary. | Store the permitted local preferences and consent artifacts. |

Neither client calculates Journey Options, ranks alternatives, interprets provider statuses, evaluates Freshness, or turns missing information into a default value. The clients can validate input shape for immediate feedback, but the server remains authoritative.

## Map and place-search handoff

The iOS client uses MapKit for place search and map presentation. The later Android client uses its selected native map SDK. A provider search result becomes server input only after the rider selects it.

The client sends:

- the selected coordinate;
- normalized display text suitable for showing back to the rider; and
- only the other fields required by the Planning Request contract.

The client does not send raw search text, autocomplete history, map browsing, unused candidates, or Apple/Android provider place identifiers. The server contract cannot require a provider-specific place ID. This preserves the privacy contract in issue #5 and allows the two clients to use different search providers.

## Local state and storage

Clients can persist only non-sensitive preferences needed for continuity:

- Bus, Metro, and YouBike filter states;
- explicit language choice;
- Product Event consent and the short-lived deletion receipts defined by issue #5;
- accessibility and display preferences; and
- the last departure-setting type, but not its associated place or Journey.

Clients do not persist selected coordinates, selected places, Planning Requests, Journey Options, Legs, or Live Status as history. A new app session starts without a prior Journey. Temporary in-memory state can keep the currently visible result while its screen remains active.

## Live Status refresh without progress tracking

Via refreshes an open result by repeating its Planning Request. The new calculation reads the current static revision and collects a new request-local evidence context before returning a complete set of Journey Options. A client never refreshes or mutates one Leg independently.

The client refresh sequence is:

1. Keep the current Journey Options visible and mark the screen as refreshing.
2. Send the same Planning Request again.
3. On success, atomically replace the complete result and show its new Live Status observation times.
4. Preserve the selected Journey Option only when the shared API supplies a stable semantic matching key. Otherwise, select the server's first-ranked Journey Option.
5. On failure, keep the previous result visible, remove the loading state, and show its actual Freshness or explain that refresh is unavailable.

The client offers explicit pull-to-refresh and can refresh after a meaningful return to the foreground. Any automatic foreground refresh uses the same sequence and must not imply progress tracking, continuous rider location, automatic Leg advancement, or automatic replanning while the rider moves.

The server's independent feed-refresh cycle does not push partial mutations into an open screen. The observation time and Freshness state remain the evidence for what the rider sees.

## Client-owned translation

All Via translation happens in the native clients. The server does not compose localized Via interface text or translated Guidance sentences.

For Via-owned content, the server returns stable semantic codes plus typed parameters. This includes Guidance instructions, warnings, evidence and Freshness reasons, empty-result reasons, and errors. Each client maps the same code to its Traditional Chinese and English resources, formats its parameters, and supplies localized accessibility text.

When an upstream operator supplies authored text, the server can return the available source variants with locale metadata and required attribution. The client selects the best available variant and applies the defined fallback. It must not branch on the text or treat untranslated text as a machine-readable status.

The shared API must version semantic codes. Adding a translation does not require an API change; changing the meaning or parameters of a code does.

## Behavioral parity

Both clients must provide equivalent behavior for:

- editing origin, destination, and departure setting;
- selecting independent Bus, Metro, and YouBike filters while Walking remains an automatic connector;
- requesting and showing up to three Journey Options in server-defined order;
- presenting duration, arrival time, Travel Modes, Walking time, Transfer count, and Live Status observation time;
- selecting a Journey Option and updating its highlighted map path;
- opening the separate Details view and presenting every Leg and its static Guidance;
- explaining that Start does not track the rider or advance through Legs;
- refreshing and handling a result with stale or unavailable Live Status;
- location permission, reduced accuracy, and manual-origin fallback; and
- all defined empty, degraded, and failure outcomes.

The clients implement the same named presentation states:

| State | Required meaning and rider actions |
| --- | --- |
| `editing` | The rider can edit request inputs and submit a valid Planning Request. |
| `loading` | No result is ready; request inputs remain identifiable and cancellation/back behavior is native. |
| `results` | One or more Journey Options are available for selection and Details. |
| `refreshing` | The prior complete result remains visible while its replacement is requested. |
| `empty` | Calculation succeeded but no feasible Journey Option exists; the structured reason and relevant recovery actions are shown. |
| `invalid_request` | A request field is invalid; the client identifies the field without echoing sensitive server data. |
| `temporarily_unavailable` | No coherent planning result or permitted evidence is available; retry is offered. |
| `unexpected_failure` | The result cannot be trusted; no partial result from that failed calculation is shown. |

The presentation can differ, but the available facts and recovery actions cannot silently differ.

## Platform-specific affordances

iOS and Android use their native sheet, screen-flow, permission, typography, accessibility, feedback, and gesture conventions. MapKit-specific annotation behavior does not become an Android requirement, and Android-specific components do not become an iOS requirement.

A platform-specific affordance is permitted when it:

- preserves the same Journey Planning capability and semantic result;
- does not remove a fact or rider action required by the behavior contract;
- does not introduce a new server-side domain rule; and
- remains covered by that platform's interaction and accessibility tests.

An essential product behavior cannot remain Apple-only merely because iOS shipped first. Any intentional capability difference requires a separate recorded product decision.

## iOS-first workflow and parity release gate

iOS can discover and refine interactions before Android implementation. Each accepted iOS interaction must be recorded in platform-neutral terms: inputs, states, facts, actions, transitions, errors, and accessibility intent. Screenshots are supporting examples, not the shared specification.

When Android is built, both clients must pass:

- the same versioned shared-API fixtures;
- a shared behavior checklist for Planning Request editing, filters, result ordering, selection, Details, Guidance, Live Status observation time and Freshness, refresh, empty results, degraded evidence, and failures;
- contract tests proving that every server semantic code has supported client handling and required translations; and
- platform-specific visual, interaction, permission, and accessibility tests.

Cross-platform tests compare semantics and behavior, not pixels. A server contract change cannot ship until both released clients have a compatible handling path or the API versioning policy provides a safe transition.

## Validation contract

The decision is implemented only when automated or repeatable checks prove that:

- place search works without sending raw queries or provider place identifiers to Via;
- identical Planning Requests and planning-input revisions produce equivalent Journey Option facts on both clients;
- neither client ranks Journey Options or interprets raw provider status;
- every Via-owned rider-facing message is generated from a stable code and client translation resource;
- Traditional Chinese and English cover every supported semantic code, including accessibility text;
- no persisted client record contains a selected coordinate, place, Journey, Leg, or Live Status history;
- refresh retains the old result until one complete replacement succeeds;
- refresh failure never presents old Live Status as fresh;
- foreground refresh does not request background location or start progress tracking;
- each named presentation state exposes equivalent facts and recovery actions on both platforms; and
- the same behavior fixtures pass on iOS and Android even when their visual snapshots differ.

## Consequences for other Wayfinder decisions

- The current provider-neutral contract in `server/src/api/` and `ios/Via/Services/API/` defines request fields, stable semantic codes and typed parameters, network revisions, stable Journey Option matching where safe, and structured result/error outcomes. It must not return Via-owned translated sentences.
- Issue #5 remains the storage and transmission limit for client state, place search, consent artifacts, and Feedback Reports.
- Issue #7 defines the accepted iOS interaction hierarchy. This decision extracts its essential behavior from its Apple Maps-style presentation.
- ADR 0002 and ADR 0006 remain authoritative for provider-neutral Live Status, Freshness, and TDX-derived typed IDs. Native clients display their results but do not reproduce those rules.
- Android delivery can lag iOS, but the shared behavior contract and fixtures must be maintained from the first iOS implementation.
