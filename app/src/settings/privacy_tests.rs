use settings::schema::SettingSchemaEntry;
use settings::{Setting, SettingSurfaces, SettingsMode};

use super::{IsCloudConversationStorageEnabled, IsCrashReportingEnabled, IsTelemetryEnabled};

#[test]
fn local_agent_privacy_ignores_cloud_reporting_preferences() {
    warpui::App::test((), |mut app| async move {
        let privacy = app.add_model(|ctx| {
            let settings = super::PrivacySettings::mock(ctx);
            settings
                .auth_state
                .set_credentials(Some(crate::auth::credentials::Credentials::LocalAgent));
            settings
        });
        let _analytics =
            warp_core::features::FeatureFlag::AgentModeAnalytics.override_enabled(true);

        privacy.read(&app, |settings, ctx| {
            let snapshot = settings.get_snapshot(ctx);
            assert!(snapshot.should_disable_telemetry());
            assert!(!snapshot.is_crash_reporting_enabled);
            assert!(!snapshot.should_collect_ai_ugc_telemetry());
            assert_eq!(snapshot.cloud_conversation_storage_enabled, Some(false));
            assert!(settings.is_telemetry_enabled);
            assert!(settings.is_cloud_conversation_storage_enabled);
        });
    });
}

#[test]
fn privacy_settings_apply_to_gui_and_tui() {
    for storage_key in [
        IsTelemetryEnabled::toml_key(),
        IsCrashReportingEnabled::toml_key(),
        IsCloudConversationStorageEnabled::toml_key(),
    ] {
        let entry = inventory::iter::<SettingSchemaEntry>
            .into_iter()
            .find(|entry| entry.storage_key == storage_key)
            .unwrap_or_else(|| panic!("missing schema entry for {storage_key}"));
        let surfaces = (entry.surfaces_fn)();

        assert_eq!(surfaces, SettingSurfaces::ALL, "{storage_key}");
        assert!(surfaces.includes(SettingsMode::Gui), "{storage_key}");
        assert!(surfaces.includes(SettingsMode::Tui), "{storage_key}");
    }
}
