//! 复制成功提示音：内嵌 16-bit PCM WAV，按平方曲线调整音量。
//!
//! 播放在短命工作线程里完成：Windows 的同步 `PlaySoundW` 持有内存直到播放结束，
//! macOS 的 `NSSound` 则持有到播放结束。调用方（包括偏好页试听）不等待音频设备。

const COPY_SOUND: &[u8] = include_bytes!("../assets/sounds/copy.wav");

/// 百分比转换为感知音量增益；手改配置超出上限时按满音量处理。
fn perceptual_gain(percent: u8) -> f32 {
    let normalized = f32::from(percent.min(100)) / 100.0;
    normalized * normalized
}

/// 查找完整 RIFF/WAVE 的 PCM 数据，允许附加块及奇数长度块的填充字节。
#[cfg(any(target_os = "windows", target_os = "linux", test))]
fn pcm_data_range(bytes: &[u8]) -> Result<std::ops::Range<usize>, &'static str> {
    fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
        Some(u16::from_le_bytes(
            bytes.get(offset..offset + 2)?.try_into().ok()?,
        ))
    }
    fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
        Some(u32::from_le_bytes(
            bytes.get(offset..offset + 4)?.try_into().ok()?,
        ))
    }

    if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err("not a RIFF/WAVE file");
    }
    let end = (u32_at(bytes, 4).ok_or("missing RIFF size")? as usize)
        .checked_add(8)
        .filter(|end| *end >= 12 && *end <= bytes.len())
        .ok_or("truncated RIFF file")?;
    let mut offset: usize = 12;
    let mut block_align = None;
    let mut data = None;
    while offset < end {
        let header_end = offset
            .checked_add(8)
            .filter(|next| *next <= end)
            .ok_or("truncated chunk header")?;
        let id = bytes.get(offset..offset + 4).ok_or("missing chunk id")?;
        let length = u32_at(bytes, offset + 4).ok_or("missing chunk size")? as usize;
        let chunk_end = header_end
            .checked_add(length)
            .filter(|next| *next <= end)
            .ok_or("truncated chunk data")?;
        let chunk = bytes
            .get(header_end..chunk_end)
            .ok_or("missing chunk data")?;
        match id {
            b"fmt " => {
                if block_align.is_some()
                    || length < 16
                    || u16_at(chunk, 0) != Some(1)
                    || u16_at(chunk, 14) != Some(16)
                {
                    return Err("expected one 16-bit PCM format chunk");
                }
                let channels = u16_at(chunk, 2).ok_or("missing channels")?;
                let align = u16_at(chunk, 12).ok_or("missing block alignment")?;
                if channels == 0
                    || channels.checked_mul(2) != Some(align)
                    || u32_at(chunk, 4).unwrap_or(0) == 0
                {
                    return Err("invalid PCM format");
                }
                block_align = Some(usize::from(align));
            }
            b"data" => {
                if data.is_some() {
                    return Err("multiple PCM data chunks");
                }
                data = Some(header_end..chunk_end);
            }
            _ => {}
        }
        offset = chunk_end
            .checked_add(length % 2)
            .filter(|next| *next <= end)
            .ok_or("missing chunk padding")?;
    }
    let align = block_align.ok_or("missing PCM format chunk")?;
    let data = data.ok_or("missing PCM data chunk")?;
    if data.is_empty() || !data.len().is_multiple_of(align) {
        return Err("incomplete PCM samples");
    }
    Ok(data)
}

/// 只缩放 PCM 样本，保留所有头和其它块；满音量直接借用原始字节，不分配副本。
#[cfg(any(target_os = "windows", target_os = "linux", test))]
fn scale_pcm_wav(bytes: &[u8], percent: u8) -> Result<std::borrow::Cow<'_, [u8]>, &'static str> {
    let data = pcm_data_range(bytes)?;
    if percent >= 100 {
        return Ok(std::borrow::Cow::Borrowed(bytes));
    }
    let gain = perceptual_gain(percent);
    let mut scaled = bytes.to_vec();
    let samples = scaled.get_mut(data).ok_or("missing PCM data")?;
    for sample in samples.chunks_exact_mut(2) {
        let pair: &mut [u8; 2] = sample.try_into().map_err(|_| "incomplete PCM sample")?;
        let value = (f32::from(i16::from_le_bytes(*pair)) * gain)
            .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16;
        *pair = value.to_le_bytes();
    }
    Ok(std::borrow::Cow::Owned(scaled))
}

