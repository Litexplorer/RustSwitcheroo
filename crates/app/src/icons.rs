//! 窗口图标的后台加载与纹理缓存（US-044）。
//!
//! 参数：全是对上层透明的事实——窗口标识、平台层解好的位图、egui 上下文
//! 核心流程：UI 线程把"需要图标的窗口"投给后台线程 → 后台线程逐个调用平台层
//! [`window_icon`] 并把结果回传 → UI 线程在之后某一帧把位图转成 [`egui::TextureHandle`]
//! 存进缓存并请求重绘，列表里的灰色占位方块就被真图标替换。同一窗口只请求一次，
//! 取不到图标的也记一条"已尝试"，不会反复请求；[`IconCache`] 的 `Drop` 会关掉请求端并
//! join 后台线程，保证进程退出时线程不会继续访问已释放的资源。

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use eframe::egui;
use switcheroo_core::{WindowId, WindowInfo};
use switcheroo_platform_win::{window_icon, IconImage};

/// 后台线程回传的一条结果：`None` 表示这个窗口没有任何可用图标。
type Loaded = (WindowId, Option<IconImage>);

/// 图标缓存 + 后台加载线程。
///
/// 参数：`worker` 后台加载线程与两条 channel；`textures` 已解码并上传的纹理
/// （`None` = 已尝试且没有图标）；`pending` 已投递但还没回结果的窗口
/// 核心流程：UI 线程只碰 `textures` / `pending`，后台线程只碰 channel，两端不共享内存。
pub struct IconCache {
    /// 后台线程入口（请求端 + 结果端 + 线程句柄）。
    worker: LoadWorker,
    /// 图标纹理缓存：值 `None` 表示"试过了，这个窗口没有图标"，同样不再重复请求。
    textures: HashMap<WindowId, Option<egui::TextureHandle>>,
    /// 已投递但还没回结果的窗口，避免同一批快照被重复排队。
    pending: HashSet<WindowId>,
}

impl IconCache {
    /// 参数：无
    /// 核心流程：起一个后台加载线程并建两张空表。线程起不来（极少见）时日志里留一行警告，
    /// 后续请求直接失败，界面继续画占位方块，不影响列表本身。
    pub fn new() -> Self {
        Self {
            worker: LoadWorker::spawn(),
            textures: HashMap::new(),
            pending: HashSet::new(),
        }
    }

    /// 参数：`windows` 当前窗口快照
    /// 核心流程：先按快照裁掉已消失窗口的缓存与排队（窗口句柄会被系统复用，留着可能串图）→
    /// 再给每个还没有图标、也还没排队的窗口投递一次请求。只在快照变化（唤起、关闭移除）时
    /// 调用，因此一次唤起最多提交一批请求，不会每帧都提交。
    pub fn sync(&mut self, windows: &[WindowInfo]) {
        self.textures
            .retain(|id, _| windows.iter().any(|window| window.id == *id));
        self.pending
            .retain(|id| windows.iter().any(|window| window.id == *id));

        let mut queued = 0;
        for window in windows {
            let id = window.id;
            if self.textures.contains_key(&id) || self.pending.contains(&id) {
                continue;
            }
            let sent = self
                .worker
                .requests
                .as_ref()
                .is_some_and(|requests| requests.send(id).is_ok());
            if sent {
                self.pending.insert(id);
                queued += 1;
            }
        }
        if queued > 0 {
            log::debug!("图标请求入队 {queued} 个");
        }
    }

    /// 参数：`ctx` egui 上下文（用来上传纹理）
    /// 核心流程：用 `try_recv` 把后台线程已经解好的结果**不阻塞地**全部取走 → 每张位图转成
    /// `ColorImage` 再 `load_texture` 成纹理存进缓存，解不出来就记一条 `None` → 返回本次
    /// 新增的图标数量，调用方据此 `request_repaint`，让占位方块立刻被替换。
    pub fn poll(&mut self, ctx: &egui::Context) -> usize {
        let mut arrived = 0;
        // `try_recv` 返回 Empty（暂时没有）或 Disconnected（后台线程已退出）时循环结束。
        while let Ok((id, image)) = self.worker.loaded.try_recv() {
            self.pending.remove(&id);
            let texture = image.and_then(|image| upload(ctx, id, image));
            if texture.is_some() {
                arrived += 1;
            }
            self.textures.insert(id, texture);
        }
        arrived
    }

