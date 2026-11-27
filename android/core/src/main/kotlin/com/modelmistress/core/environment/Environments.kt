package com.modelmistress.core.environment

import com.modelmistress.core.json.Json
import com.modelmistress.core.json.JsonException
import com.modelmistress.core.tools.Workspace
import java.io.File
import java.io.IOException
import java.util.UUID

/**
 * A sandbox: a folder of its own on this device, where a chat's tools read and change files. [folder] is relative to the
 * store's base folder. [links] are the ids of the sandboxes this one is linked to. A link works both ways.
 */
data class Environment(
    val id: String,
    val name: String,
    val folder: String,
    val createdAt: Long,
    val links: List<String>,
)

/**
 * Keeps the sandboxes on this device, listed in one index file under [base]. Every sandbox is its own folder, and the tools
 * of a chat can reach only that folder and the sandboxes it is linked to. The sandboxes are app-private: nothing outside
 * them is reachable from the tools.
 *
 * The first sandbox, [DEFAULT_ID], keeps the files that were added before sandboxes existed. Chats saved before then work in it.
 */
class EnvironmentStore(private val base: File, private val now: () -> Long = { System.currentTimeMillis() }) {
    private val index = File(base, INDEX_NAME)

    init {
        base.mkdirs()
        save(read())
    }

    /** Every sandbox, the default one first. */
    @Synchronized
    fun list(): List<Environment> = read()

    /** The sandbox with [id], or null when there is none. */
    @Synchronized
    fun get(id: String): Environment? = read().firstOrNull { it.id == id }

    /** The folder that holds sandbox [id] on this device, or null when there is no such sandbox. */
    @Synchronized
    fun folderOf(id: String): File? = get(id)?.let { File(base, it.folder) }

    /** Makes a new, empty sandbox called [name]. Its folder is made when the first file goes in. */
    @Synchronized
    fun create(name: String): Environment {
        val id = UUID.randomUUID().toString()
        val environment = Environment(id, cleanName(name), "$FOLDER_PREFIX/$id", now(), emptyList())
        save(read() + environment)
        return environment
    }

    /** Gives the sandbox [id] a new name. False when there is no such sandbox. */
    @Synchronized
    fun rename(id: String, name: String): Boolean {
        val all = read()
        if (all.none { it.id == id }) return false
        save(all.map { if (it.id == id) it.copy(name = cleanName(name)) else it })
        return true
    }

    /**
     * Removes the sandbox [id] with every file in it, and takes it out of the links of the others. False when there is no
     * such sandbox.
     */
    @Synchronized
    fun delete(id: String): Boolean {
        val all = read()
        val target = all.firstOrNull { it.id == id } ?: return false
        save(all.filter { it.id != id }.map { it.copy(links = it.links - id) })
        File(base, target.folder).deleteRecursively()
        return true
    }

    /** Links the sandboxes [id] and [otherId], both ways. False when either is missing, or they are the same one. */
    @Synchronized
    fun link(id: String, otherId: String): Boolean {
        if (id == otherId) return false
        val all = read()
        if (all.none { it.id == id } || all.none { it.id == otherId }) return false
        save(
            all.map { env ->
                when (env.id) {
                    id -> env.copy(links = (env.links + otherId).distinct())
                    otherId -> env.copy(links = (env.links + id).distinct())
                    else -> env
                }
            },
        )
        return true
    }

    /** Removes the link between [id] and [otherId], both ways. False when they were not linked. */
    @Synchronized
    fun unlink(id: String, otherId: String): Boolean {
        val all = read()
        val linked = all.firstOrNull { it.id == id }?.links?.contains(otherId) == true
        if (!linked) return false
        save(
            all.map { env ->
                when (env.id) {
                    id -> env.copy(links = env.links - otherId)
                    otherId -> env.copy(links = env.links - id)
                    else -> env
                }
            },
        )
        return true
    }

