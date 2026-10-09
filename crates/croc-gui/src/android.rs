//! Android 侧的 Kotlin 桥（G10）：前台服务、通知进度、SAF 另存与系统分享。
//!
//! 只有 Rust → Kotlin 这一个方向。Kotlin 那边是
//! `gen/android/.../CrocBridge.kt` 的 `@JvmStatic` 方法；两边靠**类名 + 方法名 + JNI 描述符**
//! 对上，改任何一边的方法签名都不会有编译期报错，只会在真机上抛 `NoSuchMethodError`。
//! 所以改签名时两边必须一起改。
//!
//! 桌面端这几个函数是空的：Android 之外没有这套东西，调用方不该写 `cfg` 分叉。

/// 桥的类（斜杠分隔的 JNI 写法）。只有 Android 用到。
#[cfg(target_os = "android")]
const BRIDGE: &str = "cn/sumitm/crocw/CrocBridge";

/// 起前台服务。
#[cfg(target_os = "android")]
pub fn start_service() {
    with_env("startService", |env| env.call_static_method(BRIDGE, "startService", "()V", &[]));
}

/// 停前台服务。
#[cfg(target_os = "android")]
pub fn stop_service() {
    with_env("stopService", |env| env.call_static_method(BRIDGE, "stopService", "()V", &[]));
}

/// 把传输进度写进前台服务的通知。
#[cfg(target_os = "android")]
pub fn notify_progress(title: &str, text: &str) {
    with_env("notifyProgress", |env| {
        let title = env.new_string(title)?;
        let text = env.new_string(text)?;
        let args = [jni::objects::JValue::from(&title), jni::objects::JValue::from(&text)];
        env.call_static_method(BRIDGE, "notifyProgress", "(Ljava/lang/String;Ljava/lang/String;)V", &args)
    });
}

/// 让用户挑一个位置，把已落盘的文件存过去（SAF）。
#[cfg(target_os = "android")]
pub fn export_file(path: &str) {
    with_env("exportFile", |env| {
        let path = env.new_string(path)?;
        let mime = env.new_string("*/*")?;
        let args = [jni::objects::JValue::from(&path), jni::objects::JValue::from(&mime)];
        env.call_static_method(BRIDGE, "exportFile", "(Ljava/lang/String;Ljava/lang/String;)V", &args)
    });
}

/// 交给别的应用（系统分享）。
#[cfg(target_os = "android")]
pub fn share_file(path: &str) {
    with_env("shareFile", |env| {
        let path = env.new_string(path)?;
        let args = [jni::objects::JValue::from(&path)];
        env.call_static_method(BRIDGE, "shareFile", "(Ljava/lang/String;)V", &args)
    });
}

/// 附着到当前线程，跑一次 JNI 调用。
///
/// 参数在闭包里造，是因为 `JString` 的生命周期绑在附着出来的 `JNIEnv` 上。
#[cfg(target_os = "android")]
fn with_env<F>(name: &str, call: F)
where
    F: for<'a, 'b> FnOnce(&'a mut jni::JNIEnv<'b>) -> jni::errors::Result<jni::objects::JValueOwned<'a>>,
{
    use jni::JavaVM;

    let context = ndk_context::android_context();
    // `ndk-context` 交出来的只有裸指针，取 `JavaVM` 只能走 `from_raw`；这里不接管它的生命周期。
    #[expect(unsafe_code, reason = "ndk-context 只提供裸指针")]
    let Ok(vm) = (unsafe { JavaVM::from_raw(context.vm().cast()) }) else {
        tracing::debug!("拿不到 JavaVM，跳过 {name}");
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        tracing::debug!("附着到 JVM 失败，跳过 {name}");
        return;
    };
    if let Err(error) = call(&mut env) {
        tracing::debug!(%error, "{name} 调用失败");
        // 不清掉挂起的异常，后面每一次 JNI 调用都会继续报同一个错。
        let _ = env.exception_clear();
    }
}

#[cfg(not(target_os = "android"))]
pub fn start_service() {}

#[cfg(not(target_os = "android"))]
pub fn stop_service() {}

#[cfg(not(target_os = "android"))]
pub fn notify_progress(_title: &str, _text: &str) {}

#[cfg(not(target_os = "android"))]
pub fn export_file(_path: &str) {}

#[cfg(not(target_os = "android"))]
pub fn share_file(_path: &str) {}
