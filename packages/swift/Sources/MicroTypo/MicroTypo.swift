import Foundation
import MicroTypoFFI
import Synchronization

public struct MicroTypoError: Error, CustomStringConvertible, Sendable {
    public let name: String
    public let message: String
    public let code: String?
    public let detailsJSON: String?

    public var description: String { "\(name): \(message)" }

    @unsafe init(taking error: OpaquePointer) {
        defer { unsafe microtypo_error_free(error) }
        name = unsafe String(cString: microtypo_error_name(error))
        message = unsafe String(cString: microtypo_error_message(error))
        code = unsafe microtypo_error_code(error).map { unsafe String(cString: $0) }
        detailsJSON = unsafe microtypo_error_details(error).map { unsafe String(cString: $0) }
    }
}

public struct Processed: Sendable, Equatable {
    public let text: String
    public let changes: [Range<Int>]
}

public struct ProcessOptions: Encodable, Sendable, Equatable {
    public var changes: Bool
    public var edits: Bool
    public var trace: Bool

    public init(changes: Bool = false, edits: Bool = false, trace: Bool = false) {
        self.changes = changes
        self.edits = edits
        self.trace = trace
    }
}

public struct Edit: Sendable, Equatable {
    public let start: Int
    public let end: Int
    public let text: [UInt16]
}

public struct TraceEntry: Sendable, Equatable {
    public enum Kind: String, Sendable {
        case regex
        case handler
    }

    public let rule: [UInt16]
    public let kind: Kind
    public let count: Int
    public let ranges: [Range<Int>]
}

public struct Diagnostics: Sendable, Equatable {
    public let text: [UInt16]
    public let trace: [TraceEntry]?
    public let changes: [Range<Int>]?
    public let edits: [Edit]?
}

@safe public final class MicroTypo: Sendable {
    public struct Config: Sendable {
        public struct Pattern: Encodable, Sendable, Equatable {
            public var source: String
            public var flags: String

            public init(source: String, flags: String) {
                self.source = source
                self.flags = flags
            }
        }

        public struct Rule: Encodable, Sendable, Equatable {
            public var id: String
            public var pattern: Pattern
            public var replacement: String
            public var enabled: Bool?
            public var cycled: Bool?
            public var htmlOnly: Bool?

            public init(
                id: String, pattern: Pattern, replacement: String, enabled: Bool? = nil,
                cycled: Bool? = nil, htmlOnly: Bool? = nil
            ) {
                self.id = id
                self.pattern = pattern
                self.replacement = replacement
                self.enabled = enabled
                self.cycled = cycled
                self.htmlOnly = htmlOnly
            }
        }

        public struct RuleGroup: Encodable, Sendable, Equatable {
            public var name: String
            public var rules: [Rule]
            public var title: String?
            public var position: String?

            public init(name: String, rules: [Rule], title: String? = nil, position: String? = nil) {
                self.name = name
                self.rules = rules
                self.title = title
                self.position = position
            }
        }
        public struct Input: Encodable, Sendable, Equatable {
            public var format: String?
            public var template: String?
            public var fields: [String]?
            public var exclude: [String]?
            public var placeholders: String?
            public var pragmas: String?
            public var math: Bool?
            public var attributes: [String]?

            public init(
                format: String? = nil, template: String? = nil,
                fields: [String]? = nil, exclude: [String]? = nil,
                placeholders: String? = nil, pragmas: String? = nil,
                math: Bool? = nil, attributes: [String]? = nil
            ) {
                self.format = format
                self.template = template
                self.fields = fields
                self.exclude = exclude
                self.placeholders = placeholders
                self.pragmas = pragmas
                self.math = math
                self.attributes = attributes
            }
        }

        public struct Render: Encodable, Sendable, Equatable {
            public var paragraphs: Bool?
            public var autolink: Bool?
            public var breakline: Bool?
            public var nowrap: String?
            public var hanging: String?
            public var prefix: String?

