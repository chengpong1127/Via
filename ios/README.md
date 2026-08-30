# Via for iOS

Native SwiftUI and MapKit client for Via Journey Planning.

## Requirements

- Xcode 26 or newer
- iOS 17 or newer

## Open and run

Open `Via.xcodeproj`, choose the `Via` scheme, and run on an iPhone simulator.

The repository does not yet expose the specified `POST /v1/journey-plans` endpoint. The app therefore injects `DemoJourneyPlanner`, a deterministic in-memory implementation, at the app root. Replace it with an HTTP implementation of `JourneyPlanning` when the shared endpoint lands; presentation code and session state do not need to change.

The demo never persists selected places, coordinates, Journey Options, Legs, or Live Status. Only the three Travel Mode filters are stored as local preferences.
