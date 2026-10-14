//! 跨平台从「任意文件路径」抽取图标，落地为 PNG 字节。
//! macOS 走 NSWorkspace.iconForFile；Windows 的 Shell 路径由 `file_icon_provider` 封装，
//! 配置宿主 helper 后在独立子进程中执行。

use std::path::Path;

#[cfg(target_os = "windows")]
mod helper;
#[cfg(target_os = "windows")]
pub use helper::{run_helper, set_helper_exe};

use image::{codecs::png::PngEncoder, ColorType, ImageEncoder};

/// App 图标 / 文件类型图标统一默认像素尺寸。
/// 256 覆盖 Retina 下 ~96–128pt 高清显示（列表卡片 + 拖拽预览同源），
/// 单张 PNG ~20–60KB；按文件类型缓存，整库通常几十张，总占用可忽略。
const DEFAULT_ICON_PIXEL_SIZE: u32 = 256;

/// 目录的 icon 缓存 key。`<>` 是 macOS/Windows 文件名非法字符，不会与真实路径撞 key。
pub const DIR_CACHE_KEY: &str = "<dir>";

/// 生成文件 icon 的缓存 key，用于 `file_type_icons` 表查询。
///
/// 规则：
/// - 目录：[`DIR_CACHE_KEY`]
/// - `.app` / `.exe`：完整路径（每个 bundle 单独缓存）
/// - 有扩展名：`.pdf`、`.txt`（小写、带点）
/// - 无扩展名：文件名本身（`Makefile`、`README` 等由 OS 决定是否有专属 icon）
pub fn get_icon_cache_key(path: &Path) -> String {
    if path.is_dir() {
        return DIR_CACHE_KEY.to_string();
    }

    let extname = path.extension().and_then(|s| s.to_str()).unwrap_or("");

    // .app / .exe 用完整路径
    if (cfg!(target_os = "macos") && extname == "app")
        || (cfg!(target_os = "windows") && extname == "exe")
    {
        return path.to_string_lossy().to_string();
    }

    // 无扩展名：用文件名本身（OS 会按需给 Makefile 等返回专属 icon，其余返回通用文件 icon）
    if extname.is_empty() {
        return path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
    }

    // 普通扩展名：小写、带点
    format!(".{}", extname.to_lowercase())
}

/// 抽取指定路径的图标 PNG 字节。`size` 为 `None` 时用内置默认值。
/// 失败一律返回 `None`，由调用方决定回退。
///
/// Windows 的 exe 先直接读图标资源：经 Shell 取图标会把 Shell 命名空间和它的图标缓存
/// 留在进程里（实测常驻约 4.5 MB），直接读只多约 0.4 MB。读不到（没有图标资源等）再走
/// helper；未配置 helper 的 core 使用者仍在当前进程内走 Shell。
pub fn icon_png(path: &Path, size: Option<u32>) -> Option<Vec<u8>> {
    let size = size.unwrap_or(DEFAULT_ICON_PIXEL_SIZE);
    #[cfg(target_os = "windows")]
    let direct = exe_icon::extract(path, size);
    #[cfg(not(target_os = "windows"))]
    let direct = None;
    let icon = match direct {
        Some(icon) => icon,
        None => {
            #[cfg(target_os = "windows")]
            if helper::HELPER_EXE.get().is_some() {
                return helper::helper_icon_png(path, size);
            }
            shell_icon(path, size)?
        }
    };
    encode_icon(icon, path, size)
}

#[cfg(not(target_os = "linux"))]
fn shell_icon(path: &Path, size: u32) -> Option<file_icon_provider::Icon> {
    match file_icon_provider::get_file_icon(path, size as u16) {
        Ok(icon) => Some(icon),
        Err(err) => {
            log::warn!(
                "icon_png: get_file_icon failed for {}: {err:?}",
                path.display()
            );
            None
        }
    }
}

