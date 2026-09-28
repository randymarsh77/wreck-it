use wreck_it_core::{config::RepoConfig, task_manager::validate_and_append_task, types::Task};

#[test]
fn configured_response_backlog_deserializes_and_has_ordered_dependencies() {
    let config: RepoConfig = toml::from_str(include_str!("../../.wreck-it/config.toml")).unwrap();
    let context = config
        .ralphs
        .iter()
        .find(|r| r.name == "feature-dev")
        .unwrap();
    assert_eq!(
        config.resolve_task_path(&context.task_file),
        "tasks/response-tasks.json"
    );
    let tasks: Vec<Task> =
        serde_json::from_str(include_str!("../../tasks/response-tasks.json")).unwrap();
    let mut accepted: Vec<Task> = vec![];
    for task in tasks {
        for dependency in &task.depends_on {
            let prerequisite = accepted
                .iter()
                .find(|t| &t.id == dependency)
                .expect("dependencies must precede consumers in the backlog");
            assert!(prerequisite.phase < task.phase);
        }
        validate_and_append_task(&mut accepted, task).unwrap();
    }
    assert!(!accepted.is_empty());
}
