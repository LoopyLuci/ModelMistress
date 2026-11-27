#include <jni.h>

#include <algorithm>
#include <cstdint>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <vector>

#include "llama.h"

// Handle-based bridge: a model is loaded once and kept until nativeFree, and each reply clears the KV cache and decodes
// the whole conversation again. Replies stream back through the Kotlin TokenCallback one piece at a time.

namespace {

constexpr int32_t kDefaultContextTokens = 2048;
constexpr int32_t kMinContextTokens = 128;
constexpr int32_t kBatchTokens = 256;
constexpr int32_t kMaxMessages = 256;
constexpr int32_t kMaxNewTokens = 4096;
constexpr jint kReasonStop = 0;
constexpr jint kReasonLength = 1;

std::once_flag g_backend_init;

struct Engine {
    llama_model* model = nullptr;
    llama_context* context = nullptr;
    const llama_vocab* vocab = nullptr;
    int32_t context_tokens = 0;

    ~Engine() {
        if (context != nullptr) llama_free(context);
        if (model != nullptr) llama_model_free(model);
    }
};

void throwJava(JNIEnv* env, const char* className, const char* message) {
    jclass cls = env->FindClass(className);
    if (cls != nullptr) {
        env->ThrowNew(cls, message);
        env->DeleteLocalRef(cls);
    }
}

std::string copyString(JNIEnv* env, jstring value, const char* what) {
    if (value == nullptr) throw std::invalid_argument(std::string(what) + " must not be null.");
    const char* chars = env->GetStringUTFChars(value, nullptr);
    if (chars == nullptr) throw std::runtime_error(std::string("Could not read ") + what + ".");
    std::string result(chars);
    env->ReleaseStringUTFChars(value, chars);
    return result;
}

std::string copyBytes(JNIEnv* env, jbyteArray bytes) {
    if (bytes == nullptr) throw std::invalid_argument("A chat message field must not be null.");
    const jsize length = env->GetArrayLength(bytes);
    std::string result(static_cast<size_t>(length), '\0');
    if (length > 0) {
        env->GetByteArrayRegion(bytes, 0, length, reinterpret_cast<jbyte*>(&result[0]));
        if (env->ExceptionCheck()) throw std::runtime_error("Could not read a chat message.");
    }
    return result;
}

Engine* engineFrom(jlong handle) {
    if (handle == 0) throw std::invalid_argument("The model is not loaded.");
    return reinterpret_cast<Engine*>(handle);
}

void decodePrompt(llama_context* context, std::vector<llama_token>& tokens) {
    for (size_t offset = 0; offset < tokens.size(); offset += kBatchTokens) {
        const auto count = static_cast<int32_t>(
            std::min(static_cast<size_t>(kBatchTokens), tokens.size() - offset)
        );
        llama_batch batch = llama_batch_get_one(tokens.data() + offset, count);
        if (llama_decode(context, batch) != 0) {
            throw std::runtime_error("llama.cpp could not process the conversation.");
        }
    }
}

// Sends one piece of reply text to Kotlin. A false answer, or an exception in the callback, stops the reply.
bool sendPiece(JNIEnv* env, jobject callback, jmethodID onToken, const std::string& piece) {
    jbyteArray chunk = env->NewByteArray(static_cast<jsize>(piece.size()));
    if (chunk == nullptr) throw std::runtime_error("Could not allocate a reply chunk.");
    env->SetByteArrayRegion(
        chunk, 0, static_cast<jsize>(piece.size()), reinterpret_cast<const jbyte*>(piece.data())
    );
    const jboolean keepGoing = env->CallBooleanMethod(callback, onToken, chunk);
    env->DeleteLocalRef(chunk);
    return !env->ExceptionCheck() && keepGoing == JNI_TRUE;
}

}  // namespace