fn encode_icon(icon: file_icon_provider::Icon, path: &Path, size: u32) -> Option<Vec<u8>> {
    // write_image 在缓冲长度与宽高对不上时会 panic；系统给的图标按理不会，这里先核对。
    let expected = u64::from(icon.width) * u64::from(icon.height) * 4;
    if icon.pixels.len() as u64 != expected {
        log::warn!(
            "icon_png: {}x{} icon has {} bytes for {}",
            icon.width,
            icon.height,
            icon.pixels.len(),
            path.display()
        );
        return None;
    }
    let mut out = Vec::with_capacity(
        (u64::from(size) * u64::from(size) * 4 / 2).min(16 * 1024 * 1024) as usize,
    );
    let encoder = PngEncoder::new(&mut out);
    if let Err(err) = encoder.write_image(
        &icon.pixels,
        icon.width,
        icon.height,
        ColorType::Rgba8.into(),
    ) {
        log::warn!("icon_png: encode PNG failed for {}: {err}", path.display());
        return None;
    }
    Some(out)
}

/// 直接从 exe 的图标资源取 RGBA（第 0 个图标组，系统按请求尺寸挑帧或缩放），不经 Shell。
#[cfg(target_os = "windows")]
mod exe_icon {
    use std::os::windows::ffi::OsStrExt as _;
    use std::path::Path;

    use file_icon_provider::Icon;
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DestroyIcon, GetIconInfo, PrivateExtractIconsW, HICON, ICONINFO,
    };

    pub(super) fn extract(path: &Path, size: u32) -> Option<Icon> {
        let is_exe = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"));
        if !is_exe {
            return None;
        }
        let side = i32::try_from(size).ok()?;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut icon: HICON = std::ptr::null_mut();
        let mut id = 0u32;
        let count =
            unsafe { PrivateExtractIconsW(wide.as_ptr(), 0, side, side, &mut icon, &mut id, 1, 0) };
        // 出错时返回 0xFFFFFFFF；文件里没有图标时返回 0。
        if count != 1 || icon.is_null() {
            return None;
        }
        let _icon = Guard(icon, |icon| unsafe {
            DestroyIcon(icon);
        });

        let mut info: ICONINFO = unsafe { std::mem::zeroed() };
        if unsafe { GetIconInfo(icon, &mut info) } == 0 {
            return None;
        }
        let _mask = Guard(info.hbmMask, delete_bitmap);
        let _color = Guard(info.hbmColor, delete_bitmap);
        // 单色图标没有彩色位图，交给 Shell。
        if info.hbmColor.is_null() {
            return None;
        }

        let (width, height, mut pixels) = bgra(info.hbmColor)?;
        // 老式图标没有 alpha 通道，透明区域在掩码里（掩码白色 = 透明）。
        if pixels.chunks_exact(4).all(|pixel| pixel[3] == 0) {
            let (mask_width, mask_height, mask) = bgra(info.hbmMask)?;
            if (mask_width, mask_height) != (width, height) {
                return None;
            }
            for (pixel, mask) in pixels.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
                pixel[3] = if mask[0] == 0 { 255 } else { 0 };
            }
        }
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }

        Some(Icon {
            width,
            height,
            pixels,
        })
    }

    /// 位图按 32 位自上而下读出 BGRA。
    fn bgra(bitmap: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
        let mut header: BITMAP = unsafe { std::mem::zeroed() };
        let read =
            unsafe { GetObjectW(bitmap, size_of::<BITMAP>() as i32, (&raw mut header).cast()) };
        if read == 0 || header.bmWidth <= 0 || header.bmHeight <= 0 {
            return None;
        }
        let (width, height) = (header.bmWidth as u32, header.bmHeight as u32);
        let mut pixels = vec![0u8; width as usize * height as usize * 4];

        let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = header.bmWidth;
        info.bmiHeader.biHeight = -header.bmHeight;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;

        let dc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
        if dc.is_null() {
            return None;
        }
        let lines = unsafe {
            GetDIBits(
                dc,
                bitmap,
                0,
                height,
                pixels.as_mut_ptr().cast(),
                &mut info,
                DIB_RGB_COLORS,
            )
        };
        unsafe { DeleteDC(dc) };

        (lines == height as i32).then_some((width, height, pixels))
    }

    fn delete_bitmap(bitmap: HBITMAP) {
        if !bitmap.is_null() {
            unsafe { DeleteObject(bitmap) };
        }
    }

    /// 离开作用域时释放 GDI / 图标句柄。
    struct Guard<T: Copy>(T, fn(T));

    impl<T: Copy> Drop for Guard<T> {
        fn drop(&mut self) {
            (self.1)(self.0);
        }
    }
}

