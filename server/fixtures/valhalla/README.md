# Valhalla contract fixture

`bike.osm.pbf` is the 12,980-byte bicycle fixture from Valhalla 3.8.3 commit
`a60c7cb`, retained here so graph builds do not depend on mutable map downloads.

- Source: `test/data/bike.osm.pbf` in `valhalla/valhalla`
- SHA-256: `d4cfdad98205cb8d60b0ed542010c152377748179d2958ee9afe4c1373d6d1c4`

This tiny graph proves adapter behavior and startup readiness only. It is not a
Taipei route-quality fixture and must never be presented as production evidence.
