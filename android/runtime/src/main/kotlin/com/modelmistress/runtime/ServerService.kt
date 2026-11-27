package com.modelmistress.runtime

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.drawable.Icon
import android.os.Build
import android.os.IBinder

/**
 * Keeps the local server running in the background with a notification, so other apps can keep using the models while
 * the owner's app is closed. The notification has a Stop action.
 */
class ServerService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val runtime = ModelRuntime.get(this)
        if (intent?.action == ACTION_STOP) {
            runtime.stopServer()
            stopForegroundAndSelf()
            return START_NOT_STICKY
        }
        ensureChannel()
        startForegroundCompat(notification("Starting the server"))
        val state = runtime.startServer()
        if (!state.running) {
            stopForegroundAndSelf()
            return START_NOT_STICKY
        }
        getSystemService(NotificationManager::class.java).notify(
            NOTIFICATION_ID,
            notification("Serving ${state.baseUrl}"),
        )
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        ModelRuntime.get(this).stopServer()
        super.onDestroy()
    }

    private fun stopForegroundAndSelf() {
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun startForegroundCompat(notification: Notification) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
    }

    private fun ensureChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL_ID, "Model server", NotificationManager.IMPORTANCE_LOW),
        )
    }

    @Suppress("DEPRECATION")
    private fun notification(text: String): Notification {
        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, CHANNEL_ID)
        } else {
            Notification.Builder(this)
        }
        val stopIntent = PendingIntent.getService(
            this,
            0,
            Intent(this, ServerService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val stop = Notification.Action.Builder(
            Icon.createWithResource(this, R.drawable.ic_server),
            "Stop",
            stopIntent,
        ).build()
        return builder
            .setSmallIcon(R.drawable.ic_server)
            .setContentTitle("ModelMistress server")
            .setContentText(text)
            .setOngoing(true)
            .addAction(stop)
            .build()
    }

    companion object {
        const val ACTION_START = "com.modelmistress.runtime.action.START"
        const val ACTION_STOP = "com.modelmistress.runtime.action.STOP"
        private const val CHANNEL_ID = "model-server"
        private const val NOTIFICATION_ID = 7

        /** Starts the server in the background, with a notification while it runs. */
        fun start(context: Context) {
            val intent = Intent(context, ServerService::class.java).setAction(ACTION_START)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                context.startForegroundService(intent)
            } else {
                context.startService(intent)
            }
        }

        fun stop(context: Context) {
            context.startService(Intent(context, ServerService::class.java).setAction(ACTION_STOP))
        }
    }
}
