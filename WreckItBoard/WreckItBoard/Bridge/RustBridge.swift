import Foundation

/// Swift wrapper around the wreck-it C FFI functions.
///
/// The Rust static library (`libwreck_it.a`) must be linked into the app
/// target.  Build the Rust library with:
///
/// ```bash
/// cargo build --release
/// ```
///
/// Then add `target/release/libwreck_it.a` to the Xcode project's
/// "Link Binary With Libraries" build phase and set the appropriate
/// header search path for the generated C header (`wreck_it.h`).
///
/// When the Rust library is not linked (e.g. during SwiftUI previews),
/// `ProjectStore` falls back to in-memory sample data.
enum RustBridge {
    /// Errors returned by the FFI layer.
    enum BridgeError: LocalizedError {
        case ffiError(String)
        case decodingError(String)

        var errorDescription: String? {
            switch self {
            case .ffiError(let msg): return "FFI error: \(msg)"
            case .decodingError(let msg): return "Decoding error: \(msg)"
            }
        }
    }

    // MARK: - FFI function declarations
    //
    // These match the `extern "C"` signatures in `src/ffi.rs`.  When the
    // Rust static library is linked, the linker resolves them.  The actual
    // declarations are provided by the generated C header; these typealias
    // stubs allow the Swift code to compile even when the header isn't
    // available yet.

    // NOTE: In a fully integrated build, replace these stubs with:
    //   import WreckItFFI   (a module map pointing at the generated header)

    private static func callFFI(_ ptr: UnsafeMutablePointer<CChar>?) throws -> Data {
        guard let ptr = ptr else {
            throw BridgeError.ffiError("null pointer returned from FFI")
        }
        let str = String(cString: ptr)
        // The Rust side allocated this — free it.
        wreck_it_free_string(ptr)

        guard let data = str.data(using: .utf8) else {
            throw BridgeError.decodingError("invalid UTF-8 from FFI")
        }

        // Check for error envelope: {"error":"…"}
        if let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let errMsg = obj["error"] as? String {
            throw BridgeError.ffiError(errMsg)
        }

        return data
    }

    // MARK: - Public API

    static func listTasks(taskFile: String) throws -> [WreckItTask] {
        let data = try callFFI(wreck_it_list_tasks(taskFile))
        return try JSONDecoder().decode([WreckItTask].self, from: data)
    }

    static func createTask(
        taskFile: String, id: String, description: String, labels: [String]
    ) throws -> WreckItTask {
        let labelsJSON = try String(data: JSONEncoder().encode(labels), encoding: .utf8) ?? "[]"
        let data = try callFFI(
            wreck_it_create_task(taskFile, id, description, labelsJSON))
        return try JSONDecoder().decode(WreckItTask.self, from: data)
    }

    static func createSubTask(
        taskFile: String, id: String, parentId: String,
        description: String, labels: [String]
    ) throws -> WreckItTask {
        let labelsJSON = try String(data: JSONEncoder().encode(labels), encoding: .utf8) ?? "[]"
        let data = try callFFI(
            wreck_it_create_sub_task(taskFile, id, parentId, description, labelsJSON))
        return try JSONDecoder().decode(WreckItTask.self, from: data)
    }

    static func moveTask(
        taskFile: String, id: String, status: String
    ) throws -> WreckItTask {
        let data = try callFFI(wreck_it_move_task(taskFile, id, status))
        return try JSONDecoder().decode(WreckItTask.self, from: data)
    }

    static func deleteTask(taskFile: String, id: String) throws {
        _ = try callFFI(wreck_it_delete_task(taskFile, id))
    }

    static func epicProgress(taskFile: String, epicId: String) throws -> Double? {
        let data = try callFFI(wreck_it_epic_progress(taskFile, epicId))
        let obj = try JSONSerialization.jsonObject(with: data) as? [String: Any]
        return obj?["progress"] as? Double
    }
}
