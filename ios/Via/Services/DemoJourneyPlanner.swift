import Foundation

actor DemoJourneyPlanner: JourneyPlanning {
    func plan(_ request: PlanningRequest) async throws -> JourneyResult {
        try await Task.sleep(for: .milliseconds(550))
        try Task.checkCancellation()

        let allOptions = makeOptions(origin: request.origin.coordinate, destination: request.destination.coordinate)
        let options = allOptions.filter { option in
            option.travelModes
                .filter { $0 != .walking }
                .allSatisfy(request.selectedModes.contains)
        }

        let generatedAt = Date()
        return JourneyResult(
            apiVersion: "1.0",
            semanticCatalogVersion: "1.0",
            generatedAt: generatedAt,
            networkRevision: NetworkRevision(
                staticNetworkRevision: "demo-static-1",
                scheduleRevision: "demo-schedule-1"
            ),
            options: Array(options.prefix(3))
        )
    }

    private func makeOptions(origin: Coordinate, destination: Coordinate) -> [JourneyOption] {
        let now = Date()
        let taipeiMain = origin
        let destination = destination
        let shandao = Coordinate(latitude: 25.0447, longitude: 121.5236)
        let cityHall = Coordinate(latitude: 25.0410, longitude: 121.5654)
        let xinyi = Coordinate(latitude: 25.0337, longitude: 121.5648)

        return [
            JourneyOption(
                id: "option_metro",
                matchingKey: "metro-blue-xinyi-walk",
                journeyClass: .reliable,
                label: .bestNow,
                departureTime: now.addingTimeInterval(180),
                arrivalTime: now.addingTimeInterval(1_740),
                durationSeconds: 1_560,
                walkingSeconds: 420,
                transferCount: 1,
                travelModes: [.walking, .transit],
                liveStatusSummary: LiveStatusSummary(state: .fresh, observedAt: now.addingTimeInterval(-120)),
                overview: interpolate([taipeiMain, shandao, cityHall, xinyi, destination]),
                legs: [
                    JourneyLeg(id: "walk-main", mode: .walking, title: "Walk to Taipei Main Station", subtitle: "Enter through M8", durationSeconds: 240, guidance: [GuidanceInstruction(title: "Walk toward the Blue Line", detail: "Use entrance M8 and follow signs for Bannan Line.")]),
                    JourneyLeg(id: "metro-blue", mode: .transit, title: "Bannan Line toward Nangang Exhibition Center", subtitle: "Taipei Main Station → Taipei City Hall · 5 stops", durationSeconds: 900, guidance: [GuidanceInstruction(title: "Board the Bannan Line", detail: "Ride toward Nangang Exhibition Center and leave at Taipei City Hall.")], transit: TransitLegMetadata(mode: .metro, serviceID: "demo-blue", serviceLabel: "Bannan Line", headsign: "Nangang Exhibition Center", boardingPointID: "taipei-main", alightingPointID: "city-hall")),
                    JourneyLeg(id: "walk-xinyi", mode: .walking, title: "Walk to the destination", subtitle: "About 3 minutes", durationSeconds: 180, guidance: [GuidanceInstruction(title: "Leave from Exit 2", detail: "Continue southeast toward the destination.")])
                ]
            ),
            JourneyOption(
                id: "option_bus",
                matchingKey: "bus-307-walk",
                journeyClass: .reliable,
                label: .fewerTransfers,
                departureTime: now.addingTimeInterval(240),
                arrivalTime: now.addingTimeInterval(1_920),
                durationSeconds: 1_680,
                walkingSeconds: 360,
                transferCount: 0,
                travelModes: [.walking, .transit],
                liveStatusSummary: LiveStatusSummary(state: .fresh, observedAt: now.addingTimeInterval(-70)),
                overview: interpolate([taipeiMain, Coordinate(latitude: 25.0440, longitude: 121.5360), Coordinate(latitude: 25.0390, longitude: 121.5520), destination]),
                legs: [
                    JourneyLeg(id: "walk-bus", mode: .walking, title: "Walk to the 307 bus stop", subtitle: "Zhongxiao West Road", durationSeconds: 180, guidance: [GuidanceInstruction(title: "Walk to the bus stop", detail: "Board from the eastbound stop.")]),
                    JourneyLeg(id: "bus-307", mode: .transit, title: "Bus 307 toward Xinyi District", subtitle: "8 stops", durationSeconds: 1_320, guidance: [GuidanceInstruction(title: "Board bus 307", detail: "Get off near Taipei 101.")], transit: TransitLegMetadata(mode: .bus, serviceID: "demo-307", serviceLabel: "Bus 307", headsign: "Xinyi District", boardingPointID: "bus-stop-origin", alightingPointID: "bus-stop-destination")),
                    JourneyLeg(id: "walk-arrive", mode: .walking, title: "Walk to the destination", subtitle: "About 3 minutes", durationSeconds: 180, guidance: [GuidanceInstruction(title: "Continue to the destination", detail: nil)])
                ]
            ),
        ]
    }

    private func interpolate(_ controlPoints: [Coordinate]) -> [Coordinate] {
        guard controlPoints.count > 1 else { return controlPoints }
        return zip(controlPoints, controlPoints.dropFirst()).flatMap { start, end in
            (0..<8).map { step in
                let fraction = Double(step) / 8
                return Coordinate(
                    latitude: start.latitude + ((end.latitude - start.latitude) * fraction),
                    longitude: start.longitude + ((end.longitude - start.longitude) * fraction)
                )
            }
        } + [controlPoints.last!]
    }
}
