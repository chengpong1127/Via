import MapKit
import SwiftUI

struct SearchPanel: View {
    enum Purpose: Hashable {
        case explore
        case endpoint(EndpointRole)
    }

    @Bindable var model: ExploreModel
    let purpose: Purpose
    @FocusState private var focused: Bool

    private var search: SearchModel { model.search }

    var body: some View {
        VStack(spacing: 0) {
            if case let .endpoint(role) = purpose {
                SheetHeader(title: role == .origin ? "Choose Start" : "Choose Destination") {
                    model.cancelEndpointPicking()
                }
            }
            searchBar
                .padding(.horizontal, 16)
                .padding(.top, purpose == .explore ? 18 : 4)
                .padding(.bottom, 10)
            content
        }
        .onChange(of: focused) {
            if focused { model.detent = .large }
        }
        .onChange(of: model.detent) {
            if model.detent == .collapsed { focused = false }
        }
        .onAppear {
            if case .endpoint = purpose { focused = true }
        }
    }

    private var searchBar: some View {
        HStack(spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(.secondary)
                TextField(placeholder, text: Bindable(search).query)
                    .focused($focused)
                    .submitLabel(.search)
                    .autocorrectionDisabled()
                    .onSubmit { Task { await submit() } }
                if !search.query.isEmpty {
                    Button("Clear", systemImage: "xmark.circle.fill") {
                        search.clear()
                        if model.route == .results { model.route = .search }
                        focused = true
                    }
                    .labelStyle(.iconOnly)
                    .foregroundStyle(.secondary)
                }
            }
            .padding(.horizontal, 14)
            .frame(height: 48)
            .glassEffect(.regular.interactive(), in: .capsule)

            if purpose == .explore && (focused || model.route == .results) {
                Button("Cancel") {
                    focused = false
                    model.closeResults()
                }
                .transition(.move(edge: .trailing).combined(with: .opacity))
            }
        }
        .animation(.smooth(duration: 0.2), value: focused)
    }

    private var placeholder: LocalizedStringKey {
        switch purpose {
        case .explore: "Search Maps"
        case .endpoint: "Search for a place or address"
        }
    }

    @ViewBuilder
    private var content: some View {
        List {
            if case .endpoint = purpose, search.query.isEmpty {
                Button {
                    choose(.currentLocation)
                } label: {
                    PlaceRow(symbol: "location.fill", tint: .blue, title: String(localized: "My Location"), subtitle: nil, distance: nil)
                }
            }

            if purpose == .explore, search.query.isEmpty {
                Section("Find Nearby") {
                    ForEach(QuickSearch.allCases) { item in
                        Button {
                            Task { await findNearby(item) }
                        } label: {
                            PlaceRow(symbol: item.symbol, tint: item.tint, title: String(localized: item.title), subtitle: nil, distance: nil)
                        }
                    }
                }
            }

            if search.isSearching {
                HStack {
                    Spacer()
                    ProgressView()
                    Spacer()
                }
                .listRowSeparator(.hidden)
            } else if let results = search.results {
                if results.isEmpty {
                    ContentUnavailableView(
                        search.failed ? "Search Unavailable" : "No Results",
                        systemImage: "magnifyingglass",
                        description: Text(search.failed ? "Check your connection and try again." : "Try a different place name or address.")
                    )
                    .listRowSeparator(.hidden)
                }
                ForEach(results) { result in
                    Button {
                        pick(result)
                    } label: {
                        PlaceRow(
                            symbol: result.mapItem.pointOfInterestCategory?.symbol ?? "mappin",
                            tint: .red,
                            title: result.place.name,
                            subtitle: result.place.subtitle,
                            distance: distance(to: result.place.coordinate)
                        )
                    }
                }
            } else {
                ForEach(search.completions, id: \.self) { completion in
                    Button {
                        Task { await choose(completion) }
                    } label: {
                        PlaceRow(
                            symbol: completion.subtitle.isEmpty ? "magnifyingglass" : "mappin",
                            tint: completion.subtitle.isEmpty ? .gray : .red,
                            title: completion.title,
                            subtitle: completion.subtitle.isEmpty ? nil : completion.subtitle,
                            distance: nil
                        )
                    }
                }
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .scrollDismissesKeyboard(.immediately)
        .buttonStyle(.plain)
    }

    private func submit() async {
        focused = false
        let results = await search.submit()
        if purpose == .explore, !results.isEmpty {
            model.showResults(results)
        }
    }

    private func findNearby(_ item: QuickSearch) async {
        focused = false
        search.query = String(localized: item.title)
        let center = model.location.coordinate ?? Coordinate(search.service.region.center)
        let results = switch item {
        case .metro: await search.nearbyTransit(.metro, around: center)
        case .bus: await search.nearbyTransit(.bus, around: center)
        case .bike: await search.nearbyBikeStations(around: center)
        case .food: await search.nearby([.restaurant], around: center)
        case .coffee: await search.nearby([.cafe], around: center)
        }
        if !results.isEmpty {
            model.showResults(results)
        }
    }

    private func choose(_ completion: MKLocalSearchCompletion) async {
        focused = false
        let results = await search.choose(completion)
        if results.count == 1, let only = results.first {
            pick(only)
        } else if purpose == .explore, !results.isEmpty {
            model.showResults(results)
        }
    }

    private func pick(_ result: PlaceResult) {
        switch purpose {
        case .explore: model.show(result)
        case let .endpoint(role): model.choose(.place(result.place), for: role)
        }
    }

    private func choose(_ endpoint: JourneyEndpoint) {
        if case let .endpoint(role) = purpose {
            model.choose(endpoint, for: role)
        }
    }

    private func distance(to coordinate: Coordinate) -> String? {
        model.location.coordinate.map { $0.distance(to: coordinate).distanceText }
    }
}

struct PlaceRow: View {
    let symbol: String
    let tint: Color
    let title: String
    let subtitle: String?
    let distance: String?

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: symbol)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.white)
                .frame(width: 32, height: 32)
                .background(tint.gradient, in: .circle)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.body)
                    .lineLimit(1)
                if distance != nil || subtitle != nil {
                    Text([distance, subtitle].compactMap(\.self).joined(separator: " · "))
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .contentShape(.rect)
        .padding(.vertical, 2)
    }
}

