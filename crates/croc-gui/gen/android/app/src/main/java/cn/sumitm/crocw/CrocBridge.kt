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
import androidx.documentfile.provider.DocumentFile
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

  /// 把用户选的 SAF 目录整个拷进应用私有目录，返回拷出来的真实路径。
  ///
  /// 发送目录必须走这一步：引擎靠 `std::fs` 递归走目录树（`files::survey` 的
  /// `symlink_metadata` 起手就要一个真实路径），而 SAF 目录只有 `content://` URI，
  /// 直接把 URI 交给引擎必然 `No such file or directory`。拷完由调用方把返回的路径
  /// 当普通目录发，发完再用 [removeStaged] 删掉。
  ///
  /// 拷到 `cacheDir/crocw-send/<时间戳>` 而不是 `filesDir`：它在缓存区，系统在空间
  /// 紧张时可以自己回收，也与我们自己发完就删的语义一致。时间戳避免同一目录发两次时
  /// 撞上前一次没删干净的残留。
  ///
  /// @return 拷出来的目录绝对路径；选不到目录或一个文件都没拷出来时返回 null。
  @JvmStatic
  fun stageDirectory(treeUri: String): String? {
    val context = appContext ?: return null
    val tree = runCatching { Uri.parse(treeUri) }.getOrNull() ?: return null
    val source = DocumentFile.fromTreeUri(context, tree) ?: return null
    val root = File(context.cacheDir, "crocw-send/${System.currentTimeMillis()}") 
    if (!root.mkdirs()) return null
    // 目录本身可能是空的：那就没有一个可发的文件，直接失败而不是发一个空目录。
    return if (copyTree(context, source, root) > 0) root.absolutePath else null
  }

  /// 删掉 [stageDirectory] 拷出来的目录。
  ///
  /// 用路径前缀校验：`stageDirectory` 给出的是 `cacheDir` 下的路径，Rust 那边只是原样
  /// 传回来，不该让它有机会删掉别处的东西。
  @JvmStatic
  fun removeStaged(path: String): Boolean {
    val context = appContext ?: return false
    val root = context.cacheDir.absolutePath + "/crocw-send/"
    if (!path.startsWith(root)) return false
    return runCatching { File(path).deleteRecursively() }.getOrDefault(false)
  }

  /// 递归把一个 SAF 目录里的文件与子目录拷到 [target]，返回拷出的文件数。
  ///
  /// 逐个条目查询而不是用 `DocumentFile.findFile` 拼路径：SAF 的文件名与真实路径不是
  /// 一回事，只有 `uri` 能定位字节。子目录按名字建，重名走 `createFile` 的自动改名。
  private fun copyTree(context: Context, source: DocumentFile, target: File): Int {
    var copied = 0
    for (child in source.listFiles()) {
      val name = child.name ?: continue
      if (child.isDirectory) {
        val nested = File(target, name)
        if (nested.mkdirs()) copied += copyTree(context, child, nested)
        continue
      }
      val ok = runCatching {
        val destination = File(target, name)
        context.contentResolver.openInputStream(child.uri)?.use { input ->
          destination.outputStream().use { output -> input.copyTo(output) }
          true
        } ?: false
      }.getOrDefault(false)
      if (ok) copied++
    }
    return copied
  }


  /// 把已经落盘的文件发布到公共下载目录。
  ///
  /// 往 `MediaStore.Downloads` 插条目不需要任何运行时权限（minSdk 30，没有
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

  /// 把已经落在应用私有目录里的文件，发布到用户选定的 SAF 目录。
  ///
  /// `treeUri` 是 SAF 的目录 URI（`content://com.android.externalstorage.documents/tree/...`），
  /// 由 `croc-saf-plugin` 起 `ACTION_OPEN_DOCUMENT_TREE` 拿到，并在那边持久化了授权。
  /// 往目录里建条目的权限来自目录本身，不是 `MANAGE_EXTERNAL_STORAGE`，所以不需要任何
  /// 运行时权限。
  ///
  /// @return 落到的文件名，失败返回 null。
  @JvmStatic
  fun safPublish(treeUri: String, localPath: String, name: String): String? {
    val context = appContext ?: return null
    val file = File(localPath)
    if (!file.exists()) return null
    val tree = runCatching { Uri.parse(treeUri) }.getOrNull() ?: return null
    // 只认 tree URI：`fromTreeUri` 收到文档 URI 时也能返回一个 DocumentFile，但
    // createFile 落到的位置与用户选的不一致，不如直接失败。
    val directory = DocumentFile.fromTreeUri(context, tree) ?: return null
    val mime = MimeTypeMap.getSingleton().getMimeTypeFromExtension(file.extension) ?: "application/octet-stream"
    // 同名条目已存在时 `createFile` 会自动改名（foo(1).ext），这与桌面端 `--rename`
    // 的语义一致，不需要先删。
    val target = runCatching { directory.createFile(mime, name) }.getOrNull() ?: return null
    return runCatching {
      val out = context.contentResolver.openOutputStream(target.uri)
        ?: throw java.io.IOException("openOutputStream(${target.uri})")
      out.use { file.inputStream().use { input -> input.copyTo(out) } }
      name
    }.getOrElse {
      // 拷到一半失败要撤掉条目，否则目标目录里留一个半截文件。
      runCatching { target.delete() }
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
