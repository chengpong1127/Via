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
Release builds require an HTTPS `VIA_API_BASE_URL` build setting and fail at startup
when it is missing or insecure.

## Structure

| Folder | Contents |
| --- | --- |
| `App` | App entry point and dependency wiring |
| `Domain` | Places, planning requests and journey results; the `JourneyPlanner` interface |
| `Services/API` | `APIClient`, DTOs matching the server OpenAPI schema, and mapping |
| `Services/Planning` | `HTTPJourneyPlanner` for production, `DemoJourneyPlanner` for previews and offline tests |
| `Services/Location`, `Services/Search` | Core Location and MapKit search (`MKLocalSearchCompleter`, `MKLocalSearch`, POI lookup) |
| `Features/Explore` | Map screen, sheet routing and route overlays |
| `Features/Search`, `Features/Place`, `Features/Directions` | Sheet panels |
| `Components` | Shared badges, headers and mode styling |

Production planning flows through `JourneyPlanner → HTTPJourneyPlanner → APIClient`
and calls `POST /v1/journey-plans`. Responses are not cached; an empty result shows
"No Routes Found". Navigation is not implemented yet.

Only the selected transport modes (`bus`, `metro`, `tra`, `thsr`, `bike_share`) are
stored, as a local preference; walking is always implicit. Selected places and
results are not persisted. Times are shown in Asia/Taipei.

## Tests

```sh
xcodebuild -project Via.xcodeproj -scheme Via -destination 'platform=iOS Simulator,name=iPhone 17' -parallel-testing-enabled NO test
```
