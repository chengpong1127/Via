---
Status: Superseded by ADR-0008
---

# Demand-driven realtime resolution

The former design resolved departure timing incrementally while transit search
expanded. ADR-0008 replaces that behavior with a bounded static Top-K corridor
search, one request-local timing snapshot load, and a provider-free temporal
search.

The mode-owned timing policies introduced here remain: Bus, Metro, and TRA own
their realtime parsing and static fallback; THSR is static-only; timing basis
and reliability are separate domain facts. Only the point at which those
sources are invoked changed.
