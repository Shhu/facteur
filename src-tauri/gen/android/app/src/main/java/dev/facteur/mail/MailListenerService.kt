package dev.facteur.mail

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat

/**
 * Keeps facteur's process alive in the background so the engine's listening connections
 * (IMAP IDLE / NOTIFY) keep running and new mail is announced right away. It does no work
 * itself: it only holds a discreet, persistent notification, as Android requires.
 */
class MailListenerService : Service() {

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    createChannel(this)
    val open = PendingIntent.getActivity(
      this, 0, Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )
    val notification: Notification = NotificationCompat.Builder(this, CHANNEL_ID)
      .setSmallIcon(R.mipmap.ic_launcher)
      .setContentTitle(getString(R.string.listener_title))
      .setContentText(getString(R.string.listener_text))
      .setContentIntent(open)
      .setOngoing(true)
      .setSilent(true)
      .setPriority(NotificationCompat.PRIORITY_MIN)
      .build()
    val type = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE else 0
    ServiceCompat.startForeground(this, NOTIFICATION_ID, notification, type)
    // Restarted by Android if it had to stop it.
    return START_STICKY
  }

  companion object {
    private const val CHANNEL_ID = "facteur-listener"
    private const val NOTIFICATION_ID = 7301
    private const val PREFS = "facteur"
    private const val KEY_ENABLED = "background_listening"

    /** On by default; the user can turn it off in Settings. */
    fun isEnabled(context: Context): Boolean =
      context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(KEY_ENABLED, true)

    fun setEnabled(context: Context, enabled: Boolean) {
      context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putBoolean(KEY_ENABLED, enabled).apply()
      if (enabled) start(context) else stop(context)
    }

    fun start(context: Context) {
      val intent = Intent(context, MailListenerService::class.java)
      try {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) context.startForegroundService(intent) else context.startService(intent)
      } catch (e: Exception) {
        // Not allowed right now (app not in the foreground): it will start next time it opens.
        android.util.Log.w("facteur", "background listening not started: $e")
      }
    }

    fun stop(context: Context) {
      context.stopService(Intent(context, MailListenerService::class.java))
    }

    private fun createChannel(context: Context) {
      if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
      val manager = context.getSystemService(NotificationManager::class.java)
      if (manager.getNotificationChannel(CHANNEL_ID) != null) return
      val channel = NotificationChannel(CHANNEL_ID, context.getString(R.string.listener_channel), NotificationManager.IMPORTANCE_MIN)
      channel.setShowBadge(false)
      manager.createNotificationChannel(channel)
    }
  }
}
