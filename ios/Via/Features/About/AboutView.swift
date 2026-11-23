import SwiftUI

struct AboutView: View {
    let info: AboutInfo
    @Environment(\.dismiss) private var dismiss

    init(info: AboutInfo = .current) {
        self.info = info
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    LabeledContent("Version", value: info.versionText)
                }
                Section("Data Sources") {
                    SourceRow(
                        name: "OpenStreetMap",
                        detail: "© OpenStreetMap contributors, ODbL",
                        url: URL(string: "https://www.openstreetmap.org/copyright")
                    )
                    SourceRow(
                        name: "MOTC TDX",
                        detail: "Data from MOTC TDX platform. Open Government Data License, version 1.0.",
                        url: URL(string: "https://tdx.transportdata.tw")
                    )
                    SourceRow(name: "GTFS", detail: "Transit schedules and routes from TDX.", url: nil)
                    SourceRow(name: "YouBike", detail: "Bike availability from TDX.", url: nil)
                    SourceRow(name: "Apple Maps", detail: "Map and place search by Apple Maps.", url: nil)
                }
                if info.privacyPolicyURL != nil || info.supportURL != nil {
                    Section {
                        if let url = info.privacyPolicyURL {
                            Link("Privacy Policy", destination: url)
                        }
                        if let url = info.supportURL {
                            Link("Support", destination: url)
                        }
                    }
                }
            }
            .navigationTitle("About Via")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }
}

private struct SourceRow: View {
    let name: LocalizedStringKey
    let detail: LocalizedStringKey
    let url: URL?

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            if let url {
                Link(destination: url) { Text(name).font(.body.weight(.medium)) }
            } else {
                Text(name).font(.body.weight(.medium))
            }
            Text(detail)
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
    }
}

struct AboutButton: View {
    @State private var isPresented = false

    var body: some View {
        Button("About", systemImage: "info.circle") { isPresented = true }
            .labelStyle(.iconOnly)
            .font(.title3)
            .padding(10)
            .glassEffect(.regular.interactive(), in: .circle)
            .buttonStyle(.plain)
            .sheet(isPresented: $isPresented) { AboutView() }
    }
}

struct AttributionFooter: View {
    let attributions: [Attribution]

    var body: some View {
        if !attributions.isEmpty {
            VStack(alignment: .leading, spacing: 2) {
                ForEach(attributions, id: \.self) { attribution in
                    if let url = attribution.url {
                        Link(attribution.displayText, destination: url)
                    } else {
                        Text(attribution.displayText)
                    }
                }
            }
            .font(.caption2)
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
