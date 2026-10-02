//! 迷你 PNG 解码器：只为画彩色 emoji。
//!
//! 彩色 emoji 字体（Noto Color Emoji 这类）把每个字形存成 CBDT 表里的一张 PNG，
//! ab_glyph 只把压缩后的字节递出来，所以这里自己解。emoji 字体用的都是
//! 8 位、非隔行、调色板或真彩，这几种够用了。

use anyhow::{Context, Result, bail};
use flate2::read::ZlibDecoder;
use std::io::Read;

/// 解出来的图：RGBA8，非预乘。
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub fn decode(data: &[u8]) -> Result<Image> {
    if data.len() < 8 || &data[..8] != b"\x89PNG\r\n\x1a\n" {
        bail!("不是 PNG");
    }
    let mut pos = 8;
    let (mut width, mut height) = (0u32, 0u32);
    let (mut depth, mut color_type, mut interlace) = (8u8, 0u8, 0u8);
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut transparency: Vec<u8> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();
    while pos + 8 <= data.len() {
        let length = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let kind = &data[pos + 4..pos + 8];
        let body = data.get(pos + 8..pos + 8 + length).context("PNG 块越界")?;
        match kind {
            b"IHDR" => {
                width = u32::from_be_bytes(body[0..4].try_into().unwrap());
                height = u32::from_be_bytes(body[4..8].try_into().unwrap());
                depth = body[8];
                color_type = body[9];
                interlace = body[12];
            }
            b"PLTE" => palette = body.as_chunks::<3>().0.iter().map(|c| [c[0], c[1], c[2]]).collect(),
            b"tRNS" => transparency = body.to_vec(),
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        pos += 12 + length; // 4 长度 + 4 类型 + 数据 + 4 CRC
    }
    if width == 0 || height == 0 {
        bail!("PNG 里没有 IHDR");
    }
    if depth != 8 {
        bail!("只支持 8 位 PNG（这张是 {depth} 位）");
    }
    if interlace != 0 {
        bail!("不支持隔行 PNG");
    }
    let channels = match color_type {
        0 => 1, // 灰度
        2 => 3, // 真彩
        3 => 1, // 调色板
        4 => 2, // 灰度 + alpha
        6 => 4, // 真彩 + alpha
        other => bail!("不认识的 PNG 颜色类型 {other}"),
    };

    let mut raw = Vec::new();
    ZlibDecoder::new(&idat[..])
        .read_to_end(&mut raw)
        .context("PNG 解压失败")?;
    let stride = width as usize * channels;
    let mut pixels = vec![0u8; stride * height as usize];
    let mut prev = vec![0u8; stride];
    for y in 0..height as usize {
        let start = y * (stride + 1);
        let filter = *raw.get(start).context("PNG 数据不完整")?;
        let src = raw
            .get(start + 1..start + 1 + stride)
            .context("PNG 数据不完整")?;
        let dst = &mut pixels[y * stride..(y + 1) * stride];
        unfilter(filter, src, &prev, dst, channels)?;
        prev.copy_from_slice(dst);
    }

    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    for (index, out) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let px = &pixels[index * channels..index * channels + channels];
        let (r, g, b, a) = match color_type {
            0 => (px[0], px[0], px[0], 255),
            2 => (px[0], px[1], px[2], 255),
            3 => {
                let color = palette.get(px[0] as usize).copied().unwrap_or([0, 0, 0]);
                (color[0], color[1], color[2], transparency.get(px[0] as usize).copied().unwrap_or(255))
            }
            4 => (px[0], px[0], px[0], px[1]),
            _ => (px[0], px[1], px[2], px[3]),
        };
        out.copy_from_slice(&[r, g, b, a]);
    }
    Ok(Image { width, height, rgba })
}

fn unfilter(filter: u8, src: &[u8], prev: &[u8], dst: &mut [u8], channels: usize) -> Result<()> {
    match filter {
        0 => dst.copy_from_slice(src),
        1 => {
            for i in 0..dst.len() {
                let left = if i >= channels { dst[i - channels] } else { 0 };
                dst[i] = src[i].wrapping_add(left);
            }
        }
        2 => {
            for i in 0..dst.len() {
                dst[i] = src[i].wrapping_add(prev[i]);
            }
        }
        3 => {
            for i in 0..dst.len() {
                let left = if i >= channels { dst[i - channels] as u16 } else { 0 };
                let up = prev[i] as u16;
                dst[i] = src[i].wrapping_add(((left + up) / 2) as u8);
            }
        }
        4 => {
            for i in 0..dst.len() {
                let left = if i >= channels { dst[i - channels] } else { 0 };
                let up = prev[i];
                let up_left = if i >= channels { prev[i - channels] } else { 0 };
                dst[i] = src[i].wrapping_add(paeth(left, up, up_left));
            }
        }
        other => bail!("不认识的 PNG 过滤类型 {other}"),
    }
    Ok(())
}

fn paeth(left: u8, up: u8, up_left: u8) -> u8 {
    let (a, b, c) = (left as i32, up as i32, up_left as i32);
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ab_glyph::{Font, FontRef};

    /// 拿真字体里的 emoji 位图当素材：它是 PNG，而且是我们实际要解的那种。
    fn emoji_png() -> Vec<u8> {
        let file = std::fs::File::open("/usr/share/fonts/noto/NotoColorEmoji.ttf")
            .expect("测试要装 noto-fonts-emoji");
        let map = unsafe { memmap2::Mmap::map(&file) }.unwrap();
        let font = FontRef::try_from_slice_and_index(&map, 0).unwrap();
        let image = font
            .glyph_raster_image2(font.glyph_id('😀'), 136)
            .expect("😀 应该有位图");
        image.data.to_vec()
    }

    #[test]
    fn decodes_color_emoji_png() {
        let image = decode(&emoji_png()).expect("解 PNG");
        assert!(image.width >= 64 && image.height >= 64, "尺寸 {}x{}", image.width, image.height);
        assert_eq!(image.rgba.len(), (image.width * image.height * 4) as usize);
        // 😀 是黄的：不透明像素里应该有 r、g 明显高于 b 的
        let yellow = image
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[3] > 200 && px[0] > 180 && px[1] > 150 && px[2] < 120)
            .count();
        assert!(yellow > 100, "没找到黄色像素（找到 {yellow} 个）");
    }

    #[test]
    fn rejects_junk() {
        assert!(decode(b"not a png at all").is_err());
    }
}
