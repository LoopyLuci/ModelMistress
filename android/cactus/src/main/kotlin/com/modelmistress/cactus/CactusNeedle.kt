package com.modelmistress.cactus

import java.io.File
import java.io.FileInputStream
import java.nio.ByteBuffer
import java.nio.MappedByteBuffer
import java.nio.channels.FileChannel

/** The Cactus Needle 3 model file: pinned to one Hugging Face revision, with its size and SHA-256. */
object CactusNeedleModel {
    const val REPOSITORY = "Cactus-Compute/needle3"
    const val REVISION = "c95d25b4efa1e9601e785d92438e4ade0dbddbf8"
    const val FILE_NAME = "needle3.cact"
    const val SIZE_BYTES = 35_335_380L
    const val SHA256 = "c9d915eca282ed42d1a09b143b592adb4cc6744ffe2d294adf5cfc5548170c38"
    const val DOWNLOAD_URL = "https://huggingface.co/$REPOSITORY/resolve/$REVISION/$FILE_NAME"

    /** The flag the native runtime sets when this model is loaded. */
    internal const val RUNTIME_FLAG = 1
}

/** The JNI calls into the prebuilt Cactus runtime. Declared here; implemented in src/main/cpp/cactus_jni.cpp. */
internal object CactusNative {
    @JvmStatic
    external fun nativeLoadModel(model: ByteBuffer)

    @JvmStatic
    external fun nativeModels(): Int

    @JvmStatic
    external fun nativeReset()

    @JvmStatic
    external fun nativeInitialize(systemPrompt: String?, toolsJson: String?)

    @JvmStatic
    external fun nativeComplete(input: String, maxNewTokens: Int): String
}

/**
 * Needle 3 on this device, the tool-call proposer behind Misty's reflex. The native runtime keeps one model per process and
 * has no call to unload it, so the file is loaded once and stays in memory after that. Calls are serialised.
 */
object CactusNeedle {
    private const val MAX_NEW_TOKENS = 2048
    private const val DEFAULT_NEW_TOKENS = 256

    private val loadedLibrary: Boolean by lazy {
        runCatching { System.loadLibrary("cactus_bridge") }.isSuccess
    }

    private var mapping: MappedByteBuffer? = null

    /** True when the native runtime loads on this device's ABI. Never throws. */
    fun runtimeAvailable(): Boolean = loadedLibrary

    /**
     * Returns the raw JSON proposal Needle writes for [input] under [toolsJson]. The file [model] must be the pinned Needle
     * file; the caller checks its integrity first. Throws IllegalStateException when the runtime is unavailable or the
     * model does not load.
     */
    @Synchronized
    fun propose(
        model: File,
        input: String,
        toolsJson: String,
        maxNewTokens: Int = DEFAULT_NEW_TOKENS,
    ): String {
        check(loadedLibrary) { "Cactus inference is unavailable for this Android ABI." }
        require(input.isNotBlank()) { "Needle input must not be blank." }
        require(maxNewTokens in 1..MAX_NEW_TOKENS) { "maxNewTokens must be between 1 and $MAX_NEW_TOKENS." }
        ensureLoaded(model)
        check(CactusNative.nativeModels() and CactusNeedleModel.RUNTIME_FLAG != 0) {
            "The Cactus runtime did not report Needle as loaded."
        }
        CactusNative.nativeReset()
        CactusNative.nativeInitialize(null, toolsJson)
        return CactusNative.nativeComplete(input, maxNewTokens)
    }

    private fun ensureLoaded(model: File) {
        if (CactusNative.nativeModels() and CactusNeedleModel.RUNTIME_FLAG != 0) return
        val bytes = FileInputStream(model).channel.use { channel ->
            channel.map(FileChannel.MapMode.READ_ONLY, 0, model.length())
        }
        CactusNative.nativeLoadModel(bytes)
        // Kept alive for the life of the process: the runtime reads the mapping while it runs.
        mapping = bytes
    }
}
