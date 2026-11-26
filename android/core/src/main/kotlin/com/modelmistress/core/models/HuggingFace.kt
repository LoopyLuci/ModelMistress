package com.modelmistress.core.models

import com.modelmistress.core.json.Json
import com.modelmistress.core.json.JsonException
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL

/** A Hugging Face refusal or a repo that cannot be used. The message is for the owner. */
class HfException(message: String) : IOException(message)

/**
 * A repo the owner names: "org/name", optionally with a quantisation after a colon ("org/name:Q4_K_M"). The usual web
 * forms are accepted too ("https://huggingface.co/org/name", "hf.co/org/name").
 */
data class HfSpec(val repo: String, val quant: String?) {
    init {
        require(REPO.matches(repo)) { "Name a Hugging Face repo as org/name." }
        require(quant == null || QUANT.matches(quant)) { "A quantisation uses letters, digits, '_', '-' or '.'." }
    }

    companion object {
        private val REPO = Regex("[A-Za-z0-9][A-Za-z0-9._-]{0,95}/[A-Za-z0-9][A-Za-z0-9._-]{0,95}")
        private val QUANT = Regex("[A-Za-z0-9][A-Za-z0-9._-]{0,31}")

        fun parse(text: String): HfSpec {
            var rest = text.trim()
            rest = rest.removePrefix("https://").removePrefix("http://")
            rest = rest.removePrefix("huggingface.co/").removePrefix("hf.co/").trimEnd('/')
            val colon = rest.lastIndexOf(':')
            val repo = if (colon >= 0) rest.substring(0, colon) else rest
            val quant = if (colon >= 0) rest.substring(colon + 1).trim().ifEmpty { null } else null
            return HfSpec(repo.trim(), quant)
        }
    }
}

/** One file in a repo. [sha256] is the checksum Hugging Face publishes for it, or null when the repo does not store one. */
data class HfFile(val path: String, val sizeBytes: Long, val sha256: String?) {
    val name: String
        get() = path.substringAfterLast('/')

    /** The quantisation the file name says, such as "Q4_K_M" or "IQ4_XS", or null when the name does not say. */
    val quant: String?
        get() = QUANT_IN_NAME.find(name.removeSuffix(".gguf"))?.value?.uppercase()

    private companion object {
        val QUANT_IN_NAME = Regex(
            "(?i)(?<![A-Za-z0-9])(IQ[1-4]_[A-Z0-9_]+|Q[1-8](?:_[A-Z0-9]+)*|BF16|F16|F32)(?![A-Za-z0-9])",
        )
    }
}

/** The repo's current revision and every file in it. */
data class RepoFiles(val revision: String, val files: List<HfFile>)

/** Reads a repo's file list and picks the GGUF model to download, the way the desktop app does. */
object HuggingFaceFiles {
    /** Used when the owner names no quantisation: the first of these the repo has. */
    val QUANT_PREFERENCE = listOf("Q4_K_M", "Q4_K_S", "Q5_K_M", "Q4_0", "Q6_K", "Q8_0", "IQ4_XS", "Q3_K_M")

    private val SPLIT_PART = Regex("-\\d+-of-\\d+\\.gguf$", RegexOption.IGNORE_CASE)

    /** Reads the answer of `GET /api/models/{repo}/revision/main?blobs=true`. */
    fun parseRepoFiles(text: String): RepoFiles {
        val root = Json.parse(text) as? Map<*, *> ?: throw JsonException("Hugging Face sent an answer this app cannot read.")
        val revision = root["sha"] as? String ?: throw JsonException("Hugging Face gave no revision for this repo.")
        val siblings = root["siblings"] as? List<*> ?: emptyList<Any?>()
        val files = siblings.mapNotNull { entry ->
            val item = entry as? Map<*, *> ?: return@mapNotNull null
            val path = item["rfilename"] as? String ?: return@mapNotNull null
            val lfs = item["lfs"] as? Map<*, *>
            val size = ((lfs?.get("size") as? Number) ?: (item["size"] as? Number))?.toLong() ?: 0L
            val sha = (lfs?.get("sha256") as? String)?.lowercase()?.takeIf { SHA256.matches(it) }
            HfFile(path, size, sha)
        }
        return RepoFiles(revision, files)
    }

