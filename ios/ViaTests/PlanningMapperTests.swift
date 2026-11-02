import Foundation
import Testing
@testable import Via

struct PlanningMapperTests {
    let origin = Place(name: "  Taipei Main Station ", coordinate: Coordinate(latitude: 25.04776, longitude: 121.51706))
    let destination = Place(name: "Taipei 101", coordinate: Coordinate(latitude: 25.03396, longitude: 121.56447))

    @Test func encodesRequestInServerShape() throws {
        let date = try #require(RFC3339.date(from: "2026-11-01T09:30:00Z"))
        let request = PlanningRequest(
            origin: origin,
            destination: destination,
            modes: [.bikeShare, .metro, .bus],
            departure: .arriveBy(date)
        )
        let data = try JSONEncoder().encode(PlanningMapper.dto(for: request))
        let json = try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])

        #expect(json["selected_modes"] as? [String] == ["bus", "metro", "bike_share"])
        let originJSON = try #require(json["origin"] as? [String: Any])
        #expect(originJSON["display_text"] as? String == "Taipei Main Station")
        let coordinate = try #require(originJSON["coordinate"] as? [String: Double])
        #expect(coordinate == ["latitude": 25.04776, "longitude": 121.51706])
        let departure = try #require(json["departure_setting"] as? [String: String])
        #expect(departure == ["type": "arrive_by", "time": "2026-11-01T17:30:00+08:00"])
    }

    @Test func encodesDepartNowWithoutTime() throws {
        let request = PlanningRequest(origin: origin, destination: destination, modes: [], departure: .now)
        let data = try JSONEncoder().encode(PlanningMapper.dto(for: request))
        let json = try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
        #expect(json["departure_setting"] as? [String: String] == ["type": "depart_now"])
        #expect(json["selected_modes"] as? [String] == [])
    }

    @Test func normalizesDisplayText() {
        #expect(PlanningMapper.displayText("Cafe\u{301}") == "Caf\u{e9}")
        #expect(PlanningMapper.displayText("   ") == "Pinned location")
        #expect(PlanningMapper.displayText(String(repeating: "a", count: 250)).count == 200)
    }

    @Test func decodesJourneyOptions() throws {
        let outcome = try JSONDecoder().decode(PlanningOutcomeDTO.self, from: Fixtures.optionsResponse)
        let plan = try PlanningMapper.plan(from: outcome)

        let option = try #require(plan.options.first)
        #expect(option.label == .bestNow)
        #expect(option.duration == 1_560)
        #expect(option.transferCount == 1)
        #expect(option.liveStatus == .fresh)
        #expect(option.overview.count == 2)
        #expect(option.legs.count == 3)

        let walk = option.legs[0]
        #expect(walk.kind == .walking)
        #expect(walk.guidance.map(\.code) == ["depart", "turn_left", "arrive"])
        #expect(walk.guidance[1].geometryIndex == 1)
        #expect(walk.guidance[1].streetName == "Zhongxiao W. Rd.")
        #expect(walk.guidance[0].streetName == nil)
        #expect(walk.attributions == [Attribution(text: "OpenStreetMap", url: URL(string: "https://www.openstreetmap.org/copyright"), licence: "ODbL")])

        let ride = try #require(option.legs[1].transit)
        #expect(ride == TransitRide(
            mode: .metro,
            serviceLabel: "BL",
            headsign: "Nangang Exhibition Center",
            patternID: "pattern-1",
            intermediateStops: [Stop(name: "NTU Hospital", coordinate: Coordinate(latitude: 25.0441, longitude: 121.5161), viaID: "stop-3")]
        ))
        #expect(option.legs[1].from.viaID == "stop-1")

        guard case let .bikeShare(bike) = option.legs[2].kind else {
            Issue.record("Expected a bike share leg")
            return
        }
        #expect(bike.dropoffStationName == "City Hall")
    }

    @Test func decodesEmptyResult() throws {
        let json = #"{"generated_at":"2026-11-01T09:30:00+08:00","empty":{"code":"no_feasible_journey_option"}}"#
        let outcome = try JSONDecoder().decode(PlanningOutcomeDTO.self, from: Data(json.utf8))
        let plan = try PlanningMapper.plan(from: outcome)
        #expect(plan.options.isEmpty)
    }

    @Test func rejectsUnknownLegDetails() throws {
        let json = String(decoding: Fixtures.optionsResponse, as: UTF8.self)
            .replacingOccurrences(of: #""type": "transit""#, with: #""type": "ferry""#)
        let outcome = try JSONDecoder().decode(PlanningOutcomeDTO.self, from: Data(json.utf8))
        #expect(throws: JourneyPlanningError.unsupportedResponse) {
            try PlanningMapper.plan(from: outcome)
        }
    }

    @Test func decodesPolyline6() throws {
        let coordinates = try #require(Polyline6.decode("_lxwn@g_ywfFn}Ycr{A"))
        #expect(coordinates.count == 2)
        #expect(abs(coordinates[0].latitude - 25.04776) < 1e-9)
        #expect(abs(coordinates[1].longitude - 121.56447) < 1e-9)
        #expect(Polyline6.decode("") == [])
        #expect(Polyline6.decode("_lxwn@") == nil)
    }
}

