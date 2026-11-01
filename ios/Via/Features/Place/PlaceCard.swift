import MapKit
import SwiftUI

struct PlaceCard: View {
    let model: ExploreModel
    let result: PlaceResult

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                header
                actions
                details
            }
            .padding(.horizontal, 20)
            .padding(.top, 20)
            .padding(.bottom, 24)
        }
        .scrollBounceBehavior(.basedOnSize)
    }

    private var header: some View {
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 4) {
                Text(result.place.name)
                    .font(.title2.bold())
                    .lineLimit(2)
                if let summary {
                    Text(summary)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
            }
            Spacer(minLength: 12)
            CloseButton(action: model.closePlace)
        }
    }

    private var summary: String? {
        let distance = model.location.coordinate.map { $0.distance(to: result.place.coordinate).distanceText }
        let parts = [distance, result.place.subtitle].compactMap(\.self)
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    private var actions: some View {
        HStack(spacing: 10) {
            Button {
                model.startDirections(to: result)
            } label: {
                Label("Directions", systemImage: "arrow.triangle.turn.up.right.diamond.fill")
                    .font(.headline)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 6)
            }
            .buttonStyle(.glassProminent)

            if let phone = result.mapItem.phoneNumber,
               let url = URL(string: "tel:\(phone.filter { $0.isNumber || $0 == "+" })") {
                Link(destination: url) {
                    Label("Call", systemImage: "phone.fill")
                        .labelStyle(.iconOnly)
                        .font(.headline)
                        .padding(.vertical, 6)
                }
                .buttonStyle(.glass)
            }
            if let website = result.mapItem.url {
                Link(destination: website) {
                    Label("Website", systemImage: "safari.fill")
                        .labelStyle(.iconOnly)
                        .font(.headline)
                        .padding(.vertical, 6)
                }
                .buttonStyle(.glass)
            }
        }
        .controlSize(.large)
    }

    @ViewBuilder
    private var details: some View {
        if let address = result.mapItem.address?.fullAddress {
            VStack(alignment: .leading, spacing: 4) {
                Text("Address")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                Text(address)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(14)
            .background(.fill.quaternary, in: .rect(cornerRadius: 14))
        }
    }
}
