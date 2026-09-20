//! libvipsネイティブ形式(.v)のデコード
//! 64バイトヘッダ+無圧縮ピクセル列(バンドインターリーブ)

use image::DynamicImage;

// マジック4バイトはMSBファースト書き込み、後続フィールドは書き込み元ネイティブ順
const MAGIC_INTEL: [u8; 4] = [0xb6, 0xa6, 0xf2, 0x08];
const MAGIC_SPARC: [u8; 4] = [0x08, 0xf2, 0xa6, 0xb6];
const HEADER_SIZE: usize = 64;

pub(crate) fn is_vips(head: &[u8]) -> bool {
	head.len() >= 4 && (head[0..4] == MAGIC_INTEL || head[0..4] == MAGIC_SPARC)
}

pub(crate) struct VipsHeader {
	pub width: u32,
	pub height: u32,
	pub bands: u32,
	band_fmt: u32,
	coding: u32,
	big_endian: bool,
}

pub(crate) fn parse_header(data: &[u8]) -> Result<VipsHeader, String> {
	if data.len() < HEADER_SIZE {
		return Err("TruncatedHeader".to_owned());
	}
	let big_endian = if data[0..4] == MAGIC_INTEL {
		false
	} else if data[0..4] == MAGIC_SPARC {
		true
	} else {
		return Err("BadMagic".to_owned());
	};
	let read_u32 = |offset: usize| {
		let b = [
			data[offset],
			data[offset + 1],
			data[offset + 2],
			data[offset + 3],
		];
		if big_endian {
			u32::from_be_bytes(b)
		} else {
			u32::from_le_bytes(b)
		}
	};
	Ok(VipsHeader {
		width: read_u32(4),
		height: read_u32(8),
		bands: read_u32(12),
		// オフセット16は旧Bbits
		band_fmt: read_u32(20),
		coding: read_u32(24),
		big_endian,
	})
}

pub(crate) fn decode(data: &[u8], header: &VipsHeader) -> Result<DynamicImage, String> {
	// VIPS_CODING_NONEのみ対応
	if header.coding != 0 {
		return Err(format!("UnsupportedCoding {}", header.coding));
	}
	if !(1..=4).contains(&header.bands) {
		return Err(format!("UnsupportedBands {}", header.bands));
	}
	let pixels = (header.width as u64).saturating_mul(header.height as u64);
	let samples = pixels.saturating_mul(header.bands as u64);
	// BandFmt: 0=uchar 2=ushort
	let buf = match header.band_fmt {
		0 => {
			let len = usize::try_from(samples).map_err(|_| "TooLarge".to_owned())?;
			let end = HEADER_SIZE
				.checked_add(len)
				.ok_or_else(|| "TooLarge".to_owned())?;
			if data.len() < end {
				return Err("TruncatedPixelData".to_owned());
			}
			data[HEADER_SIZE..end].to_vec()
		}
		2 => {
			let bytes = samples.saturating_mul(2);
			let len = usize::try_from(bytes).map_err(|_| "TooLarge".to_owned())?;
			let end = HEADER_SIZE
				.checked_add(len)
				.ok_or_else(|| "TooLarge".to_owned())?;
			if data.len() < end {
				return Err("TruncatedPixelData".to_owned());
			}
			data[HEADER_SIZE..end]
				.chunks_exact(2)
				.map(|b| {
					let v = if header.big_endian {
						u16::from_be_bytes([b[0], b[1]])
					} else {
						u16::from_le_bytes([b[0], b[1]])
					};
					(v >> 8) as u8
				})
				.collect()
		}
		fmt => return Err(format!("UnsupportedBandFmt {}", fmt)),
	};
	let img = match header.bands {
		1 => image::GrayImage::from_raw(header.width, header.height, buf)
			.map(DynamicImage::ImageLuma8),
		2 => image::GrayAlphaImage::from_raw(header.width, header.height, buf)
			.map(DynamicImage::ImageLumaA8),
		3 => {
			image::RgbImage::from_raw(header.width, header.height, buf).map(DynamicImage::ImageRgb8)
		}
		4 => image::RgbaImage::from_raw(header.width, header.height, buf)
			.map(DynamicImage::ImageRgba8),
		_ => None,
	};
	img.ok_or_else(|| "BufferSizeMismatch".to_owned())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn build_vips(
		width: u32,
		height: u32,
		bands: u32,
		band_fmt: u32,
		pixel_bytes: &[u8],
	) -> Vec<u8> {
		let mut v = vec![0u8; HEADER_SIZE];
		v[0..4].copy_from_slice(&MAGIC_INTEL);
		v[4..8].copy_from_slice(&width.to_le_bytes());
		v[8..12].copy_from_slice(&height.to_le_bytes());
		v[12..16].copy_from_slice(&bands.to_le_bytes());
		v[20..24].copy_from_slice(&band_fmt.to_le_bytes());
		v.extend_from_slice(pixel_bytes);
		v
	}

	#[test]
	fn non_vips_data_is_not_detected() {
		assert!(!is_vips(b"not vips"));
	}
	#[test]
	fn uchar_rgba_decodes() {
		let data = build_vips(2, 1, 4, 0, &[1, 2, 3, 4, 5, 6, 7, 8]);
		assert!(is_vips(&data));
		let header = parse_header(&data).unwrap();
		assert_eq!((header.width, header.height, header.bands), (2, 1, 4));
		let img = decode(&data, &header).unwrap();
		assert_eq!(img.as_rgba8().unwrap().get_pixel(1, 0).0, [5, 6, 7, 8]);
	}
	#[test]
	fn ushort_gray_decodes_scaled() {
		let data = build_vips(1, 1, 1, 2, &0xab00u16.to_le_bytes());
		let header = parse_header(&data).unwrap();
		let img = decode(&data, &header).unwrap();
		assert_eq!(img.as_luma8().unwrap().get_pixel(0, 0).0, [0xab]);
	}
	#[test]
	fn truncated_pixel_data_is_err() {
		let data = build_vips(4, 4, 3, 0, &[0u8; 4]);
		let header = parse_header(&data).unwrap();
		assert!(decode(&data, &header).unwrap_err().contains("Truncated"));
	}
	#[test]
	fn labq_coding_is_err() {
		let mut data = build_vips(1, 1, 3, 0, &[0u8; 3]);
		data[24..28].copy_from_slice(&2u32.to_le_bytes());
		let header = parse_header(&data).unwrap();
		assert!(decode(&data, &header)
			.unwrap_err()
			.contains("UnsupportedCoding"));
	}
}
