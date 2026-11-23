import Foundation

struct AboutInfo {
    let versionText: String
    let privacyPolicyURL: URL?
    let supportURL: URL?

    init(infoDictionary: [String: Any]?) {
        let info = infoDictionary ?? [:]
        let version = info["CFBundleShortVersionString"] as? String ?? ""
        let build = info["CFBundleVersion"] as? String ?? ""
        versionText = build.isEmpty ? version : "\(version) (\(build))"
        privacyPolicyURL = Self.webURL(info["VIA_PRIVACY_POLICY_URL"])
        supportURL = Self.webURL(info["VIA_SUPPORT_URL"])
    }

    static var current: AboutInfo { AboutInfo(infoDictionary: Bundle.main.infoDictionary) }

    private static func webURL(_ value: Any?) -> URL? {
        guard let string = (value as? String)?.trimmingCharacters(in: .whitespaces),
              let url = URL(string: string),
              ["http", "https"].contains(url.scheme?.lowercased())
        else { return nil }
        return url
    }
}

extension Attribution {
    var displayText: String {
        licence.isEmpty ? text : "\(text) · \(licence)"
    }
}