#[cfg(all(test, target_os = "windows"))]
mod windows_tests {
    use super::*;

    fn decoded_size(png: &[u8]) -> (u32, u32) {
        let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .expect("icon_png returns a valid PNG");
        (image.width(), image.height())
    }

    #[test]
    fn icon_png_extracts_exe_folder_and_extension_icons() {
        let system = std::env::var_os("SystemRoot").expect("SystemRoot is set");
        let notepad = Path::new(&system).join("System32").join("notepad.exe");
        let temp = tempfile::tempdir().unwrap();
        let mut paths = vec![notepad, temp.path().to_path_buf()];
        for extension in [
            "txt",
            "pdf",
            "docx",
            "xlsx",
            "png",
            "zip",
            "mp3",
            "kwikpaste-unknown",
        ] {
            let path = temp.path().join(format!("sample.{extension}"));
            std::fs::write(&path, b"sample").unwrap();
            paths.push(path);
        }

        let icons: Vec<Vec<u8>> = paths
            .iter()
            .map(|path| {
                icon_png(path, None).unwrap_or_else(|| panic!("no icon for {}", path.display()))
            })
            .collect();

        for (path, png) in paths.iter().zip(&icons) {
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "{}", path.display());
            let (width, height) = decoded_size(png);
            assert!(
                width >= 16 && height >= 16,
                "{}: {width}x{height}",
                path.display()
            );
        }
        // exe 有自己的图标，与 .txt 的关联图标不同。
        assert_ne!(icons[0], icons[2]);
    }

    #[test]
    fn exe_icons_are_read_from_resources_with_transparency() {
        let system = std::env::var_os("SystemRoot").expect("SystemRoot is set");
        let notepad = Path::new(&system).join("System32").join("notepad.exe");

        let icon = exe_icon::extract(&notepad, 64).expect("notepad has an icon resource");

        assert_eq!((icon.width, icon.height), (64, 64));
        let alpha: Vec<u8> = icon.pixels.chunks_exact(4).map(|pixel| pixel[3]).collect();
        assert!(alpha.contains(&0), "has transparent pixels");
        assert!(alpha.iter().any(|&value| value > 200), "has opaque pixels");
        assert!(exe_icon::extract(&Path::new(&system).join("win.ini"), 64).is_none());
    }

    #[test]
    fn icon_png_uses_in_process_shell_without_helper_configuration() {
        assert!(helper::HELPER_EXE.get().is_none());
        let system = std::env::var_os("SystemRoot").expect("SystemRoot is set");
        let path = Path::new(&system).join("win.ini");
        assert!(icon_png(&path, Some(32)).is_some());
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn icon_png_for_finder_returns_bytes() {
        let path = PathBuf::from("/System/Library/CoreServices/Finder.app");
        let png = icon_png(&path, None).expect("expected PNG bytes");
        assert!(png.len() > 100, "PNG too small: {}", png.len());
        // PNG 签名
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        println!("Finder.app icon PNG size: {} bytes", png.len());
    }
}

/// TODO: 用无 GTK 主线程要求的图标后端支持 Linux；后台线程调用 IconTheme 会 panic。
#[cfg(target_os = "linux")]
fn shell_icon(_path: &Path, _size: u32) -> Option<file_icon_provider::Icon> {
    None
}
