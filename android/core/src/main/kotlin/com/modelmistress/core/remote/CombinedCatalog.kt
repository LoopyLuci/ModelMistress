package com.modelmistress.core.remote

import com.modelmistress.core.chat.ChatBackend
import com.modelmistress.core.chat.ModelCatalog
import com.modelmistress.core.chat.ModelInfo
import java.io.IOException

/**
 * The models this device answers with: its own, then those of every paired desktop. A desktop's model is named
 * `<desktop>@<model>` so that the id says where it runs and the router can send the request there. A desktop that cannot
 * be reached contributes no models; its error is shown where the desktop is managed, not in the model list.
 */
class CombinedCatalog(
    private val local: ModelCatalog,
    private val desktops: () -> List<DesktopHub>,
    private val endpoints: OverlayEndpoints = OverlayEndpoints.UNAVAILABLE,
    private val clock: () -> Long = System::currentTimeMillis,
) : ModelCatalog {
    private val cache = HashMap<DesktopHub, Pair<Long, List<ModelInfo>>>()

    override fun models(): List<ModelInfo> = local.models() + desktops().flatMap { remoteModels(it) }

    override fun defaultModelId(): String? = local.defaultModelId()

    override fun backend(modelId: String): ChatBackend? {
        local.backend(modelId)?.let { return it }
        val separator = modelId.indexOf(SEPARATOR)
        if (separator <= 0) return null
        val name = modelId.substring(0, separator)
        val hub = desktops().firstOrNull { it.name == name } ?: return null
        return DesktopBackend(DesktopClient(hub, endpoints), modelId.substring(separator + 1))
    }

    private fun remoteModels(hub: DesktopHub): List<ModelInfo> {
        val now = clock()
        synchronized(cache) {
            cache[hub]?.takeIf { now - it.first < CACHE_MS }?.let { return it.second }
        }
        val fresh = try {
            DesktopClient(hub, endpoints).models().map { model ->
                model.copy(
                    id = "${hub.name}$SEPARATOR${model.id}",
                    description = "On ${hub.name}" + if (model.description.isEmpty()) "" else " · ${model.description}",
                )
            }
        } catch (error: IOException) {
            emptyList()
        } catch (error: RuntimeException) {
            emptyList()
        }
        synchronized(cache) { cache[hub] = now to fresh }
        return fresh
    }

    companion object {
        /** Separates the desktop's name from its model id. Desktop names cannot contain it. */
        const val SEPARATOR = '@'
        private const val CACHE_MS = 15_000L
    }
}