extern "C" JNIEXPORT jlong JNICALL
Java_com_modelmistress_engine_LlamaNative_nativeLoad(
    JNIEnv* env,
    jclass,
    jstring modelPath,
    jint contextTokens,
    jint threads
) {
    try {
        const std::string path = copyString(env, modelPath, "The GGUF path");
        std::call_once(g_backend_init, [] { llama_backend_init(); });

        auto modelParams = llama_model_default_params();
        modelParams.n_gpu_layers = 0;
        std::unique_ptr<Engine> engine(new Engine());
        engine->model = llama_model_load_from_file(path.c_str(), modelParams);
        if (engine->model == nullptr) throw std::runtime_error("llama.cpp could not load this GGUF file.");
        engine->vocab = llama_model_get_vocab(engine->model);

        int32_t wanted = contextTokens > 0 ? contextTokens : kDefaultContextTokens;
        const int32_t trained = llama_model_n_ctx_train(engine->model);
        if (trained > 0) wanted = std::min(wanted, trained);
        wanted = std::max(wanted, kMinContextTokens);

        auto contextParams = llama_context_default_params();
        contextParams.n_ctx = static_cast<uint32_t>(wanted);
        contextParams.n_batch = static_cast<uint32_t>(std::min(kBatchTokens, wanted));
        contextParams.n_ubatch = contextParams.n_batch;
        const int32_t workers = std::max(1, static_cast<int32_t>(threads));
        contextParams.n_threads = workers;
        contextParams.n_threads_batch = workers;
        engine->context = llama_init_from_model(engine->model, contextParams);
        if (engine->context == nullptr) throw std::runtime_error("llama.cpp could not allocate the context.");
        engine->context_tokens = wanted;
        return reinterpret_cast<jlong>(engine.release());
    } catch (const std::invalid_argument& error) {
        if (!env->ExceptionCheck()) throwJava(env, "java/lang/IllegalArgumentException", error.what());
    } catch (const std::exception& error) {
        if (!env->ExceptionCheck()) throwJava(env, "java/lang/IllegalStateException", error.what());
    }
    return 0;
}

extern "C" JNIEXPORT jint JNICALL
Java_com_modelmistress_engine_LlamaNative_nativeContextTokens(JNIEnv* env, jclass, jlong handle) {
    try {
        return engineFrom(handle)->context_tokens;
    } catch (const std::exception& error) {
        if (!env->ExceptionCheck()) throwJava(env, "java/lang/IllegalStateException", error.what());
    }
    return 0;
}

extern "C" JNIEXPORT void JNICALL
Java_com_modelmistress_engine_LlamaNative_nativeFree(JNIEnv*, jclass, jlong handle) {
    delete reinterpret_cast<Engine*>(handle);
}

