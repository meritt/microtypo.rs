import Foundation
import MicroTypoFFI

public struct MicroTypoError: Error, CustomStringConvertible, Sendable {
    public let name: String
    public let message: String
    public let code: String?
    public let detailsJSON: String?

    public var description: String { "\(name): \(message)" }

    init(taking error: OpaquePointer) {
        defer { microtypo_error_free(error) }
        name = String(cString: microtypo_error_name(error))
        message = String(cString: microtypo_error_message(error))
        code = microtypo_error_code(error).map { String(cString: $0) }
        detailsJSON = microtypo_error_details(error).map { String(cString: $0) }
    }

    init(name: String, message: String) {
        self.name = name
        self.message = message
        code = nil
        detailsJSON = nil
    }
}

public struct Processed: Sendable, Equatable {
    public let text: String
    public let changes: [Range<Int>]
}

public final class MicroTypo: @unchecked Sendable {
    private let engine: OpaquePointer
    private let lock = NSLock()

    public static let version = String(cString: microtypo_version())

    public init(configJSON: String? = nil) throws(MicroTypoError) {
        var error: OpaquePointer?
        let engine = configJSON.withOptionalCString { microtypo_engine_new($0, &error) }
        guard let engine else { throw MicroTypoError(taking: error!) }
        self.engine = engine
    }

    public convenience init(config: [String: Any]) throws(MicroTypoError) {
        try self.init(configJSON: try Self.json(config))
    }

    deinit {
        microtypo_engine_free(engine)
    }

    public func process(_ text: String) throws(MicroTypoError) -> String {
        try run(text, withChanges: false).text
    }

    public func processWithChanges(_ text: String) throws(MicroTypoError) -> Processed {
        try run(text, withChanges: true)
    }

    public func applyOptions(_ options: [String: Any]) throws(MicroTypoError) {
        try call("applyOptions", [options])
    }

    public func setLayout(_ layout: String) throws(MicroTypoError) {
        try call("setLayout", [layout])
    }

    public func setPrefix(_ prefix: String) throws(MicroTypoError) {
        try call("setPrefix", [prefix])
    }

    public func addSafeTag(_ tag: String) throws(MicroTypoError) {
        try call("addSafeTag", [tag])
    }

    public func addSafeBlock(
        id: String, open: String, close: String, unsafeRegex: Bool = false
    ) throws(MicroTypoError) {
        let block: [String: Any] = ["id": id, "open": open, "close": close, "unsafeRegex": unsafeRegex]
        try call("addSafeBlock", [block])
    }

    public func call(_ method: String, argsJSON: String) throws(MicroTypoError) {
        var error: OpaquePointer?
        let ok = lock.withLock { microtypo_engine_call(engine, method, argsJSON, &error) }
        if !ok { throw MicroTypoError(taking: error!) }
    }

    private func call(_ method: String, _ args: [Any]) throws(MicroTypoError) {
        try call(method, argsJSON: try Self.json(args))
    }

    private func run(_ text: String, withChanges: Bool) throws(MicroTypoError) -> Processed {
        let units = Array(text.utf16)
        var out = microtypo_output()
        var error: OpaquePointer?
        let ok = lock.withLock {
            units.withUnsafeBufferPointer { microtypo_engine_process(engine, $0.baseAddress, $0.count, withChanges, &out, &error) }
        }
        guard ok else { throw MicroTypoError(taking: error!) }
        defer { microtypo_output_free(&out) }
        let result = String(decoding: UnsafeBufferPointer(start: out.units, count: out.len), as: UTF16.self)
        let flat = UnsafeBufferPointer(start: out.changes, count: out.changes_len * 2)
        let changes = stride(from: 0, to: flat.count, by: 2).map { flat[$0]..<flat[$0 + 1] }
        return Processed(text: result, changes: changes)
    }

    private static func json(_ value: Any) throws(MicroTypoError) -> String {
        guard JSONSerialization.isValidJSONObject(value),
            let data = try? JSONSerialization.data(withJSONObject: value)
        else {
            throw MicroTypoError(name: "TypeError", message: "argument is not representable as JSON")
        }
        return String(decoding: data, as: UTF8.self)
    }
}

extension Optional where Wrapped == String {
    fileprivate func withOptionalCString<R>(_ body: (UnsafePointer<CChar>?) -> R) -> R {
        switch self {
        case .some(let s): s.withCString { body($0) }
        case .none: body(nil)
        }
    }
}