    /// 参数：`id` 窗口标识
    /// 核心流程：查缓存——还没回来、或者试过但没图标的窗口都返回 `None`，调用方据此继续
    /// 画灰色占位方块。
    pub fn texture(&self, id: WindowId) -> Option<&egui::TextureHandle> {
        self.textures.get(&id).and_then(|entry| entry.as_ref())
    }
}

impl Default for IconCache {
    /// 参数：无
    /// 核心流程：与 [`IconCache::new`] 等价，满足 clippy 的 `new_without_default`。
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for IconCache {
    /// 参数：无
    /// 核心流程：先丢掉请求端，让后台线程的 `recv` 立刻返回错误并退出循环（不必等它把队列
    /// 里的窗口取完）→ 再 `join` 线程，保证走到这里之后不会再有线程在跑。
    fn drop(&mut self) {
        self.worker.shutdown();
    }
}

/// 后台加载线程：请求端、结果端与线程句柄。
///
/// 参数：`requests` UI → 后台的窗口标识队列；`loaded` 后台 → UI 的结果队列；`join` 线程句柄
/// 核心流程：三条东西一起持有，`shutdown` 时先关请求端再 join，顺序不能反。
struct LoadWorker {
    /// 请求端：`None` 表示已经投递过退出信号（`shutdown` 之后）。
    requests: Option<Sender<WindowId>>,
    loaded: Receiver<Loaded>,
    join: Option<JoinHandle<()>>,
}

impl LoadWorker {
    /// 参数：无
    /// 核心流程：建两条 `std::sync::mpsc` 通道 → 起一个具名线程跑 [`load_loop`]。线程起不来
    /// 时保留通道（请求端会直接返回错误、结果端立即断开），于是上层退化成"没有图标"，不 panic。
    fn spawn() -> Self {
        let (requests, request_rx) = mpsc::channel::<WindowId>();
        let (loaded_tx, loaded) = mpsc::channel::<Loaded>();

        let join = match std::thread::Builder::new()
            .name("icon-loader".to_string())
            .spawn(move || load_loop(request_rx, loaded_tx))
        {
            Ok(handle) => Some(handle),
            Err(err) => {
                log::warn!("图标加载线程启动失败，本次运行不加载图标：{err}");
                None
            }
        };

        Self {
            requests: Some(requests),
            loaded,
            join,
        }
    }