    /**
     * The tools' view of sandbox [id]: its own folder, with each linked sandbox under its name. Null when there is no such
     * sandbox. Linked names are made safe and kept distinct.
     */
    @Synchronized
    fun workspace(id: String): Workspace? {
        val all = read()
        val environment = all.firstOrNull { it.id == id } ?: return null
        val byId = all.associateBy { it.id }
        val names = LinkedHashMap<String, File>()
        for (linkedId in environment.links) {
            val other = byId[linkedId] ?: continue
            val stem = Workspace.safeName(other.name).take(MAX_LINK_NAME).trimEnd()
            var name = stem
            var number = 2
            while (name in names) {
                name = "$stem $number"
                number++
            }
            names[name] = File(base, other.folder)
        }
        return Workspace(File(base, environment.folder), names)
    }

    private fun read(): List<Environment> {
        val parsed = if (index.isFile) {
            runCatching { parse(index.readText(Charsets.UTF_8)) }.getOrElse { emptyList() }
        } else {
            emptyList()
        }
        return if (parsed.any { it.id == DEFAULT_ID }) parsed else listOf(defaultEnvironment()) + parsed
    }

    private fun save(environments: List<Environment>) {
        val text = Json.write(
            linkedMapOf(
                "version" to VERSION,
                "environments" to environments.map { environment ->
                    linkedMapOf(
                        "id" to environment.id,
                        "name" to environment.name,
                        "folder" to environment.folder,
                        "created_at" to environment.createdAt,
                        "links" to environment.links,
                    )
                },
            ),
        )
        val partial = File(base, "$INDEX_NAME.part")
        try {
            partial.writeText(text, Charsets.UTF_8)
            if (index.exists() && !index.delete()) throw IOException("The sandbox list could not be replaced.")
            if (!partial.renameTo(index)) throw IOException("The sandbox list could not be saved.")
        } finally {
            partial.delete()
        }
    }

    companion object {
        /** The sandbox that holds the files added before sandboxes existed. */
        const val DEFAULT_ID = "default"
        private const val DEFAULT_FOLDER = "workspace"
        private const val FOLDER_PREFIX = "environments"
        private const val INDEX_NAME = "environments.json"
        private const val VERSION = 1
        private const val MAX_NAME = 60
        private const val MAX_LINK_NAME = 70

        private val ID_PATTERN = Regex("[A-Za-z0-9-]{1,64}")
        private val FOLDER_PATTERN = Regex("workspace|environments/[A-Za-z0-9-]{1,64}")

        fun defaultEnvironment(): Environment = Environment(DEFAULT_ID, "Older shared files", DEFAULT_FOLDER, 0L, emptyList())

        private fun cleanName(raw: String): String = raw.trim().take(MAX_NAME).ifEmpty { "Sandbox" }

        private fun parse(text: String): List<Environment> {
            val root = Json.parse(text) as? Map<*, *> ?: throw JsonException("The sandbox list must be a JSON object.")
            if ((root["version"] as? Number)?.toInt() != VERSION) throw JsonException("The sandbox list is from an unknown version.")
            val entries = root["environments"] as? List<*> ?: throw JsonException("The sandbox list has no entries.")
            return entries.map { entry ->
                val item = entry as? Map<*, *> ?: throw JsonException("A sandbox entry must be a JSON object.")
                val id = item["id"] as? String ?: throw JsonException("A sandbox has no id.")
                val folder = item["folder"] as? String ?: throw JsonException("A sandbox has no folder.")
                require(ID_PATTERN.matches(id) || id == DEFAULT_ID) { "A sandbox id is not usable." }
                require(FOLDER_PATTERN.matches(folder)) { "A sandbox folder is not usable." }
                Environment(
                    id = id,
                    name = item["name"] as? String ?: "Sandbox",
                    folder = folder,
                    createdAt = (item["created_at"] as? Number)?.toLong() ?: 0L,
                    links = (item["links"] as? List<*>)?.filterIsInstance<String>()?.distinct() ?: emptyList(),
                )
            }
        }
    }
}
