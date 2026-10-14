//! Currently only support utf8 mode

#![no_std]

extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

#[cfg(feature = "std")]
use std::io::{self, Write};

const UTF8_START: &[u8] = &[0x1B, 0x25, 0x47];
const UTF8_END: &[u8] = &[0x1B, 0x25, 0x40];

/// Wrapper for reduce allocation
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct CText<'s> {
    utf8: &'s str,
}

impl<'s> fmt::Debug for CText<'s> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.utf8)
    }
}

impl<'s> fmt::Display for CText<'s> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.utf8)
    }
}

impl<'s> CText<'s> {
    pub const fn new(utf8: &'s str) -> Self {
        Self { utf8 }
    }

    pub const fn len(self) -> usize {
        self.utf8.len() + UTF8_START.len() + UTF8_END.len()
    }

    #[cfg(feature = "std")]
    pub fn write(self, mut out: impl Write) -> io::Result<usize> {
        let mut writed = 0;
        writed += out.write(UTF8_START)?;
        writed += out.write(self.utf8.as_bytes())?;
        writed += out.write(UTF8_END)?;
        Ok(writed)
    }
}

/// Encoding utf8 to COMPOUND_TEXT with utf8 escape
pub fn utf8_to_compound_text(text: &str) -> Vec<u8> {
    let mut ret = Vec::with_capacity(text.len() + 6);
    ret.extend_from_slice(UTF8_START);
    ret.extend_from_slice(text.as_bytes());
    ret.extend_from_slice(UTF8_END);
    ret
}

#[derive(Debug, Clone)]
pub enum DecodeError {
    InvalidEncoding,
    UnsupportedEncoding,
    Utf8Error(alloc::string::FromUtf8Error),
}

impl From<alloc::string::FromUtf8Error> for DecodeError {
    fn from(err: alloc::string::FromUtf8Error) -> Self {
        DecodeError::Utf8Error(err)
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEncoding => write!(f, "Invalid compound text"),
            Self::UnsupportedEncoding => write!(f, "This encoding is not supported yet"),
            Self::Utf8Error(e) => write!(f, "Not a valid utf8 {}", e),
        }
    }
}

macro_rules! decode {
    ($decoder:expr, $out:expr, $bytes:expr, $last:expr) => {
        loop {
            let (ret, _, _) = $decoder.decode_to_string($bytes, $out, $last);

            match ret {
                encoding_rs::CoderResult::InputEmpty => break,
                encoding_rs::CoderResult::OutputFull => {
                    $out.reserve(
                        $decoder
                            .max_utf8_buffer_length($bytes.len())
                            .unwrap_or_default(),
                    );
                }
            }
        }
    };
}

/// [kwikpaste patch 0001] 一个转义段的字符集。上游只支持 UTF-8 和 ISO-2022-JP，
/// 中文输入法（fcitx 等）提交 GB2312 段时直接报 UnsupportedEncoding；这里补齐 GB2312 与
/// KS X 1001，并把整段解码改成逐段扫描（转义段外的裸字节保留原样的 UTF-8 处理）。
#[derive(Clone, Copy, PartialEq)]
enum SegmentCharset {
    /// `ESC % G`：UTF-8（结束标记 `ESC % @` 自成一段，无需剥离）。
    Utf8,
    /// `ESC $ ( B`：JIS X 0208，交给 ISO-2022-JP 解码器处理转义序列本身。
    Iso2022Jp,
    /// `ESC $ ( A`：GB2312（GL 双字节，加 0x80 即 EUC-CN，落在 GB18030 内）。
    Gb2312,
    /// `ESC $ ( C`：KS X 1001（GL 双字节，加 0x80 即 EUC-KR）。
    KsX1001,
    /// `ESC ( B`、`ESC % @` 等回到 ASCII 的段。
    Ascii,
}

/// 解码 GL 编码（GB2312 / KS X 1001）的数据段：两字节一组，各加 0x80 转成
/// EUC 形式后再按对应编码解码；结尾落单的字节忽略。
fn decode_gl_pairs(
    data: &[u8],
    encoding: &'static encoding_rs::Encoding,
    out: &mut String,
) -> Result<(), DecodeError> {
    let mut euc = Vec::with_capacity(data.len());
    for pair in data.chunks(2) {
        if let [first, second] = pair {
            euc.extend_from_slice(&[first | 0x80, second | 0x80]);
        }
    }
    let mut decoder = encoding.new_decoder_without_bom_handling();
    decode!(decoder, out, &euc, true);
    Ok(())
}

