import Foundation
import SwiftUI

/// Observable data store that manages the project's task list.
///
/// In production mode this calls through to the Rust FFI layer via
/// `RustBridge`.  For SwiftUI previews and development without the Rust
/// library linked, it falls back to in-memory sample data.
@MainActor
class ProjectStore: ObservableObject {
    @Published var tasks: [WreckItTask] = []
    @Published var showNewTaskSheet = false
    @Published var errorMessage: String?

    /// Path to the JSON task file managed by wreck-it.
    var taskFilePath: String

    /// Whether to use the Rust FFI bridge (false for previews).
    private let useRustBridge: Bool

    init(taskFilePath: String = "", useRustBridge: Bool = false) {
        self.taskFilePath = taskFilePath
        self.useRustBridge = useRustBridge
    }

    /// Convenience initialiser for SwiftUI previews using sample data.
    static var preview: ProjectStore {
        let store = ProjectStore()
        store.tasks = WreckItTask.samples
        return store
    }

    // MARK: - Derived collections

    /// All top-level tasks that have at least one child.
    var epics: [WreckItTask] {
        let parentIds = Set(tasks.compactMap(\.parentId))
        return tasks.filter { $0.parentId == nil && parentIds.contains($0.id) }
    }

    /// All tasks with no parent (standalone, not an epic).
    var standaloneTasks: [WreckItTask] {
        let parentIds = Set(tasks.compactMap(\.parentId))
        return tasks.filter { $0.parentId == nil && !parentIds.contains($0.id) }
    }

    /// Sub-tasks for a given epic id.
    func subTasks(of epicId: String) -> [WreckItTask] {
        tasks.filter { $0.parentId == epicId }
    }

    /// Progress (0–1) for an epic.
    func epicProgress(_ epicId: String) -> Double {
        let subs = subTasks(of: epicId)
        guard !subs.isEmpty else { return 0 }
        let done = subs.filter { $0.status == .completed }.count
        return Double(done) / Double(subs.count)
    }

    /// Tasks grouped by status (for the board view).
    func tasksByStatus(_ status: TaskStatus) -> [WreckItTask] {
        tasks.filter { $0.status == status }
    }

    // MARK: - CRUD operations

    func reload() {
        if useRustBridge {
            do {
                tasks = try RustBridge.listTasks(taskFile: taskFilePath)
            } catch {
                errorMessage = error.localizedDescription
            }
        }
        // In preview mode tasks are set directly.
    }

    func createTask(id: String, description: String, parentId: String?, labels: [String]) {
        if useRustBridge {
            do {
                if let pid = parentId {
                    _ = try RustBridge.createSubTask(
                        taskFile: taskFilePath, id: id, parentId: pid,
                        description: description, labels: labels)
                } else {
                    _ = try RustBridge.createTask(
                        taskFile: taskFilePath, id: id,
                        description: description, labels: labels)
                }
                reload()
            } catch {
                errorMessage = error.localizedDescription
            }
        } else {
            // In-memory fallback for previews.
            let task = WreckItTask(
                id: id, description: description, status: .pending,
                parentId: parentId, labels: labels)
            tasks.append(task)
        }
    }

    func moveTask(id: String, to status: TaskStatus) {
        if useRustBridge {
            do {
                _ = try RustBridge.moveTask(
                    taskFile: taskFilePath, id: id, status: status.rawValue)
                reload()
            } catch {
                errorMessage = error.localizedDescription
            }
        } else {
            if let idx = tasks.firstIndex(where: { $0.id == id }) {
                tasks[idx].status = status
            }
        }
    }

    func deleteTask(id: String) {
        if useRustBridge {
            do {
                try RustBridge.deleteTask(taskFile: taskFilePath, id: id)
                reload()
            } catch {
                errorMessage = error.localizedDescription
            }
        } else {
            tasks.removeAll { $0.id == id || $0.parentId == id }
        }
    }
}
