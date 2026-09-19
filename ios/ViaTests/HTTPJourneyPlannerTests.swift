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

    @Test("HTTP planner decodes a normal server-authoritative Journey Option")
    func decodesNormalResult() async throws {
        StubURLProtocol.reset(statusCode: 200, body: Self.normalResult)
        let planner = HTTPJourneyPlanner(
            baseURL: URL(string: "https://api.example.test")!,
            urlSession: Self.stubSession()
        )

        let result = try await planner.plan(Self.request())

        #expect(result.options.count == 1)
        #expect(result.options[0].id == "option_01_fixture_match")
        #expect(result.options[0].journeyClass == .reliable)
        #expect(result.options[0].travelModes == [.walking, .transit])
        #expect(result.options[0].legs[0].mode == .transit)
        #expect(result.options[0].legs[0].transit?.mode == .metro)
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

    private static let normalResult = Data(#"{
      "api_version":"1.0",
      "semantic_catalog_version":"1.0",
      "generated_at":"2026-08-29T10:08:00+08:00",
      "network_revision":{
        "static_network_revision":"static_test",
        "schedule_revision":"schedule_test"
      },
      "journey_options":[{
        "id":"option_01_fixture_match",
        "matching_key":"match_fixture",
        "class":"reliable",
        "label":"best_now",
        "departure_time":"2026-08-29T10:08:00+08:00",
        "arrival_time":"2026-08-29T10:18:00+08:00",
        "duration_seconds":600,
        "walking_seconds":600,
        "transfer_count":0,
        "travel_modes":["walking","transit"],
        "live_status_summary":{"state":"not_applicable","observed_at":null},
        "overview_geometry":{"encoding":"polyline6","points":"??"},
        "legs":[{
          "id":"leg_01_1_fixture_match",
          "mode":"transit",
          "from":{"display_name":"Origin"},
          "to":{"display_name":"Destination"},
          "duration_seconds":600,
          "guidance":[],
          "details":{"type":"transit","transit_mode":"metro","via_service_id":"metro-1","via_boarding_point_id":"stop-1","via_alighting_point_id":"stop-2","service_label":"Blue Line","headsign":"Taipei City Hall"}
        }]
      }]
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
