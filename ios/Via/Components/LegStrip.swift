import SwiftUI

struct LegStrip: View {
    let legs: [Leg]

    var body: some View {
        ViewThatFits(in: .horizontal) {
            strip(compact: false)
            strip(compact: true)
        }
    }

    private var visibleLegs: [Leg] {
        legs.filter { $0.kind != .walking || $0.duration >= 60 }
    }

    private func strip(compact: Bool) -> some View {
        HStack(spacing: 8) {
            ForEach(Array(visibleLegs.enumerated()), id: \.element.id) { index, leg in
                if index > 0 {
                    Image(systemName: "chevron.right")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.tertiary)
                }
                LegBadge(leg: leg, compact: compact)
            }
        }
        .fixedSize()
    }
}

struct LegBadge: View {
    let leg: Leg
    var compact = false

    var body: some View {
        switch leg.kind {
        case .walking:
            MinutesSymbol(symbol: "figure.walk", duration: leg.duration, showsMinutes: !compact)
                .foregroundStyle(.primary)
        case let .transit(ride):
            HStack(spacing: 4) {
                ServiceBadge(ride: ride)
                if !compact {
                    Image(systemName: ride.mode.outlineSymbol)
                        .font(.title3)
                        .foregroundStyle(.primary)
                }
            }
        case .bikeShare:
            MinutesSymbol(symbol: "bicycle", duration: leg.duration, showsMinutes: !compact)
                .foregroundStyle(Color.bikeShare)
        }
    }
}

private struct MinutesSymbol: View {
    let symbol: String
    let duration: TimeInterval
    let showsMinutes: Bool

    var body: some View {
        HStack(alignment: .lastTextBaseline, spacing: 1) {
            Image(systemName: symbol)
                .font(.title3)
            if showsMinutes {
                Text(verbatim: "\(Int((duration / 60).rounded(.up)))")
                    .font(.caption.weight(.semibold))
                    .baselineOffset(-4)
            }
        }
    }
}

struct ServiceBadge: View {
    let ride: TransitRide

    var body: some View {
        let style = ride.lineStyle
        Text(ride.shortLabel)
            .font(.subheadline.weight(.bold))
            .lineLimit(1)
            .foregroundStyle(style.foreground)
            .padding(.horizontal, 7)
            .frame(minWidth: 30, minHeight: 28)
            .background(style.background, in: .rect(cornerRadius: 6))
            .fixedSize()
            .accessibilityLabel(Text("\(String(localized: ride.mode.title)) \(ride.serviceLabel)"))
    }
}
