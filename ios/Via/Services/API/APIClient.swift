import Foundation

nonisolated enum APIError: Error, Equatable, Sendable {
    case http(status: Int, error: APIErrorBody?)
    case transport(URLError.Code)
    case invalidResponse
    case decoding
}

nonisolated struct APIErrorBody: Decodable, Equatable, Sendable {
    let code: String
    let field: String?
}

nonisolated struct APIClient: Sendable {
    let baseURL: URL
    let session: URLSession
    let timeout: TimeInterval

    init(baseURL: URL, session: URLSession = .shared, timeout: TimeInterval = 30) {
        self.baseURL = baseURL
        self.session = session
        self.timeout = timeout
    }

    func post<Body: Encodable, Response: Decodable>(
        _ path: String,
        body: Body,
        as response: Response.Type
    ) async throws -> Response {
        var request = URLRequest(url: baseURL.appending(path: path))
        request.httpMethod = "POST"
        request.timeoutInterval = timeout
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.httpBody = try JSONEncoder().encode(body)

        let data: Data
        let urlResponse: URLResponse
        do {
            (data, urlResponse) = try await session.data(for: request)
        } catch let error as URLError {
            throw error.code == .cancelled ? CancellationError() : APIError.transport(error.code)
        }

        guard let http = urlResponse as? HTTPURLResponse else {
            throw APIError.invalidResponse
        }
        guard (200..<300).contains(http.statusCode) else {
            let envelope = try? JSONDecoder().decode(ErrorEnvelope.self, from: data)
            throw APIError.http(status: http.statusCode, error: envelope?.error)
        }
        do {
            return try JSONDecoder().decode(Response.self, from: data)
        } catch {
            throw APIError.decoding
        }
    }
}

private nonisolated struct ErrorEnvelope: Decodable {
    let error: APIErrorBody
}
