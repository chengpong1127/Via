# Via for iOS

Native SwiftUI and MapKit client for Via Journey Planning.

## Requirements

- Xcode 26 or newer
- iOS 17 or newer

## Open and run

Open `Via.xcodeproj`, choose the `Via` scheme, and run on an iPhone simulator.

The app uses `HTTPJourneyPlanner` at the app root and calls `POST /v1/journey-plans` through `Services/API/JourneyAPI` and `APIClient`. Debug builds default to `http://127.0.0.1:3000`; Release builds require an explicitly configured HTTPS `VIA_API_BASE_URL` and fail at startup when it is missing or insecure. Responses are server-authoritative, are not cached, and successful empty results are represented as an empty `JourneyResult`.

`DemoJourneyPlanner` remains available for previews, offline UI development, and deterministic tests. It is selected explicitly by preview/test code; it is not injected into the real app. Production feature code depends on `JourneyPlanning`, not on a concrete HTTP transport. Future Navigation code belongs under `Features/Navigation` and is intentionally not implemented here.

The demo never persists selected places, coordinates, Journey Options, Legs, or Live Status. The selected transport systems (`bus`, `metro`, `tra`, `thsr`, and `bike_share`) are stored as local preferences; Walking remains implicit.
