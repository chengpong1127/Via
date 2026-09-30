import MapKit
import SwiftUI

struct JourneyMapView: View {
    @Environment(JourneySession.self) private var session
    @State private var cameraPosition: MapCameraPosition = .region(LocationSearchRegion.taiwanWide)
    @State private var mapViewport: MKCoordinateRegion?
    @State private var detailsOption: JourneyOption?

    var body: some View {
        ZStack(alignment: .bottom) {
            map
                .ignoresSafeArea()

            VStack(spacing: 0) {
                topBar
                Spacer()
            }
            .padding(.horizontal, 16)
            .padding(.top, 8)

            DirectionsPanel(
                searchRegion: LocationSearchRegion.preferred(
                    mapViewport: mapViewport
                ),
                onShowDetails: { detailsOption = $0 }
            )
            .frame(maxHeight: 570)
            .transition(.move(edge: .bottom).combined(with: .opacity))
        }
        .animation(.snappy, value: session.state)
        .sheet(item: $detailsOption) { option in
            JourneyDetailsView(option: option)
                .presentationDetents([.large])
                .presentationDragIndicator(.visible)
        }
        .onChange(of: session.selectedOptionID) {
            frameSelectedJourney()
        }
        .onChange(of: session.result?.generatedAt) {
            frameSelectedJourney()
        }
    }

    private var map: some View {
        Map(position: $cameraPosition) {
            if case .place(let origin) = session.origin {
                Marker("From", systemImage: "location.fill", coordinate: origin.coordinate.clLocationCoordinate)
                    .tint(Color.viaBlue)
            }
            if let destination = session.destination {
                Marker("To", systemImage: "flag.fill", coordinate: destination.coordinate.clLocationCoordinate)
                    .tint(.brown)
            }
            if let option = session.selectedOption, option.overview.count > 1 {
                MapPolyline(coordinates: option.overview.map(\.clLocationCoordinate))
                    .stroke(.white.opacity(0.9), lineWidth: 9)
                MapPolyline(coordinates: option.overview.map(\.clLocationCoordinate))
                    .stroke(Color.viaBlue, style: StrokeStyle(lineWidth: 6, lineCap: .round, lineJoin: .round))
            }
        }
        .mapStyle(.standard(elevation: .realistic))
        .onMapCameraChange(frequency: .onEnd) { context in
            mapViewport = context.region
        }
        .mapControls {
            MapCompass()
            MapScaleView()
        }
    }

    private var topBar: some View {
        HStack {
            (Text("V") + Text("i").foregroundStyle(Color.viaBlue) + Text("a"))
                .font(.title2.weight(.black))
                .tracking(-1)
                .accessibilityLabel("Via")

            Spacer()

            Menu {
                Button("Reset Journey", systemImage: "arrow.counterclockwise") {
                    session.origin = .currentLocation
                    session.destination = nil
                    session.returnToEditing()
                    mapViewport = nil
                    cameraPosition = .region(LocationSearchRegion.taiwanWide)
                }
            } label: {
                Image(systemName: "ellipsis")
                    .font(.headline)
                    .frame(width: 42, height: 42)
                    .background(.regularMaterial, in: Circle())
                    .shadow(color: .black.opacity(0.12), radius: 8, y: 3)
            }
            .accessibilityLabel("More options")
        }
    }

    private func frameSelectedJourney() {
        guard let points = session.selectedOption?.overview, !points.isEmpty else { return }
        let latitudes = points.map(\.latitude)
        let longitudes = points.map(\.longitude)
        guard let minLatitude = latitudes.min(), let maxLatitude = latitudes.max(),
              let minLongitude = longitudes.min(), let maxLongitude = longitudes.max() else { return }

        cameraPosition = .region(MKCoordinateRegion(
            center: CLLocationCoordinate2D(
                latitude: (minLatitude + maxLatitude) / 2,
                longitude: (minLongitude + maxLongitude) / 2
            ),
            span: MKCoordinateSpan(
                latitudeDelta: max(0.025, (maxLatitude - minLatitude) * 1.5),
                longitudeDelta: max(0.025, (maxLongitude - minLongitude) * 1.35)
            )
        ))
    }

}

#Preview {
    JourneyMapView()
        .environment(JourneySession(
            planner: DemoJourneyPlanner(),
            locationProvider: LocationService(),
            origin: .place(Place(name: "Taipei Main Station", coordinate: Coordinate(latitude: 25.0478, longitude: 121.5170))),
            destination: Place(name: "Taipei 101", coordinate: Coordinate(latitude: 25.0340, longitude: 121.5645))
        ))
}
