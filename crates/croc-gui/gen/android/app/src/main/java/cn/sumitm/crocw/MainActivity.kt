package cn.sumitm.crocw

import android.Manifest
import android.os.Build
import android.os.Bundle
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import java.io.File

class MainActivity : TauriActivity() {
  private val requestNotifications =
    registerForActivityResult(ActivityResultContracts.RequestPermission()) { /* 没有通知也能传，只是看不见进度 */ }

  /// SAF 另存：Rust 侧（CrocBridge.exportFile）请求之后，由这里把文件拷到用户挑的位置。
  private var pendingExport: String? = null
  private val exportLauncher = registerForActivityResult(ActivityResultContracts.CreateDocument("*/*")) { uri ->
    val source = pendingExport
    pendingExport = null
    if (uri == null || source == null) return@registerForActivityResult
    val file = File(source)
    runCatching {
      contentResolver.openOutputStream(uri)?.use { out -> file.inputStream().use { it.copyTo(out) } }
    }
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    CrocBridge.bind(this)
    askForNotificationPermission()
    // v1：服务跟着 Activity 的生命周期走（Rust 侧按传输状态启停的那条路见
    // crates/croc-gui/src/android.rs 的 start_service / stop_service）。
    TransferForegroundService.start(this)
  }

  override fun onDestroy() {
    TransferForegroundService.stop(this)
    CrocBridge.unbind(this)
    super.onDestroy()
  }

  fun requestExport(path: String, mime: String) {
    pendingExport = path
    runCatching { exportLauncher.launch(File(path).name) }
  }

  private fun askForNotificationPermission() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
    requestNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
  }
}
