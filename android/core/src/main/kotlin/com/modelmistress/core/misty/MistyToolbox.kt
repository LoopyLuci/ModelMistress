package com.modelmistress.core.misty

import com.modelmistress.core.tools.Bounded
import com.modelmistress.core.tools.ToolCall
import com.modelmistress.core.tools.ToolClass
import com.modelmistress.core.tools.ToolOutcome
import com.modelmistress.core.tools.ToolSource
import com.modelmistress.core.tools.ToolSpec

/**
 * The tools Misty offers on top of the device's own: what this phone reports about itself. Everything else, including
 * the file tools and the optional write, comes from [base].
 */
class MistyToolbox(
    private val base: ToolSource,
    private val deviceStatus: () -> String,
    private val installedModels: () -> String,
) : ToolSource {
    override val specs: List<ToolSpec> = base.specs + listOf(
        ToolSpec(
            name = "device_status",
            toolClass = ToolClass.READ,
            description = "Free memory on this device, and whether Needle is ready.",
            parameters = emptyList(),
        ),
        ToolSpec(
            name = "models_list",
            toolClass = ToolClass.READ,
            description = "The models installed on this device.",
            parameters = emptyList(),
        ),
    )

    override fun spec(name: String): ToolSpec? = specs.firstOrNull { it.name == name }

    override fun run(call: ToolCall): ToolOutcome = when (call.name) {
        "device_status" -> answer(deviceStatus)
        "models_list" -> answer(installedModels)
        else -> base.run(call)
    }

    private fun answer(block: () -> String): ToolOutcome = try {
        ToolOutcome.Ok(Bounded.cap(block()))
    } catch (error: Exception) {
        ToolOutcome.Failed(error.message ?: "the tool failed")
    }
}
