import SwiftUI

struct JourneyDetailPanel: View {
    let model: ExploreModel
    let option: JourneyOption

    var body: some View {
        VStack(spacing: 0) {
            header
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
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
                }
                .padding(.horizontal, 20)
                .padding(.bottom, 24)
            }
            .scrollBounceBehavior(.basedOnSize)
            .opacity(model.detent == .collapsed ? 0 : 1)
            .animation(.smooth(duration: 0.2), value: model.detent)
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 12) {
            summary
            if model.detent != .collapsed {
                LegStrip(legs: option.legs)
                    .transition(.opacity)
            }
        }
        .animation(.smooth(duration: 0.2), value: model.detent)
        .padding(.horizontal, 20)
        .padding(.top, 20)
        .padding(.bottom, 12)
    }

    private var summary: some View {
        HStack(alignment: .center, spacing: 12) {
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
            if model.canNavigate {
                Button("Start", systemImage: "location.north.fill") {
                    model.startNavigation(option)
                }
                .buttonStyle(.glassProminent)
                .font(.headline)
            }
        }
    }

    private func originName(_ leg: Leg) -> String {
        model.directions.origin == .currentLocation ? String(localized: "My Location") : leg.from.name
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
    @State private var showsStops = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top, spacing: 14) {
                LineSegment(color: leg.kind.routeColor)
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
                            stepList
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
            if let ride = leg.transit {
                stopsSection(ride)
            }
        }
    }

    @ViewBuilder
    private func stopsSection(_ ride: TransitRide) -> some View {
        let color = leg.kind.routeColor
        let summary = Text("Go ^[\(ride.intermediateStops.count + 1) stop](inflect: true), \(leg.duration.travelTime)")
        if ride.intermediateStops.isEmpty {
            lineRow(LineSegment(color: color)) {
                summary.foregroundStyle(.secondary)
            }
        } else {
            lineRow(LineSegment(color: color)) {
                Button {
                    withAnimation(.smooth(duration: 0.25)) { showsStops.toggle() }
                } label: {
                    Group {
                        if showsStops {
                            Text("Hide stops")
                        } else {
                            summary
                        }
                    }
                    .foregroundStyle(Color.accentColor)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
            }
            if showsStops {
                ForEach(Array(ride.intermediateStops.enumerated()), id: \.offset) { _, stop in
                    lineRow(ZStack {
                        LineSegment(color: color)
                        StopDot(color: color, size: 11, lineWidth: 2.5)
                    }) {
                        HStack {
                            Text(stop.name)
                                .lineLimit(1)
                            Spacer(minLength: 8)
                            if let arrival = stop.arrivalTime {
                                Text(arrival.formatted(.clockTime))
                                    .monospacedDigit()
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }
                .transition(.opacity)
            }
        }
    }

    private func lineRow(_ line: some View, @ViewBuilder label: () -> some View) -> some View {
        HStack(spacing: 14) {
            line
                .frame(width: 28)
            label()
                .font(.subheadline)
                .padding(.leading, 12)
                .padding(.vertical, 7)
        }
    }

    private var stepList: some View {
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
    }

    private var walkingSummary: String {
        let walk = String(localized: "Walk \(leg.duration.travelTime)")
        let meters = zip(leg.path, leg.path.dropFirst()).map { $0.distance(to: $1) }.reduce(0, +)
        guard meters >= 10 else { return walk }
        let distance = Measurement(value: meters, unit: UnitLength.meters)
            .formatted(.measurement(width: .abbreviated, usage: .road))
        return "\(walk) · \(distance)"
    }

    private var steps: [Guidance] {
        guard case .bikeShare = leg.kind else { return [] }
        return leg.guidance.filter { !["depart", "arrive", "bike_share"].contains($0.code) }
    }

    @ViewBuilder
    private var content: some View {
        switch leg.kind {
        case .walking:
            Label {
                Text(walkingSummary)
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
                VStack(alignment: .leading, spacing: 1) {
                    Text("Get off at \(leg.to.name)")
                        .font(.subheadline.weight(.semibold))
                    if let previous = ride.intermediateStops.last {
                        Text("After \(previous.name)")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    }
                }
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

private struct LineSegment: View {
    let color: Color

    var body: some View {
        GeometryReader { proxy in
            Path { path in
                path.move(to: CGPoint(x: proxy.size.width / 2, y: 0))
                path.addLine(to: CGPoint(x: proxy.size.width / 2, y: proxy.size.height))
            }
            .stroke(color, style: StrokeStyle(lineWidth: 6, lineCap: .round))
        }
    }
}
