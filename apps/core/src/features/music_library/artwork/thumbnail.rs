use bytes::Bytes;

pub(crate) const MIN_SIZE: u32 = 16;
pub(crate) const MAX_SIZE: u32 = 1200;
pub(crate) const MAX_DECODED_PIXELS: u64 = 16_000_000;

pub(crate) fn fit(bytes: &[u8], size: u32) -> Option<Bytes> {
    let image = image::load_from_memory(bytes).ok()?;

    fitted(&image, size).map(|(bytes, _, _)| bytes)
}

pub(crate) fn fitted(image: &image::DynamicImage, size: u32) -> Option<(Bytes, u32, u32)> {
    if image.width() <= size && image.height() <= size {
        return None;
    }

    let thumbnail = image.thumbnail(size, size);
    let mut encoded = std::io::Cursor::new(Vec::new());
    thumbnail
        .write_to(&mut encoded, image::ImageFormat::Jpeg)
        .ok()?;

    Some((
        Bytes::from(encoded.into_inner()),
        thumbnail.width(),
        thumbnail.height(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let image =
            image::RgbImage::from_fn(width, height, |x, _| image::Rgb([(x % 256) as u8, 64, 192]));
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut encoded, image::ImageFormat::Jpeg)
            .unwrap();

        encoded.into_inner()
    }

    #[test]
    fn an_image_larger_than_the_request_is_shrunk_to_fit() {
        let fitted = fit(&jpeg(600, 300), 200).expect("a 600px image shrinks to 200");

        let decoded = image::load_from_memory(&fitted).unwrap();

        assert_eq!((decoded.width(), decoded.height()), (200, 100));
    }

    #[test]
    fn an_image_that_already_fits_is_left_alone() {
        assert!(fit(&jpeg(200, 200), 300).is_none());
    }

    #[test]
    fn a_payload_that_is_not_an_image_is_refused() {
        assert!(fit(b"not an image", 300).is_none());
    }
}
