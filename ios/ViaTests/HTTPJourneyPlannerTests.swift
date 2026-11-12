import Foundation
import Testing
@testable import Via

@Suite(.serialized)
struct HTTPJourneyPlannerTests {
    let request = PlanningRequest(
        origin: Place(name: "A", coordinate: Coordinate(latitude: 25.04, longitude: 121.51)),
        destination: Place(name: "B", coordinate: Coordinate(latitude: 25.03, longitude: 121.56)),
        modes: [.metro]
    )

    @Test func postsToJourneyPlansAndMapsOptions() async throws {
        let planner = makePlanner { urlRequest in
            #expect(urlRequest.httpMethod == "POST")
            #expect(urlRequest.url?.absoluteString == "http://via.test/v1/journey-plans")
            #expect(urlRequest.value(forHTTPHeaderField: "Content-Type") == "application/json")
            return (200, Fixtures.optionsResponse)
        }
        let plan = try await planner.plan(request)
        #expect(plan.options.map(\.id) == ["opt_1"])
    }

    @Test(arguments: [
        (400, #"{"error":{"code":"invalid_coordinate","field":"/origin/coordinate/latitude"}}"#, JourneyPlanningError.invalidRequest(field: "/origin/coordinate/latitude")),
        (408, #"{"error":{"code":"request_timeout"}}"#, .timedOut),
        (503, #"{"error":{"code":"temporarily_unavailable"}}"#, .temporarilyUnavailable),
        (500, #"{"error":{"code":"planner_failure"}}"#, .unexpected),
        (200, #"{"unexpected":true}"#, .unsupportedResponse),
    ])
    func mapsServerErrors(status: Int, body: String, expected: JourneyPlanningError) async {
        let planner = makePlanner { _ in (status, Data(body.utf8)) }
        await #expect(throws: expected) {
            try await planner.plan(request)
        }
    }

    @Test func fetchesNearbyStops() async throws {
        StubProtocol.handler = { urlRequest in
            let components = URLComponents(url: urlRequest.url!, resolvingAgainstBaseURL: false)!
            #expect(urlRequest.httpMethod == "GET")
            #expect(components.path == "/v1/nearby-stops")
            #expect(components.queryItems == [
                URLQueryItem(name: "latitude", value: "25.04776"),
                URLQueryItem(name: "longitude", value: "121.51706"),
                URLQueryItem(name: "radius_metres", value: "1000"),
            ])
            return (200, Data("""
            {
              "generated_at": "2026-11-02T00:05:20+08:00",
              "transit_stops": [{
                "via_ids": ["s1", "s2"], "name": "台北車站",
                "coordinate": {"latitude": 25.0461, "longitude": 121.5174},
                "distance_metres": 165, "modes": ["metro", "ferry"], "route_labels": ["BL", "R"]
              }],
              "bike_stations": [
                {"via_id": "b1", "name": "YouBike2.0_太原廣場",
                 "coordinate": {"latitude": 25.0499, "longitude": 121.5172}, "distance_metres": 291,
                 "availability": {"available_bikes": 1, "available_return_slots": 28, "in_service": true,
                                  "observed_at": "2026-11-02T00:04:02+08:00"}},
                {"via_id": "b2", "name": "YouBike2.0_M2",
                 "coordinate": {"latitude": 25.048, "longitude": 121.519}, "distance_metres": 205,
                 "availability": null}
              ]
            }
            """.utf8))
        }
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubProtocol.self]
        let source = HTTPNearbyStopsSource(client: APIClient(
            baseURL: URL(string: "http://via.test")!,
            session: URLSession(configuration: configuration)
        ))

        let stops = try await source.nearbyStops(
            around: Coordinate(latitude: 25.04776, longitude: 121.51706),
            radiusMetres: 1_000
        )

        let metro = try #require(stops.transitStops.first)
        #expect(metro.viaIDs == ["s1", "s2"])
        #expect(metro.modes == [.metro])
        #expect(metro.routeLabels == ["BL", "R"])
        #expect(metro.distance == 165)
        #expect(stops.bikeStations.map(\.availability) == [
            BikeAvailability(availableBikes: 1, availableDocks: 28, inService: true),
            nil,
        ])
    }

    @Test func mapsTransportFailures() {
        #expect(HTTPJourneyPlanner.planningError(for: .transport(.cannotConnectToHost)) == .serverUnreachable)
        #expect(HTTPJourneyPlanner.planningError(for: .transport(.timedOut)) == .timedOut)
    }

    private func makePlanner(_ handler: @escaping @Sendable (URLRequest) -> (Int, Data)) -> HTTPJourneyPlanner {
        StubProtocol.handler = handler
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubProtocol.self]
        let client = APIClient(baseURL: URL(string: "http://via.test")!, session: URLSession(configuration: configuration))
        return HTTPJourneyPlanner(client: client)
    }
}

nonisolated final class StubProtocol: URLProtocol {
    nonisolated(unsafe) static var handler: (@Sendable (URLRequest) -> (Int, Data))?

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let handler = Self.handler, let url = request.url else { return }
        let (status, data) = handler(request)
        let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: data)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}
