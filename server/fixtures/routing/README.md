# Routing fixtures

This directory is a deliberately small, provider-neutral network used by the
end-to-end routing tests in `tests/routing`. It is not a copy of the
development TDX snapshot.

The common topology is:

```text
                         MRT red       MRT blue       MRT green
 F-M1 ───────── F-M2(red) ──walk── F-M2(blue) ───── F-M3(blue) ──walk── F-M3(green) ── F-M4
                            ▲      ▲       ▲
 F-BUS-A ── bus-in ── F-M2(bus) ──┘       └──────── bus-out ── F-M3(bus) ── F-BUS-B

 F-TRA-A ── TRA ── F-TRA-B ── walk transfer ── F-M1
 F-HSR-A ── THSR ─ F-HSR-B
 F-MISS-A ── bus ── F-MISS-M(in) ──walk── F-MISS-M(out) ── bus ── F-MISS-B
```

`network.json` contains 18 boarding points, three metro services with static
station departures, eight scheduled bus/rail trips, and bidirectional 120-second
`transfer_edges` with explicit 100-metre fixture distances for the MRT/MRT, MRT/bus, TRA/MRT, and missed-transfer
interchanges. The interchange points are distinct boarding points even when
they represent the same physical station.

The bus realtime fixture contains TDX-shaped observations at the fixed
timestamp `2026-08-31T10:00:00+08:00`:

- `realtime/bus.json` has two `fixture-bus-main` departures at 10:04 and 10:14.

The integration test serves this fixture through a local `TdxClient`, while
static fallback cases omit the client. Street paths are built inside the test
so each request exposes only the access, egress, or transfer edges needed by
that scenario. No test uses the external TDX, Valhalla, or network access for
its route decision.