            public init(
                paragraphs: Bool? = nil, autolink: Bool? = nil, breakline: Bool? = nil,
                nowrap: String? = nil, hanging: String? = nil, prefix: String? = nil
            ) {
                self.paragraphs = paragraphs
                self.autolink = autolink
                self.breakline = breakline
                self.nowrap = nowrap
                self.hanging = hanging
                self.prefix = prefix
            }
        }

        public var html: Bool?
        public var entities: Bool?
        public var maxInputLength: UInt32?
        public var maxProcessingMs: Double?
        public var input: Input?
        public var presets: Bool?
        public var rules: KeyValuePairs<String, Bool>
        public var render: Render?
        public var ruleGroups: [RuleGroup]?

        public init(
            html: Bool? = nil, entities: Bool? = nil, maxInputLength: UInt32? = nil,
            maxProcessingMs: Double? = nil,
            input: Input? = nil, presets: Bool? = nil, rules: KeyValuePairs<String, Bool> = [:],
            render: Render? = nil,
            ruleGroups: [RuleGroup]? = nil
        ) {
            self.html = html
            self.entities = entities
            self.maxInputLength = maxInputLength
            self.maxProcessingMs = maxProcessingMs
            self.input = input
            self.presets = presets
            self.rules = rules
            self.render = render
            self.ruleGroups = ruleGroups
        }

        private struct Fields: Encodable {
            let html: Bool?
            let entities: Bool?
            let maxInputLength: UInt32?
            let maxProcessingMs: Double?
            let input: Input?
            let presets: Bool?
            let render: Render?
            let ruleGroups: [RuleGroup]?
        }

        func json() throws -> String {
            let encoder = JSONEncoder()
            let fields = Fields(
                html: html, entities: entities, maxInputLength: maxInputLength,
                maxProcessingMs: maxProcessingMs,
                input: input, presets: presets, render: render, ruleGroups: ruleGroups
            )
            let object = String(decoding: try encoder.encode(fields), as: UTF8.self)
            if rules.isEmpty { return object }
            let pairs = try rules.map { key, on in
                "\(String(decoding: try encoder.encode(key), as: UTF8.self)):\(on)"
            }
            let rules = "\"rules\":{\(pairs.joined(separator: ","))}"
            return object == "{}" ? "{\(rules)}" : "\(object.dropLast()),\(rules)}"
        }
    }

    private struct SafeBlock: Encodable {
        let id: String
        let open: String
        let close: String
        let unsafeRegex: Bool
    }

    private let engine: Mutex<OpaquePointer>

    public static let version = unsafe String(cString: microtypo_version())

    public convenience init(config: Config = Config()) throws {
        try self.init(configJSON: try config.json())
    }

    public init(configJSON: String) throws {
        var error: OpaquePointer?
        guard let engine = unsafe microtypo_engine_new(configJSON, &error) else {
            throw unsafe MicroTypoError(taking: error!)
        }
        unsafe self.engine = Mutex(engine)
    }

    deinit {
        unsafe engine.withLock { unsafe microtypo_engine_free($0) }
    }

    public func process(_ text: String) throws -> String {
        try run(text, withChanges: false).text
    }

    public func processWithChanges(_ text: String) throws -> Processed {
        try run(text, withChanges: true)
    }

    public func process(_ text: String, options: ProcessOptions) throws -> Diagnostics {
        try process(Array(text.utf16), options: options)
    }