    /// 参数：无
    /// 核心流程：丢掉请求端（后台线程的 `recv` 随即返回错误并结束循环）→ join 线程。
    /// 线程在退出时 panic 只记一行日志，不影响 UI 线程收尾。
    fn shutdown(&mut self) {
        // 先关请求端：后台线程的 recv 立刻返回错误并退出循环，不必等它取完队列。
        drop(self.requests.take());

        if let Some(join) = self.join.take() {
            if join.join().is_err() {
                log::debug!("图标加载线程退出时 panic，结果已丢弃");
            }
        }
    }
}

/// 参数：`requests` 请求端；`loaded` 结果端
/// 核心流程：循环等窗口标识 → 调平台层 [`window_icon`]（单次最多阻塞 100ms，见平台层超时）→
/// 把结果送回 UI 线程；请求端被丢弃（进程退出）或 UI 线程不再收结果时结束循环。
fn load_loop(requests: Receiver<WindowId>, loaded: Sender<Loaded>) {
    while let Ok(id) = requests.recv() {
        let image = window_icon(id);
        match &image {
            Some(image) => log::debug!(
                "图标解码完成：id={:#X} {}x{}",
                id.raw(),
                image.width,
                image.height
            ),
            None => log::debug!("图标解码无结果：id={:#X}", id.raw()),
        }
        if loaded.send((id, image)).is_err() {
            break;
        }
    }
}

/// 参数：`ctx` egui 上下文；`id` 窗口标识（只用来生成纹理名）；`image` 平台层解好的位图
/// 核心流程：位图先过 [`color_image`] 的长度校验 → `load_texture` 上传成线性采样的纹理。
/// 校验不过就返回 `None`，上层把这一条记成"没有图标"。
fn upload(ctx: &egui::Context, id: WindowId, image: IconImage) -> Option<egui::TextureHandle> {
    let image = color_image(&image)?;
    Some(ctx.load_texture(
        format!("window-icon-{:#X}", id.raw()),
        image,
        egui::TextureOptions::LINEAR,
    ))
}

/// 参数：`image` 平台层解好的 RGBA8 位图（非预乘）
/// 核心流程：尺寸为 0 或缓冲长度不等于 `width * height * 4` 时返回 `None`——
/// `ColorImage::from_rgba_unmultiplied` 对长度不符会直接 panic，脏数据必须先挡在门外 →
/// 合法数据交给 egui 做非预乘到 `Color32` 的转换。
fn color_image(image: &IconImage) -> Option<egui::ColorImage> {
    if image.width == 0 || image.height == 0 {
        return None;
    }
    let expected = (image.width as usize)
        .checked_mul(image.height as usize)?
        .checked_mul(4)?;
    if image.rgba.len() != expected {
        log::debug!(
            "图标缓冲长度不符：{}x{} 期望 {expected} 字节，实际 {} 字节",
            image.width,
            image.height,
            image.rgba.len()
        );
        return None;
    }

    Some(egui::ColorImage::from_rgba_unmultiplied(
        [image.width as usize, image.height as usize],
        &image.rgba,
    ))
}

/// 参数：`source` 原图尺寸（逻辑像素）；`box_size` 目标方框边长（列表里是 16×16）
/// 核心流程：等比缩放到方框内——缩放比 = `box_size / 较长边`，长短边乘同一个比，因此宽高比
/// 不变；原图任一边为 0 或方框为 0 时返回零尺寸（调用方据此跳过绘制，不做除零）。
pub(crate) fn fit_icon_size(source: egui::Vec2, box_size: f32) -> egui::Vec2 {
    let longest = source.x.max(source.y);
    // NaN / 0 / 负数都算退化输入：直接返回零尺寸，免得算出一个 NaN 或负的矩形。
    if longest <= 0.0 || box_size <= 0.0 || !longest.is_finite() || !box_size.is_finite() {
        return egui::Vec2::ZERO;
    }
    source * (box_size / longest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 长度合法的位图能转成 `ColorImage`，且像素与入参一致。
    #[test]
    fn color_image_accepts_matching_buffer() {
        let image = IconImage {
            width: 2,
            height: 1,
            rgba: vec![1, 2, 3, 255, 4, 5, 6, 128],
        };
        let converted = color_image(&image).expect("长度合法应转换成功");
        assert_eq!(converted.size, [2, 1]);
        assert_eq!(
            converted.pixels[0],
            egui::Color32::from_rgba_unmultiplied(1, 2, 3, 255)
        );
        assert_eq!(
            converted.pixels[1],
            egui::Color32::from_rgba_unmultiplied(4, 5, 6, 128)
        );
    }

    /// 长度不符、宽高为 0、尺寸溢出都必须返回 `None`（不能让 egui 的断言 panic）。
    #[test]
    fn color_image_rejects_bad_buffer() {
        // 2×2 需要 16 字节，这里少一个字节。
        let short = IconImage {
            width: 2,
            height: 2,
            rgba: vec![0; 15],
        };
        assert!(color_image(&short).is_none());

        // 多出字节同样不合法。
        let long = IconImage {
            width: 1,
            height: 1,
            rgba: vec![0; 8],
        };
        assert!(color_image(&long).is_none());

        // 零尺寸 / 空缓冲。
        let empty = IconImage {
            width: 0,
            height: 0,
            rgba: Vec::new(),
        };
        assert!(color_image(&empty).is_none());

        // 溢出：u32::MAX 级别的尺寸算不出期望长度，直接拒绝。
        let huge = IconImage {
            width: u32::MAX,
            height: u32::MAX,
            rgba: vec![0; 4],
        };
        assert!(color_image(&huge).is_none());
    }

    /// 等比缩放：只按较长边定比例，宽高比不变，结果永远落在方框内。
    #[test]
    fn fit_icon_size_keeps_aspect_ratio() {
        let square = fit_icon_size(egui::vec2(32.0, 32.0), 16.0);
        assert_eq!(square, egui::vec2(16.0, 16.0));

        let already_right = fit_icon_size(egui::vec2(16.0, 16.0), 16.0);
        assert_eq!(already_right, egui::vec2(16.0, 16.0));

        // 宽图：高度按同一比例缩小。
        let wide = fit_icon_size(egui::vec2(32.0, 16.0), 16.0);
        assert_eq!(wide, egui::vec2(16.0, 8.0));

        // 小图放大到方框（图标本身只有 8×8 时也铺满 16×16 区域）。
        let small = fit_icon_size(egui::vec2(8.0, 4.0), 16.0);
        assert_eq!(small, egui::vec2(16.0, 8.0));
    }

    /// 退化输入：零尺寸原图或零尺寸方框都返回零尺寸，不让调用方算出 NaN。
    #[test]
    fn fit_icon_size_handles_zero() {
        assert_eq!(fit_icon_size(egui::vec2(0.0, 0.0), 16.0), egui::Vec2::ZERO);
        assert_eq!(fit_icon_size(egui::vec2(32.0, 32.0), 0.0), egui::Vec2::ZERO);
        assert_eq!(
            fit_icon_size(egui::vec2(0.0, 8.0), 16.0),
            egui::vec2(0.0, 16.0)
        );
    }

    /// 同一份快照连续同步两次，只入队一次——不重复请求是 US-044 的验收点之一。
    #[test]
    fn sync_only_queues_each_window_once() {
        let mut cache = IconCache::new();
        let windows = vec![
            WindowInfo::new(WindowId::new(1), "alpha", "a.exe"),
            WindowInfo::new(WindowId::new(2), "beta", "b.exe"),
        ];

        cache.sync(&windows);
        assert_eq!(cache.pending.len(), 2, "第一次同步应把两个窗口都排进队列");

        // 结果还没回来时再同步一次：两个窗口都已在 pending 里，不应重复入队。
        cache.sync(&windows);
        assert_eq!(cache.pending.len(), 2);

        // 已经试过但没有图标的窗口同样不再请求（用人工缓存项模拟"试过"）。
        cache.pending.clear();
        cache.textures.insert(WindowId::new(1), None);
        cache.sync(&windows);
        assert_eq!(cache.pending.len(), 1, "已尝试过的窗口不应再次入队");
        assert!(cache.pending.contains(&WindowId::new(2)));
    }

    /// 快照里消失的窗口连同缓存一起丢掉，避免句柄被系统复用后串图。
    #[test]
    fn sync_drops_cache_of_gone_windows() {
        let mut cache = IconCache::new();
        let windows = vec![
            WindowInfo::new(WindowId::new(1), "alpha", "a.exe"),
            WindowInfo::new(WindowId::new(2), "beta", "b.exe"),
        ];
        cache.sync(&windows);
        cache.pending.clear();
        cache.textures.insert(WindowId::new(1), None);
        cache.textures.insert(WindowId::new(2), None);

        // 窗口 2 被关掉：它的缓存应随快照一起消失，窗口 1 的保留。
        cache.sync(&windows[..1]);
        assert!(cache.textures.contains_key(&WindowId::new(1)));
        assert!(!cache.textures.contains_key(&WindowId::new(2)));
    }

    /// 后台线程靠 channel 结束：Drop 里 join 不能死锁，之后再请求也不会 panic。
    #[test]
    fn drop_shuts_down_the_worker() {
        let mut cache = IconCache::new();
        cache.sync(&[WindowInfo::new(WindowId::new(7), "seven", "7.exe")]);
        drop(cache);
    }
}
