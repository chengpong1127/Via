import Foundation

struct JourneyAPI: Sendable {
    let client: APIClient

    init(client: APIClient) {
        self.client = client
    }

    func plan(_ request: JourneyPlanRequestDTO) async throws -> JourneyPlanResponseDTO {
        try await client.send(
            request: request,
            path: "v1/journey-plans",
            response: JourneyPlanResponseDTO.self
        )
    }
}
