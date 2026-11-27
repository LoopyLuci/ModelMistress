package com.modelmistress.core.models

import com.modelmistress.core.json.JsonException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/** The answer of `GET /api/models/org/repo/revision/main?blobs=true`, trimmed to the fields the app reads. */
internal val REPO_LISTING_JSON = """
{
  "sha": "abc123",
  "siblings": [
    {"rfilename": "README.md", "size": 10},
    {"rfilename": "LFM2-2.6B-Q8_0.gguf", "lfs": {"size": 2600, "sha256": "${"b".repeat(64)}"}},
    {"rfilename": "LFM2-2.6B-Q4_K_M.gguf", "lfs": {"size": 1500, "sha256": "${"a".repeat(64)}"}},
    {"rfilename": "LFM2-2.6B-Q2_K.gguf", "size": 900},
    {"rfilename": "mmproj-F16.gguf", "lfs": {"size": 300, "sha256": "${"c".repeat(64)}"}},
    {"rfilename": "Big-Q6_K-00001-of-00002.gguf", "lfs": {"size": 4000, "sha256": "${"d".repeat(64)}"}}
  ]
}
""".trimIndent()

class HuggingFaceTest {
    @Test
    fun specReadsWebFormsAndQuantisations() {
        assertEquals(HfSpec("unsloth/Qwen3-8B-GGUF", null), HfSpec.parse("https://huggingface.co/unsloth/Qwen3-8B-GGUF/"))
        assertEquals(HfSpec("org/repo", "Q4_K_M"), HfSpec.parse("hf.co/org/repo:Q4_K_M"))
        assertEquals(HfSpec("org/repo", null), HfSpec.parse("org/repo:"))
    }

    @Test
    fun specRefusesOtherPaths() {
        for (bad in listOf("../etc", "one", "a/b/c", "org/repo:bad quant!", "")) {
            try {
                HfSpec.parse(bad)
                fail("The spec '$bad' should have been refused.")
            } catch (expected: IllegalArgumentException) {
                assertTrue(expected.message.orEmpty().isNotEmpty())
            }
        }
    }

    @Test
    fun quantisationComesFromTheFileName() {
        assertEquals("Q4_0", HfFile("LFM2-2.6B-Q4_0.gguf", 1, null).quant)
        assertEquals("IQ4_XS", HfFile("Model-IQ4_XS.gguf", 1, null).quant)
        assertEquals("Q4_K_M", HfFile("Llama-3.2-3B-Q4_K_M.gguf", 1, null).quant)
        assertNull(HfFile("plain.gguf", 1, null).quant)
    }

    @Test
    fun readsTheRepoListingAndLeavesOutProjectorsAndSplitParts() {
        val listing = HuggingFaceFiles.parseRepoFiles(REPO_LISTING_JSON)
        assertEquals("abc123", listing.revision)
        assertEquals(6, listing.files.size)
        val q2 = listing.files.first { it.name == "LFM2-2.6B-Q2_K.gguf" }
        assertEquals(900L, q2.sizeBytes)
        assertNull(q2.sha256)
        assertEquals("a".repeat(64), listing.files.first { it.name == "LFM2-2.6B-Q4_K_M.gguf" }.sha256)

        val candidates = HuggingFaceFiles.candidates(listing.files).map { it.name }
        assertEquals(listOf("LFM2-2.6B-Q2_K.gguf", "LFM2-2.6B-Q4_K_M.gguf", "LFM2-2.6B-Q8_0.gguf"), candidates)
    }

    @Test
    fun choosesTheNamedQuantOrTheUsualDefault() {
        val files = HuggingFaceFiles.parseRepoFiles(REPO_LISTING_JSON).files
        assertEquals("LFM2-2.6B-Q4_K_M.gguf", HuggingFaceFiles.choose(files, null).name)
        assertEquals("LFM2-2.6B-Q8_0.gguf", HuggingFaceFiles.choose(files, "q8_0").name)
        try {
            HuggingFaceFiles.choose(files, "Q5_K_M")
            fail("A quant the repo does not have must be refused.")
        } catch (expected: HfException) {
            assertTrue(expected.message.orEmpty().contains("No Q5_K_M model"))
        }
    }

    @Test
    fun aRepoWithOnlyProjectorsHasNothingToChoose() {
        val files = listOf(HfFile("mmproj-F16.gguf", 300, null), HfFile("README.md", 10, null))
        try {
            HuggingFaceFiles.choose(files, null)
            fail("A repo with no model must be refused.")
        } catch (expected: HfException) {
            assertTrue(expected.message.orEmpty().contains("no single-file GGUF"))
        }
    }

    @Test
    fun resolveUrlEncodesEachPathSegment() {
        val url = HuggingFaceFiles.resolveUrl(
            "https://huggingface.co",
            HfSpec("org/repo", null),
            "abc",
            HfFile("sub dir/a b.gguf", 1, null),
        )
        assertEquals("https://huggingface.co/org/repo/resolve/abc/sub%20dir/a%20b.gguf", url)
    }

    @Test
    fun refusesAListingThatIsNotJson() {
        try {
            HuggingFaceFiles.parseRepoFiles("<html>not json</html>")
            fail("A listing that is not JSON must be refused.")
        } catch (expected: JsonException) {
            assertTrue(expected.message.orEmpty().isNotEmpty())
        }
    }
}
