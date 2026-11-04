import Foundation
import MicroTypo
import Testing

@Suite struct MicroTypoTests {
    @Test func typesetsPlainText() throws {
        let typo = try MicroTypo(config: ["html": false])
        #expect(try typo.process("\"Амбер\" - город") == "«Амбер»\u{A0}— город")
    }

    @Test func reportsChangesInUTF16Units() throws {
        let typo = try MicroTypo(config: ["html": false])
        let result = try typo.processWithChanges("😀 a - b")
        #expect(result.text == "😀 a\u{A0}— b")
        #expect(result.changes == [4..<6])
    }

    @Test func surfacesConfigErrors() throws {
        let typo = try MicroTypo()
        let error = #expect(throws: MicroTypoError.self) { try typo.setLayout("none") }
        #expect(error?.name == "MicroTypoConfigError")
        #expect(error?.code == "ERR_MICROTYPO_CONFIG")
        #expect(error?.detailsJSON == #"{"layout":"none"}"#)
    }

    @Test func rejectsInvalidConfig() {
        #expect(throws: MicroTypoError.self) { try MicroTypo(configJSON: #"{"html":"yes"}"#) }
    }

    @Test(arguments: [Double.nan, Double.infinity])
    func refusesNonJSONNumbers(value: Double) {
        let error = #expect(throws: MicroTypoError.self) { try MicroTypo(config: ["maxProcessingMs": value]) }
        #expect(error?.name == "TypeError")
    }

    @Test func refusesNonJSONValues() throws {
        let typo = try MicroTypo()
        #expect(throws: MicroTypoError.self) { try typo.applyOptions(["render": Date()]) }
    }
}