// Returns { prompt tokens, generated tokens, finish reason } and streams each piece of text through callback.onToken.
extern "C" JNIEXPORT jintArray JNICALL
Java_com_modelmistress_engine_LlamaNative_nativeGenerate(
    JNIEnv* env,
    jclass,
    jlong handle,
    jobjectArray roles,
    jobjectArray contents,
    jint maxNewTokens,
    jfloat temperature,
    jfloat topP,
    jint topK,
    jlong seed,
    jobject callback
) {
    try {
        Engine* engine = engineFrom(handle);
        if (roles == nullptr || contents == nullptr || callback == nullptr) {
            throw std::invalid_argument("Messages and a token callback are required.");
        }
        const jsize count = env->GetArrayLength(roles);
        if (count < 1 || count > kMaxMessages || env->GetArrayLength(contents) != count) {
            throw std::invalid_argument("The chat message list is invalid.");
        }
        if (maxNewTokens < 1 || maxNewTokens > kMaxNewTokens) {
            throw std::invalid_argument("The reply token limit is invalid.");
        }

        std::vector<std::string> roleTexts;
        std::vector<std::string> contentTexts;
        roleTexts.reserve(static_cast<size_t>(count));
        contentTexts.reserve(static_cast<size_t>(count));
        for (jsize index = 0; index < count; ++index) {
            auto role = static_cast<jbyteArray>(env->GetObjectArrayElement(roles, index));
            auto content = static_cast<jbyteArray>(env->GetObjectArrayElement(contents, index));
            roleTexts.push_back(copyBytes(env, role));
            contentTexts.push_back(copyBytes(env, content));
            env->DeleteLocalRef(role);
            env->DeleteLocalRef(content);
            const std::string& roleText = roleTexts.back();
            if (roleText != "system" && roleText != "user" && roleText != "assistant") {
                throw std::invalid_argument("A chat message has an unsupported role.");
            }
        }
        std::vector<llama_chat_message> chat;
        chat.reserve(static_cast<size_t>(count));
        for (size_t index = 0; index < static_cast<size_t>(count); ++index) {
            chat.push_back({roleTexts[index].c_str(), contentTexts[index].c_str()});
        }

        const char* chatTemplate = llama_model_chat_template(engine->model, nullptr);
        if (chatTemplate == nullptr || *chatTemplate == '\0') chatTemplate = "chatml";
        const int32_t needed = llama_chat_apply_template(
            chatTemplate, chat.data(), chat.size(), true, nullptr, 0
        );
        if (needed <= 0) throw std::runtime_error("The chat template could not format the conversation.");
        std::vector<char> prompt(static_cast<size_t>(needed) + 1);
        const int32_t written = llama_chat_apply_template(
            chatTemplate, chat.data(), chat.size(), true, prompt.data(), static_cast<int32_t>(prompt.size())
        );
        if (written <= 0 || written > needed) {
            throw std::runtime_error("The chat template could not format the conversation.");
        }

        std::vector<llama_token> tokens(static_cast<size_t>(written) + 8);
        int32_t tokenCount = llama_tokenize(
            engine->vocab, prompt.data(), written, tokens.data(), static_cast<int32_t>(tokens.size()), true, true
        );
        if (tokenCount < 0) {
            tokens.resize(static_cast<size_t>(-tokenCount));
            tokenCount = llama_tokenize(
                engine->vocab, prompt.data(), written, tokens.data(), static_cast<int32_t>(tokens.size()), true, true
            );
        }
        if (tokenCount <= 0) throw std::runtime_error("The conversation could not be tokenized.");
        tokens.resize(static_cast<size_t>(tokenCount));
        if (tokenCount + maxNewTokens > engine->context_tokens) {
            throw std::invalid_argument(
                "The conversation and the reply need more than " + std::to_string(engine->context_tokens) +
                " tokens. Start a new chat or lower max_tokens."
            );
        }

        llama_memory_clear(llama_get_memory(engine->context), true);
        decodePrompt(engine->context, tokens);

        auto samplerParams = llama_sampler_chain_default_params();
        std::unique_ptr<llama_sampler, decltype(&llama_sampler_free)> sampler(
            llama_sampler_chain_init(samplerParams), llama_sampler_free
        );
        if (!sampler) throw std::runtime_error("llama.cpp could not start sampling.");
        if (temperature <= 0.0f) {
            llama_sampler_chain_add(sampler.get(), llama_sampler_init_greedy());
        } else {
            llama_sampler_chain_add(sampler.get(), llama_sampler_init_top_k(topK));
            llama_sampler_chain_add(sampler.get(), llama_sampler_init_top_p(topP, 1));
            llama_sampler_chain_add(sampler.get(), llama_sampler_init_temp(temperature));
            llama_sampler_chain_add(sampler.get(), llama_sampler_init_dist(static_cast<uint32_t>(seed)));
        }

        jclass callbackClass = env->GetObjectClass(callback);
        jmethodID onToken = callbackClass == nullptr
            ? nullptr
            : env->GetMethodID(callbackClass, "onToken", "([B)Z");
        if (callbackClass != nullptr) env->DeleteLocalRef(callbackClass);
        if (onToken == nullptr) throw std::runtime_error("The token callback has no onToken method.");

        jint generated = 0;
        jint reason = kReasonLength;
        std::vector<char> piece(256);
        std::vector<llama_token> single(1);
        while (generated < maxNewTokens) {
            const llama_token token = llama_sampler_sample(sampler.get(), engine->context, -1);
            if (llama_vocab_is_eog(engine->vocab, token)) {
                reason = kReasonStop;
                break;
            }
            llama_sampler_accept(sampler.get(), token);

            int32_t size = llama_token_to_piece(
                engine->vocab, token, piece.data(), static_cast<int32_t>(piece.size()), 0, false
            );
            if (size < 0) {
                piece.resize(static_cast<size_t>(-size));
                size = llama_token_to_piece(
                    engine->vocab, token, piece.data(), static_cast<int32_t>(piece.size()), 0, false
                );
            }
            if (size < 0) throw std::runtime_error("A generated token could not be turned into text.");
            ++generated;

            bool keepGoing = true;
            if (size > 0) {
                keepGoing = sendPiece(env, callback, onToken, std::string(piece.data(), static_cast<size_t>(size)));
            }
            if (env->ExceptionCheck()) return nullptr;
            if (!keepGoing) {
                reason = kReasonStop;
                break;
            }
            if (generated >= maxNewTokens) break;

            single[0] = token;
            llama_batch batch = llama_batch_get_one(single.data(), 1);
            if (llama_decode(engine->context, batch) != 0) {
                throw std::runtime_error("llama.cpp failed while writing the reply.");
            }
        }

        jintArray result = env->NewIntArray(3);
        if (result == nullptr) throw std::runtime_error("Could not return the reply statistics.");
        const jint values[3] = {static_cast<jint>(tokenCount), generated, reason};
        env->SetIntArrayRegion(result, 0, 3, values);
        return result;
    } catch (const std::invalid_argument& error) {
        if (!env->ExceptionCheck()) throwJava(env, "java/lang/IllegalArgumentException", error.what());
    } catch (const std::exception& error) {
        if (!env->ExceptionCheck()) throwJava(env, "java/lang/IllegalStateException", error.what());
    }
    return nullptr;
}
