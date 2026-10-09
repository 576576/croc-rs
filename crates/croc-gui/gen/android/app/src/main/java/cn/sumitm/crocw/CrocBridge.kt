package cn.sumitm.crocw

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
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

  @JvmStatic
  fun bind(activity: MainActivity) {
    this.activity = activity
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
