use std::path::{Component, Path, PathBuf};

/// a stored path is relative to the root it was scanned under. anything that
/// could leave that root is not a path this scanner wrote.
pub(crate) fn local_path(root: &str, relative: &str) -> Option<PathBuf> {
    let relative = Path::new(relative);

    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }

    Some(Path::new(root).join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_relative_path_joins_and_an_escaping_one_is_refused() {
        assert_eq!(
            local_path("/media", "Artist/Album/a.flac"),
            Some(PathBuf::from("/media/Artist/Album/a.flac"))
        );
        assert_eq!(local_path("/media", "../outside.flac"), None);
        assert_eq!(local_path("/media", "/etc/passwd"), None);
    }
}
