import Foundation

enum APIError: Error, Equatable, Sendable {
    case invalidResponse
    case httpStatus(code: Int, body: Data)
    case transport(URLError.Code)
    case encoding
    case decoding
}
