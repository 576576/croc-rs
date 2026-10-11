/// 运行平台判据。
///
/// 只判 Android：桌面端与 Android 在"挑目录走哪条 intent""保存到那格是什么语义"
/// "要不要给另存/分享按钮"上分叉，判据到处都要用。写在 App.tsx 里会与
/// App → SendPanel → FileList 形成循环导入，所以单开一个模块。

/// 是不是 Android。
export const isAndroid = /Android/.test(navigator.userAgent);