    /** The files a person can pick: single-file GGUF models. Vision projectors and split parts are left out. */
    fun candidates(files: List<HfFile>): List<HfFile> = files
        .filter { isSingleModel(it) }
        .sortedBy { it.sizeBytes }

    /** The model for [quant], or the default when none is named. Throws when the repo has no such model. */
    fun choose(files: List<HfFile>, quant: String?): HfFile {
        val models = candidates(files)
        if (models.isEmpty()) {
            throw HfException("This repo has no single-file GGUF model this app can download.")
        }
        if (quant != null) {
            return models.firstOrNull { it.quant.equals(quant, ignoreCase = true) }
                ?: throw HfException("No $quant model in this repo. Choose one of: ${models.joinToString { it.path }}.")
        }
        for (preferred in QUANT_PREFERENCE) {
            models.firstOrNull { it.quant == preferred }?.let { return it }
        }
        return models.first()
    }

    fun resolveUrl(baseUrl: String, spec: HfSpec, revision: String, file: HfFile): String {
        val path = file.path.split('/').joinToString("/") { segment ->
            java.net.URLEncoder.encode(segment, Charsets.UTF_8.name()).replace("+", "%20")
        }
        return "$baseUrl/${spec.repo}/resolve/$revision/$path"
    }

    private fun isSingleModel(file: HfFile): Boolean =
        file.name.endsWith(".gguf", ignoreCase = true) &&
            !file.name.lowercase().startsWith("mmproj") &&
            !SPLIT_PART.containsMatchIn(file.name)

    private val SHA256 = Regex("[0-9a-f]{64}")
}

/**
 * Talks to Hugging Face. The token, when given, is sent only to the Hugging Face host it was given for, never to the file
 * servers a download is redirected to. [baseUrl] is the Hugging Face site; tests point it at a local server.
 */
class HuggingFaceClient(
    private val token: String? = null,
    private val baseUrl: String = "https://huggingface.co",
) {
    fun repoFiles(spec: HfSpec): RepoFiles {
        val url = "$baseUrl/api/models/${spec.repo}/revision/main?blobs=true"
        val connection = connect(url, range = null)
        try {
            val status = connection.responseCode
            if (status >= 400) throw refusal(status, spec.repo)
            val body = connection.inputStream.bufferedReader(Charsets.UTF_8).use { it.readText() }
            return HuggingFaceFiles.parseRepoFiles(body)
        } finally {
            connection.disconnect()
        }
    }

    fun resolveUrl(spec: HfSpec, revision: String, file: HfFile): String =
        HuggingFaceFiles.resolveUrl(baseUrl, spec, revision, file)

    /** Opens a GET request, following redirects by hand so the token is not sent to a different host. */
    internal fun connect(url: String, range: Long?): HttpURLConnection {
        val origin = URL(url).host
        var target = URL(url)
        for (hop in 0..MAX_REDIRECTS) {
            val connection = target.openConnection() as HttpURLConnection
            connection.instanceFollowRedirects = false
            connection.requestMethod = "GET"
            connection.connectTimeout = CONNECT_TIMEOUT_MS
            connection.readTimeout = READ_TIMEOUT_MS
            connection.setRequestProperty("User-Agent", "ModelMistress-Android")
            if (range != null) connection.setRequestProperty("Range", "bytes=$range-")
            if (token != null && target.host == origin) {
                connection.setRequestProperty("Authorization", "Bearer $token")
            }
            val status = connection.responseCode
            if (status !in 300..399) return connection
            val location = connection.getHeaderField("Location")
            connection.disconnect()
            if (location == null || hop == MAX_REDIRECTS) throw HfException("Hugging Face redirected this download too often.")
            target = URL(target, location)
        }
        throw HfException("Hugging Face redirected this download too often.")
    }

    private fun refusal(status: Int, what: String): HfException = when (status) {
        401, 403 -> HfException(
            "Hugging Face refused $what. A gated or private repo needs a Hugging Face token in Settings, and the " +
                "account must have accepted the repo's terms.",
        )
        404 -> HfException("Hugging Face has no $what.")
        else -> HfException("Hugging Face answered $status for $what.")
    }

    companion object {
        const val CONNECT_TIMEOUT_MS = 15_000
        const val READ_TIMEOUT_MS = 60_000
        private const val MAX_REDIRECTS = 5
    }
}
