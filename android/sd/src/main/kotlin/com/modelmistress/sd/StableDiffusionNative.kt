package com.modelmistress.sd

/**
 * The stable-diffusion.cpp bridge. [nativeGenerate] returns the image as RGB pixels, three bytes a pixel, row by row. The
 * JNI functions are in `stable_diffusion_jni.cpp`, which names this class in each symbol.
 */
internal object StableDiffusionNative {
    @Volatile
    private var loaded = false

    @Synchronized
    fun ensureLoaded() {
        if (loaded) return
        try {
            System.loadLibrary("stable_diffusion_bridge")
        } catch (error: UnsatisfiedLinkError) {
            throw IllegalStateException(
                "On-device image generation is not available on this device. It needs an arm64 processor.",
                error,
            )
        }
        loaded = true
    }

    @JvmStatic
    external fun nativeVersion(): String

    @JvmStatic
    external fun nativeCreate(modelPath: String, threadCount: Int): Long

    @JvmStatic
    external fun nativeGenerate(
        handle: Long,
        prompt: String,
        negativePrompt: String,
        width: Int,
        height: Int,
        seed: Long,
        steps: Int,
        progressListener: ProgressListener?,
    ): ByteArray

    @JvmStatic
    external fun nativeCancel(handle: Long)

    @JvmStatic
    external fun nativeClose(handle: Long)
}

/** Receives the step a generation has reached. Optional. */
fun interface ProgressListener {
    fun onProgress(completed: Int, total: Int)
}
