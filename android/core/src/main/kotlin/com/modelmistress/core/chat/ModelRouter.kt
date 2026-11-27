package com.modelmistress.core.chat

/** Picks the model that answers a request. A missing model name or "auto" means the default model. */
class ModelRouter(private val catalog: ModelCatalog) {
    data class Route(val modelId: String, val backend: ChatBackend)

    fun models(): List<ModelInfo> = catalog.models()

    fun resolve(requested: String?): Route {
        val wanted = requested?.trim().orEmpty()
        val models = catalog.models()
        if (models.isEmpty()) {
            throw ApiException(
                503,
                "no_models",
                "No models are installed on this device. Import a GGUF model in the app first.",
            )
        }
        val id = if (wanted.isEmpty() || wanted.equals(AUTO, ignoreCase = true)) {
            defaultOrFirst(models)
        } else {
            wanted
        }
        val backend = catalog.backend(id)
            ?: throw ApiException(404, "model_not_found", "The model '$id' is not installed on this device.")
        return Route(id, backend)
    }

    private fun defaultOrFirst(models: List<ModelInfo>): String {
        val preferred = catalog.defaultModelId()
        return if (preferred != null && models.any { it.id == preferred }) preferred else models.first().id
    }

    companion object {
        const val AUTO = "auto"
    }
}
