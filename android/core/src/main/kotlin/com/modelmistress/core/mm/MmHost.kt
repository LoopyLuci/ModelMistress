package com.modelmistress.core.mm

/** A model this device can answer with, as the router's model list shows it. [kind] is `local` or `desktop`. */
data class MmModel(
    val id: String,
    val kind: String,
    val sizeBytes: Long,
    val contextTokens: Int?,
    val isDefault: Boolean,
    val description: String,
)

/** A Hugging Face download. [phase] is `idle`, `downloading`, `done`, `stopped` or `failed`. */
data class MmDownload(
    val id: String,
    val repo: String,
    val file: String,
    val phase: String,
    val done: Long,
    val total: Long,
    val message: String?,
)

/** The owner's settings that the API may read. The API key is never part of them. */
data class MmSettings(
    val toolUse: String,
    val fileWrites: String,
    val mistyEnabled: Boolean,
    val mistyReflex: Boolean,
    val defaultModel: String?,
    val allowNetwork: Boolean,
    val port: Int,
)

/** What the device has free right now, and which model is loaded. */
data class MmDevice(
    val totalMemoryBytes: Long,
    val freeMemoryBytes: Long,
    val needleReady: Boolean,
    val loadedModel: String?,
)

/**
 * The device-side operations the router's API calls. The runtime implements it on the phone; the server's tests use a fake.
 * Operations that change the device are called only after the owner has approved them, so an implementation does not ask again.
 */
interface MmHost {
    fun settings(): MmSettings

    fun device(): MmDevice

    /** Every model the device can answer with: its own, then each paired desktop's. */
    fun models(): List<MmModel>

    /** Makes the local model [id] the default. False when no such local model is installed. */
    fun setDefault(id: String): Boolean

    /** Removes the local model [id] from the device. False when no such local model is installed. */
    fun deleteModel(id: String): Boolean

    /** The GGUF files in a Hugging Face repository. Throws [IllegalArgumentException] for a repository name that is not valid. */
    fun repoFiles(repo: String): List<String>

    /** Starts downloading [file] from [repo] into the model library. Throws [IllegalStateException] when one is already running. */
    fun startDownload(repo: String, file: String): MmDownload

    /** The current and the latest downloads. */
    fun downloads(): List<MmDownload>

    /** Stops the download that is running, if any. */
    fun stopDownloads()

    /** The latest device benchmark as JSON text, or null when none has run. */
    fun benchmarkReport(): String?

    /** Starts the device benchmark in the background. False when it is already running. */
    fun startBenchmark(): Boolean
}
