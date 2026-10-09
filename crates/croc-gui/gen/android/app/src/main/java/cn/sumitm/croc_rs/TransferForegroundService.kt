package cn.sumitm.croc_rs

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import androidx.core.app.NotificationCompat

/**
 * 让传输在屏幕熄灭、Activity 被回收之后继续跑。
 *
 * Android 会掐后台网络活动：没有前台服务，Doze 一来传输就断；而前台服务不挂通知，
 * 系统会在 5 秒内判 `ForegroundServiceStartNotAllowedException`。所以两件事都必须做：
 *
 * 1. [startForeground] 必须在服务起来后 5 秒内调用；
 * 2. `PARTIAL_WAKE_LOCK` 让 CPU 在息屏时不停工。
 *
 * `PARTIAL_WAKE_LOCK` 带超时是一层保险：漏掉 release 时 10 分钟后自动放掉，而不是攥一整天。
 */
class TransferForegroundService : Service() {
  companion object {
    // 通知渠道 ID 与通知号要给 CrocBridge 用（它负责更新通知内容），所以不设成 private。
    const val CHANNEL_ID = "croc_transfers"
    private const val WAKE_LOCK_TIMEOUT_MS = 10 * 60 * 1000L
    const val NOTIFICATION_ID = 1001
    const val ACTION_STOP = "cn.sumitm.croc_rs.STOP_TRANSFERS"

    @Volatile private var wakeLock: PowerManager.WakeLock? = null

    fun start(context: Context) {
      val intent = Intent(context, TransferForegroundService::class.java)
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
        context.startForegroundService(intent)
      } else {
        context.startService(intent)
      }
    }

    fun stop(context: Context) {
      context.stopService(Intent(context, TransferForegroundService::class.java))
    }
  }

  override fun onCreate() {
    super.onCreate()
    createChannel()
    startForeground(NOTIFICATION_ID, buildNotification())
    acquireWakeLock()
  }

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    if (intent?.action == ACTION_STOP) {
      stopSelf()
      return START_NOT_STICKY
    }
    return START_STICKY
  }

  override fun onDestroy() {
    releaseWakeLock()
    super.onDestroy()
  }

  override fun onBind(intent: Intent?): IBinder? = null

  private fun createChannel() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
    val channel =
      NotificationChannel(CHANNEL_ID, getString(R.string.transfer_channel_name), NotificationManager.IMPORTANCE_LOW)
        .apply { description = getString(R.string.transfer_channel_description) }
    getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
  }

  private fun buildNotification(): Notification {
    val openApp = PendingIntent.getActivity(
      this,
      0,
      Intent(this, MainActivity::class.java),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )
    return NotificationCompat.Builder(this, CHANNEL_ID)
      .setSmallIcon(R.drawable.ic_launcher_foreground)
      .setContentTitle(getString(R.string.transfer_notification_title))
      .setContentText(getString(R.string.transfer_notification_text))
      .setOngoing(true)
      .setOnlyAlertOnce(true)
      .setContentIntent(openApp)
      .build()
  }

  private fun acquireWakeLock() {
    wakeLock = getSystemService(PowerManager::class.java)
      .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "croc-rs:transfers")
      .apply {
        // 超时是安全阀：哪条路径漏了 release，10 分钟后自动放掉，而不是攥一整天。
        acquire(WAKE_LOCK_TIMEOUT_MS)
      }
  }

  private fun releaseWakeLock() {
    wakeLock?.let { lock -> if (lock.isHeld) lock.release() }
    wakeLock = null
  }
}
