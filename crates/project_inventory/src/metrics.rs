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
pub const PRODUCTION_STATEMENT_COUNT: &str = "production_statement_count";
pub const REACTIVE_WIRING_DENSITY: &str = "reactive_wiring_density";
pub const NAV_DISPATCH_DENSITY: &str = "nav_dispatch_density";
pub const AVG_CC_PER_CONTROLLER: &str = "avg_cc_per_controller";
pub const AVG_CC_PER_FRAGMENT: &str = "avg_cc_per_fragment";
pub const AVG_STATEMENTS_PER_ENDPOINT: &str = "avg_statements_per_endpoint";

// --- EXTRA_TECH keys (depth detectors + breadth) -----------------------
/// Exact count of Firebase Admin `FirebaseMessaging.*send*` call sites (Spring).
pub const FCM_SEND_CALL_COUNT: &str = "fcm_send_call_count";
/// Best-effort count of REST endpoints that reach an FCM send (≤3-hop call graph).
pub const FCM_SENDING_ENDPOINT_COUNT: &str = "fcm_sending_endpoint_count";
/// 0–2: received messages stored in Room (1) and a DAO exposes LiveData/Flow (2).
pub const FCM_ANDROID_ROOM_STORE: &str = "fcm_android_room_store";
/// Repositories extending `JpaSpecificationExecutor` (Spring query filtering).
pub const SPEC_EXECUTOR_REPO_COUNT: &str = "spec_executor_repo_count";
/// Methods/fields whose type is `Specification<…>`.
pub const SPECIFICATION_DEF_COUNT: &str = "specification_def_count";
/// `.send(…)` call sites on a `JavaMailSender` (email).
pub const EMAIL_SEND_SITE_COUNT: &str = "email_send_site_count";
/// Custom-drawing sites: `onDraw(Canvas)` overrides, `GLSurfaceView`, `new Paint/Canvas`.
pub const GRAPHICS_CUSTOM_DRAW_COUNT: &str = "graphics_custom_draw_count";
/// Audio/video API uses (MediaPlayer, VideoView, ExoPlayer/media3, …).
pub const AV_USAGE_COUNT: &str = "av_usage_count";
/// Count of new (non-baseline) Gradle dependency coordinates not tied to a
/// curated feature category (generic breadth).
pub const EXTRA_DEPENDENCY_COUNT: &str = "extra_dependency_count";

/// Keys produced by the EXTRA_TECH depth detectors + breadth (always written,
/// zero-filled when absent), kept distinct so callers can reason about them.
pub const EXTRA_TECH_KEYS: &[&str] = &[
    FCM_SEND_CALL_COUNT,
    FCM_SENDING_ENDPOINT_COUNT,
    FCM_ANDROID_ROOM_STORE,
    SPEC_EXECUTOR_REPO_COUNT,
    SPECIFICATION_DEF_COUNT,
    EMAIL_SEND_SITE_COUNT,
    GRAPHICS_CUSTOM_DRAW_COUNT,
    AV_USAGE_COUNT,
    EXTRA_DEPENDENCY_COUNT,
];

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
    PRODUCTION_STATEMENT_COUNT,
    REACTIVE_WIRING_DENSITY,
    NAV_DISPATCH_DENSITY,
    AVG_CC_PER_CONTROLLER,
    AVG_CC_PER_FRAGMENT,
    AVG_STATEMENTS_PER_ENDPOINT,
    FCM_SEND_CALL_COUNT,
    FCM_SENDING_ENDPOINT_COUNT,
    FCM_ANDROID_ROOM_STORE,
    SPEC_EXECUTOR_REPO_COUNT,
    SPECIFICATION_DEF_COUNT,
    EMAIL_SEND_SITE_COUNT,
    GRAPHICS_CUSTOM_DRAW_COUNT,
    AV_USAGE_COUNT,
    EXTRA_DEPENDENCY_COUNT,
];
