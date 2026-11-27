package com.modelmistress.core.models

/** The model files this app loads, told apart by their extension. */
enum class ModelFormat(val extension: String) {
    /** GGUF models, run by llama.cpp. */
    GGUF(".gguf"),

    /** LiteRT-LM models, run by Google's LiteRT-LM runtime. */
    LITERT_LM(".litertlm"),
    ;

    companion object {
        /** The format of a file named [fileName], or null when its extension is not one this app loads. */
        fun of(fileName: String): ModelFormat? = entries.firstOrNull { fileName.endsWith(it.extension) }
    }
}
