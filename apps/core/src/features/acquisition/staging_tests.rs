use std::{fs, io, os::unix::fs::MetadataExt, path::PathBuf};

use super::{
    build_import_input, build_import_input_with_marker, checked_import_dir, ensure_import_marker,
    import_scratch, link_import_file,
};
use crate::features::acquisition::ports::{AcquisitionTarget, MbId};
use uuid::Uuid;

struct Staging {
    root: PathBuf,
    staging: PathBuf,
    source: PathBuf,
    id: Uuid,
}

impl Staging {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("whio-aq-input-{}", Uuid::now_v7()));
        let staging = root.join("staging");
        fs::create_dir_all(&staging).unwrap();
        let source = staging.join("One.flac");
        fs::write(&source, b"original audio bytes").unwrap();

        Self {
            root,
            staging,
            source,
            id: Uuid::now_v7(),
        }
    }

    fn dir(&self) -> PathBuf {
        import_scratch(&self.staging, self.id, 1)
    }

    fn build(&self) -> io::Result<PathBuf> {
        build_import_input(
            &self.staging,
            self.id,
            1,
            AcquisitionTarget::Release(
                MbId::parse("11111111-1111-1111-1111-111111111111").unwrap(),
            ),
            std::slice::from_ref(&self.source),
        )
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn two_builders_observing_marker_absence_create_exact_private_inputs() {
    let fixture = Staging::new();
    let second_source = fixture.staging.join("Two.flac");
    fs::write(&second_source, b"second original audio bytes").unwrap();
    let second_id = Uuid::now_v7();
    let target_id = MbId::parse("11111111-1111-1111-1111-111111111111").unwrap();
    let marker = fixture.staging.join(".whio-import");
    let (first_observed, first_absent) = tokio::sync::oneshot::channel();
    let (second_observed, second_absent) = tokio::sync::oneshot::channel();
    let (release_first, first_held) = std::sync::mpsc::channel();
    let (release_second, second_held) = std::sync::mpsc::channel();
    let first = tokio::task::spawn_blocking({
        let staging = fixture.staging.clone();
        let source = fixture.source.clone();
        let id = fixture.id;
        move || {
            build_import_input_with_marker(
                &staging,
                id,
                1,
                AcquisitionTarget::Release(target_id),
                &[source],
                |marker| {
                    assert_eq!(
                        fs::symlink_metadata(marker).unwrap_err().kind(),
                        io::ErrorKind::NotFound
                    );
                    first_observed.send(()).unwrap();
                    first_held.recv().unwrap();
                    fs::create_dir(marker)
                },
            )
        }
    });
    let second = tokio::task::spawn_blocking({
        let staging = fixture.staging.clone();
        let source = second_source.clone();
        move || {
            build_import_input_with_marker(
                &staging,
                second_id,
                1,
                AcquisitionTarget::Recording(target_id),
                &[source],
                |marker| {
                    assert_eq!(
                        fs::symlink_metadata(marker).unwrap_err().kind(),
                        io::ErrorKind::NotFound
                    );
                    second_observed.send(()).unwrap();
                    second_held.recv().unwrap();
                    let error = fs::create_dir(marker).unwrap_err();
                    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);

                    Err(error)
                },
            )
        }
    });
    let guard = std::time::Duration::from_secs(10);
    tokio::time::timeout(guard, first_absent)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(guard, second_absent)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !marker.exists(),
        "both absence observations precede either creation"
    );
    assert!(!first.is_finished() && !second.is_finished());
    release_first.send(()).unwrap();
    let first_private = tokio::time::timeout(guard, first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(marker.is_dir());
    assert!(
        !second.is_finished(),
        "the second creator stays held until the marker exists"
    );
    release_second.send(()).unwrap();
    let second_private = tokio::time::timeout(guard, second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    assert_eq!(first_private, fixture.dir());
    let second_dir = import_scratch(&fixture.staging, second_id, 1);
    assert_eq!(second_private, second_dir.join("Two.flac"));
    assert_ne!(first_private, second_dir);
    let names = |dir: &std::path::Path| {
        let mut names = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    assert_eq!(
        names(&first_private),
        vec![std::ffi::OsString::from("One.flac")]
    );
    assert_eq!(
        names(&second_dir),
        vec![std::ffi::OsString::from("Two.flac")]
    );
    assert_eq!(
        fs::read(first_private.join("One.flac")).unwrap(),
        b"original audio bytes"
    );
    assert_eq!(
        fs::read(second_private).unwrap(),
        b"second original audio bytes"
    );
    assert_eq!(fs::read(&fixture.source).unwrap(), b"original audio bytes");
    assert_eq!(
        fs::read(&second_source).unwrap(),
        b"second original audio bytes"
    );
}

#[test]
fn an_existing_marker_is_reinspected_without_following_links() {
    for node in ["symlink", "file"] {
        let fixture = Staging::new();
        let marker = fixture.staging.join(".whio-import");
        let outside = fixture.root.join("outside");
        fs::create_dir(&outside).unwrap();
        let result = ensure_import_marker(&marker, |marker| {
            if node == "symlink" {
                std::os::unix::fs::symlink(&outside, marker).unwrap();
            } else {
                fs::write(marker, b"not a directory").unwrap();
            }

            Err(io::Error::from(io::ErrorKind::AlreadyExists))
        });

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
        if node == "symlink" {
            assert_eq!(fs::read_link(&marker).unwrap(), outside);
        } else {
            assert_eq!(fs::read(&marker).unwrap(), b"not a directory");
        }
    }
}

#[test]
fn marker_creation_and_inspection_errors_are_propagated() {
    let fixture = Staging::new();
    let marker = fixture.staging.join(".whio-import");
    for code in [libc::EACCES, libc::ENOENT, libc::EIO] {
        let error =
            ensure_import_marker(&marker, |_| Err(io::Error::from_raw_os_error(code))).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(code));
        assert!(!marker.exists());
    }
    let oversized = fixture.staging.join("x".repeat(512));
    let error = ensure_import_marker(&oversized, |_| {
        panic!("inspection failure must precede creation")
    })
    .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ENAMETOOLONG));
}

