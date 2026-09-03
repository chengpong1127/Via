import MapKit
import SwiftUI

struct LocationSearchView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var results: [PlaceSearchResult] = []
    @State private var isSearching = false
    @FocusState private var searchIsFocused: Bool

    let title: String
    let onSelect: (SelectedPlace) -> Void

    var body: some View {
        NavigationStack {
            List {
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
                        onSelect(result.place)
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
                if query.isEmpty {
                    ContentUnavailableView(
                        "Search Taipei and New Taipei",
                        systemImage: "magnifyingglass",
                        description: Text("Search for a station, landmark, or address to start planning your journey.")
                    )
                } else if results.isEmpty && !isSearching {
                    ContentUnavailableView.search(text: query)
                }
            }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search places")
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
            request.region = MKCoordinateRegion(
                center: CLLocationCoordinate2D(latitude: 25.04, longitude: 121.54),
                span: MKCoordinateSpan(latitudeDelta: 0.5, longitudeDelta: 0.5)
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
    let place: SelectedPlace

    init(mapItem: MKMapItem) {
        let placemark = mapItem.placemark
        let displayName = mapItem.name ?? placemark.title ?? String(localized: "Selected place")
        name = displayName
        subtitle = [placemark.subLocality, placemark.locality]
            .compactMap { $0 }
            .filter { $0 != displayName }
            .joined(separator: ", ")
            .nilIfEmpty
        place = SelectedPlace(
            displayText: displayName,
            coordinate: Coordinate(
                latitude: placemark.coordinate.latitude,
                longitude: placemark.coordinate.longitude
            )
        )
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
