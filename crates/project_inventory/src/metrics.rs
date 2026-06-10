//! Stable metric keys persisted in `repo_structural_metrics`.

pub const CONTROLLER_COUNT: &str = "controller_count";
pub const SERVICE_COUNT: &str = "service_count";
pub const ENTITY_COUNT: &str = "entity_count";
pub const REPOSITORY_COUNT: &str = "repository_count";
pub const ENDPOINT_COUNT: &str = "endpoint_count";
pub const FRAGMENT_COUNT: &str = "fragment_count";
pub const ACTIVITY_COUNT: &str = "activity_count";
pub const VIEWMODEL_COUNT: &str = "viewmodel_count";
pub const ROOM_DATABASE_COUNT: &str = "room_database_count";
pub const CUSTOM_QUERY_COUNT: &str = "custom_query_count";
pub const SCHEDULED_TASK_COUNT: &str = "scheduled_task_count";
pub const OBSERVE_CALL_COUNT: &str = "observe_call_count";
pub const NAV_DISPATCH_COUNT: &str = "nav_dispatch_count";
pub const REACTIVE_STATE_FIELD_COUNT: &str = "reactive_state_field_count";
pub const PRODUCTION_LOC: &str = "production_loc";
pub const REACTIVE_WIRING_DENSITY: &str = "reactive_wiring_density";
pub const NAV_DISPATCH_DENSITY: &str = "nav_dispatch_density";
pub const AVG_CC_PER_CONTROLLER: &str = "avg_cc_per_controller";
pub const AVG_CC_PER_FRAGMENT: &str = "avg_cc_per_fragment";
pub const AVG_STATEMENTS_PER_ENDPOINT: &str = "avg_statements_per_endpoint";

/// All keys written for a successful scan (for tests and diagnostics).
pub const ALL_KEYS: &[&str] = &[
    CONTROLLER_COUNT,
    SERVICE_COUNT,
    ENTITY_COUNT,
    REPOSITORY_COUNT,
    ENDPOINT_COUNT,
    FRAGMENT_COUNT,
    ACTIVITY_COUNT,
    VIEWMODEL_COUNT,
    ROOM_DATABASE_COUNT,
    CUSTOM_QUERY_COUNT,
    SCHEDULED_TASK_COUNT,
    OBSERVE_CALL_COUNT,
    NAV_DISPATCH_COUNT,
    REACTIVE_STATE_FIELD_COUNT,
    PRODUCTION_LOC,
    REACTIVE_WIRING_DENSITY,
    NAV_DISPATCH_DENSITY,
    AVG_CC_PER_CONTROLLER,
    AVG_CC_PER_FRAGMENT,
    AVG_STATEMENTS_PER_ENDPOINT,
];
