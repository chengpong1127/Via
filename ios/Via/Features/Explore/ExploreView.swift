import MapKit
import SwiftUI

struct ExploreView: View {
    @Bindable var model: ExploreModel

    var body: some View {
        MapReader { proxy in
            Map(position: $model.camera, selection: $model.mapSelection) {
                UserAnnotation(anchor: .center) { _ in
                    UserLocationDot(heading: model.location.heading.map { $0 - model.cameraHeading })
                }
                if let navigation = model.navigation {
                    NavigationMapContent(tracker: navigation.tracker, destination: navigation.destination)
                } else if model.isPlanning {
                    JourneyMapContent(directions: model.directions, focusedLegID: model.focusedLegID, select: model.select)
                } else {
                    ForEach(model.resultMarkers) { result in
                        Marker(result.place.name, coordinate: result.place.coordinate.clCoordinate)
                            .tag(MapSelection(result.id))
                    }
                    if let place = model.selectedPlace, !model.resultMarkers.contains(place), model.mapSelection?.feature == nil {
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
            .overlay(alignment: .bottomTrailing) {
                if model.isNavigating {
                    Button("Re-center", systemImage: model.isFollowingUser ? "location.fill" : "location", action: model.recenter)
                        .labelStyle(.iconOnly)
                        .font(.system(size: 24, weight: .semibold))
                        .frame(width: 56, height: 56)
                        .contentShape(.circle)
                        .glassEffect(.regular.interactive(), in: .circle)
                        .buttonStyle(.plain)
                        .foregroundStyle(.blue)
                        .padding(.trailing, 16)
                        .padding(.bottom, (model.detent == .navigationEnd ? PresentationDetent.navigationEndHeight : PresentationDetent.navigationStageHeight) - 16)
                        .animation(.smooth, value: model.detent)
                }
            }
            .onMapCameraChange(frequency: .onEnd) { context in
                model.search.service.region = context.region
            }
            .onMapCameraChange(frequency: .continuous) { context in
                if abs(context.camera.heading - model.cameraHeading) > 1 {
                    model.cameraHeading = context.camera.heading
                }
            }
            .onChange(of: model.location.coordinate) { _, coordinate in
                if let coordinate { model.userMoved(to: coordinate) }
            }
            .onChange(of: model.camera) {
                model.cameraChanged()
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
        .sheet(isPresented: .constant(true)) {
            Group {
                if let navigation = model.navigation {
                    NavigationSheet(
                        navigation: navigation,
                        end: model.endNavigation,
                        toggle: model.toggleNavigationPanel,
                        isExpanded: model.detent != .navigationStage
                    )
                } else {
                    ExploreSheet(model: model)
                }
            }
            .presentationDetents(model.detents, selection: $model.detent)
            .presentationBackgroundInteraction(.enabled(upThrough: model.isNavigating ? .navigationEnd : .medium))
            .presentationContentInteraction(.scrolls)
            .interactiveDismissDisabled()
        }
        .task {
            model.location.requestAuthorizationIfNeeded()
        }
        .onChange(of: model.isNavigating) { _, navigating in
            UIApplication.shared.isIdleTimerDisabled = navigating
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
    let select: (JourneyOption) -> Void

    private static let labelFractions = [0.5, 0.3, 0.7, 0.2, 0.8]

    var body: some MapContent {
        if let selected = directions.selectedOption {
            ForEach(directions.options.filter { $0.id != selected.id }) { option in
                MapPolyline(coordinates: option.overview.map(\.clCoordinate))
                    .stroke(.gray.opacity(0.55), style: StrokeStyle(lineWidth: 5, lineCap: .round, lineJoin: .round))
            }
            RouteLegsContent(option: selected, focusedLegID: focusedLegID)
            if focusedLegID == nil {
                ForEach(Array(directions.options.enumerated()), id: \.element.id) { index, option in
                    if let point = option.overview.point(atFraction: Self.labelFractions[index % Self.labelFractions.count]) {
                        Annotation(option.duration.travelTime, coordinate: point.clCoordinate, anchor: .bottom) {
                            RouteTimeLabel(option: option, isSelected: option.id == selected.id) {
                                select(option)
                            }
                        }
                        .annotationTitles(.hidden)
                    }
                }
            }
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
            MapPolyline(coordinates: coordinates)
                .stroke(.white, style: StrokeStyle(lineWidth: 9, lineCap: .round, lineJoin: .round))
            MapPolyline(coordinates: coordinates)
                .stroke(
                    leg.kind.routeColor.opacity(dimmed ? 0.35 : 1),
                    style: StrokeStyle(lineWidth: 6, lineCap: .round, lineJoin: .round)
                )
        }
        TransitStopsContent(legs: option.legs)
        ForEach(option.legs.filter { if case .walking = $0.kind { false } else { true } }) { leg in
            Annotation(leg.from.name, coordinate: leg.from.coordinate.clCoordinate, anchor: .center) {
                StopDot(color: leg.kind.routeColor)
            }
            .annotationTitles(.hidden)
            Annotation(leg.to.name, coordinate: leg.to.coordinate.clCoordinate, anchor: .center) {
                StopDot(color: leg.kind.routeColor)
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

private struct RouteTimeLabel: View {
    let option: JourneyOption
    let isSelected: Bool
    let select: () -> Void

    var body: some View {
        Button(action: select) {
            VStack(alignment: .leading, spacing: 0) {
                Text(option.duration.travelTime)
                    .font(.subheadline.bold())
                    .monospacedDigit()
                if isSelected, let label = option.label {
                    Text(label.title)
                        .font(.caption.weight(.semibold))
                }
            }
            .foregroundStyle(isSelected ? Color.white : Color.accentColor)
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(isSelected ? Color.accentColor : Color(.systemBackground), in: .rect(cornerRadius: 12))
            .shadow(color: .black.opacity(0.18), radius: 4, y: 2)
        }
        .buttonStyle(.plain)
        .zIndex(isSelected ? 1 : 0)
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }
}

private extension [Coordinate] {
    func point(atFraction fraction: Double) -> Coordinate? {
        guard let first else { return nil }
        let lengths = zip(self, dropFirst()).map { $0.distance(to: $1) }
        var remaining = lengths.reduce(0, +) * fraction
        for (index, length) in lengths.enumerated() {
            if remaining <= length, length > 0 {
                let ratio = remaining / length
                let start = self[index]
                let end = self[index + 1]
                return Coordinate(
                    latitude: start.latitude + (end.latitude - start.latitude) * ratio,
                    longitude: start.longitude + (end.longitude - start.longitude) * ratio
                )
            }
            remaining -= length
        }
        return last ?? first
    }
}

struct StopDot: View {
    let color: Color
    var size: CGFloat = 11
    var lineWidth: CGFloat = 3

    var body: some View {
        Circle()
            .fill(.white)
            .stroke(color, lineWidth: lineWidth)
            .frame(width: size, height: size)
    }
}

struct TransitStopsContent: MapContent {
    let legs: [Leg]

    var body: some MapContent {
        ForEach(legs) { leg in
            if let ride = leg.transit {
                ForEach(Array(ride.intermediateStops.enumerated()), id: \.offset) { _, stop in
                    Annotation(stop.name, coordinate: stop.coordinate.clCoordinate, anchor: .center) {
                        StopDot(color: leg.kind.routeColor, size: 8, lineWidth: 2)
                    }
                    .annotationTitles(.hidden)
                }
            }
        }
    }
}

struct UserLocationDot: View {
    let heading: Double?

    var body: some View {
        ZStack {
            if let heading {
                HeadingBeam()
                    .fill(
                        RadialGradient(
                            colors: [Color.blue.opacity(0.55), Color.blue.opacity(0)],
                            center: .center,
                            startRadius: 4,
                            endRadius: 44
                        )
                    )
                    .frame(width: 88, height: 88)
                    .rotationEffect(.degrees(heading))
                    .animation(.smooth(duration: 0.2), value: heading)
            }
            Circle()
                .fill(.blue)
                .stroke(.white, lineWidth: 3)
                .frame(width: 20, height: 20)
                .shadow(color: .black.opacity(0.25), radius: 3, y: 1)
        }
        .frame(width: 88, height: 88)
        .accessibilityHidden(true)
    }
}

private nonisolated struct HeadingBeam: Shape {
    func path(in rect: CGRect) -> Path {
        let center = CGPoint(x: rect.midX, y: rect.midY)
        var path = Path()
        path.move(to: center)
        path.addArc(
            center: center,
            radius: rect.width / 2,
            startAngle: .degrees(-90 - 32),
            endAngle: .degrees(-90 + 32),
            clockwise: false
        )
        path.closeSubpath()
        return path
    }
}