nonisolated enum Fixtures {
    static let optionsResponse = Data("""
    {
      "generated_at": "2026-11-01T09:30:00.123+08:00",
      "journey_options": [{
        "id": "opt_1",
        "matching_key": "key-1",
        "label": "best_now",
        "departure_time": "2026-11-01T09:31:00+08:00",
        "arrival_time": "2026-11-01T09:57:00+08:00",
        "duration_seconds": 1560,
        "walking_seconds": 420,
        "transfer_count": 1,
        "travel_modes": ["walking", "transit", "bicycle"],
        "live_status_summary": {"state": "fresh", "observed_at": "2026-11-01T09:29:00+08:00"},
        "overview_geometry": {"encoding": "polyline6", "points": "_lxwn@g_ywfFn}Ycr{A"},
        "legs": [
          {
            "id": "leg_1", "mode": "walking",
            "from": {"display_name": "Taipei Main Station", "coordinate": {"latitude": 25.04776, "longitude": 121.51706}},
            "to": {"display_name": "Taipei Main Station", "coordinate": {"latitude": 25.0478, "longitude": 121.5171}, "via_id": "stop-1"},
            "departure_time": "2026-11-01T09:31:00+08:00", "arrival_time": "2026-11-01T09:35:00+08:00",
            "duration_seconds": 240,
            "geometry": {"encoding": "polyline6", "points": "_lxwn@g_ywfFn}Ycr{A"},
            "guidance": [{"code": "depart", "parameters": {"geometry_index": "0"}}, {"code": "turn_left", "parameters": {"geometry_index": "1", "street_name": "Zhongxiao W. Rd."}}, {"code": "arrive", "parameters": {"geometry_index": "1"}}],
            "details": {"type": "walking"},
            "attribution": [{"text": "OpenStreetMap", "url": "https://www.openstreetmap.org/copyright", "licence": "ODbL"}]
          },
          {
            "id": "leg_2", "mode": "transit",
            "from": {"display_name": "Taipei Main Station", "coordinate": {"latitude": 25.0478, "longitude": 121.5171}, "via_id": "stop-1"},
            "to": {"display_name": "Taipei City Hall", "coordinate": {"latitude": 25.041, "longitude": 121.5654}, "via_id": "stop-2"},
            "departure_time": "2026-11-01T09:36:00+08:00", "arrival_time": "2026-11-01T09:50:00+08:00",
            "duration_seconds": 840,
            "geometry": {"encoding": "polyline6", "points": "_lxwn@g_ywfFn}Ycr{A"},
            "guidance": [{"code": "board_metro", "parameters": {"service_label": "BL", "boarding_point_name": "Taipei Main Station"}}],
            "details": {
              "type": "transit", "transit_mode": "metro", "via_pattern_id": "pattern-1",
              "via_boarding_point_id": "stop-1", "via_alighting_point_id": "stop-2",
              "service_label": "BL", "headsign": "Nangang Exhibition Center",
              "intermediate_stops": [{"display_name": "NTU Hospital", "coordinate": {"latitude": 25.0441, "longitude": 121.5161}, "via_id": "stop-3"}]
            },
            "attribution": []
          },
          {
            "id": "leg_3", "mode": "bicycle",
            "from": {"display_name": "City Hall Exit 2", "coordinate": {"latitude": 25.041, "longitude": 121.5654}},
            "to": {"display_name": "City Hall", "coordinate": {"latitude": 25.03396, "longitude": 121.56447}},
            "departure_time": "2026-11-01T09:51:00+08:00", "arrival_time": "2026-11-01T09:57:00+08:00",
            "duration_seconds": 360,
            "geometry": {"encoding": "polyline6", "points": "_lxwn@g_ywfFn}Ycr{A"},
            "guidance": [{"code": "bike_share", "parameters": {"pickup_station_name": "City Hall Exit 2", "dropoff_station_name": "City Hall"}}],
            "details": {
              "type": "bike_share", "pickup_station_id": "b1", "pickup_station_name": "City Hall Exit 2",
              "dropoff_station_id": "b2", "dropoff_station_name": "City Hall"
            },
            "attribution": [{"text": "OpenStreetMap", "url": "https://www.openstreetmap.org/copyright", "licence": "ODbL"}]
          }
        ]
      }]
    }
    """.utf8)
}
