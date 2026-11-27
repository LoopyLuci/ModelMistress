package com.modelmistress.core.bench

/**
 * Models the advisor may suggest. Each is a Q4_K_M GGUF file on Hugging Face, so the owner can download it from the Models tab.
 * The sizes were read from the Hugging Face file listings; they are the sizes the download will take, in bytes.
 */
object CandidateCatalog {
    private const val MB = 1_048_576L

    val models: List<CandidateModel> = listOf(
        CandidateModel("Qwen2.5 0.5B Instruct", "Qwen/Qwen2.5-0.5B-Instruct-GGUF", "qwen2.5-0.5b-instruct-q4_k_m.gguf", 469 * MB, 0.5),
        CandidateModel("LFM2 1.2B", "LiquidAI/LFM2-1.2B-GGUF", "LFM2-1.2B-Q4_K_M.gguf", 697 * MB, 1.2),
        CandidateModel("SmolLM2 1.7B Instruct", "HuggingFaceTB/SmolLM2-1.7B-Instruct-GGUF", "smollm2-1.7b-instruct-q4_k_m.gguf", 1007 * MB, 1.7),
        CandidateModel("Qwen2.5 1.5B Instruct", "Qwen/Qwen2.5-1.5B-Instruct-GGUF", "qwen2.5-1.5b-instruct-q4_k_m.gguf", 1066 * MB, 1.5),
        CandidateModel("LFM2 2.6B", "LiquidAI/LFM2-2.6B-GGUF", "LFM2-2.6B-Q4_K_M.gguf", 1491 * MB, 2.6),
        CandidateModel("Qwen2.5 3B Instruct", "Qwen/Qwen2.5-3B-Instruct-GGUF", "qwen2.5-3b-instruct-q4_k_m.gguf", 2007 * MB, 3.0),
        CandidateModel("Qwen3 4B", "Qwen/Qwen3-4B-GGUF", "Qwen3-4B-Q4_K_M.gguf", 2382 * MB, 4.0),
    )
}
