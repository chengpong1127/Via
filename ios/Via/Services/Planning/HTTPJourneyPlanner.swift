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
    static var baseURL: URL {
        let configured = Bundle.main.object(forInfoDictionaryKey: "VIA_API_BASE_URL") as? String
        if let configured, let url = URL(string: configured), url.host() != nil {
#if !DEBUG
            precondition(url.scheme == "https", "VIA_API_BASE_URL must use HTTPS in Release builds")
#endif
            return url
        }
#if DEBUG
        return URL(string: "http://127.0.0.1:3000")!
#else
        preconditionFailure("VIA_API_BASE_URL must be configured for Release builds")
#endif
    }
}