    public func process(_ units: [UInt16], options: ProcessOptions = ProcessOptions()) throws -> Diagnostics {
        let optionsJSON = String(decoding: try JSONEncoder().encode(options), as: UTF8.self)
        let outcome: Result<Diagnostics, MicroTypoError> = unsafe engine.withLock { engine in
            var out = unsafe microtypo_output()
            var error: OpaquePointer?
            let ok = unsafe units.withUnsafeBufferPointer {
                unsafe microtypo_engine_process_diagnostics(
                    engine, $0.baseAddress, $0.count, optionsJSON, &out, &error)
            }
            guard ok else { return .failure(unsafe MicroTypoError(taking: error!)) }
            defer { unsafe microtypo_output_free(&out) }
            let text = unsafe Array(UnsafeBufferPointer(start: out.units, count: out.len))
            let flat = unsafe UnsafeBufferPointer(start: out.changes, count: out.changes_len * 2)
            let changes =
                options.changes
                ? stride(from: 0, to: flat.count, by: 2).map { unsafe flat[$0]..<flat[$0 + 1] } : nil
            let rawEdits = unsafe UnsafeBufferPointer(start: out.edits, count: out.edits_len)
            let edits =
                options.edits
                ? (0..<rawEdits.count).map { index in
                    let edit = unsafe rawEdits[index]
                    return Edit(
                        start: edit.start, end: edit.end,
                        text: unsafe Array(UnsafeBufferPointer(start: edit.units, count: edit.len)))
                } : nil
            let rawTrace = unsafe UnsafeBufferPointer(start: out.trace, count: out.trace_len)
            let trace =
                options.trace
                ? (0..<rawTrace.count).map { index in
                    let entry = unsafe rawTrace[index]
                    let rule = unsafe Array(UnsafeBufferPointer(start: entry.rule, count: entry.rule_len))
                    let ranges = unsafe UnsafeBufferPointer(start: entry.ranges, count: entry.ranges_len * 2)
                    return TraceEntry(
                        rule: rule, kind: entry.kind == 0 ? .regex : .handler, count: entry.count,
                        ranges: stride(from: 0, to: ranges.count, by: 2).map {
                            unsafe ranges[$0]..<ranges[$0 + 1]
                        })
                } : nil
            return .success(Diagnostics(text: text, trace: trace, changes: changes, edits: edits))
        }
        return try outcome.get()
    }

    public func registerRuleGroup(
        _ group: Config.RuleGroup, name: String? = nil, position: String? = nil
    ) throws {
        let encoder = JSONEncoder()
        let definition = String(decoding: try encoder.encode(group), as: UTF8.self)
        var options: [String: String] = [:]
        options["name"] = name
        options["position"] = position
        let placement = String(decoding: try encoder.encode(options), as: UTF8.self)
        try call("registerRuleGroup", argsJSON: "[\(definition),\(placement)]")
    }

    public func applyOptions(_ options: Config) throws {
        try call("applyOptions", argsJSON: "[\(try options.json())]")
    }

    public func setLayout(_ layout: String) throws {
        try call("setLayout", [layout])
    }

    public func setPrefix(_ prefix: String) throws {
        try call("setPrefix", [prefix])
    }

    public func addSafeTag(_ tag: String) throws {
        try call("addSafeTag", [tag])
    }

    public func addSafeBlock(id: String, open: String, close: String, unsafeRegex: Bool = false)
        throws
    {
        try call(
            "addSafeBlock", [SafeBlock(id: id, open: open, close: close, unsafeRegex: unsafeRegex)])
    }

    private func call(_ method: String, _ args: [some Encodable]) throws {
        try call(method, argsJSON: String(decoding: try JSONEncoder().encode(args), as: UTF8.self))
    }

    private func call(_ method: String, argsJSON: String) throws(MicroTypoError) {
        let failure: MicroTypoError? = unsafe engine.withLock { engine in
            var error: OpaquePointer?
            if unsafe microtypo_engine_call(engine, method, argsJSON, &error) { return nil }
            return unsafe MicroTypoError(taking: error!)
        }
        if let failure { throw failure }
    }

    private func run(_ text: String, withChanges: Bool) throws -> Processed {
        let output = try process(Array(text.utf16), options: ProcessOptions(changes: withChanges))
        return Processed(text: String(decoding: output.text, as: UTF16.self), changes: output.changes ?? [])
    }
}
