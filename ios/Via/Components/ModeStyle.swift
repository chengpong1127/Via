import CoreLocation
import SwiftUI

extension TransitMode {
    var symbol: String {
        switch self {
        case .bus: "bus.fill"
        case .metro: "tram.fill.tunnel"
        case .rail: "train.side.front.car"
        case .highSpeedRail: "train.side.front.car"
        }
    }

    var color: Color {
        switch self {
        case .bus: Color(red: 0.13, green: 0.6, blue: 0.33)
        case .metro: Color(red: 0.0, green: 0.42, blue: 0.82)
        case .rail: Color(red: 0.24, green: 0.27, blue: 0.62)
        case .highSpeedRail: Color(red: 0.93, green: 0.42, blue: 0.08)
        }
    }

    var title: LocalizedStringResource {
        switch self {
        case .bus: "Bus"
        case .metro: "Metro"
        case .rail: "TRA"
        case .highSpeedRail: "THSR"
        }
    }
}

extension PlanningMode {
    var symbol: String {
        switch self {
        case .bus: TransitMode.bus.symbol
        case .metro: TransitMode.metro.symbol
        case .tra: TransitMode.rail.symbol
        case .thsr: TransitMode.highSpeedRail.symbol
        case .bikeShare: "bicycle"
        }
    }

    var title: LocalizedStringResource {
        switch self {
        case .bus: "Bus"
        case .metro: "Metro"
        case .tra: "TRA"
        case .thsr: "THSR"
        case .bikeShare: "YouBike"
        }
    }
}

extension LegKind {
    var symbol: String {
        switch self {
        case .walking: "figure.walk"
        case let .transit(ride): ride.mode.symbol
        case .bikeShare: "bicycle"
        }
    }

    var color: Color {
        switch self {
        case .walking: .secondary
        case let .transit(ride): ride.mode.color
        case .bikeShare: .bikeShare
        }
    }

    var routeColor: Color {
        switch self {
        case .walking: .blue
        default: color
        }
    }
}

extension JourneyLabel {
    var title: LocalizedStringResource {
        switch self {
        case .bestNow: "Best now"
        case .fewerTransfers: "Fewer transfers"
        case .lessWalking: "Less walking"
        }
    }
}

extension Color {
    static let bikeShare = Color(red: 0.95, green: 0.65, blue: 0.0)
}

extension TimeZone {
    static let via = TimeZone(identifier: "Asia/Taipei")!
}

extension FormatStyle where Self == Date.FormatStyle {
    static var clockTime: Date.FormatStyle {
        Date.FormatStyle(date: .omitted, time: .shortened, timeZone: .via)
    }
}

extension TimeInterval {
    var travelTime: String {
        let minutes = Int((self / 60).rounded(.up))
        return Duration.seconds(max(minutes, 1) * 60)
            .formatted(.units(allowed: [.hours, .minutes], width: .abbreviated))
    }
}

extension CLLocationDistance {
    var distanceText: String {
        Measurement(value: self, unit: UnitLength.meters)
            .formatted(.measurement(width: .abbreviated, usage: .road))
    }
}
