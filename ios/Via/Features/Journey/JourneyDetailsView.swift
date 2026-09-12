import SwiftUI

struct JourneyDetailsView: View {
    @Environment(\.dismiss) private var dismiss
    let option: JourneyOption

    var body: some View {
        NavigationStack {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    summary
                    ForEach(option.legs) { leg in
                        legView(leg)
                    }
                    Label("Guidance is static and does not track your progress.", systemImage: "location.slash")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .padding(.vertical, 22)
                }
                .padding(.horizontal, 18)
            }
            .navigationTitle("Details")
            .navigationBarTitleDisplayMode(.large)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done", action: dismiss.callAsFunction)
                }
            }
        }
    }

    private var summary: some View {
        HStack(alignment: .bottom) {
            VStack(alignment: .leading, spacing: 3) {
                Text("\(option.durationSeconds / 60) min")
                    .font(.title2.bold())
                Text("Arrive \(option.arrivalTime.formatted(date: .omitted, time: .shortened))")
                    .foregroundStyle(.secondary)
            }
            Spacer()
            HStack(spacing: 4) {
                ForEach(option.travelModes) { mode in
                    ModeBadge(mode: mode, compact: true)
                }
            }
        }
        .padding(.bottom, 16)
    }

    private func legView(_ leg: JourneyLeg) -> some View {
        HStack(alignment: .top, spacing: 13) {
            Image(systemName: leg.symbolName)
                .font(.headline)
                .foregroundStyle(.white)
                .frame(width: 39, height: 39)
                .background(Color.viaBlue, in: RoundedRectangle(cornerRadius: 11, style: .continuous))
                .accessibilityHidden(true)

            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .firstTextBaseline) {
                    Text(leg.title).font(.headline)
                    Spacer()
                    Text("\(max(1, leg.durationSeconds / 60)) min")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Text(leg.subtitle)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)

                ForEach(leg.guidance) { instruction in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(instruction.title).font(.subheadline.weight(.semibold))
                        if let detail = instruction.detail {
                            Text(detail).font(.footnote).foregroundStyle(.secondary)
                        }
                    }
                    .padding(.top, 5)
                }
            }
        }
        .padding(.vertical, 18)
        .overlay(alignment: .bottom) { Divider() }
        .accessibilityElement(children: .combine)
    }
}
