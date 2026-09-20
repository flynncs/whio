// fixtures are a toolbox; each suite uses a subset
#![allow(dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use whio_core::{
    delivery::StreamDelivery,
    media::{MediaBody, MediaFetchError, MediaFetcher, MediaInfo},
    playback::PlayableMedia,
};

struct StubMediaFetcher;

#[async_trait]
impl MediaFetcher for StubMediaFetcher {
    async fn probe(&self, _media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError> {
        Ok(MediaInfo {
            content_length: 1,
            supports_ranges: true,
            content_type: Some("audio/mpeg".to_owned()),
        })
    }

    async fn open_continuous(&self, _media: &PlayableMedia) -> Result<MediaBody, MediaFetchError> {
        Ok(MediaBody {
            content_length: Some(1),
            chunks: Box::pin(tokio_stream::once(Ok(Bytes::from_static(&[0])))),
        })
    }
}

pub fn pictured_flac(directory: &std::path::Path) -> std::path::PathBuf {
    use lofty::file::{AudioFile, TaggedFileExt};

    let destination = directory.join("a.flac");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagged.flac"),
        &destination,
    )
    .unwrap();

    let mut tagged = lofty::probe::Probe::open(&destination)
        .unwrap()
        .read()
        .unwrap();
    {
        let has_primary = tagged.primary_tag_mut().is_some();
        let tag = if has_primary {
            tagged.primary_tag_mut().expect("checked above")
        } else {
            tagged.first_tag_mut().expect("the fixture carries a tag")
        };
        tag.push_picture(lofty::picture::Picture::new_unchecked(
            lofty::picture::PictureType::CoverFront,
            Some(lofty::picture::MimeType::Jpeg),
            None,
            vec![0xFF, 0xD8, 0xFF, 0xE0],
        ));
    }
    tagged
        .save_to_path(&destination, lofty::config::WriteOptions::default())
        .unwrap();

    destination
}

pub fn delivery() -> Arc<StreamDelivery> {
    Arc::new(StreamDelivery::new(
        Arc::new(StubMediaFetcher),
        std::env::temp_dir(),
        whio_core::delivery::DEFAULT_MAX_SESSIONS,
    ))
}
