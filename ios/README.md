# Via for iOS

Native SwiftUI and MapKit client for Via journey planning, modeled on the Apple Maps
experience: a full-screen map with a persistent bottom sheet for search, place cards
and transit directions.

## Requirements

- Xcode 27 or newer
- iOS 26 or newer

## Open and run

Open `Via.xcodeproj`, choose the `Via` scheme, and run on an iPhone simulator. Source
folders are synchronized groups, so new files under `Via/` and `ViaTests/` are picked
up without editing the project.

Debug builds call `http://127.0.0.1:3000`, which a simulator reaches on the host.
To run on a phone, copy `Config/Local.xcconfig.example` to `Config/Local.xcconfig`
(ignored by git) and set `VIA_API_BASE_URL` to an address the phone can reach, such as
the Mac's Tailscale Serve name (`https://<machine>.<tailnet>.ts.net`, see
`server/README.md`); write `//` as `/$()/` because xcconfig treats `//` as a comment.
App Transport Security allows plain HTTP only to IP addresses, so a host name needs
HTTPS. `VIA_BUNDLE_ID` there overrides the app and extension bundle identifiers if
`org.via.journey` is unavailable to your team.
Release builds require an HTTPS `VIA_API_BASE_URL` build setting and fail at startup
when it is missing or insecure. The setting reaches the app through `Via-Info.plist`,
so either configuration can be pointed elsewhere, for example
`xcodebuild … VIA_API_BASE_URL=http://127.0.0.1:3100`.

## Structure

| Folder | Contents |
| --- | --- |
| `App` | App entry point and dependency wiring |
| `Domain` | Places, planning requests and journey results; the `JourneyPlanner` interface |
| `Services/API` | `APIClient`, DTOs matching the server OpenAPI schema, and mapping |
| `Services/Planning` | `HTTPJourneyPlanner`, the production planner |
| `Services/Location`, `Services/Search` | Core Location and MapKit search (`MKLocalSearchCompleter`, `MKLocalSearch`, POI lookup) |
| `Features/Explore` | Map screen, sheet routing and route overlays |
| `Features/Search`, `Features/Place`, `Features/Directions` | Sheet panels |
| `Features/Navigation` | Turn-by-turn banner, stop countdown, alerts and re-routing |
| `Features/Navigation/Feedback` | Live Activity, spoken guidance with haptics, and local notifications |
| `Components` | Shared badges, headers and mode styling |
| `Shared` | `NavigationActivityAttributes`, compiled into the app and the extension |
| `ViaLiveActivity` (target) | Widget extension rendering the Lock Screen and Dynamic Island activity |

Production planning flows through `JourneyPlanner → HTTPJourneyPlanner → APIClient`
and calls `POST /v1/journey-plans`. Responses are not cached; an empty result shows
"No Routes Found". `Features/Navigation` follows a selected journey on-device using the
leg guidance `geometry_index`/`street_name` parameters and transit `intermediate_stops`.
While a trip is active, location keeps updating in the background (`UIBackgroundModes`
`location`, `CLBackgroundActivitySession`), and `NavigationModel` publishes a
`NavigationStatus` and `NavigationCue`s to each `NavigationFeedback`: the Live Activity
shows the current step and arrival time, turns are spoken about 30 m ahead (`audio`
background mode), and alerts and re-routes post notifications when the app is not
in the foreground.

Only the selected transport modes (`bus`, `metro`, `tra`, `thsr`, `bike_share`) are
stored, as a local preference; walking is always implicit. Selected places and
results are not persisted. Times are shown in Asia/Taipei.

## Tests

```sh
xcodebuild -project Via.xcodeproj -scheme Via -destination 'platform=iOS Simulator,name=iPhone 17' -parallel-testing-enabled NO test
```
