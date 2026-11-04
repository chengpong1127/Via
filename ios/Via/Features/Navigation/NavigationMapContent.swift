import MapKit
import SwiftUI

struct NavigationMapContent: MapContent {
    let tracker: NavigationTracker
    let destination: Place

    var body: some MapContent {
        ForEach(Array(tracker.route.legs.enumerated()), id: \.element.leg.id) { index, leg in
            let parts = parts(of: leg, at: index)
            if parts.traveled.count >= 2 {
                RouteLine(coordinates: parts.traveled, color: .gray.opacity(0.75), casing: .gray)
            }
            if parts.remaining.count >= 2 {
                RouteLine(coordinates: parts.remaining, color: leg.leg.kind.routeColor, casing: leg.leg.kind.routeCasing)
            }
        }
        TransitStopsContent(legs: tracker.route.legs.map(\.leg))
        ForEach(tracker.route.legs.filter { !$0.isStreet }, id: \.leg.id) { leg in
            Annotation(leg.leg.from.name, coordinate: leg.leg.from.coordinate.clCoordinate, anchor: .center) {
                StopDot(color: leg.leg.kind.routeColor)
            }
            .annotationTitles(.hidden)
            Annotation(leg.leg.to.name, coordinate: leg.leg.to.coordinate.clCoordinate, anchor: .center) {
                StopDot(color: leg.leg.kind.routeColor)
            }
            .annotationTitles(.hidden)
        }
        Marker(destination.name, systemImage: "mappin", coordinate: destination.coordinate.clCoordinate)
            .tint(.red)
    }

    private func parts(of leg: NavigationLeg, at index: Int) -> (traveled: [Coordinate], remaining: [Coordinate]) {
        let drawn = leg.leg.path.count >= 2 ? leg.leg.path : leg.path
        if tracker.isFinished || index < tracker.legIndex { return (drawn, []) }
        guard index == tracker.legIndex, leg.isStreet else { return ([], drawn) }
        return leg.split(at: tracker.distanceAlong)
    }
}

private struct RouteLine: MapContent {
    let coordinates: [Coordinate]
    let color: Color
    let casing: Color

    var body: some MapContent {
        let points = coordinates.map(\.clCoordinate)
        MapPolyline(coordinates: points)
            .stroke(casing, style: StrokeStyle(lineWidth: 12, lineCap: .round, lineJoin: .round))
        MapPolyline(coordinates: points)
            .stroke(color, style: StrokeStyle(lineWidth: 8, lineCap: .round, lineJoin: .round))
    }
}

private extension LegKind {
    var routeCasing: Color {
        switch self {
        case .walking: Color(red: 0.05, green: 0.33, blue: 0.85)
        default: routeColor.mix(with: .black, by: 0.3)
        }
    }
}
