import SwiftUI

struct JourneyDetailPanel: View {
    let model: ExploreModel
    let option: JourneyOption

    var body: some View {
        VStack(spacing: 0) {
            header
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    LegStrip(legs: option.legs)
                        .padding(.bottom, 16)
                    ForEach(Array(option.legs.enumerated()), id: \.element.id) { index, leg in
                        StopRow(
                            name: index == 0 ? originName(leg) : leg.from.name,
                            time: leg.departureTime,
                            color: leg.kind.routeColor,
                            wait: index == 0 ? 0 : leg.departureTime.timeIntervalSince(option.legs[index - 1].arrivalTime)
                        )
                        LegRow(leg: leg, isFocused: model.focusedLegID == leg.id) {
                            model.focus(leg)
                        }
                    }
                    if let last = option.legs.last {
                        StopRow(name: last.to.name, time: last.arrivalTime, color: .red, wait: 0, isDestination: true)
                    }
                    attributions
                }
                .padding(.horizontal, 20)
                .padding(.bottom, 24)
            }
            .scrollBounceBehavior(.basedOnSize)
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            CloseButton(systemImage: "chevron.left", action: model.closeJourney)
            VStack(alignment: .leading, spacing: 2) {
                Text(option.duration.travelTime)
                    .font(.title2.bold())
                HStack(spacing: 6) {
                    Text("\(option.departureTime.formatted(.clockTime))–\(option.arrivalTime.formatted(.clockTime))")
                    LiveBadge(status: option.liveStatus)
                }
                .font(.subheadline)
                .foregroundStyle(.secondary)
            }
            Spacer()
        }
        .padding(.horizontal, 20)
        .padding(.top, 20)
        .padding(.bottom, 12)
    }

    private func originName(_ leg: Leg) -> String {
        model.directions.origin == .currentLocation ? String(localized: "My Location") : leg.from.name
    }

    @ViewBuilder
    private var attributions: some View {
        let items = option.attributions
        if !items.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(items, id: \.self) { attribution in
                    if let url = attribution.url {
                        Link("\(attribution.text) (\(attribution.licence))", destination: url)
                    } else {
                        Text(verbatim: "\(attribution.text) (\(attribution.licence))")
                    }
                }
            }
            .font(.caption2)
            .foregroundStyle(.secondary)
            .padding(.top, 20)
        }
    }
}

private struct StopRow: View {
    let name: String
    let time: Date
    let color: Color
    let wait: TimeInterval
    var isDestination = false

    var body: some View {
        HStack(alignment: .center, spacing: 14) {
            Group {
                if isDestination {
                    Image(systemName: "mappin.circle.fill")
                        .font(.title3)
                        .foregroundStyle(.red)
                } else {
                    Circle()
                        .fill(.background)
                        .stroke(color, lineWidth: 3)
                        .frame(width: 14, height: 14)
                }
            }
            .frame(width: 28)
            VStack(alignment: .leading, spacing: 1) {
                Text(name)
                    .font(.headline)
                    .lineLimit(2)
                if wait >= 60 {
                    Text("Wait \(wait.travelTime)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            Spacer(minLength: 8)
            Text(time.formatted(.clockTime))
                .font(.subheadline.weight(.medium))
                .monospacedDigit()
                .foregroundStyle(.secondary)
        }
        .padding(.vertical, 6)
    }
}

private struct LegRow: View {
    let leg: Leg
    let isFocused: Bool
    let focus: () -> Void
    @State private var expanded = false

    var body: some View {
        HStack(alignment: .top, spacing: 14) {
            connector
                .frame(width: 28)
            VStack(alignment: .leading, spacing: 8) {
                Button(action: focus) {
                    content
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(.rect)
                }
                .buttonStyle(.plain)
                if !steps.isEmpty {
                    DisclosureGroup(isExpanded: $expanded) {
                        VStack(alignment: .leading, spacing: 8) {
                            ForEach(Array(steps.enumerated()), id: \.offset) { _, step in
                                Label {
                                    Text(step.instruction)
                                } icon: {
                                    Image(systemName: step.symbol)
                                }
                                .font(.subheadline)
                            }
                        }
                        .padding(.top, 6)
                    } label: {
                        Text("^[\(steps.count) step](inflect: true)")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .padding(.vertical, 12)
            .padding(.horizontal, 12)
            .background(isFocused ? AnyShapeStyle(.fill.quaternary) : AnyShapeStyle(.clear), in: .rect(cornerRadius: 12))
        }
    }

    private var steps: [Guidance] {
        guard case .walking = leg.kind else { return [] }
        return leg.guidance.filter { $0.code != "depart" && $0.code != "arrive" }
    }

    @ViewBuilder
    private var connector: some View {
        GeometryReader { proxy in
            Path { path in
                path.move(to: CGPoint(x: proxy.size.width / 2, y: 0))
                path.addLine(to: CGPoint(x: proxy.size.width / 2, y: proxy.size.height))
            }
            .stroke(
                leg.kind.routeColor,
                style: leg.kind == .walking
                    ? StrokeStyle(lineWidth: 4, lineCap: .round, dash: [0.5, 8])
                    : StrokeStyle(lineWidth: 6, lineCap: .round)
            )
        }
    }

    @ViewBuilder
    private var content: some View {
        switch leg.kind {
        case .walking:
            Label {
                Text("Walk \(leg.duration.travelTime)")
                    .font(.subheadline.weight(.semibold))
            } icon: {
                Image(systemName: "figure.walk")
            }
            .foregroundStyle(.secondary)
        case let .transit(ride):
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 8) {
                    ServiceBadge(ride: ride)
                    if ride.serviceLabel != String(localized: ride.mode.title) {
                        Text(ride.mode.title)
                            .font(.subheadline.weight(.semibold))
                    }
                }
                if !ride.headsign.isEmpty {
                    Text("Toward \(ride.headsign)")
                        .font(.subheadline)
                }
                Text("Ride \(leg.duration.travelTime) · Get off at \(leg.to.name)")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
        case let .bikeShare(ride):
            VStack(alignment: .leading, spacing: 6) {
                Label {
                    Text("Ride YouBike \(leg.duration.travelTime)")
                        .font(.subheadline.weight(.semibold))
                } icon: {
                    Image(systemName: "bicycle")
                        .foregroundStyle(Color.bikeShare)
                }
                Text("Return at \(ride.dropoffStationName)")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
        }
    }
}

extension Guidance {
    var instruction: LocalizedStringKey {
        switch code {
        case "continue": "Continue straight"
        case "turn_left": "Turn left"
        case "turn_right": "Turn right"
        case "u_turn": "Make a U-turn"
        case "enter_roundabout": "Enter the roundabout"
        case "exit_roundabout": "Exit the roundabout"
        case "depart": "Start walking"
        case "arrive": "Arrive"
        default: LocalizedStringKey(code.replacingOccurrences(of: "_", with: " ").capitalized)
        }
    }

    var symbol: String {
        switch code {
        case "turn_left": "arrow.turn.up.left"
        case "turn_right": "arrow.turn.up.right"
        case "u_turn": "arrow.uturn.down"
        case "enter_roundabout", "exit_roundabout": "arrow.triangle.2.circlepath"
        default: "arrow.up"
        }
    }
}
