import SwiftUI

struct NavigationBanner: View {
    let navigation: NavigationModel

    var body: some View {
        VStack(spacing: 8) {
            HStack(alignment: .center, spacing: 14) {
                Image(systemName: symbol)
                    .font(.system(size: 34, weight: .semibold))
                    .frame(width: 48)
                    .foregroundStyle(tint)
                VStack(alignment: .leading, spacing: 2) {
                    headline
                        .font(.title2.bold())
                        .lineLimit(2)
                        .minimumScaleFactor(0.7)
                    subtitle
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
                Spacer(minLength: 0)
                if navigation.isRerouting {
                    ProgressView()
                }
            }
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .glassEffect(.regular, in: .rect(cornerRadius: 24))
            if let notice = navigation.notice {
                NoticeCapsule(alert: notice.alert)
                    .transition(.move(edge: .top).combined(with: .opacity))
                    .onTapGesture(perform: navigation.dismissNotice)
            }
        }
        .padding(.horizontal, 12)
        .animation(.smooth, value: navigation.notice)
        .sensoryFeedback(.warning, trigger: navigation.notice) { _, new in new != nil }
        .task(id: navigation.notice?.id) {
            guard navigation.notice != nil else { return }
            try? await Task.sleep(for: .seconds(6))
            navigation.dismissNotice()
        }
    }

    private var tracker: NavigationTracker { navigation.tracker }
    private var leg: Leg { navigation.currentLeg.leg }

    private var symbol: String {
        if navigation.isFinished { return "mappin.circle.fill" }
        if navigation.isRerouting { return "arrow.triangle.branch" }
        switch leg.kind {
        case let .transit(ride):
            return tracker.boarded ? ride.mode.symbol : "figure.walk.arrival"
        case .walking, .bikeShare:
            return tracker.nextManeuver?.guidance.navigationSymbol ?? "arrow.up"
        }
    }

    private var tint: Color {
        navigation.isFinished ? .red : leg.kind.routeColor
    }

    @ViewBuilder
    private var headline: some View {
        if navigation.isFinished {
            Text("You've arrived")
        } else if navigation.isRerouting {
            Text("Rerouting…")
        } else {
            switch leg.kind {
            case let .transit(ride):
                if !tracker.boarded {
                    Text("Board \(ride.serviceLabel)")
                } else if let stops = tracker.stopsRemaining, stops <= 1 {
                    Text("Get off next stop")
                } else if let stops = tracker.stopsRemaining {
                    Text("^[\(stops) stop](inflect: true) left")
                }
            case .walking, .bikeShare:
                if let distance = tracker.distanceToManeuver {
                    Text(distance.navigationDistance)
                }
            }
        }
    }

    @ViewBuilder
    private var subtitle: some View {
        if navigation.isFinished {
            Text(navigation.destination.name)
        } else if navigation.rerouteFailed {
            Text("Couldn't update the route. Keep following the current one.")
        } else {
            switch leg.kind {
            case let .transit(ride):
                if tracker.boarded {
                    Text("Get off at \(leg.to.name)")
                } else if ride.headsign.isEmpty {
                    Text("At \(leg.from.name) · \(leg.departureTime.formatted(.clockTime))")
                } else {
                    Text("Toward \(ride.headsign) · \(leg.from.name) · \(leg.departureTime.formatted(.clockTime))")
                }
            case .walking, .bikeShare:
                if let maneuver = tracker.nextManeuver {
                    maneuver.guidance.code == "arrive" ? arrival : maneuver.guidance.detailedInstruction
                }
            }
        }
    }

    private var arrival: Text {
        switch (leg.kind, navigation.tracker.nextLeg?.leg.kind) {
        case let (.bikeShare(ride), _):
            Text("Return the bike at \(ride.dropoffStationName)")
        case (_, .transit?):
            Text("Arrive at \(leg.to.name)")
        case let (_, .bikeShare(ride)?):
            Text("Pick up a YouBike at \(ride.pickupStationName)")
        default:
            Text("Arrive at \(navigation.destination.name)")
        }
    }
}

private struct NoticeCapsule: View {
    let alert: NavigationAlert

