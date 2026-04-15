import XCTest
@testable import CsmMac

/// Exercises the UserDefaults round-trip for [`AppPreferences`]. The
/// launch-at-login toggle is NOT covered — `SMAppService` requires a
/// properly bundled/registered app and fails in test processes; that path
/// is validated manually against a signed build.
final class AppPreferencesTests: XCTestCase {

    private func makeDefaults(_ suite: String = UUID().uuidString) -> UserDefaults {
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        return defaults
    }

    func testServerUrlRoundTripsThroughUserDefaults() {
        let defaults = makeDefaults()
        let prefs1 = AppPreferences(defaults: defaults)
        XCTAssertEqual(prefs1.serverUrl, "")
        prefs1.serverUrl = "http://example:7685"

        // A fresh AppPreferences instance reads the persisted value — proves
        // the didSet actually wrote to UserDefaults.
        let prefs2 = AppPreferences(defaults: defaults)
        XCTAssertEqual(prefs2.serverUrl, "http://example:7685")
    }

    func testLogLevelDefaultsToInfoAndPersists() {
        let defaults = makeDefaults()
        let prefs1 = AppPreferences(defaults: defaults)
        XCTAssertEqual(prefs1.logLevel, "info")
        prefs1.logLevel = "debug"

        let prefs2 = AppPreferences(defaults: defaults)
        XCTAssertEqual(prefs2.logLevel, "debug")
    }

    func testInvalidStoredLogLevelFallsBackToInfo() {
        let defaults = makeDefaults()
        defaults.set("nonsense", forKey: AppPreferences.Key.logLevel)
        let prefs = AppPreferences(defaults: defaults)
        XCTAssertEqual(prefs.logLevel, "info")
    }

    func testConfiguredServerUrlNilForBlankOrWhitespace() {
        let defaults = makeDefaults()
        let prefs = AppPreferences(defaults: defaults)
        XCTAssertNil(prefs.configuredServerUrl)
        prefs.serverUrl = "   "
        XCTAssertNil(prefs.configuredServerUrl)
        prefs.serverUrl = "http://host:123"
        XCTAssertEqual(prefs.configuredServerUrl, "http://host:123")
    }

    func testLogLevelsContainsStandardDirectives() {
        for expected in ["trace", "debug", "info", "warn", "error"] {
            XCTAssertTrue(
                AppPreferences.logLevels.contains(expected),
                "logLevels missing \(expected)"
            )
        }
    }
}
