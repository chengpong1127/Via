import Foundation
import Testing
@testable import Via

struct AboutInfoTests {
    @Test func readsVersionAndBuild() {
        let info = AboutInfo(infoDictionary: [
            "CFBundleShortVersionString": "0.2.0",
            "CFBundleVersion": "7",
        ])
        #expect(info.versionText == "0.2.0 (7)")
    }

    @Test func versionOmitsMissingBuild() {
        let info = AboutInfo(infoDictionary: ["CFBundleShortVersionString": "0.2.0"])
        #expect(info.versionText == "0.2.0")
    }

    @Test func versionIsEmptyWithoutInfo() {
        #expect(AboutInfo(infoDictionary: [:]).versionText == "")
    }

    @Test func readsLinksFromInfoDictionary() {
        let info = AboutInfo(infoDictionary: [
            "VIA_PRIVACY_POLICY_URL": "https://example.com/privacy",
            "VIA_SUPPORT_URL": "https://example.com/support",
        ])
        #expect(info.privacyPolicyURL == URL(string: "https://example.com/privacy"))
        #expect(info.supportURL == URL(string: "https://example.com/support"))
    }

    @Test func hidesEmptyMissingOrNonWebLinks() {
        let info = AboutInfo(infoDictionary: [
            "VIA_PRIVACY_POLICY_URL": "",
            "VIA_SUPPORT_URL": "not a url",
        ])
        #expect(info.privacyPolicyURL == nil)
        #expect(info.supportURL == nil)
        #expect(AboutInfo(infoDictionary: [:]).privacyPolicyURL == nil)
    }

    @Test func trimsSurroundingNewlinesFromLinks() {
        let info = AboutInfo(infoDictionary: ["VIA_SUPPORT_URL": "https://example.com/support\n"])
        #expect(info.supportURL == URL(string: "https://example.com/support"))
    }

    @Test func attributionDisplayTextJoinsTextAndLicence() {
        let attribution = Attribution(text: "© OpenStreetMap contributors", url: nil, licence: "ODbL")
        #expect(attribution.displayText == "© OpenStreetMap contributors · ODbL")
    }

    @Test func attributionDisplayTextOmitsEmptyLicence() {
        let attribution = Attribution(text: "TDX", url: nil, licence: "")
        #expect(attribution.displayText == "TDX")
    }
}