    var body: some View {
        Label {
            message
                .font(.headline)
        } icon: {
            Image(systemName: symbol)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .foregroundStyle(.white)
        .background(color, in: .capsule)
    }

    private var message: Text {
        switch alert {
        case .leaveNow: Text("Time to leave")
        case .hurry: Text("Hurry to make your connection")
        case let .stopsLeft(count): Text("^[\(count) stop](inflect: true) left")
        case let .nextStop(name): Text("Next stop: \(name)")
        case let .getOff(name): Text("Get off at \(name)")
        case .arrived: Text("You've arrived")
        }
    }

    private var symbol: String {
        switch alert {
        case .leaveNow: "figure.walk.departure"
        case .hurry: "hare.fill"
        case .stopsLeft, .nextStop: "bell.fill"
        case .getOff: "figure.walk.arrival"
        case .arrived: "mappin.circle.fill"
        }
    }

    private var color: Color {
        switch alert {
        case .hurry: .orange
        case .getOff, .nextStop: .red
        default: .blue
        }
    }
}

struct NavigationTray: View {
    @Bindable var navigation: NavigationModel
    let end: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(navigation.estimatedArrival.formatted(.clockTime))
                        .font(.title3.bold())
                        .monospacedDigit()
                    Group {
                        if navigation.isFinished {
                            Text("Arrived")
                        } else {
                            Text("\(remaining.travelTime) · arrival")
                        }
                    }
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                }
                Spacer()
                Button {
                    withAnimation(.smooth) { navigation.isMinimized.toggle() }
                } label: {
                    Image(systemName: navigation.isMinimized ? "chevron.up" : "chevron.down")
                        .font(.body.weight(.semibold))
                        .frame(width: 22, height: 22)
                }
                .buttonStyle(.glass)
                .buttonBorderShape(.circle)
                .accessibilityLabel(navigation.isMinimized ? Text("Show steps") : Text("Hide steps"))
                Button(navigation.isFinished ? "Done" : "End", role: .destructive, action: end)
                    .buttonStyle(.glassProminent)
                    .tint(.red)
            }
            if !navigation.isMinimized {
                Divider()
                LegTimeline(navigation: navigation)
            }
        }
        .padding(16)
        .glassEffect(.regular, in: .rect(cornerRadius: 24))
        .padding(.horizontal, 12)
    }

    private var remaining: TimeInterval {
        max(navigation.estimatedArrival.timeIntervalSince(navigation.now), 0)
    }
}

private struct LegTimeline: View {
    let navigation: NavigationModel

    var body: some View {
        let legs = navigation.tracker.route.legs
        let current = navigation.tracker.legIndex
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(legs.enumerated()), id: \.element.leg.id) { index, item in
                HStack(spacing: 12) {
                    LegBadge(leg: item.leg, compact: true)
                        .frame(minWidth: 44, alignment: .leading)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(summary(item.leg))
                            .font(.subheadline.weight(index == current ? .semibold : .regular))
                        Text(item.leg.departureTime.formatted(.clockTime))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Spacer(minLength: 0)
                    if index < current || navigation.isFinished {
                        Image(systemName: "checkmark.circle.fill")
                            .foregroundStyle(.secondary)
                    } else if index == current {
                        Image(systemName: "location.fill")
                            .foregroundStyle(.blue)
                    }
                }
                .opacity(index < current ? 0.5 : 1)
            }
        }
    }

    private func summary(_ leg: Leg) -> LocalizedStringKey {
        switch leg.kind {
        case .walking:
            "Walk \(leg.duration.travelTime) to \(leg.to.name)"
        case let .transit(ride):
            ride.intermediateStops.isEmpty
                ? "Ride to \(leg.to.name)"
                : "Ride ^[\(ride.intermediateStops.count + 1) stop](inflect: true) to \(leg.to.name)"
        case let .bikeShare(ride):
            "Ride YouBike to \(ride.dropoffStationName)"
        }
    }
}

extension Guidance {
    var detailedInstruction: Text {
        guard let street = streetName, code != "arrive", code != "depart" else { return Text(instruction) }
        return Text("\(Text(instruction)) onto \(street)")
    }

    var navigationSymbol: String {
        code == "arrive" ? "mappin.and.ellipse" : symbol
    }
}

extension Double {
    var navigationDistance: String {
        let rounded = self < 100 ? (self / 5).rounded() * 5 : (self / 10).rounded() * 10
        return Measurement(value: rounded, unit: UnitLength.meters)
            .formatted(.measurement(width: .abbreviated, usage: .road))
    }
}