#[test]
fn rebuilding_a_private_input_never_copies_onto_the_original_inode() {
    let fixture = Staging::new();
    let dir = fixture.build().unwrap();
    let private = dir.join("One.flac");
    assert_eq!(
        fs::metadata(&fixture.source).unwrap().ino(),
        fs::metadata(&private).unwrap().ino()
    );
    assert_eq!(fixture.build().unwrap(), dir);
    assert_eq!(fs::read(&fixture.source).unwrap(), b"original audio bytes");
    assert_eq!(fs::read(&private).unwrap(), b"original audio bytes");
}

#[test]
fn only_cross_device_link_errors_copy_the_input() {
    let fixture = Staging::new();
    let copied = fixture.staging.join("copied.flac");
    link_import_file(&fixture.source, &copied, |_, _| {
        Err(io::Error::from_raw_os_error(libc::EXDEV))
    })
    .unwrap();
    assert_eq!(fs::read(&copied).unwrap(), b"original audio bytes");
    assert_ne!(
        fs::metadata(&fixture.source).unwrap().ino(),
        fs::metadata(&copied).unwrap().ino()
    );

    for code in [libc::EACCES, libc::ENOENT, libc::EEXIST, libc::EIO] {
        let destination = fixture.staging.join(format!("failed-{code}.flac"));
        let error = link_import_file(&fixture.source, &destination, |_, _| {
            Err(io::Error::from_raw_os_error(code))
        })
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(code));
        assert!(!destination.exists());
    }
    assert_eq!(fs::read(&fixture.source).unwrap(), b"original audio bytes");
}

#[test]
fn an_existing_link_is_never_overwritten_by_copy_fallback() {
    let fixture = Staging::new();
    let destination = fixture.staging.join("alias.flac");
    fs::hard_link(&fixture.source, &destination).unwrap();
    let error = link_import_file(&fixture.source, &destination, |_, _| {
        panic!("an existing node must be rejected before linking")
    })
    .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&fixture.source).unwrap(), b"original audio bytes");
    assert_eq!(fs::read(&destination).unwrap(), b"original audio bytes");
}

#[test]
fn private_symlinks_are_rejected_before_creation_or_cleanup() {
    for node in ["root", "marker", "attempt", "destination"] {
        let fixture = Staging::new();
        let outside = fixture.root.join("outside");
        fs::create_dir(&outside).unwrap();
        let secret = outside.join("One.flac");
        fs::write(&secret, b"outside audio").unwrap();
        let marker = fixture.staging.join(".whio-import");
        match node {
            "root" => {
                fs::rename(&fixture.staging, fixture.root.join("original-staging")).unwrap();
                std::os::unix::fs::symlink(&outside, &fixture.staging).unwrap();
            }
            "marker" => {
                std::os::unix::fs::symlink(&outside, &marker).unwrap();
            }
            "attempt" => {
                fs::create_dir(&marker).unwrap();
                std::os::unix::fs::symlink(&outside, fixture.dir()).unwrap();
            }
            "destination" => {
                fs::create_dir_all(fixture.dir()).unwrap();
                std::os::unix::fs::symlink(&secret, fixture.dir().join("One.flac")).unwrap();
            }
            _ => unreachable!(),
        }

        assert!(fixture.build().is_err(), "{node}");
        assert!(
            checked_import_dir(&fixture.staging, fixture.id, 1).is_err(),
            "{node}"
        );
        assert_eq!(fs::read(&secret).unwrap(), b"outside audio", "{node}");
        if node != "root" {
            assert_eq!(
                fs::read(&fixture.source).unwrap(),
                b"original audio bytes",
                "{node}"
            );
        }
    }
}

#[test]
fn private_non_directory_nodes_and_inspection_errors_are_rejected() {
    for node in ["marker", "attempt"] {
        let fixture = Staging::new();
        let marker = fixture.staging.join(".whio-import");
        let bad = if node == "marker" {
            marker
        } else {
            fs::create_dir(&marker).unwrap();
            fixture.dir()
        };
        fs::write(&bad, b"not a directory").unwrap();

        assert!(fixture.build().is_err());
        assert!(checked_import_dir(&fixture.staging, fixture.id, 1).is_err());
        assert_eq!(fs::read(&bad).unwrap(), b"not a directory");
        assert_eq!(fs::read(&fixture.source).unwrap(), b"original audio bytes");
    }
    let fixture = Staging::new();
    let oversized = fixture.staging.join("x".repeat(512));
    assert!(super::directory_exists(&oversized).is_err());
}
