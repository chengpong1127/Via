# 0001. The server retains no request coordinates

Status: accepted

## Context
Via plans trips from the user's current location and chosen places. The App Store
privacy label and the public privacy policy (`docs/privacy/`) state that location is
used for app functionality and is not stored. Operators normally debug with request
logs, and Traefik, Valhalla and HTTP client errors all tend to include URLs or bodies
with coordinates.

## Decision
No request coordinate reaches any log, trace, metric label, error text or history store,
at any log level (owner decision, 2026-11-23). Infrastructure logs may keep IP address and
timestamp for at most 7 days. Traefik access logs are off; node logs are capped by
journald and a cleanup timer (see [log-privacy.md](../design/log-privacy.md)).

## Alternatives
- Keep coordinates in debug logs only: rejected, because a policy promise has no debug exception and logs ship and persist.
- Round or hash coordinates in logs: partly used (3-decimal rounding only inside test assertions); rejected as the policy, because rounded locations are still location data.
- Keep trip history for analytics or support: rejected, no accounts or tracking in v1.

## Consequences
Easier: a short privacy policy, no deletion or export requests, no personal data held.
Harder: production bugs about a specific trip cannot be replayed from logs; reproduce with
fixtures or stop and mode IDs, counts and timings (these may be logged). A new log site,
upstream client or proxy must be checked against this rule, and the policy must not be
published before the stripping change is deployed.
