import MapKit
import SwiftUI

struct ExploreView: View {
    @Bindable var model: ExploreModel

    var body: some View {
        MapReader { proxy in
            Map(position: $model.camera, selection: $model.mapSelection) {
                UserAnnotation()
                if let navigation = model.navigation {
                    RouteLegsContent(option: navigation.option, focusedLegID: nil)
                } else if model.isPlanning {
                    JourneyMapContent(directions: model.directions, focusedLegID: model.focusedLegID)
                } else {
                    ForEach(model.resultMarkers) { result in
                        Marker(result.place.name, coordinate: result.place.coordinate.clCoordinate)
                            .tag(MapSelection(result.id))
                    }
                    if let place = model.selectedPlace, !model.resultMarkers.contains(place) {
                        Marker(place.place.name, coordinate: place.place.coordinate.clCoordinate)
                            .tag(MapSelection(place.id))
                    }
                }
            }
            .mapStyle(.standard(
                elevation: .realistic,
                emphasis: model.isPlanning ? .muted : .automatic,
                pointsOfInterest: model.isPlanning ? .excludingAll : .all
            ))
            .mapControls {
                if !model.isNavigating {
                    MapUserLocationButton()
                    MapPitchToggle()
                }
                MapCompass()
            }
            .safeAreaInset(edge: .top) {
                if let navigation = model.navigation {
                    NavigationBanner(navigation: navigation)
                }
            }
            .safeAreaInset(edge: .bottom) {
                if let navigation = model.navigation {
                    VStack(alignment: .trailing, spacing: 12) {
                        if !model.camera.followsUserLocation {
                            Button("Re-center", systemImage: "location.north.line.fill", action: model.recenter)
                                .buttonStyle(.glass)
                        }
                        NavigationTray(navigation: navigation, end: model.endNavigation)
                    }
                }
            }
            .onMapCameraChange(frequency: .onEnd) { context in
                model.search.service.region = context.region
            }
            .onChange(of: model.mapSelection) {
                Task { await model.handleMapSelection() }
            }
            .simultaneousGesture(
                LongPressGesture(minimumDuration: 0.5)
                    .sequenced(before: DragGesture(minimumDistance: 0))
                    .onEnded { value in
                        guard case let .second(true, drag?) = value,
                              let coordinate = proxy.convert(drag.location, from: .local)
                        else { return }
                        Task { await model.dropPin(at: Coordinate(coordinate)) }
                    }
            )
        }
        .onGeometryChange(for: CGSize.self) { $0.size } action: { model.mapSize = $0 }
        .sheet(isPresented: Binding(get: { !model.isNavigating }, set: { _ in })) {
            ExploreSheet(model: model)
                .presentationDetents([.collapsed, .medium, .large], selection: $model.detent)
                .presentationBackgroundInteraction(.enabled(upThrough: .medium))
                .presentationContentInteraction(.scrolls)
                .interactiveDismissDisabled()
        }
        .task {
            model.location.requestAuthorizationIfNeeded()
        }
        .onChange(of: model.isNavigating) { _, navigating in
            UIApplication.shared.isIdleTimerDisabled = navigating
        }
        .task(id: model.isNavigating) {
            guard model.isNavigating else { return }
            try? await Task.sleep(for: .milliseconds(500))
            model.recenter()
        }
    }
}

private struct ExploreSheet: View {
    @Bindable var model: ExploreModel

    var body: some View {
        Group {
            switch model.route {
            case .search, .results:
                SearchPanel(model: model, purpose: .explore)
            case let .pickEndpoint(role):
                SearchPanel(model: model, purpose: .endpoint(role))
            case let .place(result):
                PlaceCard(model: model, result: result)
            case .directions:
                DirectionsPanel(model: model)
            case .journey:
                if let option = model.journeyOption {
                    JourneyDetailPanel(model: model, option: option)
                } else {
                    DirectionsPanel(model: model)
                }
            }
        }
        .animation(.smooth(duration: 0.25), value: model.route)
    }
}

struct JourneyMapContent: MapContent {
    let directions: DirectionsModel
    let focusedLegID: String?

    var body: some MapContent {
        if let selected = directions.selectedOption {
            ForEach(directions.options.filter { $0.id != selected.id }) { option in
                MapPolyline(coordinates: option.overview.map(\.clCoordinate))
                    .stroke(.gray.opacity(0.55), style: StrokeStyle(lineWidth: 5, lineCap: .round, lineJoin: .round))
            }
            RouteLegsContent(option: selected, focusedLegID: focusedLegID)
        } else if case let .place(destination)? = directions.destination {
            Marker(destination.name, coordinate: destination.coordinate.clCoordinate)
                .tint(.red)
        }
    }
}

struct RouteLegsContent: MapContent {
    let option: JourneyOption
    let focusedLegID: String?

    var body: some MapContent {
        ForEach(option.legs) { leg in
            let coordinates = leg.path.map(\.clCoordinate)
            let dimmed = focusedLegID != nil && focusedLegID != leg.id
            if case .walking = leg.kind {
                MapPolyline(coordinates: coordinates)
                    .stroke(
                        leg.kind.routeColor.opacity(dimmed ? 0.35 : 1),
                        style: StrokeStyle(lineWidth: 5, lineCap: .round, lineJoin: .round, dash: [0.5, 9])
                    )
            } else {
                MapPolyline(coordinates: coordinates)
                    .stroke(.white, style: StrokeStyle(lineWidth: 9, lineCap: .round, lineJoin: .round))
                MapPolyline(coordinates: coordinates)
                    .stroke(
                        leg.kind.routeColor.opacity(dimmed ? 0.35 : 1),
                        style: StrokeStyle(lineWidth: 6, lineCap: .round, lineJoin: .round)
                    )
            }
        }
        ForEach(option.legs.filter { if case .walking = $0.kind { false } else { true } }) { leg in
            Annotation(leg.from.name, coordinate: leg.from.coordinate.clCoordinate, anchor: .center) {
                StopDot(color: leg.kind.color)
            }
            .annotationTitles(.hidden)
            Annotation(leg.to.name, coordinate: leg.to.coordinate.clCoordinate, anchor: .center) {
                StopDot(color: leg.kind.color)
            }
            .annotationTitles(.hidden)
        }
        if let first = option.legs.first {
            Annotation(first.from.name, coordinate: first.from.coordinate.clCoordinate, anchor: .center) {
                StopDot(color: .blue, size: 14)
            }
            .annotationTitles(.hidden)
        }
        if let last = option.legs.last {
            Marker(last.to.name, systemImage: "mappin", coordinate: last.to.coordinate.clCoordinate)
                .tint(.red)
        }
    }
}

private struct StopDot: View {
    let color: Color
    var size: CGFloat = 11

    var body: some View {
        Circle()
            .fill(.white)
            .stroke(color, lineWidth: 3)
            .frame(width: size, height: size)
    }
}
