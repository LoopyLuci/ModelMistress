package com.modelmistress.runtime

import android.content.Context
import com.modelmistress.core.json.Json
import com.modelmistress.core.json.JsonException
import com.modelmistress.core.remote.DesktopHub

/**
 * The desktops this device is paired with. Kept in the app's private preferences, next to the server key. A key here is
 * a peer key the desktop issued; it can be revoked on the desktop at any time. A desktop reached through the Transferd
 * overlay is stored by its overlay id; one reached by address is stored by its address.
 */
class DesktopStore(context: Context) {
    private val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    fun list(): List<DesktopHub> {
        val text = prefs.getString(KEY, null) ?: return emptyList()
        val entries = try {
            Json.parse(text) as? List<*>
        } catch (error: JsonException) {
            null
        } ?: return emptyList()
        return entries.mapNotNull { entry ->
            val map = entry as? Map<*, *> ?: return@mapNotNull null
            try {
                val name = map["name"] as? String ?: return@mapNotNull null
                val key = map["key"] as? String ?: return@mapNotNull null
                val overlayId = map["overlayId"] as? String
                if (overlayId != null) {
                    DesktopHub.overlay(name, overlayId, key)
                } else {
                    DesktopHub(name = name, url = map["url"] as? String ?: return@mapNotNull null, key = key)
                }
            } catch (error: IllegalArgumentException) {
                null
            }
        }
    }

    /** Saves [hub], replacing any desktop with the same name. */
    fun save(hub: DesktopHub) {
        write(list().filterNot { it.name == hub.name } + hub)
    }

    fun remove(name: String) {
        write(list().filterNot { it.name == name })
    }

    private fun write(hubs: List<DesktopHub>) {
        val text = Json.write(
            hubs.map { hub ->
                buildMap<String, Any> {
                    put("name", hub.name)
                    put("key", hub.key)
                    val overlayId = hub.overlayId
                    if (overlayId != null) put("overlayId", overlayId) else put("url", hub.url)
                }
            },
        )
        prefs.edit().putString(KEY, text).apply()
    }

    private companion object {
        const val PREFS = "desktops"
        const val KEY = "hubs"
    }
}
