import Foundation
import Testing
@testable import Via

@Suite(.serialized)
struct HTTPJourneyPlannerTests {
    @Test("Locale normalization accepts common Apple identifiers")
    func normalizesAppleLocales() {
        #expect(LocaleNormalization.bcp47("en_US") == "en-US")
        #expect(LocaleNormalization.bcp47("zh_Hant_TW") == "zh-Hant-TW")
        #expect(LocaleNormalization.bcp47("en_US@calendar=gregorian") == "en-US")
    }

    @Test("Apple locale identifiers are normalized to BCP-47 and request JSON matches the API")
    func requestMatchesContract() async throws {
        StubURLProtocol.reset(statusCode: 200, body: Self.emptyResult)
        let planner = HTTPJourneyPlanner(
            baseURL: URL(string: "https://api.example.test")!,
            urlSession: Self.stubSession()
        )

        let request = PlanningRequest(
            origin: Place(
                name: "Origin",
                coordinate: Coordinate(latitude: 25.04776, longitude: 121.51706)
            ),
            destination: Place(
                name: "Destination",
                coordinate: Coordinate(latitude: 25.03396, longitude: 121.56447)
            ),
            selectedModes: [.metro],
            departureSetting: .departAt(Date(timeIntervalSince1970: 1_756_600_000)),
            locale: "en_US"
        )

        let result = try await planner.plan(request)
        #expect(result.options.isEmpty)
        let body = try #require(StubURLProtocol.lastRequest?.httpBody)
        let json = try #require(JSONSerialization.jsonObject(with: body) as? [String: Any])
        #expect(json["locale"] as? String == "en-US")
        #expect(json["selected_modes"] as? [String] == ["metro"])
        #expect(json["departure_setting"] as? [String: Any] != nil)
        let origin = try #require(json["origin"] as? [String: Any])
        let originCoordinate = try #require(origin["coordinate"] as? [String: Any])
        #expect(originCoordinate["latitude"] as? Double == 25.04776)
        #expect(originCoordinate["longitude"] as? Double == 121.51706)
        #expect(StubURLProtocol.lastRequest?.url?.path == "/v1/journey-plans")
    }

    @Test("HTTP planner decodes and maps the shared server journey fixtures")
    func decodesSharedResultFixtures() async throws {
        let planner = HTTPJourneyPlanner(
            baseURL: URL(string: "https://api.example.test")!,
            urlSession: Self.stubSession()
        )

        let walkingResult = try await Self.plan(planner, fixture: "walking-result.json")
        let walkingOption = try #require(walkingResult.options.first)
        #expect(walkingOption.id == "option_01_fixture_match")
        #expect(walkingOption.legs.map(\.mode) == [.walking])

        let transitResult = try await Self.plan(planner, fixture: "transit-result.json")
        let transitOption = try #require(transitResult.options.first)
        let transitLeg = try #require(transitOption.legs.first { $0.mode == .transit })
        let transit = try #require(transitLeg.transit)
        #expect(transitLeg.mode == .transit)
        #expect(transit.mode == .metro)
        #expect(transit.patternID == "metro-pattern-1")
        #expect(transit.serviceLabel == "Blue Line")
        #expect(transit.headsign == "Nangang Exhibition Center")
        #expect(transit.boardingPointID == "metro-stop-1")
        #expect(transit.alightingPointID == "metro-stop-2")

        let bikeShareResult = try await Self.plan(planner, fixture: "bike-share-result.json")
        let bikeShareOption = try #require(bikeShareResult.options.first)
        let bikeShareLeg = try #require(bikeShareOption.legs.first { $0.mode == .bicycle })
        let bikeShare = try #require(bikeShareLeg.bikeShare)
        #expect(bikeShareLeg.mode == .bicycle)
        #expect(bikeShare.pickupStationID == "youbike-pickup")
        #expect(bikeShare.pickupStationName == "Taipei Main Station YouBike")
        #expect(bikeShare.dropoffStationID == "youbike-dropoff")
        #expect(bikeShare.dropoffStationName == "Taipei 101 YouBike")
    }