private enum QuickSearch: String, CaseIterable, Identifiable {
    case metro
    case bus
    case bike
    case food
    case coffee

    var id: Self { self }

    var title: LocalizedStringResource {
        switch self {
        case .metro: "Metro Stations"
        case .bus: "Bus Stops"
        case .bike: "YouBike Stations"
        case .food: "Restaurants"
        case .coffee: "Coffee"
        }
    }

    var symbol: String {
        switch self {
        case .metro: "tram.fill.tunnel"
        case .bus: "bus.fill"
        case .bike: "bicycle"
        case .food: "fork.knife"
        case .coffee: "cup.and.saucer.fill"
        }
    }

    var tint: Color {
        switch self {
        case .metro: TransitMode.metro.color
        case .bus: TransitMode.bus.color
        case .bike: .bikeShare
        case .food: .orange
        case .coffee: .brown
        }
    }
}

extension MKPointOfInterestCategory {
    var symbol: String {
        switch self {
        case .publicTransport: "tram.fill"
        case .restaurant: "fork.knife"
        case .cafe: "cup.and.saucer.fill"
        case .hotel: "bed.double.fill"
        case .park, .nationalPark: "leaf.fill"
        case .museum: "building.columns.fill"
        case .store: "bag.fill"
        case .hospital: "cross.case.fill"
        case .school, .university: "graduationcap.fill"
        case .airport: "airplane"
        default: "mappin"
        }
    }
}
