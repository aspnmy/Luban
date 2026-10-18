//! 图片压缩算法 —— 从 Luban（`master` 分支 Kotlin 实现）移植。
//!
//! 移植来源与对应关系：
//!   `algorithm/CompressionCalculator.kt` → [`calculate_target`]
//!   `compression/JpegCompressor.kt`       → [`compress_to_target`]（质量二分搜索）
//!   `io/ImageLoader.kt`                   → [`decode_with_orientation`]（EXIF 转正）
//!
//! ⚠️ 与原实现的差异（均为平台替换，语义保持一致）：
//!   - Android `Bitmap` → `image::DynamicImage`；
//!   - TurboJpeg 原生编码 → `image` 内置 JPEG 编码器（质量参数语义相同）；
//!   - `inSampleSize` 预降采样 → 解码后统一用 Lanczos3 缩放（少了两次重采样，画质更好）；

use image::{codecs::jpeg::JpegEncoder, DynamicImage, ImageFormat};
use std::io::Cursor;

/// 压缩目标（对应 Kotlin 的 `CompressionTarget`）
#[derive(Debug, Clone, Copy)]
pub struct CompressionTarget {
    /// 目标宽（已按原图方向还原为宽×高）
    pub width: u32,
    /// 目标高
    pub height: u32,
    /// 估算体积（KB）—— 长图会作为质量搜索的目标上限
    pub estimated_size_kb: u32,
    /// 是否长图（宽高比 ≤ 0.4）
    pub is_long_image: bool,
    /// 长图才有目标体积；普通图仅用于估算
    pub target_size_kb: Option<u32>,
}

/// 目标边长基准（对应 `CompressionCalculator.baseShort`）
const BASE_SHORT: u32 = 1440;
/// 长边墙：超过则视为超长图（`wallLong`）
const WALL_LONG: u32 = 10800;
/// 长图判定比例阈值（`wallRatio`）
const WALL_RATIO: f64 = 0.4;
/// 超大图陷阱阈值：超过则缩到原短边 1/4（`trapPixels`）
const TRAP_PIXELS: u64 = 40_960_000;
/// 像素硬上限（`capPixels`）
const CAP_PIXELS: u64 = 10_240_000;

/// 计算压缩目标尺寸与估算体积。
///
/// 完整复刻原算法顺序（顺序影响结果，⛔ 不可调换）：
/// 1. 短边定 `BASE_SHORT`，长边按比例；
/// 2. 超长图（`long >= WALL_LONG` 且 `ratio > WALL_RATIO`）⇒ 反转基准：长边定 `BASE_SHORT`；
/// 3. 超大图（`pixels > TRAP_PIXELS`）⇒ 短边取原短边 1/4，仅在小于当前目标时生效；
/// 4. ⛔ 不放大：目标短边 > 原短边 ⇒ 回落到原尺寸；
/// 5. 总像素超 `CAP_PIXELS` ⇒ 按 `sqrt` 等比缩；
/// 6. 偶数对齐、最小 2px；
/// 7. 按最终像素估体积，长图取该值作目标上限。
pub fn calculate_target(width: u32, height: u32) -> Option<CompressionTarget> {
    if width == 0 || height == 0 {
        return None;
    }

    let short_side = width.min(height);
    let long_side = width.max(height);
    let ratio = short_side as f64 / long_side as f64;
    let pixel_count = width as u64 * height as u64;

    // 1. 基准：短边 1440
    let mut target_short = BASE_SHORT;
    let mut target_long = (target_short as f64 / ratio) as u32;

    // 2. 超长图反转基准（长边定 1440）
    if long_side >= WALL_LONG && ratio > WALL_RATIO {
        target_long = BASE_SHORT;
        target_short = (target_long as f64 * ratio) as u32;
    }

    // 3. 超大图陷阱：短边降到原短边 1/4
    if pixel_count > TRAP_PIXELS {
        let trap_short = (short_side as f64 * 0.25) as u32;
        if trap_short < target_short {
            target_short = trap_short;
            target_long = (target_short as f64 / ratio) as u32;
        }
    }

    // 4. ⛔ 不放大
    if target_short > short_side {
        target_short = short_side;
        target_long = long_side;
    }

    // 5. 像素上限
    let current_pixels = target_short as u64 * target_long as u64;
    if current_pixels > CAP_PIXELS {
        let scale = ((CAP_PIXELS as f64 / current_pixels as f64).sqrt() * 1000.0).floor() / 1000.0;
        target_short = (target_short as f64 * scale) as u32;
        target_long = (target_long as f64 * scale) as u32;
    }

    // 6. 偶数对齐 + 最小 2
    target_short = (target_short / 2) * 2;
    target_long = (target_long / 2) * 2;
    target_short = target_short.max(2);
    target_long = target_long.max(2);

    // 还原到「宽×高」——原图竖构图则目标是 (短, 长)
    let (final_w, final_h) = if width < height {
        (target_short, target_long)
    } else {
        (target_long, target_short)
    };

    // 7. 体积估算：像素数 × 经验系数
    let final_pixels = final_w as u64 * final_h as u64;
    let factor = match final_pixels {
        p if p < 500_000 => 0.0005,
        p if p < 1_000_000 => 0.00015,
        p if p < 3_000_000 => 0.00011,
        _ => 0.000025,
    };
    let mut estimated_size = (final_pixels as f64 * factor) as u32;
    estimated_size = estimated_size.max(20);

    // 极窄长图（截图类）保底 250KB，避免质量搜索压得过狠
    if ratio < 0.2 && estimated_size < 400 {
        estimated_size = estimated_size.max(250);
    }

    let is_long_image = ratio <= 0.4;
    let target_size_kb = if is_long_image { Some(estimated_size) } else { None };

    Some(CompressionTarget {
        width: final_w,
        height: final_h,
        estimated_size_kb: estimated_size,
        is_long_image,
        target_size_kb,
    })
}

