# iOS App Store release

Epic #21 goal G1. Shipped in PRs #20 (release config), #43 (assets and policy pages),
#45 (final icon), #48 (About and attribution). This note keeps the audit, the decisions and
the owner-only items.

## Audit findings (researcher, 2026-11-23) and where each landed
1. No asset catalog or app icon: added (#43, generated placeholder; final art in #45).
2. No `PrivacyInfo.xcprivacy`: added for app and Live Activity extension (#20). Tracking false; precise location for app functionality, not linked, no tracking; UserDefaults reason `CA92.1`; the extension declares nothing.
3. Release had an empty `VIA_API_BASE_URL` and a launch-time `preconditionFailure`: Release now builds only with an https URL (run-script check; whitespace and empty host rejected), and the app no longer crashes on bad config (#20). Archives must set `VIA_API_BASE_URL`; use the gitignored `Local.Release.xcconfig` or a CI setting.
4. No data attribution in the UI: About sheet and journey footer (#48), see below.
5. No privacy policy, support URL or App Store Connect listing: pages in `docs/privacy/` and `docs/support/` (zh-Hant and English, no external resources), URL keys `VIA_PRIVACY_POLICY_URL` and `VIA_SUPPORT_URL` in the plist.
6. Location string did not mention navigation while the screen is locked: fixed with zh-Hant. `ITSAppUsesNonExemptEncryption = NO` set.
Fine as found: ATS, background modes, Live Activity, `Localizable.xcstrings` zh-Hant complete, network-error UX.
Still open from the audit: accessibility pass (VoiceOver, Dynamic Type; About was checked at the largest Dynamic Type), server-emitted TDX attribution, review notes for App Review (Taiwan-only test coordinates, why audio and location background modes are needed).

## Owner decisions (2026-11-23)
- Keep Apple team and bundle ID as configured; minimum iOS stays 26.
- Privacy and support pages on GitHub Pages (Settings, Pages, main `/docs`; needs a public repo or a plan that allows it). Text drafted by the team; every legal statement carries an `<!-- OWNER: confirm -->` marker until the owner confirms it.
- Controller is the owner as an individual; the privacy contact is the owner's own email. Both stay out of this repo's notes; the policy pages must have the real name and contact before publishing.
- No accounts, tracking or third-party SDKs; place search is Apple MapKit; data from TDX, OpenStreetMap and YouBike via TDX. Location is sent to the Via server only to plan trips, view nearby stops or re-plan during navigation; coordinates are processed in memory and not stored ([ADR 0001](../adr/0001-no-request-coordinates-retained.md)).
- Review of the policy text required: say what is really sent (current location plus chosen origin and destination), a Retention and deletion section (nothing held, so nothing to delete or export), coordinates are not sent to TDX (only stop and scope IDs) or other third parties, Valhalla is self-hosted, MapKit is covered by Apple's policy, no age-gated content. zh-Hant says coordinates are not recorded, not "UserDefaults". A leftover wording nit: the zh-Hant sentence about other data to delete or export should match the English "else".

## Data attribution rules
- TDX data requires the attribution 「資料介接交通部 TDX 平臺」 under the 政府資料開放授權條款第1版, plus the badge when the asset exists (the badge image was not available).
- About sheet lists: app name and version, OpenStreetMap (c) contributors under ODbL, TDX and its licence link, GTFS from TDX, YouBike availability via TDX, Apple Maps. Journey detail shows the journey's server `attributions` as a footer row.
- Known issues at last test: the About button was clipped at the lowest Explore sheet detent (move to the sheet header); the zh-Hant About sheet and the journey footer were not captured on a simulator.

## App name (researcher, 2026-11-23)
Plain "Via" is taken: Via Transportation, Inc. publishes "Via" and many "Via <city>" transit apps, holds US word marks 5,958,489 and 5,958,490 (Class 9 app, 38, 39) and opposes VIA-formative marks (TTAB 91278752). "Via" plus a suffix looks free by search but carries high confusion and trademark risk (App Store guidelines 2.3.7, 4.1(c), 5.2.1). Recommendation: lead with a different name; the repo and code may keep "Via". Taiwan TIPO was not checked. Not legal advice. The final icon work (#45) uses the Tilo logo.

## Release checklist
- Owner full name and contact in the policy; GitHub Pages enabled; policy published only after the log-stripping change is deployed (see [log-privacy.md](log-privacy.md)).
- Final icon art and app name; App Store Connect metadata, screenshots and age rating.
- Archive with an https `VIA_API_BASE_URL`; signed archive and TestFlight upload (never done; archives were only built unsigned).
- Tests ran on an iOS 27 simulator under heavy machine load; the Release-fails-on-empty-URL check was shown against main (builds and would crash at launch) and the fixed branch (fails at build time).
