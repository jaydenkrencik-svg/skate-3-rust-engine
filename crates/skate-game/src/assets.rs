use crate::app::FrameSet;
use bevy::{asset::LoadState, gltf::Gltf, prelude::*};
use skate_data::GameAssets;

#[derive(Resource)]
pub(crate) struct AssetManifest(pub GameAssets);
#[derive(Resource)]
pub(crate) struct CharacterAsset(pub Handle<Gltf>);
#[derive(Resource, Default, PartialEq, Eq)]
pub(crate) enum AssetStatus {
    #[default]
    Loading,
    Ready,
    Failed,
}

pub(crate) struct GameAssetsPlugin;
impl Plugin for GameAssetsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AssetStatus>()
            .add_systems(Startup, load)
            .add_systems(Update, check.in_set(FrameSet::Assets));
    }
}
fn load(mut commands: Commands, server: Res<AssetServer>, manifest: Res<AssetManifest>) {
    commands.insert_resource(CharacterAsset(
        server.load(manifest.0.character_scene.clone()),
    ));
}
fn check(
    server: Res<AssetServer>,
    character: Res<CharacterAsset>,
    mut status: ResMut<AssetStatus>,
    mut exit: MessageWriter<AppExit>,
) {
    if *status != AssetStatus::Loading {
        return;
    }
    if let Some(LoadState::Failed(error)) = server.get_load_state(character.0.id()) {
        error!("Character asset failed to load: {error}");
        *status = AssetStatus::Failed;
        exit.write(AppExit::error());
    } else if server.is_loaded_with_dependencies(character.0.id()) {
        *status = AssetStatus::Ready;
    }
}

/// A reader for an asset source rooted at a directory: the OS filesystem on
/// native builds, the `skate_fs` virtual filesystem in the browser.
pub(crate) fn directory_reader(root: std::path::PathBuf) -> Box<dyn bevy::asset::io::ErasedAssetReader> {
    #[cfg(not(target_arch = "wasm32"))]
    return Box::new(bevy::asset::io::file::FileAssetReader::new(root));
    #[cfg(target_arch = "wasm32")]
    return Box::new(VfsAssetReader { root });
}

#[cfg(target_arch = "wasm32")]
struct VfsAssetReader {
    root: std::path::PathBuf,
}

#[cfg(target_arch = "wasm32")]
impl bevy::asset::io::AssetReader for VfsAssetReader {
    async fn read<'a>(&'a self, path: &'a std::path::Path) -> Result<impl bevy::asset::io::Reader + 'a, bevy::asset::io::AssetReaderError> {
        use bevy::asset::io::AssetReaderError;
        match skate_fs::read(self.root.join(path)) {
            Ok(bytes) => Ok(bevy::asset::io::VecReader::new(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(AssetReaderError::NotFound(path.to_owned())),
            Err(e) => Err(AssetReaderError::Io(e.into())),
        }
    }

    async fn read_meta<'a>(&'a self, path: &'a std::path::Path) -> Result<impl bevy::asset::io::Reader + 'a, bevy::asset::io::AssetReaderError> {
        Err::<bevy::asset::io::VecReader, _>(bevy::asset::io::AssetReaderError::NotFound(path.to_owned()))
    }

    async fn read_directory<'a>(&'a self, path: &'a std::path::Path) -> Result<Box<bevy::asset::io::PathStream>, bevy::asset::io::AssetReaderError> {
        let entries = skate_fs::read_dir(self.root.join(path))
            .map_err(|_| bevy::asset::io::AssetReaderError::NotFound(path.to_owned()))?;
        let paths: Vec<_> = entries.flatten().map(|entry| path.join(entry.file_name())).collect();
        Ok(Box::new(bevy::tasks::futures_lite::stream::iter(paths)))
    }

    async fn is_directory<'a>(&'a self, path: &'a std::path::Path) -> Result<bool, bevy::asset::io::AssetReaderError> {
        Ok(skate_fs::is_dir(self.root.join(path)))
    }
}
