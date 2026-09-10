# Via for iOS

Native SwiftUI and MapKit client for Via Journey Planning.

## Requirements

- Xcode 26 or newer
- iOS 17 or newer

## Open and run

Open `Via.xcodeproj`, choose the `Via` scheme, and run on an iPhone simulator.

The app uses `HTTPJourneyPlanner` at the app root and calls `POST /v1/journey-plans`. Debug builds default to `http://127.0.0.1:3000`; Release builds require an explicitly configured HTTPS `VIA_API_BASE_URL` and fail at startup when it is missing or insecure. Responses are server-authoritative, are not cached, and successful empty results are represented as an empty `JourneyResult`.

`DemoJourneyPlanner` remains available for previews and deterministic tests. It is not injected into the real app.

The demo never persists selected places, coordinates, Journey Options, Legs, or Live Status. Only the three Travel Mode filters are stored as local preferences.
