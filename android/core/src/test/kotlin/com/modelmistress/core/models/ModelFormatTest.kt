package com.modelmistress.core.models

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ModelFormatTest {
    @Test
    fun recognisesEachExtension() {
        assertEquals(ModelFormat.GGUF, ModelFormat.of("qwen.gguf"))
        assertEquals(ModelFormat.LITERT_LM, ModelFormat.of("gemma.litertlm"))
    }

    @Test
    fun refusesOtherNamesAndPartialDownloads() {
        assertNull(ModelFormat.of("notes.txt"))
        assertNull(ModelFormat.of("gguf"))
        assertNull(ModelFormat.of("qwen.gguf.part"))
        assertNull(ModelFormat.of("gemma.litertlm.part"))
    }
}
