use super::{
    read_summary_engine_for_path,
    tree_sitter::{
        engine::{TreeSitterFolderOptions, load_tree_sitter_folder},
        syntax::TreeSitterLanguage,
    },
    types::ReadFolder,
};
use std::sync::Arc;
pub fn prepare_read_folder(
    path: &str,
    folder: Option<Arc<dyn ReadFolder>>,
    selected: &Arc<dyn ReadFolder>,
) -> Option<Arc<dyn ReadFolder>> {
    let folder = folder?;
    if !Arc::ptr_eq(&folder, selected) || read_summary_engine_for_path(path) != Some("wasm") {
        return Some(folder);
    }
    Some(
        load_tree_sitter_folder(
            TreeSitterLanguage::Js,
            TreeSitterFolderOptions::new(folder.clone()),
        )
        .unwrap_or(folder),
    )
}
