import SwiftUI

struct ModeButton: View {
    let modes: Set<PlanningMode>
    let action: () -> Void

    private var selected: [PlanningMode] { PlanningMode.allCases.filter(modes.contains) }

    var body: some View {
        Button(action: action) {
            HStack(spacing: 6) {
                if selected.isEmpty {
                    Image(systemName: "figure.walk")
                    Text("Walk Only")
                } else {
                    ForEach(selected) { mode in
                        Image(systemName: mode.symbol)
                            .foregroundStyle(mode.color)
                    }
                }
                Image(systemName: "chevron.down")
                    .font(.caption.weight(.bold))
            }
            .font(.subheadline.weight(.semibold))
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(.fill.tertiary, in: .capsule)
            .contentShape(.capsule)
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Transport Modes")
        .accessibilityValue(selected.isEmpty ? Text("Walk Only") : Text(selected.map { String(localized: $0.title) }.formatted(.list(type: .and))))
    }
}

struct ModePicker: View {
    let apply: (Set<PlanningMode>) -> Void
    @State private var modes: Set<PlanningMode>
    @Environment(\.dismiss) private var dismiss

    init(modes: Set<PlanningMode>, apply: @escaping (Set<PlanningMode>) -> Void) {
        self.apply = apply
        _modes = State(initialValue: modes)
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    ForEach(PlanningMode.allCases) { mode in
                        Toggle(isOn: binding(for: mode)) {
                            Label {
                                Text(mode.title)
                            } icon: {
                                Image(systemName: mode.symbol)
                                    .foregroundStyle(mode.color)
                            }
                        }
                    }
                } footer: {
                    Text("Walking is always included.")
                }
            }
            .sensoryFeedback(.selection, trigger: modes)
            .navigationTitle("Transport Modes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Done", systemImage: "chevron.left") {
                        apply(modes)
                        dismiss()
                    }
                    .tint(.primary)
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button(modes.count == PlanningMode.allCases.count ? "Clear All" : "Select All") {
                        modes = modes.count == PlanningMode.allCases.count ? [] : Set(PlanningMode.allCases)
                    }
                }
            }
        }
    }

    private func binding(for mode: PlanningMode) -> Binding<Bool> {
        Binding {
            modes.contains(mode)
        } set: { isOn in
            if isOn { modes.insert(mode) } else { modes.remove(mode) }
        }
    }
}
