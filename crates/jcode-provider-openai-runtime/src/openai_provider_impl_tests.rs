#[cfg(test)]
mod lcm_ownership_tests {
    use crate::provider_helpers::provider_native_threshold_for_engine;

    #[test]
    fn lcm_suppresses_provider_native_auto_compaction() {
        assert_eq!(
            provider_native_threshold_for_engine(
                jcode_base::config::CompactionEngine::Rolling,
                Some(100_000),
            ),
            Some(100_000)
        );
        assert_eq!(
            provider_native_threshold_for_engine(
                jcode_base::config::CompactionEngine::Lcm,
                Some(100_000),
            ),
            None
        );
    }
}
