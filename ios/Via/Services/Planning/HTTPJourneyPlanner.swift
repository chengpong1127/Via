import Foundation

nonisolated struct HTTPJourneyPlanner: JourneyPlanner {
    private let client: APIClient

    init(client: APIClient) {
        self.client = client
    }

    func plan(_ request: PlanningRequest) async throws -> JourneyPlan {
        let outcome: PlanningOutcomeDTO
        do {
            outcome = try await client.post(
                "v1/journey-plans",
                body: PlanningMapper.dto(for: request),
                as: PlanningOutcomeDTO.self
            )
        } catch let error as APIError {
            throw Self.planningError(for: error)
        }
        return try PlanningMapper.plan(from: outcome)
    }

    static func planningError(for error: APIError) -> JourneyPlanningError {
        switch error {
        case let .http(status, body):
            switch status {
            case 400: .invalidRequest(field: body?.field)
            case 408: .timedOut
            case 503: .temporarilyUnavailable
            default: .unexpected
            }
        case .transport(.timedOut):
            .timedOut
        case .transport:
            .serverUnreachable
        case .invalidResponse, .decoding:
            .unsupportedResponse
        }
    }
}

nonisolated enum ViaServer {
    static let unconfiguredURL = URL(string: "https://unconfigured.invalid")!

    static var baseURL: URL {
        let configured = Bundle.main.object(forInfoDictionaryKey: "VIA_API_BASE_URL") as? String
#if DEBUG
        return validatedURL(configured, allowsInsecure: true) ?? URL(string: "http://127.0.0.1:3000")!
#else
        return validatedURL(configured, allowsInsecure: false) ?? unconfiguredURL
#endif
    }

    static func validatedURL(_ configured: String?, allowsInsecure: Bool) -> URL? {
        guard let configured,
              let url = URL(string: configured.trimmingCharacters(in: .whitespaces)),
              url.host() != nil,
              let scheme = url.scheme?.lowercased()
        else { return nil }
        return scheme == "https" || (allowsInsecure && scheme == "http") ? url : nil
    }
}
