#ifndef WRECK_IT_H
#define WRECK_IT_H

#include <stdint.h>

/// Free a string previously returned by a wreck_it_* function.
void wreck_it_free_string(char *ptr);

/// List all tasks. Returns a JSON array.
char *wreck_it_list_tasks(const char *task_file);

/// Get a single task by id. Returns a JSON object or {"error":…}.
char *wreck_it_get_task(const char *task_file, const char *id);

/// List epics (top-level tasks with children). Returns a JSON array.
char *wreck_it_list_epics(const char *task_file);

/// List sub-tasks of a given parent. Returns a JSON array.
char *wreck_it_list_sub_tasks(const char *task_file, const char *parent_id);

/// Create a top-level task. labels_json is a JSON array of strings.
/// Returns the created task as JSON.
char *wreck_it_create_task(const char *task_file, const char *id,
                           const char *description,
                           const char *labels_json);

/// Create a sub-task under a parent. Returns the created task as JSON.
char *wreck_it_create_sub_task(const char *task_file, const char *id,
                               const char *parent_id,
                               const char *description,
                               const char *labels_json);

/// Update a task. update_json is a JSON object with optional fields.
/// Returns the updated task as JSON.
char *wreck_it_update_task(const char *task_file, const char *id,
                           const char *update_json);

/// Delete a task. Returns {"ok":true} or {"error":…}.
char *wreck_it_delete_task(const char *task_file, const char *id);

/// Move a task to a new status. Returns the updated task as JSON.
char *wreck_it_move_task(const char *task_file, const char *id,
                         const char *status);

/// Return the progress of an epic as {"progress":0.5} or {"progress":null}.
char *wreck_it_epic_progress(const char *task_file, const char *epic_id);

#endif /* WRECK_IT_H */
