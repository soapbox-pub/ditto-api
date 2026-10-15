//! SVG → JPEG rasterization, with external images fetched up front.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;

use image::ImageFormat;
use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{self, ImageHrefResolver, ImageKind, roxmltree};
use tokio::time::Instant;

use super::emoji::FONTDB;
use super::theme::WIDTH;
use crate::config::FetchLimits;
use crate::fetch::{is_https_url, limited_bytes, safe_fetch};

/// Images keyed by the `href` they're referenced by.
pub type Images = HashMap<String, Vec<u8>>;

/// The https `href`s of an SVG's `<image>` elements.
fn image_hrefs(svg: &str) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(svg) else {
        return vec![];
    };
    let mut hrefs: Vec<String> = doc
        .descendants()
        .filter(|n| n.has_tag_name("image"))
        .filter_map(|n| {
            n.attribute("href")
                .or_else(|| n.attribute(("http://www.w3.org/1999/xlink", "href")))
        })
        .filter(|h| is_https_url(h))
        .map(str::to_string)
        .collect();
    hrefs.dedup();
    hrefs
}

/// Fetch every https image an SVG references into `images`.
///
/// Each gets its own `fetch_timeout`. Ones that fail or are too big are left
/// out, and the image renders without them.
pub async fn fetch_images(
    client: &reqwest::Client,
    svg: &str,
    limits: &FetchLimits,
    images: &mut Images,
) {
    let fetches = image_hrefs(svg).into_iter().map(|url| async move {
        let deadline = Instant::now() + limits.fetch_timeout;
        let resp = safe_fetch(client, &url, None, deadline)
            .await
            .filter(|r| r.status().is_success())?;
        let bytes = limited_bytes(resp, limits.max_image_bytes).await.ok()?;
        Some((url, bytes))
    });
    images.extend(
        futures::future::join_all(fetches)
            .await
            .into_iter()
            .flatten(),
    );
}

/// Hand an image to resvg in a format it decodes natively, converting
/// anything else to PNG.
fn image_kind(data: &[u8]) -> Option<ImageKind> {
    let data = Arc::new(data.to_vec());
    Some(match image::guess_format(&data).ok()? {
        ImageFormat::Png => ImageKind::PNG(data),
        ImageFormat::Jpeg => ImageKind::JPEG(data),
        ImageFormat::Gif => ImageKind::GIF(data),
        ImageFormat::WebP => ImageKind::WEBP(data),
        _ => {
            let decoded = image::load_from_memory(&data).ok()?;
            let mut png = Cursor::new(Vec::new());
            decoded.write_to(&mut png, ImageFormat::Png).ok()?;
            ImageKind::PNG(Arc::new(png.into_inner()))
        }
    })
}

/// Rasterize an SVG to a JPEG, `WIDTH` pixels wide.
///
/// `<image>` references are resolved from `images` only; nothing is read
/// from disk or the network here. CPU-bound, so run it off the async
/// runtime.
pub fn svg_to_jpeg(svg: &str, images: Images) -> anyhow::Result<Vec<u8>> {
    let images = Arc::new(images);
    let resolver = ImageHrefResolver {
        resolve_data: ImageHrefResolver::default_data_resolver(),
        resolve_string: Box::new(move |href, _| image_kind(images.get(href)?)),
    };
    let opts = usvg::Options {
        image_href_resolver: resolver,
        fontdb: FONTDB.clone(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(svg, &opts)?;

    let scale = WIDTH as f32 / tree.size().width();
    let width = WIDTH as u32;
    let height = (tree.size().height() * scale).ceil() as u32;
    let mut pixmap = Pixmap::new(width, height).ok_or_else(|| anyhow::anyhow!("empty canvas"))?;
    resvg::render(
        &tree,
        Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    // The encoder drops alpha, and dropping it from premultiplied pixels
    // composites onto black, which is how ditto-server flattened for JPEG.
    // 4:2:0 chroma subsampling is what sharp used. Its other size win,
    // optimized Huffman tables, is left off: this encoder's optimized output
    // decodes as garbage in zune-jpeg (libjpeg reads it fine), and crawlers
    // decode with all sorts of things.
    let mut jpeg = Vec::new();
    let mut encoder = Encoder::new(&mut jpeg, 80);
    encoder.set_sampling_factor(SamplingFactor::R_4_2_0);
    encoder.encode(pixmap.data(), width as u16, height as u16, ColorType::Rgba)?;
    Ok(jpeg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_https_image_hrefs() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink">
            <image href="https://a.example/x.png?a=1&amp;b=2"/>
            <image xlink:href="https://b.example/y.png"/>
            <image href="http://c.example/z.png"/>
            <image href="internal"/>
        </svg>"#;
        assert_eq!(
            image_hrefs(svg),
            ["https://a.example/x.png?a=1&b=2", "https://b.example/y.png"]
        );
    }

    #[test]
    fn rasterizes_with_resolved_images() {
        let mut red = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]))
            .write_to(&mut red, ImageFormat::Bmp)
            .unwrap();
        let images = Images::from([("red".to_string(), red.into_inner())]);

        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630">
            <rect width="1200" height="630" fill="#0000ff"/>
            <image href="red" x="0" y="0" width="600" height="630"/>
            <image href="missing" x="600" y="0" width="600" height="630"/>
        </svg>"##;
        let jpeg = svg_to_jpeg(svg, images).unwrap();
        let img = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        assert_eq!(img.dimensions(), (1200, 630));
        let [r, _, b] = img.get_pixel(300, 300).0;
        assert!(r > 200 && b < 50, "left half is the BMP, converted");
        let [r, _, b] = img.get_pixel(900, 300).0;
        assert!(r < 50 && b > 200, "right half falls back to the background");
    }
}
