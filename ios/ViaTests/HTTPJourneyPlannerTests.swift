import Foundation
import Testing
@testable import Via

@Suite(.serialized)
struct HTTPJourneyPlannerTests {
    let request = PlanningRequest(
        origin: Place(name: "A", coordinate: Coordinate(latitude: 25.04, longitude: 121.51)),
        destination: Place(name: "B", coordinate: Coordinate(latitude: 25.03, longitude: 121.56)),
        modes: [.metro],
        departure: .now
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