pub fn compound_text_to_utf8(bytes: &[u8]) -> Result<String, DecodeError> {
    // COMPOUND_TEXT 由若干「转义序列 + 数据」段组成，数据里不再出现 ESC。
    // 逐段按各自的字符集解码后拼接；段外的裸字节按 UTF-8 处理（与原实现一致）。
    let mut out = String::new();
    let mut charset = SegmentCharset::Ascii;
    let mut rest = bytes;

    loop {
        let Some(escape) = rest.iter().position(|byte| *byte == 0x1B) else {
            break;
        };
        let data = &rest[..escape];
        match charset {
            SegmentCharset::Utf8 | SegmentCharset::Ascii => {
                out.push_str(&String::from_utf8(data.to_vec())?);
            }
            SegmentCharset::Iso2022Jp => {
                let mut decoder = encoding_rs::ISO_2022_JP.new_decoder_without_bom_handling();
                decode!(decoder, &mut out, &[0x1B, 0x24, 0x42], false);
                if !data.is_empty() {
                    decode!(decoder, &mut out, data, true);
                }
            }
            SegmentCharset::Gb2312 => decode_gl_pairs(data, &encoding_rs::GB18030, &mut out)?,
            SegmentCharset::KsX1001 => decode_gl_pairs(data, &encoding_rs::EUC_KR, &mut out)?,
        }

        let after_escape = &rest[escape..];
        // 转义序列：`ESC % X`、`ESC ( X` 为 3 字节，`ESC $ ( X` 为 4 字节。
        let (sequence, next) = match after_escape.get(1) {
            Some(0x24) if after_escape.get(2) == Some(&0x28) => match after_escape.get(3) {
                Some(_) => (&after_escape[..4], &after_escape[4..]),
                None => return Err(DecodeError::InvalidEncoding),
            },
            Some(0x25 | 0x28) => match after_escape.get(2) {
                Some(_) => (&after_escape[..3], &after_escape[3..]),
                None => return Err(DecodeError::InvalidEncoding),
            },
            _ => return Err(DecodeError::InvalidEncoding),
        };
        charset = match sequence {
            [0x1B, 0x25, 0x47] => SegmentCharset::Utf8,
            [0x1B, 0x24, 0x28, 0x42] => SegmentCharset::Iso2022Jp,
            [0x1B, 0x24, 0x28, 0x41] => SegmentCharset::Gb2312,
            [0x1B, 0x24, 0x28, 0x43] => SegmentCharset::KsX1001,
            // 其余（ASCII 恢复、G1/G2 指定等无数据指定）按 ASCII 段处理。
            _ => SegmentCharset::Ascii,
        };
        rest = next;
    }

    match charset {
        SegmentCharset::Utf8 | SegmentCharset::Ascii => {
            out.push_str(&String::from_utf8(rest.to_vec())?);
        }
        SegmentCharset::Iso2022Jp => {
            let mut decoder = encoding_rs::ISO_2022_JP.new_decoder_without_bom_handling();
            decode!(decoder, &mut out, &[0x1B, 0x24, 0x42], false);
            if !rest.is_empty() {
                decode!(decoder, &mut out, rest, true);
            }
        }
        SegmentCharset::Gb2312 => decode_gl_pairs(rest, &encoding_rs::GB18030, &mut out)?,
        SegmentCharset::KsX1001 => decode_gl_pairs(rest, &encoding_rs::EUC_KR, &mut out)?,
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn korean() {
        const UTF8: &str = "가나다";
        const COMP: &[u8] = &[
            27, 37, 71, 234, 176, 128, 235, 130, 152, 235, 139, 164, 27, 37, 64,
        ];
        assert_eq!(crate::utf8_to_compound_text(UTF8), COMP);
        assert_eq!(crate::compound_text_to_utf8(COMP).unwrap(), UTF8);
    }

    #[test]
    fn iso_2011_jp() {
        const UTF8: &str = "東京";
        const COMP: &[u8] = &[27, 36, 40, 66, 69, 108, 53, 126];
        assert_eq!(crate::compound_text_to_utf8(COMP).unwrap(), UTF8);
    }

    #[test]
    fn gb2312() {
        // 「中」的 EUC-CN 编码是 0xD6D0，GL 形式各减 0x80。
        const COMP: &[u8] = &[27, 36, 40, 65, 0x56, 0x50];
        assert_eq!(crate::compound_text_to_utf8(COMP).unwrap(), "中");
    }

    #[test]
    fn ksx1001() {
        // 「가」的 EUC-KR 编码是 0xB0A1，GL 形式各减 0x80。
        const COMP: &[u8] = &[27, 36, 40, 67, 0x30, 0x21];
        assert_eq!(crate::compound_text_to_utf8(COMP).unwrap(), "가");
    }

    #[test]
    fn mixed_segments() {
        // 「A」+ UTF-8 段「中」+ 结束标记 + 「B」。
        const COMP: &[u8] = &[65, 27, 37, 71, 228, 184, 173, 27, 37, 64, 66];
        assert_eq!(crate::compound_text_to_utf8(COMP).unwrap(), "A中B");
    }
}
