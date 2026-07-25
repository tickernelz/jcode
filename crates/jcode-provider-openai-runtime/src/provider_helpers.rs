pub(super) fn provider_native_threshold_for_engine(
    engine: jcode_base::config::CompactionEngine,
    threshold: Option<usize>,
) -> Option<usize> {
    (engine != jcode_base::config::CompactionEngine::Lcm)
        .then_some(threshold)
        .flatten()
}

/// Whether a model catalog fetch error is an auth rejection (401/403) that a
/// token force-refresh may fix, as opposed to a network/server failure.
pub(super) fn catalog_error_is_auth_rejection(err: &anyhow::Error) -> bool {
    err.downcast_ref::<jcode_base::provider::ModelCatalogHttpStatus>()
        .is_some_and(|status| status.0 == 401 || status.0 == 403)
}
