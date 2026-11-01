import SwiftUI

struct LegStrip: View {
    let legs: [Leg]

    var body: some View {
        ViewThatFits(in: .horizontal) {
            strip(compact: false)
            strip(compact: true)
        }
    }

    private func strip(compact: Bool) -> some View {
        HStack(spacing: 4) {
            ForEach(Array(legs.enumerated()), id: \.element.id) { index, leg in
                if index > 0 {
                    Image(systemName: "chevron.compact.right")
                        .font(.caption2)
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
            HStack(spacing: 1) {
                Image(systemName: "figure.walk")
                if !compact {
                    Text(verbatim: "\(Int((leg.duration / 60).rounded(.up)))")
                        .font(.caption2.weight(.semibold))
                }
            }
            .foregroundStyle(.secondary)
            .font(.subheadline)
        case let .transit(ride):
            ServiceBadge(ride: ride, compact: compact)
        case .bikeShare:
            Image(systemName: "bicycle")
                .font(.caption.weight(.bold))
                .foregroundStyle(.white)
                .padding(.horizontal, 6)
                .padding(.vertical, 3)
                .background(leg.kind.color, in: .rect(cornerRadius: 5))
        }
    }
}

struct ServiceBadge: View {
    let ride: TransitRide
    var compact = false

    var body: some View {
        HStack(spacing: 3) {
            Image(systemName: ride.mode.symbol)
            if !compact || ride.serviceLabel.count <= 4 {
                Text(ride.serviceLabel)
                    .lineLimit(1)
            }
        }
        .font(.caption.weight(.bold))
        .foregroundStyle(.white)
        .padding(.horizontal, 6)
        .padding(.vertical, 3)
        .background(ride.mode.color, in: .rect(cornerRadius: 5))
    }
}
