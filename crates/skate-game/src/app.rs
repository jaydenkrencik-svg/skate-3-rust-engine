use crate::{
    animation, assets, camera,
    config::Config,
    graph_runtime::StockGraphs,
    input,
    physics::{GamePhysics, PhysicsPlugin, SkaterRuntime},
    verification, world,
};
use bevy::{
    prelude::*,
    render::{
        RenderPlugin,
        settings::{Backends, InstanceFlags, RenderCreation, WgpuFeatures, WgpuSettings},
    },
};
use skate_data::GameAssets;

/// Bevy's defaults plus, on request, the query features that make
/// `RenderDiagnosticsPlugin` report per-pass GPU time.
///
/// Opt-in: a required feature the adapter lacks aborts device creation, and the
/// queries are not free. Without them a pass's cost can only be inferred from
/// invocation counts, which says nothing about how long the pass took. Set
/// `SKATE_GPU_TIMING=1` to get `render/**/elapsed_gpu`.
fn wgpu_features() -> WgpuFeatures {
    let default = WgpuSettings::default().features;
    if std::env::var_os("SKATE_GPU_TIMING").is_some_and(|v| v != "0") {
        default
            | WgpuFeatures::TIMESTAMP_QUERY
            | WgpuFeatures::TIMESTAMP_QUERY_INSIDE_ENCODERS
            | WgpuFeatures::PIPELINE_STATISTICS_QUERY
    } else {
        default
    }
}

#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone)]
pub(crate) enum FrameSet {
    Assets,
    Physics,
    Animation,
    Verification,
}

#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone)]
pub(crate) enum SimulationSet {
    Input,
    Controls,
    Physics,
}

pub(crate) fn build(
    config: Config,
    manifest: GameAssets,
    graphs: StockGraphs,
    physics: GamePhysics,
    skater: SkaterRuntime,
) -> App {
    let retail_scene = config.map.as_ref().is_some_and(|map| crate::retail_render::RetailScene::for_map(map));
    let mut app = App::new();
    // Browser: Bevy's own loads go through skate-fs as well, so a data folder
    // the player picked on disk serves models and textures like the server.
    #[cfg(target_arch = "wasm32")]
    app.register_asset_source(
        bevy::asset::io::AssetSourceId::Default,
        bevy::asset::io::AssetSourceBuilder::new(|| crate::assets::directory_reader(skate_fs::WEB_ROOT.into())),
    );
    crate::custom_models::register_source(&mut app);
    crate::modding::register_source(&mut app);
    // The browser build renders through WebGPU into the loader page's canvas,
    // fetches Bevy assets from the server (there are no .meta files), and
    // keeps Bevy's LogPlugin, which logs to the console; natively
    // profiling::init owns logging.
    #[cfg(not(target_arch = "wasm32"))]
    let (backends, canvas, meta_check) = (Backends::VULKAN, None, bevy::asset::AssetMetaCheck::default());
    #[cfg(target_arch = "wasm32")]
    let (backends, canvas, meta_check) = (Backends::BROWSER_WEBGPU, Some("#skate-canvas".to_string()), bevy::asset::AssetMetaCheck::Never);
    let plugins = DefaultPlugins
            .set(AssetPlugin {
                file_path: config.asset_root.to_string_lossy().into_owned(),
                meta_check,
                ..default()
            })
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: config.multiplayer.title.clone().unwrap_or_else(||"Skate 3 Rust Engine".into()),
                    resolution: (1280, 800).into(),
                    canvas,
                    fit_canvas_to_parent: true,
                    prevent_default_event_handling: true,
                    ..default()
                }),
                ..default()
            })
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(WgpuSettings {
                    backends: Some(backends),
                    // Existing machine's validation layer rejects wgpu atomic shaders.
                    // This workaround belongs only to the rendering adapter.
                    instance_flags: InstanceFlags::empty(),
                    features: wgpu_features(),
                    ..default()
                }),
                ..default()
            }).build()
            // Gameplay and menu navigation both use raw pad states from
            // input::platform (SDL3, XInput fallback, the Gamepad API in the
            // browser). No game system consumes Bevy gamepad events/rumble; its
            // second device backend can stall PreUpdate (70.68 ms in the
            // University capture).
            .disable::<bevy::gilrs::GilrsPlugin>();
    #[cfg(not(target_arch = "wasm32"))]
    let plugins = plugins.disable::<bevy::log::LogPlugin>();
    app.add_plugins(plugins)
    .insert_resource(bevy::winit::WinitSettings {focused_mode:bevy::winit::UpdateMode::Continuous,unfocused_mode:bevy::winit::UpdateMode::Continuous})
    .insert_resource(config)
    .insert_resource(crate::retail_render::RetailScene(retail_scene))
    .insert_resource(assets::AssetManifest(manifest))
    .insert_resource(graphs)
    .insert_resource(physics)
    .insert_resource(skater)
    .configure_sets(
        FixedUpdate,
        (
            SimulationSet::Input,
            SimulationSet::Controls,
            SimulationSet::Physics,
        )
            .chain(),
    )
    .configure_sets(
        Update,
        (
            FrameSet::Assets,
            FrameSet::Physics,
            FrameSet::Animation,
            FrameSet::Verification,
        )
            .chain(),
    )
    .add_plugins(crate::frame_timing::FrameTimingPlugin)
    .add_plugins((
        crate::retail_render::RetailRenderPlugin,
        input::InputPlugin,
        PhysicsPlugin,
        crate::presentation::PresentationPlugin,
        crate::replay::ReplayPlugin,
        assets::GameAssetsPlugin,
        animation::AnimationPlugin,
        world::WorldPlugin,
        crate::grind_world::GrindGeometryPlugin,
        camera::CameraPlugin,
        crate::graphics_menu::GraphicsMenuPlugin,
        crate::map_transition::MapTransitionPlugin,
        crate::render_capacity::RenderCapacityPlugin,
        verification::VerificationPlugin,
        crate::performance::PerformancePlugin,
    ));
    app.add_plugins((crate::session_marker::SessionMarkerPlugin, crate::customiser::CustomiserPlugin));
    app.add_plugins(crate::custom_models::CustomModelsPlugin);
    app.add_plugins(crate::trigger_volumes::TriggerVolumesPlugin);
    app.add_plugins(crate::retail_menus::RetailMenusPlugin);
    app.add_plugins(crate::modding::ModdingPlugin);
    crate::teleport_menu::install(&mut app);
    app.add_plugins(crate::updater::UpdaterPlugin);
    app.add_plugins(crate::multiplayer::MultiplayerPlugin);
    app.add_plugins(crate::scoring_hud::ScoringHudPlugin);
    app.add_plugins(crate::debug_cam::DebugCamPlugin);
    app.add_plugins(crate::water_splash::WaterSplashPlugin);
    app.add_plugins(crate::ui_audio::UiAudioPlugin);
    app.add_plugins(crate::game_audio::GameAudioPlugin);
    app.add_systems(Last, crate::crash_context::sample);
    crate::profiling::install(&mut app);
    app
}
