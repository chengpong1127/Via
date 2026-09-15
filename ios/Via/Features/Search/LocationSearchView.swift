import MapKit
import SwiftUI

struct LocationSearchView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var results: [PlaceSearchResult] = []
    @State private var isSearching = false
    @FocusState private var searchIsFocused: Bool

    let title: String
    let searchRegion: MKCoordinateRegion?
    let allowsCurrentLocation: Bool
    let onSelect: (JourneyEndpoint) -> Void

    init(
        title: String,
        searchRegion: MKCoordinateRegion? = nil,
        allowsCurrentLocation: Bool = false,
        onSelect: @escaping (JourneyEndpoint) -> Void
    ) {
        self.title = title
        self.searchRegion = searchRegion
        self.allowsCurrentLocation = allowsCurrentLocation
        self.onSelect = onSelect
    }

    var body: some View {
        NavigationStack {
            List {
                if allowsCurrentLocation {
                    Button {
                        onSelect(.currentLocation)
                        dismiss()
                    } label: {
                        Label {
                            VStack(alignment: .leading, spacing: 2) {
                                Text("Current Location")
                                    .foregroundStyle(.primary)
                                Text("Use your current position as the starting point")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        } icon: {
                            Image(systemName: "location.fill")
                                .foregroundStyle(Color.viaBlue)
                        }
                    }
                    .accessibilityIdentifier("location.current")
                }

                if isSearching {
                    HStack {
                        Spacer()
                        ProgressView()
                        Spacer()
                    }
                    .listRowBackground(Color.clear)
                }

                ForEach(results) { result in
                    Button {
                        onSelect(.place(result.place))
                        dismiss()
                    } label: {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(result.name).foregroundStyle(.primary)
                            if let subtitle = result.subtitle {
                                Text(subtitle).font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            }
            .overlay {
                if query.isEmpty && !allowsCurrentLocation {
                    ContentUnavailableView(
                        "Search for a place",
                        systemImage: "magnifyingglass",
                        description: Text("Search for a station, landmark, or address anywhere in Taiwan.")
                    )
                } else if results.isEmpty && !isSearching {
                    ContentUnavailableView.search(text: query)
                }
            }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: allowsCurrentLocation ? "Search another place" : "Search destinations")
            .searchFocusedIfAvailable($searchIsFocused)
            .task(id: query) {
                await search()
            }
            .task {
                searchIsFocused = true
            }
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", action: dismiss.callAsFunction)
                }
            }
        }
    }

    private func search() async {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.count >= 2 else {
            results = []
            isSearching = false
            return
        }

        do {
            try await Task.sleep(for: .milliseconds(250))
            guard !Task.isCancelled else { return }
            isSearching = true

            let request = MKLocalSearch.Request()
            request.naturalLanguageQuery = trimmed
            request.region = LocationSearchRegion.preferred(
                mapViewport: searchRegion,
                userCoordinate: nil
            )
            let response = try await MKLocalSearch(request: request).start()
            guard !Task.isCancelled else { return }

            results = response.mapItems.prefix(15).map(PlaceSearchResult.init)
            isSearching = false
        } catch is CancellationError {
            return
        } catch {
            results = []
            isSearching = false
        }
    }
}

private struct PlaceSearchResult: Identifiable {
    let id = UUID()
    let name: String
    let subtitle: String?
    let place: Place

    init(mapItem: MKMapItem) {
        let placemark = mapItem.placemark
        let displayName = mapItem.name ?? placemark.title ?? String(localized: "Selected place")
        name = displayName
        subtitle = [placemark.subLocality, placemark.locality]
            .compactMap { $0 }
            .filter { $0 != displayName }
            .joined(separator: ", ")
            .nilIfEmpty
        place = Place(
            name: displayName,
            subtitle: subtitle,
            coordinate: Coordinate(
                latitude: placemark.coordinate.latitude,
                longitude: placemark.coordinate.longitude
            )
        )
    }
}

enum LocationSearchRegion {
    static let taiwanWide = MKCoordinateRegion(
        center: CLLocationCoordinate2D(latitude: 23.7, longitude: 120.96),
        span: MKCoordinateSpan(latitudeDelta: 4.6, longitudeDelta: 4.2)
    )

    static func preferred(
        mapViewport: MKCoordinateRegion?,
        userCoordinate: Coordinate?
    ) -> MKCoordinateRegion {
        if let mapViewport {
            return mapViewport
        }
        if let userCoordinate {
            return MKCoordinateRegion(
                center: userCoordinate.clLocationCoordinate,
                span: MKCoordinateSpan(latitudeDelta: 0.5, longitudeDelta: 0.5)
            )
        }
        return taiwanWide
    }
}

private extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}

private extension View {
    @ViewBuilder
    func searchFocusedIfAvailable(_ isFocused: FocusState<Bool>.Binding) -> some View {
        if #available(iOS 18.0, *) {
            searchFocused(isFocused)
        } else {
            self
        }
    }
}
