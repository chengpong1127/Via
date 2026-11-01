import SwiftUI

struct DepartureEditor: View {
    private enum Kind: Hashable {
        case departAt
        case arriveBy
    }

    let apply: (DepartureSetting) -> Void
    @State private var kind: Kind
    @State private var date: Date
    @Environment(\.dismiss) private var dismiss

    init(setting: DepartureSetting, apply: @escaping (DepartureSetting) -> Void) {
        self.apply = apply
        switch setting {
        case .now:
            _kind = State(initialValue: .departAt)
            _date = State(initialValue: .now)
        case let .departAt(date):
            _kind = State(initialValue: .departAt)
            _date = State(initialValue: date)
        case let .arriveBy(date):
            _kind = State(initialValue: .arriveBy)
            _date = State(initialValue: date)
        }
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 16) {
                Picker("Timing", selection: $kind) {
                    Text("Depart At").tag(Kind.departAt)
                    Text("Arrive By").tag(Kind.arriveBy)
                }
                .pickerStyle(.segmented)
                DatePicker("Time", selection: $date, in: Date.now.addingTimeInterval(-3_600)...)
                    .datePickerStyle(.wheel)
                    .labelsHidden()
                    .environment(\.timeZone, .via)
                Spacer()
            }
            .padding()
            .navigationTitle("Choose Time")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", systemImage: "xmark", role: .cancel) { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done", systemImage: "checkmark", role: .confirm) {
                        apply(kind == .departAt ? .departAt(date) : .arriveBy(date))
                        dismiss()
                    }
                }
            }
        }
    }
}
