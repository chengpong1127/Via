import SwiftUI

struct JourneyOptionCard: View {
    let option: JourneyOption
    let isSelected: Bool
    let onSelect: () -> Void
    let onShowDetails: () -> Void
    let onStart: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .top) {
                    VStack(alignment: .leading, spacing: 7) {
                        if let label = option.label {
                            Text(LocalizedStringKey(label.titleKey))
                                .font(.caption2.bold())
                                .textCase(.uppercase)
                                .tracking(0.8)
                                .foregroundStyle(.secondary)
                        }
                        HStack(spacing: 5) {
                            ForEach(Array(option.travelModes.enumerated()), id: \.offset) { index, mode in
                                ModeBadge(mode: mode, compact: true)
                                if index < option.travelModes.count - 1 {
                                    Image(systemName: "chevron.right")
                                        .font(.caption2.bold())
                                        .foregroundStyle(.tertiary)
                                }
                            }
                        }
                    }

                    Spacer()

                    VStack(alignment: .trailing, spacing: 1) {
                        Text(durationText)
                            .font(.title3.bold())
                        Text("Arrive \(option.arrivalTime.formatted(date: .omitted, time: .shortened))")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

                HStack {
                    Label(walkingText, systemImage: "figure.walk")
                    Spacer()
                    Label(transferText, systemImage: "arrow.triangle.branch")
                    Spacer()
                    if let observed = option.liveStatusSummary.observedAt {
                        Label(observed.formatted(date: .omitted, time: .shortened), systemImage: "clock.badge.checkmark")
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)

                if isSelected {
                    Divider()
                    HStack {
                        Button(action: onShowDetails) {
                            Label("Details", systemImage: "list.bullet")
                        }
                        .buttonStyle(.bordered)

                        Spacer()

                        Button("Start", action: onStart)
                            .buttonStyle(.borderedProminent)
                            .tint(Color.viaBlue)
                    }
                }
        }
        .padding(13)
        .background(.background, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 18, style: .continuous)
                .stroke(isSelected ? Color.viaBlue : .clear, lineWidth: 2)
        }
        .shadow(color: .black.opacity(0.06), radius: 8, y: 3)
        .contentShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
        .onTapGesture(perform: onSelect)
        .accessibilityElement(children: .contain)
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }

    private var durationText: String {
        let minutes = max(1, option.durationSeconds / 60)
        return String(localized: "\(minutes) min")
    }

    private var walkingText: String {
        String(localized: "\(option.walkingSeconds / 60) min walk")
    }

    private var transferText: String {
        option.transferCount == 1
            ? String(localized: "1 transfer")
            : String(localized: "\(option.transferCount) transfers")
    }
}
