mod circuit_breaker;
pub(crate) mod config;
mod controller;
pub(crate) mod native_official;
pub(crate) mod protocol;
mod proxy;

pub(crate) use controller::resume_after_failed_update;

pub(crate) use controller::{
    attach_app_handle, begin_update_handoff, direct_document, ensure_conversion_route,
    ensure_shutdown_allowed, get_status, initialize, recover_stale_route, refresh_saved_routes,
    reset_health, save_settings, shutdown_all, with_provider_change, FailoverSettings,
    FailoverStatus,
};
