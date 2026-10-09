package cn.sumitm.crocw

import android.app.Activity
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import android.webkit.MimeTypeMap
import androidx.core.content.FileProvider
import java.io.File

/**
 * Rust 侧调过来的入口（G10）。
 *
 * Rust → Kotlin 只有一个方向：这些方法的签名必须保持 `@JvmStatic` 且参数只用到基本类型与
 * String，JNI 那边（`crates/croc-gui/src/android.rs`）按名字和描述符去查它们，改签名会让
 * 调用在运行时才失败（NoSuchMethodError），编译期完全看不出来。
 *
 * `activity` 由 [MainActivity] 在 onCreate 里挂上、onDestroy 里摘掉：SAF 的另存与系统分享
 * 都要一个 Activity 才能起界面，而 Rust 调进来时只有 ApplicationContext。
 */
object CrocBridge {
  @Volatile private var activity: MainActivity? = null

  /// 发布到下载目录不需要 Activity，用 ApplicationContext 就够，这样界面退到后台之后
  /// 也能跑完。它不随 unbind 清空：ApplicationContext 不持有 Activity，留着不会泄漏。
  @Volatile private var appContext: Context? = null

  @JvmStatic
  fun bind(activity: MainActivity) {
    this.activity = activity
    this.appContext = activity.applicationContext
  }

  @JvmStatic
  fun unbind(activity: MainActivity) {
    if (this.activity === activity) this.activity = null
  }

  @JvmStatic
  fun startService() {
    activity?.let { TransferForegroundService.start(it) }
  }

  @JvmStatic
  fun stopService() {
    activity?.let { TransferForegroundService.stop(it) }
  }

  @JvmStatic
  fun notifyProgress(title: String, text: String) {
    val current = activity ?: return
    val manager = current.getSystemService(Context.NOTIFICATION_SERVICE) as? android.app.NotificationManager
      ?: return
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
      current.checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) !=
      android.content.pm.PackageManager.PERMISSION_GRANTED
    ) {
      return
    }
    val openApp = android.app.PendingIntent.getActivity(
      current,
      0,
      Intent(current, MainActivity::class.java),
      android.app.PendingIntent.FLAG_IMMUTABLE or android.app.PendingIntent.FLAG_UPDATE_CURRENT,
    )
    val notification = androidx.core.app.NotificationCompat.Builder(current, TransferForegroundService.CHANNEL_ID)
      .setSmallIcon(R.drawable.ic_launcher_foreground)
      .setContentTitle(title)
      .setContentText(text)
      .setOngoing(true)
      .setOnlyAlertOnce(true)
      .setContentIntent(openApp)
      .build()
    manager.notify(TransferForegroundService.NOTIFICATION_ID, notification)
  }

  /// 应用私有目录，接收时先落在这里。
  @JvmStatic
  fun sandboxDir(): String? = appContext?.filesDir?.absolutePath

  /// 把已经落盘的文件发布到公共下载目录。
  ///
  /// 往 `MediaStore.Downloads` 插条目**不需要任何运行时权限**（minSdk 30，没有
  /// `WRITE_EXTERNAL_STORAGE` 那条路）；只有改动别人创建的文件才要
  /// `MANAGE_EXTERNAL_STORAGE` 或 SAF。
  ///
  /// @return 落到的文件名，失败返回 null。
  @JvmStatic
  fun publishToDownloads(path: String): String? {
    val context = appContext ?: return null
    val file = File(path)
    if (!file.exists()) return null
    val name = file.name
    val resolver = context.contentResolver
    val values = ContentValues().apply {
      put(MediaStore.Downloads.DISPLAY_NAME, name)
      put(MediaStore.Downloads.MIME_TYPE, MimeTypeMap.getSingleton()
        .getMimeTypeFromExtension(file.extension) ?: "*/*")
      put(MediaStore.Downloads.RELATIVE_PATH, Environment.DIRECTORY_DOWNLOADS)
      // 先把条目标成待定：拷字节期间别的应用不该看到半个文件。
      put(MediaStore.Downloads.IS_PENDING, 1)
    }
    val uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values) ?: return null
    return runCatching {
      val out = resolver.openOutputStream(uri) ?: throw java.io.IOException("openOutputStream($uri)")
      out.use { file.inputStream().use { it.copyTo(out) } }
      values.clear()
      values.put(MediaStore.Downloads.IS_PENDING, 0)
      resolver.update(uri, values, null, null)
      name
    }.getOrElse {
      resolver.delete(uri, null, null)
      null
    }
  }

  /// SAF 另存：把已经落在应用私有目录里的文件，让用户自己挑一个位置存。
  @JvmStatic
  fun exportFile(path: String, mime: String) {
    activity?.requestExport(path, mime)
  }

  /// 系统分享：交给别的应用（云盘、聊天工具……）。走 FileProvider，不让对方拿到本应用
  /// 私有目录的读权限。
  @JvmStatic
  fun shareFile(path: String) {
    val current = activity ?: return
    val file = File(path)
    if (!file.exists()) return
    val uri: Uri = FileProvider.getUriForFile(current, "${current.packageName}.fileprovider", file)
    val intent = Intent(Intent.ACTION_SEND).apply {
      type = current.contentResolver.getType(uri) ?: "*/*"
      putExtra(Intent.EXTRA_STREAM, uri)
      addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    }
    runCatching { current.startActivity(Intent.createChooser(intent, null)) }
  }

  fun activityOrNull(): Activity? = activity
}
