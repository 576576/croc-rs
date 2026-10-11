//! Android 侧的 Kotlin 桥（G10）：前台服务、通知进度、落盘目录、发布到下载目录、
//! SAF 目录发布、SAF 目录暂存、SAF 另存与系统分享。
//!
//! 只有 Rust → Kotlin 这一个方向。Kotlin 那边是
//! `gen/android/.../CrocBridge.kt` 的 `@JvmStatic` 方法；两边靠类名 + 方法名 + JNI 描述符
//! 对上，改任何一边的方法签名都不会有编译期报错，只会在真机上抛 `NoSuchMethodError`。
//! 所以改签名时两边必须一起改。
//!
//! 桌面端这几个函数是空的：Android 之外没有这套东西，调用方不该写 `cfg` 分叉。

/// 桥的类（斜杠分隔的 JNI 写法）。只有 Android 用到。
#[cfg(target_os = "android")]
const BRIDGE: &str = "cn/sumitm/crocw/CrocBridge";

/// 找到 [`BRIDGE`] 这个类。
///
/// 不能交给 `env.find_class`：`FindClass` 只认"当前这条 Java 调用栈"上的
/// classloader，而命令是在 Tauri 的 async 线程上跑的——那种线程是从 native 附着上
/// 去的，栈上没有 Java 帧，`FindClass` 会退到系统 classloader，于是抛
/// `ClassNotFoundException`：应用的类它一个也搜不到。可靠的路是先拿到
/// ApplicationContext 的 `ClassLoader`，再由它按名字 `loadClass`。
#[cfg(target_os = "android")]
fn bridge_class<'a>(
    env: &mut jni::JNIEnv<'a>,
    context: &jni::objects::JObject<'a>,
) -> jni::errors::Result<jni::objects::JClass<'a>> {
    let loader = env.call_method(context, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?;
    let name = env.new_string(BRIDGE.replace('/', "."))?;
    let found = env.call_method(
        loader.l()?,
        "loadClass",
        "(Ljava/lang/String;)Ljava/lang/Class;",
        &[jni::objects::JValue::from(&name)],
    )?;
    Ok(found.l()?.into())
}

/// 起前台服务。
#[cfg(target_os = "android")]
pub fn start_service() {
    with_env("startService", |env, class| env.call_static_method(class, "startService", "()V", &[]));
}

/// 停前台服务。
#[cfg(target_os = "android")]
pub fn stop_service() {
    with_env("stopService", |env, class| env.call_static_method(class, "stopService", "()V", &[]));
}

/// 把传输进度写进前台服务的通知。
#[cfg(target_os = "android")]
pub fn notify_progress(title: &str, text: &str) {
    with_env("notifyProgress", |env, class| {
        let title = env.new_string(title)?;
        let text = env.new_string(text)?;
        let args = [jni::objects::JValue::from(&title), jni::objects::JValue::from(&text)];
        env.call_static_method(class, "notifyProgress", "(Ljava/lang/String;Ljava/lang/String;)V", &args)
    });
}

/// 让用户挑一个位置，把已落盘的文件存过去（SAF）。
#[cfg(target_os = "android")]
pub fn export_file(path: &str) {
    with_env("exportFile", |env, class| {
        let path = env.new_string(path)?;
        let mime = env.new_string("*/*")?;
        let args = [jni::objects::JValue::from(&path), jni::objects::JValue::from(&mime)];
        env.call_static_method(class, "exportFile", "(Ljava/lang/String;Ljava/lang/String;)V", &args)
    });
}

/// 交给别的应用（系统分享）。
#[cfg(target_os = "android")]
pub fn share_file(path: &str) {
    with_env("shareFile", |env, class| {
        let path = env.new_string(path)?;
        let args = [jni::objects::JValue::from(&path)];
        env.call_static_method(class, "shareFile", "(Ljava/lang/String;)V", &args)
    });
}

/// 把一段文本交给别的应用（分享接收链接）。
#[cfg(target_os = "android")]
pub fn share_text(text: &str) {
    with_env("shareText", |env, class| {
        let text = env.new_string(text)?;
        let args = [jni::objects::JValue::from(&text)];
        env.call_static_method(class, "shareText", "(Ljava/lang/String;)V", &args)
    });
}

/// 把已落盘的文件发布到公共下载目录，返回落到的文件名。
///
/// 用 `MediaStore` 插条目，不需要任何运行时权限；失败时返回 `None`。
#[cfg(target_os = "android")]
pub fn publish_to_downloads(path: &str) -> Option<String> {
    attach("publishToDownloads", |env, class| {
        let path = env.new_string(path)?;
        let args = [jni::objects::JValue::from(&path)];
        let returned =
            env.call_static_method(class, "publishToDownloads", "(Ljava/lang/String;)Ljava/lang/String;", &args)?;
        read_string(env, returned.l()?)
    })
}

/// 应用私有目录：接收时先落在这里，完成后再发布到下载目录。
#[cfg(target_os = "android")]
pub fn sandbox_dir() -> Option<String> {
    attach("sandboxDir", |env, class| {
        let returned = env.call_static_method(class, "sandboxDir", "()Ljava/lang/String;", &[])?;
        read_string(env, returned.l()?)
    })
}

