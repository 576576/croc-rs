package cn.sumitm.crocw.saf

import android.app.Activity
import android.content.Intent
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import androidx.activity.result.ActivityResult

/**
 * 让用户挑一个 Storage Access Framework 目录。
 *
 * 官方 `tauri-plugin-dialog` 在 Android 上走 `ACTION_GET_CONTENT`（`DialogPlugin.kt` 里
 * 还留着 `// TODO: ACTION_OPEN_DOCUMENT ??`），那个 intent 只出文件、选不了目录，
 * 授权也无法持久化。这里改用 `ACTION_OPEN_DOCUMENT_TREE` 并显式
 * `takePersistableUriPermission`。
 *
 * 类名与包名被 Rust 侧按字符串引用：`crates/croc-saf-plugin/src/mobile.rs` 的
 * `PLUGIN_IDENTIFIER` / `register_android_plugin` 的类名参数。改名两边必须一起改，
 * 否则真机上 `ClassNotFoundException`，编译期看不出来。
 */
@TauriPlugin
class SafPlugin(private val activity: Activity) : Plugin(activity) {

  /**
   * 起系统的目录选择器。
   *
   * 三个 flag 缺一不可：读、写，以及 `FLAG_GRANT_PERSISTABLE_URI_PERMISSION`——
   * 没有最后那个，`takePersistableUriPermission` 会抛 `SecurityException`，
   * 用户挑的目录在应用重启后就用不了了。
   */
  @Command
  fun pickDirectory(invoke: Invoke) {
    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
      addFlags(
        Intent.FLAG_GRANT_READ_URI_PERMISSION
          or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
          or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION
      )
    }
    startActivityForResult(invoke, intent, "pickDirectoryResult")
  }

  /**
   * 选择结果。
   *
   * 用户取消时 `result.data` 为 null——那是正常路径，回一个 `uri: null` 而不是错误，
   * 前端据此保持原值。
   */
  @ActivityCallback
  fun pickDirectoryResult(invoke: Invoke, result: ActivityResult) {
    val uri = result.data?.data
    if (uri == null) {
      // `put(key, null)` 在 JSObject 的重载里会落到 `Any?`，序列化出来是 JSON null；
      // 写 `JSONObject.NULL` 是同一个结果，但意图更明确。
      invoke.resolve(JSObject().put("uri", org.json.JSONObject.NULL))
      return
    }
    try {
      activity.contentResolver.takePersistableUriPermission(
        uri,
        Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
      )
    } catch (error: SecurityException) {
      // 少数 provider 不给持久化授权。这一次会话里仍能写，只是重启后失效——
      // 不值得让整个选择失败，把 URI 照常交出去。
      android.util.Log.w("crocw-saf", "目录授权无法持久化：$uri", error)
    }
    invoke.resolve(JSObject().put("uri", uri.toString()))
  }
}
