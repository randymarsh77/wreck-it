import Foundation

/// Mirror of the Rust `TaskStatus` enum.
enum TaskStatus: String, Codable, CaseIterable, Identifiable {
    case pending
    case inprogress
    case completed
    case failed

    var id: String { rawValue }

    var displayName: String {
        switch self {
        case .pending:    return "Pending"
        case .inprogress: return "In Progress"
        case .completed:  return "Completed"
        case .failed:     return "Failed"
        }
    }

    var iconName: String {
        switch self {
        case .pending:    return "circle"
        case .inprogress: return "play.circle.fill"
        case .completed:  return "checkmark.circle.fill"
        case .failed:     return "xmark.circle.fill"
        }
    }

    var color: String {
        switch self {
        case .pending:    return "gray"
        case .inprogress: return "blue"
        case .completed:  return "green"
        case .failed:     return "red"
        }
    }
}

/// Mirror of the Rust `Task` struct.
struct WreckItTask: Codable, Identifiable, Equatable {
    let id: String
    var description: String
    var status: TaskStatus
    var role: String?
    var kind: String?
    var cooldownSeconds: Int?
    var phase: Int?
    var dependsOn: [String]?
    var priority: Int?
    var complexity: Int?
    var failedAttempts: Int?
    var lastAttemptAt: Int?
    var inputs: [String]?
    var outputs: [TaskArtefact]?
    var runtime: String?
    var preconditionPrompt: String?
    var parentId: String?
    var labels: [String]?

    enum CodingKeys: String, CodingKey {
        case id, description, status, role, kind, phase, priority, complexity
        case cooldownSeconds = "cooldown_seconds"
        case dependsOn = "depends_on"
        case failedAttempts = "failed_attempts"
        case lastAttemptAt = "last_attempt_at"
        case inputs, outputs, runtime
        case preconditionPrompt = "precondition_prompt"
        case parentId = "parent_id"
        case labels
    }

    /// Whether this task is an epic (has children).
    var isEpic: Bool { false } // Computed by the store based on children

    /// Human-readable label list.
    var labelText: String {
        (labels ?? []).joined(separator: ", ")
    }
}

/// Mirror of the Rust `TaskArtefact` struct.
struct TaskArtefact: Codable, Equatable {
    let kind: String
    let name: String
    let path: String
}

// MARK: - Sample Data

extension WreckItTask {
    static let samples: [WreckItTask] = [
        WreckItTask(id: "epic-1", description: "Authentication system", status: .pending,
                     labels: ["backend"]),
        WreckItTask(id: "sub-1", description: "Add login endpoint", status: .inprogress,
                     parentId: "epic-1", labels: ["backend", "api"]),
        WreckItTask(id: "sub-2", description: "Add signup endpoint", status: .pending,
                     parentId: "epic-1", labels: ["backend", "api"]),
        WreckItTask(id: "sub-3", description: "Add password reset", status: .completed,
                     parentId: "epic-1"),
        WreckItTask(id: "epic-2", description: "Dashboard UI", status: .pending,
                     labels: ["frontend"]),
        WreckItTask(id: "sub-4", description: "Design wireframes", status: .completed,
                     parentId: "epic-2", labels: ["design"]),
        WreckItTask(id: "sub-5", description: "Implement dashboard layout", status: .failed,
                     parentId: "epic-2", labels: ["frontend"]),
        WreckItTask(id: "standalone-1", description: "Update CI pipeline", status: .pending,
                     labels: ["devops"]),
    ]
}