/// 把已落盘的文件发布到用户选定的 SAF 目录（目录 URI），返回落到的文件名。
///
/// 与 [`publish_to_downloads`] 并列的另一条出口：用户在界面上挑过目录就走这条，
/// 没挑过则落公共下载目录。失败时返回 `None`。
#[cfg(target_os = "android")]
pub fn saf_publish(tree_uri: &str, local_path: &str, name: &str) -> Option<String> {
    attach("safPublish", |env, class| {
        let tree = env.new_string(tree_uri)?;
        let path = env.new_string(local_path)?;
        let name = env.new_string(name)?;
        let args =
            [jni::objects::JValue::from(&tree), jni::objects::JValue::from(&path), jni::objects::JValue::from(&name)];
        let returned = env.call_static_method(
            class,
            "safPublish",
            "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
            &args,
        )?;
        read_string(env, returned.l()?)
    })
}

/// 把用户选的 SAF 目录拷进应用私有目录，返回拷出来的真实路径。
///
/// 发送目录用：引擎靠 `std::fs` 走目录树，只认真实路径，SAF 的 `content://` URI
/// 交给它必然报 `No such file or directory`。发完由 [`remove_staged`] 删掉。
/// 目录空或拷贝失败时返回 `None`。
#[cfg(target_os = "android")]
pub fn stage_directory(tree_uri: &str) -> Option<String> {
    attach("stageDirectory", |env, class| {
        let tree = env.new_string(tree_uri)?;
        let args = [jni::objects::JValue::from(&tree)];
        let returned =
            env.call_static_method(class, "stageDirectory", "(Ljava/lang/String;)Ljava/lang/String;", &args)?;
        read_string(env, returned.l()?)
    })
}

/// 删掉 [`stage_directory`] 拷出来的目录。
#[cfg(target_os = "android")]
pub fn remove_staged(path: &str) {
    with_env("removeStaged", |env, class| {
        let value = env.new_string(path)?;
        let args = [jni::objects::JValue::from(&value)];
        env.call_static_method(class, "removeStaged", "(Ljava/lang/String;)Z", &args)
    });
}

/// 附着到当前线程，跑一次 JNI 调用。
///
/// 参数在闭包里造，是因为 `JString` 的生命周期绑在附着出来的 `JNIEnv` 上。
#[cfg(target_os = "android")]
fn with_env<F>(name: &str, call: F)
where
    F: for<'a, 'b> FnOnce(
        &'a mut jni::JNIEnv<'b>,
        &'a jni::objects::JClass<'b>,
    ) -> jni::errors::Result<jni::objects::JValueOwned<'a>>,
{
    attach::<(), _>(name, move |env, class| call(env, class).map(|_| None));
}

/// 同 [`with_env`]，但把闭包的结果带回来。
#[cfg(target_os = "android")]
fn attach<T, F>(name: &str, call: F) -> Option<T>
where
    F: for<'a, 'b> FnOnce(&'a mut jni::JNIEnv<'b>, &'a jni::objects::JClass<'b>) -> jni::errors::Result<Option<T>>,
{
    use jni::JavaVM;

    let context = ndk_context::android_context();
    // `ndk-context` 交出来的只有裸指针，取 `JavaVM` 只能走 `from_raw`；这里不接管它的生命周期。
    #[expect(unsafe_code, reason = "ndk-context 只提供裸指针")]
    let Ok(vm) = (unsafe { JavaVM::from_raw(context.vm().cast()) }) else {
        tracing::warn!("拿不到 JavaVM，跳过 {name}");
        return None;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        tracing::warn!("附着到 JVM 失败，跳过 {name}");
        return None;
    };
    // ApplicationContext 是 tao 在 android_setup 里 `new_global_ref` 出来的，借一下，
    // 不接管它的生命周期——它活在整个进程上。
    #[expect(unsafe_code, reason = "ndk-context 只提供裸指针")]
    let application = unsafe { jni::objects::JObject::from_raw(context.context() as jni::sys::jobject) };
    let class = match bridge_class(&mut env, &application) {
        Ok(class) => class,
        Err(error) => {
            tracing::warn!(%error, "{name}：找不到 {BRIDGE}");
            let _ = env.exception_clear();
            return None;
        }
    };
    match call(&mut env, &class) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "{name} 调用失败");
            // 不清掉挂起的异常，后面每一次 JNI 调用都会继续报同一个错。
            let _ = env.exception_clear();
            None
        }
    }
}

/// 读一个 Java 字符串；`null` 读成 `None`。
#[cfg(target_os = "android")]
fn read_string(env: &mut jni::JNIEnv<'_>, object: jni::objects::JObject<'_>) -> jni::errors::Result<Option<String>> {
    if object.is_null() {
        return Ok(None);
    }
    let text: jni::objects::JString<'_> = object.into();
    Ok(Some(env.get_string(&text)?.to_string_lossy().into_owned()))
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

#[cfg(not(target_os = "android"))]
pub fn share_text(_text: &str) {}

#[cfg(not(target_os = "android"))]
pub fn publish_to_downloads(_path: &str) -> Option<String> {
    None
}

#[cfg(not(target_os = "android"))]
pub fn sandbox_dir() -> Option<String> {
    None
}

#[cfg(not(target_os = "android"))]
pub fn saf_publish(_tree_uri: &str, _local_path: &str, _name: &str) -> Option<String> {
    None
}

#[cfg(not(target_os = "android"))]
pub fn stage_directory(_tree_uri: &str) -> Option<String> {
    None
}

#[cfg(not(target_os = "android"))]
pub fn remove_staged(_path: &str) {}
