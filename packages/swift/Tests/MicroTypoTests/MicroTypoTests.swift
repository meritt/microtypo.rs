import MicroTypo
import Testing

@Suite struct MicroTypoTests {
    @Test func typedDataRulesTraceEditsAndRegistrationShareTheEngine() throws {
        let rule = MicroTypo.Config.Rule(
            id: "letters", pattern: .init(source: "a", flags: "g"), replacement: "b")
        let typo = try MicroTypo(
            config: .init(presets: false, ruleGroups: [.init(name: "terms", rules: [rule])]))
        let input = "😀 aa aa"
        let result = try typo.process(input, options: .init(changes: true, edits: true, trace: true))
        #expect(result.text == Array("😀 bb bb".utf16))
        #expect(result.trace?.count == 1)
        #expect(result.trace?.first?.rule == Array("terms.letters".utf16))
        #expect(result.trace?.first?.kind == .regex)
        #expect(result.trace?.first?.count == 4)
        #expect(result.trace?.first?.ranges == [3..<4, 4..<5, 6..<7, 7..<8])
        var reconstructed = Array(input.utf16)
        for edit in (result.edits ?? []).reversed() {
            reconstructed.replaceSubrange(edit.start..<edit.end, with: edit.text)
        }
        #expect(reconstructed == result.text)
        try typo.applyOptions(.init(rules: ["terms": false]))
        #expect(try typo.process(input) == input)
        try typo.registerRuleGroup(
            .init(
                name: "more",
                rules: [.init(id: "next", pattern: .init(source: "b", flags: "g"), replacement: "c")]),
            position: "after:terms")
        #expect(try typo.process("bb") == "cc")
        #expect(throws: MicroTypoError.self) {
            try typo.registerRuleGroup(
                .init(
                    name: "more",
                    rules: [.init(id: "bad", pattern: .init(source: "(", flags: "g"), replacement: "x")]))
        }
        #expect(try typo.process("bb") == "cc")
    }

    @Test func diagnosticsPreserveRawUTF16DataRuleUnits() throws {
        let typo = try MicroTypo(
            configJSON:
                #"{"presets":false,"input":"plain","ruleGroups":[{"name":"custom\ud800","rules":[{"id":"r\ud800","pattern":{"source":"\ud800","flags":"gu"},"replacement":"\udc00"}]}]}"#
        )
        let result = try typo.process([0xd800], options: .init(changes: true, edits: true, trace: true))
        #expect(result.text == [0xdc00])
        #expect(result.edits?.first?.text == [0xdc00])
        #expect(result.trace?.first?.rule == Array("custom".utf16) + [0xd800, 46, 114, 0xd800])
    }

    @Test func keepsMathAndMdxAndTypesetsSelectedAttributes() throws {
        let math = try MicroTypo(config: .init(input: .init(format: "markdown", math: true)))
        #expect(try math.process("$a - b$ Корвин - принц") == "$a - b$ Корвин\u{A0}— принц")
        let mdx = try MicroTypo(config: .init(input: .init(format: "mdx")))
        #expect(
            try mdx.process("{function() {} / 2} Корвин - принц")
                == "{function() {} / 2} Корвин\u{A0}— принц")
        let attributes = try MicroTypo(config: .init(input: .init(format: "html", attributes: ["alt"])))
        #expect(
            try attributes.process("<img alt=\"Корвин - принц\">") == "<img alt=\"Корвин\u{A0}— принц\">")
    }
    @Test func passesLocalizationAndPragmaOptionsThroughTypedConfig() throws {
        let typo = try MicroTypo(
            config: .init(html: false, input: .init(placeholders: "printf", pragmas: "microtypo")))
        #expect(try typo.process("%1$s - текст") == "%1$s\u{A0}— текст")
        let ignored = "<!-- microtypo-ignore-file -->\nКорвин - принц"
        #expect(try typo.process(ignored) == ignored)
        #expect(try typo.process("%1$s - текст") == "%1$s\u{A0}— текст")
    }

    @Test func plainTypedInputEmitsGlyphsUnderHtmlAndEntitiesOptions() throws {
        let typo = try MicroTypo(
            config: .init(html: true, entities: true, input: .init(format: "plain")))
        #expect(try typo.process("<code>Корвин - принц</code>") == "<code>Корвин\u{A0}— принц</code>")
    }

    @Test func typesetsPlainText() throws {
        let typo = try MicroTypo(config: .init(html: false))
        #expect(try typo.process("\"Амбер\" - город") == "«Амбер»\u{A0}— город")
    }

    @Test func reportsChangesInUTF16Units() throws {
        let typo = try MicroTypo(config: .init(html: false))
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

    @Test func rejectsInvalidConfigJSON() {
        let error = #expect(throws: MicroTypoError.self) {
            try MicroTypo(configJSON: #"{"html":"yes"}"#)
        }
        #expect(error?.message == "invalid config: html: expected a boolean")
    }

    @Test func rejectsAnUnknownInputFormat() {
        let error = #expect(throws: MicroTypoError.self) {
            try MicroTypo(config: .init(input: .init(format: "csv")))
        }
        #expect(error?.name == "MicroTypoConfigError")
    }

    @Test(arguments: [Double.nan, Double.infinity, -Double.infinity])
    func refusesNonFiniteNumbers(value: Double) {
        #expect(throws: EncodingError.self) { try MicroTypo(config: .init(maxProcessingMs: value)) }
    }

    @Test func appliesRulesInTheirOrder() throws {
        let on = try MicroTypo(config: .init(html: false, rules: ["dash": false, "dash.em": true]))
        #expect(try on.process("a - b") == "a\u{A0}— b")
        let off = try MicroTypo(config: .init(html: false, rules: ["dash.em": true, "dash": false]))
        #expect(try off.process("a - b") == "a - b")
    }

    @Test func appliesOptionsToALiveEngine() throws {
        let typo = try MicroTypo(config: .init(html: false))
        try typo.applyOptions(.init(rules: ["quote": false]))
        #expect(try typo.process("\"Амбер\" - город") == "\"Амбер\"\u{A0}— город")
    }

    @Test func keepsASafeBlockAsWritten() throws {
        let typo = try MicroTypo(config: .init(html: false))
        try typo.addSafeBlock(id: "tpl", open: "{{", close: "}}")
        #expect(try typo.process("{{ \"a\" - b }} \"c\"") == "{{ \"a\" - b }} «c»")
    }

    @Test func endsACatastrophicInputWithTheBudgetError() throws {
        let typo = try MicroTypo(
            config: .init(maxInputLength: 60_000, maxProcessingMs: 200, input: .init(format: "markdown")))
        let error = #expect(throws: MicroTypoError.self) {
            try typo.process(String(repeating: " ", count: 60_000))
        }
        #expect(error?.name == "MicroTypoBudgetError")
    }

    @Test func sharesOneEngineAcrossTasks() async throws {
        let typo = try MicroTypo(config: .init(html: false))
        let results = try await withThrowingTaskGroup(of: String.self) { group in
            for _ in 0..<16 {
                group.addTask { try typo.process("\"Амбер\" - город") }
            }
            return try await group.reduce(into: []) { $0.append($1) }
        }
        #expect(results == Array(repeating: "«Амбер»\u{A0}— город", count: 16))
    }
}