/// 异步播放一次复制提示音；静音不创建线程，失败只记日志。
#[cfg(target_os = "windows")]
pub fn play_copy(volume_percent: u8) {
    use windows::Win32::Media::Audio::{PlaySoundW, SND_MEMORY, SND_NODEFAULT, SND_SYNC};
    use windows::core::PCWSTR;

    if volume_percent == 0 {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("copy-sound".to_owned())
        .spawn(move || {
            let bytes = match scale_pcm_wav(COPY_SOUND, volume_percent) {
                Ok(bytes) => bytes,
                Err(err) => {
                    log::warn!("the copy sound data is invalid: {err}");
                    return;
                }
            };
            // 同步调用返回前，工作线程一直持有 bytes。不能加 SND_ASYNC，否则副本会提前释放。
            // 不加 SND_NOSTOP：新的播放替换同进程里的上一段声音，而不是排队或叠加。
            let played = unsafe {
                PlaySoundW(
                    PCWSTR(bytes.as_ptr().cast()),
                    None,
                    SND_MEMORY | SND_SYNC | SND_NODEFAULT,
                )
            };
            if !played.as_bool() {
                log::warn!("the copy sound could not be played");
            }
        });
    if let Err(err) = spawned {
        log::warn!("the copy sound thread could not start: {err}");
    }
}

/// 异步播放一次复制提示音；静音不创建线程，失败只记日志。
#[cfg(target_os = "macos")]
pub fn play_copy(volume_percent: u8) {
    if volume_percent == 0 {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("copy-sound".to_owned())
        .spawn(move || {
            if let Err(err) = mac::play_until_done(COPY_SOUND, perceptual_gain(volume_percent)) {
                log::warn!("the copy sound could not be played: {err}");
            }
        });
    if let Err(err) = spawned {
        log::warn!("the copy sound thread could not start: {err}");
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::time::{Duration, Instant};

    use objc2::AllocAnyThread as _;
    use objc2::rc::{Retained, autoreleasepool};
    use objc2_app_kit::NSSound;
    use objc2_foundation::NSData;

    /// 播放超过这个时长就不再等（提示音本身不到半秒）。
    const MAX_WAIT: Duration = Duration::from_secs(5);

    pub(super) fn decode(bytes: &[u8]) -> Option<Retained<NSSound>> {
        let data = NSData::with_bytes(bytes);
        NSSound::initWithData(NSSound::alloc(), &data)
    }

    /// 解码、设置增益后播放，阻塞到放完。
    pub(super) fn play_until_done(bytes: &[u8], gain: f32) -> Result<(), &'static str> {
        autoreleasepool(|_| {
            let sound = decode(bytes).ok_or("the sound data could not be decoded")?;
            sound.setVolume(gain);
            if !sound.play() {
                return Err("NSSound refused to play");
            }
            let deadline = Instant::now() + MAX_WAIT;
            while sound.isPlaying() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            if sound.isPlaying() {
                sound.stop();
            }
            Ok(())
        })
    }
}

/// 将提示音交给桌面音频工具；独立临时文件避免并发播放互相覆盖。
#[cfg(target_os = "linux")]
pub fn play_copy(volume_percent: u8) {
    if volume_percent == 0 {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("copy-sound".into())
        .spawn(move || {
            use std::io::Write;
            let Ok(bytes) = scale_pcm_wav(COPY_SOUND, volume_percent) else {
                return;
            };
            let Ok(mut file) = tempfile::Builder::new().suffix(".wav").tempfile() else {
                return;
            };
            if file.write_all(&bytes).is_err() {
                return;
            }
            for player in ["paplay", "aplay"] {
                if std::process::Command::new(player)
                    .arg(file.path())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success())
                {
                    return;
                }
            }
            log::debug!("copy sound requires paplay or aplay on Linux");
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    /// 夹具包含一个奇数长度的附加块，避免缩放器依赖固定 WAV 偏移。
    fn wave(samples: &[i16]) -> Vec<u8> {
        let mut bytes = b"RIFF\0\0\0\0WAVEJUNK\x03\0\0\0abc\0fmt \x10\0\0\0\x01\0\x01\0\x44\xac\0\0\x88\x58\x01\0\x02\0\x10\0data".to_vec();
        bytes.extend_from_slice(&(samples.len() as u32 * 2).to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes.extend_from_slice(b"JUNK\x02\0\0\0xy");
        let len = bytes.len() as u32 - 8;
        bytes[4..8].copy_from_slice(&len.to_le_bytes());
        bytes
    }

    #[test]
    fn perceptual_curve_is_quadratic_and_clamped() {
        assert_eq!(perceptual_gain(0), 0.0);
        assert_eq!(perceptual_gain(100), 1.0);
        assert_eq!(perceptual_gain(50), 0.25);
        assert_eq!(perceptual_gain(255), 1.0);
    }

    #[test]
    fn scales_signed_samples_and_preserves_non_audio_bytes() {
        let bytes = wave(&[0, 10000, -10000, i16::MIN, i16::MAX]);
        let data = pcm_data_range(&bytes).unwrap();
        let scaled = scale_pcm_wav(&bytes, 50).unwrap();
        let samples: Vec<i16> = scaled[data.clone()]
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes(pair.try_into().unwrap()))
            .collect();
        assert_eq!(samples, [0, 2500, -2500, -8192, 8191]);
        assert_eq!(&scaled[..data.start], &bytes[..data.start]);
        assert_eq!(&scaled[data.end..], &bytes[data.end..]);
        let silence = scale_pcm_wav(&bytes, 0).unwrap();
        assert!(silence[data].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn full_volume_borrows_original_including_embedded_wave() {
        for percent in [100, 255] {
            let scaled = scale_pcm_wav(COPY_SOUND, percent).unwrap();
            assert!(matches!(scaled, Cow::Borrowed(_)));
            assert_eq!(scaled.as_ptr(), COPY_SOUND.as_ptr());
            assert_eq!(scaled.as_ref(), COPY_SOUND);
        }
    }

    #[test]
    fn stereo_samples_are_scaled_and_missing_or_duplicate_chunks_are_rejected() {
        let mut bytes = wave(&[10000, -10000]);
        bytes[34..36].copy_from_slice(&2_u16.to_le_bytes());
        bytes[44..46].copy_from_slice(&4_u16.to_le_bytes());
        let data = pcm_data_range(&bytes).unwrap();
        assert_eq!(
            &scale_pcm_wav(&bytes, 50).unwrap()[data],
            &[0xc4, 0x09, 0x3c, 0xf6]
        );
        let mut no_format = bytes.clone();
        no_format[24..28].copy_from_slice(b"JUNK");
        assert!(scale_pcm_wav(&no_format, 50).is_err());
        let mut no_data = bytes.clone();
        no_data[48..52].copy_from_slice(b"JUNK");
        assert!(scale_pcm_wav(&no_data, 50).is_err());
        let mut wrong_bits = bytes.clone();
        wrong_bits[46..48].copy_from_slice(&8_u16.to_le_bytes());
        assert!(scale_pcm_wav(&wrong_bits, 100).is_err());
        bytes.extend_from_slice(b"data\x04\0\0\0\0\0\0\0");
        let len = bytes.len() as u32 - 8;
        bytes[4..8].copy_from_slice(&len.to_le_bytes());
        assert!(scale_pcm_wav(&bytes, 50).is_err());
    }

    #[test]
    fn malformed_and_truncated_waves_are_rejected() {
        let bytes = wave(&[100, -100]);
        for len in 0..bytes.len() {
            assert!(scale_pcm_wav(&bytes[..len], 50).is_err(), "length {len}");
        }
        let mut bad_format = bytes.clone();
        bad_format[32] = 3;
        assert!(scale_pcm_wav(&bad_format, 100).is_err());
        let mut odd_samples = bytes.clone();
        odd_samples[52..56].copy_from_slice(&3_u32.to_le_bytes());
        assert!(scale_pcm_wav(&odd_samples, 50).is_err());
        let mut overflow = bytes;
        overflow[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(scale_pcm_wav(&overflow, 50).is_err());
        assert!(scale_pcm_wav(b"not a wave", 100).is_err());
    }

    /// 只解码不播放：CI 的 macOS 机器上验证 NSSound 认得 WAV 和音量 setter 的签名。
    #[cfg(target_os = "macos")]
    #[test]
    fn nssound_decodes_and_sets_volume_without_playing() {
        let sound = mac::decode(COPY_SOUND).expect("NSSound decodes copy.wav");
        sound.setVolume(perceptual_gain(50));
        assert_eq!(sound.volume(), 0.25);
        assert!(sound.duration() > 0.0);
    }
}