    @Test("HTTP planner maps the shared error statuses")
    func mapsErrorStatuses() async {
        let planner = HTTPJourneyPlanner(
            baseURL: URL(string: "https://api.example.test")!,
            urlSession: Self.stubSession()
        )
        let request = Self.request()

        for (status, expected) in [
            (400, JourneyPlanningError.invalidRequest(field: "/origin")),
            (503, JourneyPlanningError.temporarilyUnavailable),
            (500, JourneyPlanningError.unexpectedFailure)
        ] as [(Int, JourneyPlanningError)] {
            StubURLProtocol.reset(statusCode: status, body: Self.errorBody)
            do {
                _ = try await planner.plan(request)
                Issue.record("expected HTTP \(status) to throw")
            } catch let error as JourneyPlanningError {
                #expect(error == expected)
            } catch {
                Issue.record("unexpected error: \(error)")
            }
        }
    }

    @Test("API client preserves non-success status and response body")
    func apiClientMapsNonSuccessResponses() async {
        StubURLProtocol.reset(statusCode: 429, body: Self.errorBody)
        let client = APIClient(
            baseURL: URL(string: "https://api.example.test")!,
            urlSession: Self.stubSession()
        )

        do {
            let _: JourneyPlanResponseDTO = try await client.send(
                request: JourneyPlanRequestDTO(request: Self.request()),
                path: "v1/journey-plans"
            )
            Issue.record("expected the API client to reject HTTP 429")
        } catch let error as APIError {
            guard case let .httpStatus(code, body) = error else {
                Issue.record("unexpected API error: \(error)")
                return
            }
            #expect(code == 429)
            #expect(body == Self.errorBody)
        } catch {
            Issue.record("unexpected error: \(error)")
        }
    }

    private static func stubSession() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubURLProtocol.self]
        return URLSession(configuration: configuration)
    }

    private static func request() -> PlanningRequest {
        PlanningRequest(
            origin: Place(name: "Origin", coordinate: Coordinate(latitude: 25, longitude: 121)),
            destination: Place(name: "Destination", coordinate: Coordinate(latitude: 25.01, longitude: 121.01)),
            selectedModes: [.metro],
            departureSetting: .departNow,
            locale: "en"
        )
    }

    private static func plan(
        _ planner: HTTPJourneyPlanner,
        fixture name: String
    ) async throws -> JourneyResult {
        StubURLProtocol.reset(statusCode: 200, body: try apiFixture(named: name))
        return try await planner.plan(request())
    }

    private static func apiFixture(named name: String) throws -> Data {
        let repositoryRoot = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        let fixtureURL = repositoryRoot
            .appendingPathComponent("server", isDirectory: true)
            .appendingPathComponent("fixtures", isDirectory: true)
            .appendingPathComponent("api", isDirectory: true)
            .appendingPathComponent(name)
        return try Data(contentsOf: fixtureURL)
    }

    private static let emptyResult = Data(#"{
      "api_version":"1.0",
      "semantic_catalog_version":"1.0",
      "request_id":"req_test",
      "generated_at":"2026-08-29T10:08:00+08:00",
      "effective_search_time":"2026-08-29T10:08:00+08:00",
      "time_zone":"Asia/Taipei",
      "network_revision":{
        "static_network_revision":"static_test",
        "schedule_revision":"schedule_test"
      },
      "policy_versions":{"freshness":"1","ranking":"1","transfer_safety":"1"},
      "warnings":[],
      "empty":{"code":"no_feasible_journey_option","required":true,"parameters":{}}
    }"#.utf8)

    private static let errorBody = Data(#"{
      "api_version":"1.0",
      "semantic_catalog_version":"1.0",
      "request_id":"req_test",
      "error":{"code":"invalid_request","required":true,"field":"/origin","parameters":{}}
    }"#.utf8)
}

private final class StubURLProtocol: URLProtocol, @unchecked Sendable {
    private static let lock = NSLock()
    private static var responseStatus = 200
    private static var responseBody = Data()
    private(set) static var lastRequest: URLRequest?

    static func reset(statusCode: Int, body: Data) {
        lock.lock()
        responseStatus = statusCode
        responseBody = body
        lastRequest = nil
        lock.unlock()
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        Self.lock.lock()
        Self.lastRequest = request
        let status = Self.responseStatus
        let body = Self.responseBody
        Self.lock.unlock()
        let response = HTTPURLResponse(
            url: request.url!,
            statusCode: status,
            httpVersion: nil,
            headerFields: ["Content-Type": "application/json"]
        )!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}
