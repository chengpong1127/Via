import MapKit
import SwiftUI

struct LocationSearchView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var searchState: LocationSearchState = .idle
    @State private var retryCount = 0
    @State private var activeSearch: MKLocalSearch?
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

                searchRows
            }
            .overlay {
                if query.isEmpty && !allowsCurrentLocation {
                    ContentUnavailableView(
                        "Search for a place",
                        systemImage: "magnifyingglass",
                        description: Text("Search for a station, landmark, or address anywhere in Taiwan.")
                    )
                } else {
                    searchMessage
                }
            }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: allowsCurrentLocation ? "Search another place" : "Search destinations")
            .searchFocusedIfAvailable($searchIsFocused)
            .task(id: SearchTaskKey(query: query, retryCount: retryCount)) {
                await search()
            }
            .onChange(of: query) {
                activeSearch?.cancel()
                activeSearch = nil
            }
            .onDisappear {
                activeSearch?.cancel()
                activeSearch = nil
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

    @ViewBuilder
    private var searchRows: some View {
        switch searchState {
        case .idle, .empty, .failed:
            EmptyView()
        case .loading(let previousResults):
            progressRow
            resultRows(previousResults)
        case .results(let results):
            resultRows(results)
        }
    }

    private var progressRow: some View {
        HStack {
            Spacer()
            ProgressView()
            Spacer()
        }
        .listRowBackground(Color.clear)
    }

    @ViewBuilder
    private func resultRows(_ results: [PlaceSearchResult]) -> some View {
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

    @ViewBuilder
    private var searchMessage: some View {
        switch searchState {
        case .empty:
            ContentUnavailableView.search(text: query)
        case .failed:
            ContentUnavailableView {
                Label("Search unavailable", systemImage: "wifi.exclamationmark")
            } description: {
                Text("Check your connection and try again.")
            } actions: {
                Button("Try again") { retryCount += 1 }
                    .buttonStyle(.borderedProminent)
            }
        case .idle:
            ContentUnavailableView.search(text: query)
        case .loading, .results:
            EmptyView()
        }
    }

    private func search() async {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        activeSearch?.cancel()
        activeSearch = nil

        guard trimmed.count >= 2 else {
            searchState = .idle
            return
        }

        do {
            try await Task.sleep(for: .milliseconds(250))
        } catch {
            guard !Task.isCancelled else { return }
            searchState = .failed
            return
        }

        guard !Task.isCancelled else { return }
        let request = MKLocalSearch.Request()
        request.naturalLanguageQuery = trimmed
        request.region = LocationSearchRegion.preferred(
            mapViewport: searchRegion
        )
        let search = MKLocalSearch(request: request)
        let previousResults = searchState.visibleResults
        activeSearch = search
        searchState = .loading(previousResults: previousResults)

        do {
            let response = try await search.start()
            guard !Task.isCancelled, activeSearch === search else { return }
            activeSearch = nil
            let results = response.mapItems.prefix(15).map(PlaceSearchResult.init)
            searchState = results.isEmpty ? .empty : .results(results)
        } catch {
            guard !Task.isCancelled, activeSearch === search else { return }
            activeSearch = nil
            searchState = .failed
        }
    }
}

private enum LocationSearchState {
    case idle
    case loading(previousResults: [PlaceSearchResult])
    case results([PlaceSearchResult])
    case empty
    case failed

    var visibleResults: [PlaceSearchResult] {
        switch self {
        case .loading(let previousResults): previousResults
        case .results(let results): results
        case .idle, .empty, .failed: []
        }
    }
}

private struct SearchTaskKey: Hashable {
    let query: String
    let retryCount: Int
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
        mapViewport: MKCoordinateRegion?
    ) -> MKCoordinateRegion {
        mapViewport ?? taiwanWide
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
