//! 应用图标的异步加载（S5）。
//!
//! 参数：无（模块级说明）
//! 核心流程：UI 线程把要取图标的 [`WindowId`] 丢进请求通道 → 后台线程（唯一一个，串行）
//! 调 `switcheroo_platform_win::window_icon` 解码 → 结果（成功或 `None`）走结果通道回传，
//! 并用调用方给的 `wake` 回调唤醒 UI 线程 → UI 线程 `try_recv` 收集后转成 D2D 位图。
//!
//! 为什么要分线程：`window_icon` 会对目标窗口发 `WM_GETICON`（每次最多等 100ms）、读窗口类
//! 图标、必要时还要从 exe 里抽图标——全部同步执行，放在 UI 线程上会让浮层卡顿。
//!
//! 为什么要 `wake` 回调：UI 线程是纯消息循环（没有定时器、没有帧循环），后台线程把结果
//! 放进通道之后必须自己唤醒它，否则结果要等到下一次按键才会被处理。这里把「怎么唤醒」
//! 留给调用方（Windows 上是 `PostMessageW`），模块本身因此不依赖任何平台类型。

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use switcheroo_core::WindowId;
use switcheroo_platform_win::{window_icon, IconImage};

/// 后台图标加载器（跟随 [`crate::app::App`] 的生命周期）。
///
/// 参数：`requests` 请求端（`None` 表示工作线程没起来或已经关掉）；`results` 结果端；
/// `worker` 工作线程句柄。
/// 核心流程：`request` 投递一个窗口 id，`drain` 把已经算好的结果全部取回；Drop 时先关掉
/// 请求端（工作线程的 `recv` 随即返回错误退出循环）再 `join`，不留孤儿线程。
pub struct IconLoader {
    requests: Option<Sender<WindowId>>,
    results: Receiver<(WindowId, Option<IconImage>)>,
    worker: Option<JoinHandle<()>>,
}

impl IconLoader {
    /// 参数：`wake` 每产出一个结果就调用一次的回调（必须是线程安全的；Windows 上就是往
    /// 浮层窗口 `PostMessageW` 一条自定义消息）。
    /// 核心流程：建请求 / 结果两条通道 → `thread::Builder::spawn` 起一个命名的后台线程 →
    /// 线程里循环「收 id → 解码 → 回传 → `wake()`」，通道断开即退出。线程起不来时只记一条
    /// warn（没有图标仍可正常使用），并把请求端置空，后续 `request` 直接跳过。
    pub fn spawn(wake: impl Fn() + Send + 'static) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<WindowId>();
        let (result_tx, result_rx) = mpsc::channel::<(WindowId, Option<IconImage>)>();

        let spawned = thread::Builder::new()
            .name("switcheroo-icon".to_string())
            .spawn(move || {
                while let Ok(id) = request_rx.recv() {
                    let image = window_icon(id);
                    log::debug!("图标加载完成：{:#X} 成功={}", id.raw(), image.is_some());
                    // 结果端断掉说明 UI 侧已经销毁，直接结束线程。
                    if result_tx.send((id, image)).is_err() {
                        break;
                    }
                    wake();
                }
                log::debug!("图标加载线程退出");
            });

        match spawned {
            Ok(worker) => Self {
                requests: Some(request_tx),
                results: result_rx,
                worker: Some(worker),
            },
            Err(err) => {
                log::warn!("图标加载线程创建失败（不显示图标）：{err}");
                Self {
                    requests: None,
                    results: result_rx,
                    worker: None,
                }
            }
        }
    }

    /// 参数：`id` 需要图标的窗口。
    /// 核心流程：往请求通道丢一个 id；通道已关闭（工作线程退出）时只记 debug，不影响 UI。
    pub fn request(&self, id: WindowId) {
        let Some(requests) = self.requests.as_ref() else {
            return;
        };
        if requests.send(id).is_err() {
            log::debug!("图标请求通道已关闭，跳过 {:#X}", id.raw());
        }
    }

    /// 参数：无。
    /// 核心流程：把结果通道里已经就绪的条目全部取出（非阻塞 `try_recv`），交给 UI 线程
    /// 一次性处理。
    pub fn drain(&self) -> Vec<(WindowId, Option<IconImage>)> {
        let mut done = Vec::new();
        while let Ok(item) = self.results.try_recv() {
            done.push(item);
        }
        done
    }
}

/// 参数：无。
/// 核心流程：先丢掉请求端，让工作线程的 `recv` 立刻返回错误退出，再 `join` 回收线程——
/// 保证窗口销毁之后不会再有线程去碰窗口 / 图标资源。
impl Drop for IconLoader {
    fn drop(&mut self) {
        self.requests = None;
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                log::warn!("图标加载线程异常退出");
            }
        }
    }
}

/// 参数：`rgba` 非预乘 RGBA8 像素（长度为 `width × height × 4`，自上而下）。
/// 核心流程：按像素把 RGBA 换成 BGRA 并把每个颜色通道乘上 alpha（除以 255、四舍五入）——
/// Direct2D 的 `B8G8R8A8_UNORM + D2D1_ALPHA_MODE_PREMULTIPLIED` 位图要求预乘，直接塞非预乘
/// 数据会让图标边缘发白。尾部不足 4 字节的部分忽略（调用方已经校验过长度）。
pub fn premultiply_bgra(rgba: &[u8]) -> Vec<u8> {
    let mut bgra = Vec::with_capacity(rgba.len());
    for pixel in rgba.chunks_exact(4) {
        let alpha = u32::from(pixel[3]);
        for channel in [pixel[2], pixel[1], pixel[0]] {
            let value = (u32::from(channel) * alpha + 127) / 255;
            bgra.push(value as u8);
        }
        bgra.push(pixel[3]);
    }
    bgra
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全透明像素乘完 alpha 之后 RGB 必须是 0（否则透明区域会留下彩边）。
    #[test]
    fn transparent_pixels_become_zero() {
        assert_eq!(premultiply_bgra(&[255, 128, 64, 0]), vec![0, 0, 0, 0]);
    }

    /// 不透明像素只做 RGBA → BGRA 通道交换，数值不变。
    #[test]
    fn opaque_pixels_swap_to_bgra() {
        assert_eq!(premultiply_bgra(&[10, 20, 30, 255]), vec![30, 20, 10, 255]);
    }

    /// 半透明像素的每个通道都乘 alpha（128/255 ≈ 0.502）。
    #[test]
    fn half_transparent_pixels_are_premultiplied() {
        assert_eq!(premultiply_bgra(&[255, 0, 0, 128]), vec![0, 0, 128, 128]);
    }
}
