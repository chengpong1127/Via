import MapKit
import Observation

@Observable
final class SearchModel {
    var query = "" {
        didSet {
            guard query != oldValue else { return }
            if results != nil { results = nil }
            if failed { failed = false }
            service.update(query: query)
        }
    }
    private(set) var results: [PlaceResult]?
    private(set) var isSearching = false
    private(set) var failed = false

    let service: PlaceSearchService

    init(service: PlaceSearchService) {
        self.service = service
    }

    var completions: [MKLocalSearchCompletion] { service.completions }

    func clear() {
        query = ""
        results = nil
    }

    func submit() async -> [PlaceResult] {
        let text = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return [] }
        return await load { try await service.results(for: text) }
    }

    func choose(_ completion: MKLocalSearchCompletion) async -> [PlaceResult] {
        await load { try await service.results(for: completion) }
    }

    private func load(_ operation: () async throws -> [PlaceResult]) async -> [PlaceResult] {
        isSearching = true
        failed = false
        defer { isSearching = false }
        do {
            let found = try await operation()
            results = found
            return found
        } catch {
            results = []
            failed = true
            return []
        }
    }
}
