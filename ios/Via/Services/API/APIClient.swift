import Foundation

struct APIClient: Sendable {
    let baseURL: URL
    let urlSession: URLSession
    let timeout: TimeInterval

    init(
        baseURL: URL,
        urlSession: URLSession = .shared,
        timeout: TimeInterval = 8
    ) {
        self.baseURL = baseURL
        self.urlSession = urlSession
        self.timeout = timeout
    }

    func send<Request: Encodable, Response: Decodable>(
        request payload: Request,
        path: String,
        response responseType: Response.Type = Response.self
    ) async throws -> Response {
        var urlRequest = URLRequest(url: baseURL.appendingPathComponent(path))
        urlRequest.httpMethod = "POST"
        urlRequest.timeoutInterval = timeout
        urlRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
        urlRequest.setValue("application/json", forHTTPHeaderField: "Accept")
        urlRequest.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        do {
            urlRequest.httpBody = try JSONEncoder().encode(payload)
        } catch {
            throw APIError.encoding
        }

        do {
            let (data, urlResponse) = try await urlSession.data(for: urlRequest)
            guard let httpResponse = urlResponse as? HTTPURLResponse else {
                throw APIError.invalidResponse
            }
            guard (200..<300).contains(httpResponse.statusCode) else {
                throw APIError.httpStatus(code: httpResponse.statusCode, body: data)
            }
            do {
                return try JSONDecoder().decode(responseType, from: data)
            } catch {
                throw APIError.decoding
            }
        } catch let error as APIError {
            throw error
        } catch let error as URLError {
            throw APIError.transport(error.code)
        }
    }
}