/// 按质量和目标体积压缩为 JPEG 字节流。
///
/// 复刻原 `JpegCompressor.compress` 的搜索策略：
/// - 给定 `fixed_quality` ⇒ 直接用它；
/// - 无目标体积 ⇒ 用默认质量 60；
/// - 有目标体积 ⇒ 先试 95，若已达标直接返回，否则在 [5, 95] 上二分找
///   「体积仍 ≤ 目标」的最大质量；全都超标则退到质量 5。
///
/// ⚠️ 二分每次都要重新编码（与原实现一致）—— 这是算法的一部分，不是浪费：
///     JPEG 体积对质量不是线性关系，必须实测。
pub fn compress_to_target(img: &DynamicImage, target_kb: Option<u32>, fixed_quality: Option<u8>) -> Result<Vec<u8>, String> {
    if let Some(q) = fixed_quality {
        return encode_jpeg(img, q);
    }

    let Some(target_kb) = target_kb else {
        return encode_jpeg(img, 60);
    };

    const LOW: u8 = 5;
    const HIGH: u8 = 95;

    // 先试最高质量，够小就直接用（避免无谓二分）
    let best_case = encode_jpeg(img, HIGH)?;
    if best_case.len() as f64 / 1024.0 <= target_kb as f64 {
        return Ok(best_case);
    }

    let mut best: Option<Vec<u8>> = None;
    let mut lo = LOW;
    let mut hi = HIGH;

    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let data = encode_jpeg(img, mid)?;
        let size_kb = data.len() as f64 / 1024.0;
        if size_kb <= target_kb as f64 {
            best = Some(data);
            if mid == HIGH {
                break; // 防 u8 溢出（mid+1 越过 255）
            }
            lo = mid + 1;
        } else {
            if mid == 0 {
                break;
            }
            hi = mid - 1;
        }
    }

    match best {
        Some(d) => Ok(d),
        None => encode_jpeg(img, LOW), // 全超标 ⇒ 退最低质量
    }
}

/// 编码为 JPEG（质量 1-100）。
fn encode_jpeg(img: &DynamicImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    let rgb = img.to_rgb8();
    JpegEncoder::new_with_quality(&mut Cursor::new(&mut buf), quality.clamp(1, 100))
        .encode_image(&rgb)
        .map_err(|e| format!("JPEG 编码失败: {e}"))?;
    Ok(buf)
}

/// 解码图片并按 EXIF Orientation 转正。
///
/// 对应原 `ImageLoader.loadFromFile` + `rotateBitmap`。
/// ⚠️ Orientation 必须读**原始字节**，解码后的 `DynamicImage` 不保留 EXIF。
pub fn decode_with_orientation(bytes: &[u8], format: ImageFormat) -> Result<DynamicImage, String> {
    let img = image::load_from_memory_with_format(bytes, format)
        .map_err(|e| format!("图片解码失败: {e}"))?;
    Ok(apply_exif_orientation(img, bytes))
}

/// 按 EXIF Orientation（1-8）把图转正。
fn apply_exif_orientation(img: DynamicImage, raw: &[u8]) -> DynamicImage {
    let Ok(exif) = exif::Reader::new().read_from_container(&mut Cursor::new(raw)) else {
        return img; // 无 EXIF 或解析失败 ⇒ 原样返回（⛔ 不当作错误）
    };
    let Some(field) = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY) else {
        return img;
    };
    let Some(v) = field.value.get_uint(0) else {
        return img;
    };

    match v {
        1 => img,
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

/// 按目标尺寸缩放（Lanczos3，与原 `createScaledBitmap(..., true)` 的双线性相比更清晰）。
///
/// ⛔ 只在「需要缩小」时调用；放大一律由 `calculate_target` 的不放大规则挡在外面。
pub fn resize_to(img: &DynamicImage, w: u32, h: u32) -> DynamicImage {
    if w >= img.width() && h >= img.height() {
        return img.clone();
    }
    img.resize_exact(w, h, image::imageops::FilterType::Lanczos3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_never_upscales() {
        // 小图（400×300）不应被放大到 1440 短边
        let t = calculate_target(400, 300).unwrap();
        assert_eq!((t.width, t.height), (400, 300));
    }

    #[test]
    fn target_caps_pixels() {
        // 8000×8000 = 64MP，超过 CAP_PIXELS ⇒ 结果像素数必须 ≤ 上限
        let t = calculate_target(8000, 8000).unwrap();
        assert!((t.width as u64 * t.height as u64) <= CAP_PIXELS);
    }

    #[test]
    fn target_keeps_orientation() {
        // 竖图（540×960）目标仍是竖构图
        let t = calculate_target(540, 960).unwrap();
        assert!(t.height >= t.width);
        // 横图（1920×1080）目标仍是横构图
        let t2 = calculate_target(1920, 1080).unwrap();
        assert!(t2.width >= t2.height);
    }

    #[test]
    fn target_dims_are_even() {
        let t = calculate_target(1234, 5678).unwrap();
        assert_eq!(t.width % 2, 0);
        assert_eq!(t.height % 2, 0);
    }

    #[test]
    fn long_image_gets_target_size() {
        // 极窄长图 ratio < 0.4 ⇒ 应给出 target_size_kb
        let t = calculate_target(600, 6000).unwrap();
        assert!(t.is_long_image);
        assert!(t.target_size_kb.is_some());
    }
}
