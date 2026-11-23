import Foundation
import Testing
@testable import Via

struct ViaServerTests {
    @Test func acceptsHTTPSURLWithHost() {
        #expect(ViaServer.validatedURL("https://example.com", allowsInsecure: false)?.absoluteString == "https://example.com")
    }

    @Test(arguments: [nil, "", "   ", "example.com", "https://", "http://example.com", "ftp://example.com"])
    func rejectsMissingOrInsecureURLWhenInsecureNotAllowed(configured: String?) {
        #expect(ViaServer.validatedURL(configured, allowsInsecure: false) == nil)
    }

    @Test func acceptsHTTPWhenInsecureAllowed() {
        #expect(ViaServer.validatedURL("http://127.0.0.1:3000", allowsInsecure: true)?.port == 3000)
    }

    @Test func rejectsMissingURLEvenWhenInsecureAllowed() {
        #expect(ViaServer.validatedURL("", allowsInsecure: true) == nil)
    }

    @Test func unconfiguredFallbackIsHTTPSWithHost() {
        #expect(ViaServer.unconfiguredURL.scheme == "https")
        #expect(ViaServer.unconfiguredURL.host() != nil)
    }
}
