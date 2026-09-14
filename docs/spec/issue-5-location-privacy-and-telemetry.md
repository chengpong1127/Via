# Issue #5 — Location privacy and product telemetry contract

Decision date: 2026-08-29  
Status: Current

## Decision

Via is anonymous by design. It has no account, advertising identifier, device fingerprint, or stable installation identifier. A random identifier may connect records only within one active app session. Via must never use it to connect separate sessions.

Via separates four data flows:

1. a **Planning Request** that exists only long enough to calculate Journey Options;
2. an **Operational Event** that measures service health without location or Journey details;
3. a consented **Product Event** that measures a small allowlist of interactions; and
4. an explicit **Feedback Report** whose optional Journey details require a separate preview and choice.

No data flow may be sold, used for advertising, used to build a rider profile, or processed by a service provider for its own purpose.

## Location access

- Request foreground “while using the app” access only when the rider asks Via to use the current location.
- Never request background location.
- Ask for precise location only because accurate Walking connectors require it.
- If the rider denies location or grants reduced accuracy, keep Journey Planning available through manual origin selection. Explain that reduced accuracy can make Walking connectors less accurate.
- Current-location permission and Product Event consent are separate choices.

This follows Apple’s direction to request location in context, prefer When In Use authorization, and provide an alternative where possible. Android must later request the minimum foreground scope and must not request location only for analytics. [Apple App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/), [Apple Core Location authorization](https://developer.apple.com/documentation/corelocation/requesting-authorization-to-use-location-services), [Google Play location policy](https://support.google.com/googleplay/android-developer/answer/9799150)

## Data contract

| Data flow | Allowed transmission | Forbidden transmission | Retention |
| --- | --- | --- | --- |
| Planning Request | Selected origin and destination coordinates; normalized display text; Bus, Metro, and YouBike filter states; locale; departure setting | Raw place-search queries, autocomplete history, map browsing, unused place candidates, advertising/device/install identifiers | Memory only until the response completes |
| Operational Event | Random request/session ID; response time; error code; stale-feed state; result count; app/data version | Coordinates, display text, stop/station IDs, Journey details, Product Event fields, stable identifiers | Individual events: 30 days; qualifying daily aggregates: 13 months |
| Product Event | Filter states; Journey Option count; selected Journey Option position and label; Guidance expanded; Start control tapped; app and OS version; language; broad time bucket | Coordinates, place names, stop/station IDs, exact Journey times, exact bike/return-slot counts, free text, Feedback Report text, identifiers that connect sessions | Individual events: 30 days; qualifying daily aggregates: 13 months |
| Feedback Report | Rider’s text; app version; data version; error codes | Origin, destination, and full Journey details unless separately attached after a preview | Report and attachments: 30 days; anonymous bug-category aggregates: 13 months |
| Diagnostic record | Stack trace; app version; error code; response time; data-version identifiers | Planning Request body, coordinates, display text, Journey details, screen recording, input replay | 30 days |

Platform privacy labels use different definitions. Apple generally treats data sent off-device as “collected” only when it remains accessible longer than needed for the real-time request; Google still requires ephemeral off-device processing to be declared in the Data safety form even when it is not shown publicly. Via must complete each platform declaration from the actual shipped data flow and SDK behavior. [Apple App Privacy Details](https://developer.apple.com/app-store/app-privacy-details/), [Google Play Data safety](https://support.google.com/googleplay/android-developer/answer/10787469)

## Planning Request handling

- Platform maps own place search and display. Via receives only the place that the rider selected.
- Delete the Planning Request when its response completes.
- Never write its body, coordinates, display text, or resulting Journey details to application, CDN, tracing, or error logs.
- Operational logs may contain only the random request ID, response time, result count, and error code.
- Remove raw IP addresses from logs when the provider supports it.
- If an infrastructure provider must retain a raw IP address for security, limit it to seven days. Keep it separate from Product Events and Feedback Reports, and prohibit all analytics or profiling use.

## Operational Events

Operational Events are enabled by default because they measure whether Journey Planning works. The privacy notice must disclose them. They must contain no location, Journey details, or identifier that connects separate app sessions.

Delete individual Operational Events after 30 days. Daily aggregate data can remain for 13 months only when the group represents at least 20 app sessions. Delete or merge smaller groups. An aggregate must not retain a key or receipt that can recover individual events.

## Product Events and consent

Product Events are off by default. Ask for consent only after the rider sees the first Journey Options. The consent screen must state the exact allowlist, purpose, retention, and withdrawal behavior in plain Traditional Chinese and English.

The same control must remain available in Settings. When the rider withdraws consent:

- stop all new Product Events immediately; and
- delete individual Product Events that have not yet become anonymous aggregates.

The app stores deletion receipts for uploaded event batches for the 30-day individual retention period. A receipt locates only its batch; it must not become a stable installation identifier or connect separate sessions. If a receipt no longer exists, Via must explain that it cannot locate already anonymous or deleted data.

## Feedback Reports

Sending a Feedback Report is always an explicit rider action.

By default, send only the rider’s text, app version, data version, and error codes. Present an unchecked “Include Journey details” control. If the rider enables it, show the exact origin, destination, Journey Option, and Live Status details before the final send action.

Return a random receipt code. The receipt permits early deletion but does not identify the rider. Delete the report and all attachments after 30 days. An anonymous bug-category count may remain for 13 months under the same minimum-20-session aggregation rule.

## Privacy notice and rider rights

Provide the privacy notice inside the app and on the Via website. It must name:

- Via as the controller;
- every data category and purpose;
- the processing period, territory, recipient, and method;
- every service provider and processing country;
- the effect of refusing location, analytics, or optional Feedback Report attachments;
- how to access, correct, stop, or erase data; and
- how to withdraw consent.

Provide an email address or web form for privacy requests. Use Feedback Report receipt codes and locally stored upload receipts to locate data. Explain when Via cannot find data because it was never retained, is already deleted, or has become an anonymous aggregate.

Taiwan’s current Personal Data Protection Act requires purpose limitation, proportionality, collection notice, security controls, and access/correction/cessation/erasure rights for data that can directly or indirectly identify a person. It does not set one universal retention period; data must be erased when the purpose or notified period ends unless a lawful exception applies. Cross-border recipients and territories belong in the collection notice. [Taiwan PDPA, Articles 2–21 and 27](https://www.pdpc.gov.tw/en/News_Html/165/), [PDPA Enforcement Rules](https://www.pdpc.gov.tw/en/News_Content/169/732/)

## Service-provider boundary

Via remains responsible for hosting, analytics, crash-reporting, feedback, and other processors. Before use, each provider must:

- act only on Via’s written instructions;
- prohibit sale, advertising, profiling, and independent use;
- disclose subprocessors and processing countries;
- support the required deletion and retention limits;
- use transport encryption and appropriate access controls; and
- permit Via to verify the actual fields and default logs produced by its SDK or service.

Do not adopt a provider that writes Planning Request bodies or location into logs, or that cannot meet the seven-day security-IP limit and 30-day individual-event limit.

## Release gate

Issue #5 is implemented only when all of these checks pass:

- The in-app and website privacy notices match the real data inventory, processors, countries, and retention periods.
- Apple App Privacy and Google Play Data safety declarations match the application and every included SDK.
- Current-location permission appears only after the rider asks to use current location.
- Manual origin selection works with denied or reduced location permission.
- Product Event consent is off by default and appears only after the first Journey Options.
- Consent, withdrawal, Feedback Report preview, receipt-based early deletion, and the privacy-request channel work.
- A production-like log inspection finds no Planning Request body, coordinates, display text, or Journey details.
- Small aggregate groups are deleted or merged until they contain at least 20 app sessions.
- Provider contracts and configurations prohibit advertising, sale, profiling, and independent use.

## Consequences for other Wayfinder decisions

- The shared Journey Planning API must treat Planning Request bodies as ephemeral and define redacted errors.
- The server architecture must separate Live Status caching from rider requests and must audit platform/CDN log behavior.
- MVP validation must report the consent rate and must not infer behavior for riders who decline Product Events.
- The native clients must keep raw place search, map browsing, consent receipts, and unsubmitted Feedback Report context on-device.
- “Anonymous” is a tested property of the complete data flow, not a claim based only on the absence of accounts.
