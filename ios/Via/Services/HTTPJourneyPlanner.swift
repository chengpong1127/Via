import Foundation

/// The app-facing production planner. Transport and server-shape details stay
/// in APIClient and are mapped back into the iOS domain model here.
actor HTTPJourneyPlanner: JourneyPlanning {
    private let client: APIClient

    init(baseURL: URL, urlSession: URLSession = .shared) {
        client = APIClient(baseURL: baseURL, urlSession: urlSession)
    }

    func plan(_ request: PlanningRequest) async throws -> JourneyResult {
        do {
            let response = try await client.send(
                request: JourneyPlanRequestDTO(request: request),
                path: "v1/journey-plans",
                response: JourneyPlanResponseDTO.self
            )
            return try JourneyMapper.result(from: response)
        } catch let error as JourneyPlanningError {
            throw error
        } catch let error as APIError {
            throw planningError(for: error)
        } catch is CancellationError {
            throw CancellationError()
        } catch {
            throw JourneyPlanningError.unexpectedFailure
        }
    }

    private func planningError(for error: APIError) -> Error {
        switch error {
        case let .httpStatus(code, body):
            switch code {
            case 400:
                return JourneyPlanningError.invalidRequest(
                    field: JourneyMapper.invalidRequestField(from: body) ?? "/"
                )
            case 503:
                return JourneyPlanningError.temporarilyUnavailable
            default:
                return JourneyPlanningError.unexpectedFailure
            }
        case let .transport(code):
            return code == .cancelled
                ? CancellationError()
                : JourneyPlanningError.temporarilyUnavailable
        case .invalidResponse, .decoding:
            return JourneyPlanningError.unsupportedResponse
        case .encoding:
            return JourneyPlanningError.unexpectedFailure
        }
    }
}

enum ViaAPIConfiguration {
    static var baseURL: URL {
        if let configured = Bundle.main.object(forInfoDictionaryKey: "VIA_API_BASE_URL") as? String,
           let url = URL(string: configured),
           url.scheme != nil,
           url.host != nil {
#if !DEBUG
            precondition(url.scheme?.lowercased() == "https", "VIA_API_BASE_URL must use HTTPS in Release builds")
#endif
            return url
        }
#if DEBUG
        // Localhost is intentionally available only to development builds.
        return URL(string: "http://127.0.0.1:3000")!
#else
        preconditionFailure("VIA_API_BASE_URL must be configured for Release builds")
#endif
    }
}
