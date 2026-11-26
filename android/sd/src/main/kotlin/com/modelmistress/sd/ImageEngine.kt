package com.modelmistress.sd

import android.graphics.Bitmap
import com.modelmistress.core.images.ImageSpec
import java.io.ByteArrayOutputStream
import java.io.Closeable

/**
 * One stable-diffusion model loaded on this device. Images are made one at a time, and each comes back as PNG bytes. A
 * generation takes minutes on a phone's CPU, so callers should expect to wait.
 */
class ImageEngine private constructor(
    private var handle: Long,
) : Closeable {

    @Synchronized
    fun generate(spec: ImageSpec): ByteArray {
        check(handle != 0L) { "The image model has been unloaded." }
        val rgb = StableDiffusionNative.nativeGenerate(
            handle = handle,
            prompt = spec.prompt,
            negativePrompt = spec.negativePrompt,
            width = spec.width,
            height = spec.height,
            seed = spec.seed,
            steps = spec.steps,
            progressListener = null,
        )
        check(rgb.size.toLong() == spec.width.toLong() * spec.height * 3) {
            "The image runtime returned an invalid pixel buffer."
        }
        return encodePng(rgb, spec.width, spec.height)
    }

    @Synchronized
    override fun close() {
        if (handle != 0L) {
            StableDiffusionNative.nativeClose(handle)
            handle = 0L
        }
    }

    companion object {
        /** Loads a single-file checkpoint. Throws [IllegalStateException] when it cannot be loaded on this device. */
        fun load(modelPath: String, threadCount: Int): ImageEngine {
            StableDiffusionNative.ensureLoaded()
            val handle = StableDiffusionNative.nativeCreate(modelPath, threadCount)
            check(handle != 0L) { "The image model could not be loaded on this device." }
            return ImageEngine(handle)
        }

        private fun encodePng(rgb: ByteArray, width: Int, height: Int): ByteArray {
            val pixels = IntArray(width * height) { index ->
                val offset = index * 3
                (0xFF shl 24) or
                    ((rgb[offset].toInt() and 0xFF) shl 16) or
                    ((rgb[offset + 1].toInt() and 0xFF) shl 8) or
                    (rgb[offset + 2].toInt() and 0xFF)
            }
            val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
            try {
                bitmap.setPixels(pixels, 0, width, 0, 0, width, height)
                val out = ByteArrayOutputStream()
                check(bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)) {
                    "The image could not be encoded as PNG."
                }
                return out.toByteArray()
            } finally {
                bitmap.recycle()
            }
        }
    }
}
